package com.greengolddog.dayweave.notifications

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.work.BackoffPolicy
import androidx.work.CoroutineWorker
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequest
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.await
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.MainActivity
import com.greengolddog.dayweave.R
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.TimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.isTimedBreakNotificationDigest
import com.greengolddog.dayweave.model.unacknowledgedTimedBreakNotificationIdentity
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.isActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Process-wide join point between notification delivery and authoritative cancellation.
 *
 * WorkManager's cancellation Operation confirms the scheduling mutation, but it does not promise
 * that an already-running CoroutineWorker has returned. Serializing the final fixed-ID cancel
 * behind any in-flight post guarantees that a completed transition barrier cannot be followed by
 * a stale notification from this process.
 */
private val timedBreakNotificationSideEffectMutex = Mutex()

private suspend fun <T> withTimedBreakNotificationSideEffect(
    block: suspend () -> T,
): T = timedBreakNotificationSideEffectMutex.withLock { block() }

internal suspend fun cancelTimedBreakNotificationWorkAndDisplayedAlert(
    cancelWork: suspend () -> Unit,
    cancelDisplayedAlert: () -> Unit,
) {
    try {
        cancelWork()
    } finally {
        // Cancellation of the caller must not leave an alert behind after scheduler cancellation.
        withContext(NonCancellable) {
            withTimedBreakNotificationSideEffect { cancelDisplayedAlert() }
        }
    }
}

/** One durable OS job exists for the exact currently persisted canonical timed pause. */
internal interface TimedBreakNotificationWorkBackend {
    suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    )
    suspend fun cancelWorkAndNotification()
}

/**
 * Reconciles only successfully encrypted planner generations. In-memory mutations that have not
 * reached SQLCipher cannot schedule, replace, or cancel an external notification side effect.
 */
internal class TimedBreakNotificationCoordinator(
    private val backend: TimedBreakNotificationWorkBackend,
) {
    private val mutex = Mutex()
    private var initialized = false
    private var scheduledDigest: String? = null

    /** False keeps the same durable state retryable; cancellation is never swallowed. */
    suspend fun reconcile(durableState: DayWeaveUiState): Boolean = mutex.withLock {
        val identity = durableState.unacknowledgedTimedBreakNotificationIdentity()
        val nextDigest = identity?.digest
        if (initialized && scheduledDigest == nextDigest) return@withLock true

        try {
            if (identity == null) {
                backend.cancelWorkAndNotification()
            } else {
                backend.ensure(
                    identity = identity,
                    clearDisplayedBeforeSchedule =
                        durableState.lastBreakEndNotificationAttemptDigest != nextDigest,
                )
            }
            initialized = true
            scheduledDigest = nextDigest
            true
        } catch (error: CancellationException) {
            initialized = false
            scheduledDigest = null
            throw error
        } catch (_: Exception) {
            false
        }
    }

    /**
     * Awaited barrier used before an authoritative transition can invalidate the current alert.
     * Clearing the memoized digest makes a failed transition's unchanged durable generation
     * schedulable again through an explicit [reconcile] call.
     */
    suspend fun cancelForAuthoritativeTransition(): Boolean = mutex.withLock {
        try {
            backend.cancelWorkAndNotification()
            initialized = true
            scheduledDigest = null
            true
        } catch (error: CancellationException) {
            // WorkManager or the final fixed-ID cancel may already have changed external state.
            // Never retain a memoized digest after an ambiguous/partial cancellation.
            initialized = false
            scheduledDigest = null
            throw error
        } catch (_: Exception) {
            initialized = false
            scheduledDigest = null
            false
        }
    }
}

/**
 * A failed barrier may already have cancelled work or a displayed alert. Restore the unchanged
 * encrypted truth synchronously; if the scheduler is still unavailable, queue its normal retry
 * path before reporting that the authoritative transition must not proceed.
 */
internal suspend fun cancelTimedBreakNotificationAndRestoreOnFailure(
    coordinator: TimedBreakNotificationCoordinator,
    unchangedDurableState: DayWeaveUiState?,
    queueReconciliationRetry: suspend () -> Unit,
): Boolean {
    suspend fun restoreUnchangedTruth() {
        if (unchangedDurableState == null) return
        val restored = try {
            coordinator.reconcile(unchangedDurableState)
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            false
        }
        if (!restored) queueReconciliationRetry()
    }

    return try {
        if (coordinator.cancelForAuthoritativeTransition()) {
            true
        } else {
            restoreUnchangedTruth()
            false
        }
    } catch (error: CancellationException) {
        // Cancellation can arrive after WorkManager or NotificationManager changed state. Restore
        // under a non-cancellable context, then preserve structured cancellation for the caller.
        withContext(NonCancellable) {
            try {
                restoreUnchangedTruth()
            } catch (_: Exception) {
                // Preserve the caller's original cancellation while leaving the normal collector
                // a queued chance to repair any still-ambiguous external state.
                runCatching { queueReconciliationRetry() }
            }
        }
        throw error
    }
}

/** A changed durable generation cancels the prior retry loop; transient backend failures do not. */
internal suspend fun reconcileTimedBreakNotificationStates(
    durableStates: Flow<DayWeaveUiState>,
    coordinator: TimedBreakNotificationCoordinator,
    retryDelayMillis: Long = 30_000L,
) {
    require(retryDelayMillis >= 0L)
    durableStates.collectLatest { durableState ->
        while (currentCoroutineContext().isActive && !coordinator.reconcile(durableState)) {
            delay(retryDelayMillis)
        }
    }
}

internal class WorkManagerTimedBreakNotificationBackend(
    context: Context,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) : TimedBreakNotificationWorkBackend {
    private val applicationContext = context.applicationContext
    private val workManager = WorkManager.getInstance(applicationContext)
    private val notifications = AndroidTimedBreakNotificationGateway(applicationContext)

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        val active = workManager.getWorkInfosForUniqueWorkFlow(UNIQUE_WORK_NAME).first()
            .filter { it.state == WorkInfo.State.ENQUEUED || it.state == WorkInfo.State.RUNNING }
        val exactTag = IDENTITY_TAG_PREFIX + identity.digest
        if (active.any { exactTag in it.tags }) return

        if (active.isNotEmpty()) {
            cancelTimedBreakNotificationWorkAndDisplayedAlert(
                cancelWork = { workManager.cancelUniqueWork(UNIQUE_WORK_NAME).await() },
                cancelDisplayedAlert = notifications::cancel,
            )
        } else if (clearDisplayedBeforeSchedule) {
            withTimedBreakNotificationSideEffect { notifications.cancel() }
        }
        workManager.enqueueUniqueWork(
            UNIQUE_WORK_NAME,
            EXISTING_WORK_POLICY,
            buildTimedBreakNotificationWorkRequest(identity, nowEpochMillis()),
        ).await()
    }

    override suspend fun cancelWorkAndNotification() {
        cancelTimedBreakNotificationWorkAndDisplayedAlert(
            cancelWork = { workManager.cancelUniqueWork(UNIQUE_WORK_NAME).await() },
            cancelDisplayedAlert = notifications::cancel,
        )
    }

    companion object {
        internal const val UNIQUE_WORK_NAME = "dayweave-current-timed-break-notification-v1"
        internal const val WORK_TAG = "dayweave-timed-break-notification"
        internal const val IDENTITY_TAG_PREFIX = "dayweave-timed-break-digest:"
        internal val EXISTING_WORK_POLICY = ExistingWorkPolicy.REPLACE
    }
}

internal fun buildTimedBreakNotificationWorkRequest(
    identity: TimedBreakNotificationIdentity,
    nowEpochMillis: Long,
): OneTimeWorkRequest = OneTimeWorkRequest.Builder(TimedBreakEndedWorker::class.java)
    .setInitialDelay(
        (identity.deadlineEpochMillis - nowEpochMillis).coerceAtLeast(0L),
        TimeUnit.MILLISECONDS,
    )
    // Clock changes can make an otherwise valid deadline job run early. Retry that case only;
    // notification/channel permission denial is terminal and cannot create a retry storm.
    .setBackoffCriteria(BackoffPolicy.LINEAR, 1L, TimeUnit.MINUTES)
    .setInputData(
        Data.Builder()
            .putString(TimedBreakEndedWorker.INPUT_IDENTITY_DIGEST, identity.digest)
            .build(),
    )
    .addTag(WorkManagerTimedBreakNotificationBackend.WORK_TAG)
    .addTag(WorkManagerTimedBreakNotificationBackend.IDENTITY_TAG_PREFIX + identity.digest)
    .build()

internal enum class TimedBreakPreparation {
    READY,
    NOT_DUE,
    STALE,
    ALREADY_HANDLED,
    UNAVAILABLE,
}

internal enum class TimedBreakClaimValidation {
    CURRENT,
    STALE,
    UNAVAILABLE,
}

internal interface TimedBreakNotificationStateAccess {
    suspend fun prepare(expectedDigest: String): TimedBreakPreparation
    suspend fun validateClaim(expectedDigest: String): TimedBreakClaimValidation
}

/** Reads and CAS-updates the singleton store, whose durableState contains SQLCipher-confirmed data. */
internal class PlannerTimedBreakNotificationStateAccess(
    private val plannerStore: PlannerStore,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) : TimedBreakNotificationStateAccess {
    override suspend fun prepare(expectedDigest: String): TimedBreakPreparation {
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) return TimedBreakPreparation.UNAVAILABLE
        val durableBefore = plannerStore.durableState.value
            ?: return TimedBreakPreparation.UNAVAILABLE
        classify(durableBefore, expectedDigest, requireClaimed = false)?.let {
            if (it != TimedBreakPreparation.READY) return it
        }
        classify(plannerStore.state.value, expectedDigest, requireClaimed = false)?.let {
            if (it != TimedBreakPreparation.READY) return it
        }

        val receipt = plannerStore.claimTimedBreakEndNotificationDelivery(expectedDigest)
            ?: return classify(
                plannerStore.state.value,
                expectedDigest,
                requireClaimed = false,
            )?.takeUnless { it == TimedBreakPreparation.READY }
                ?: TimedBreakPreparation.UNAVAILABLE
        if (!receipt.awaitDurable()) return TimedBreakPreparation.UNAVAILABLE

        val durable = plannerStore.durableState.value ?: return TimedBreakPreparation.UNAVAILABLE
        listOf(durable, plannerStore.state.value).forEach { state ->
            classify(state, expectedDigest, requireClaimed = true)?.let { result ->
                if (result != TimedBreakPreparation.READY) return result
            }
        }
        return TimedBreakPreparation.READY
    }

    override suspend fun validateClaim(expectedDigest: String): TimedBreakClaimValidation {
        if (plannerStore.loadState.value != PlannerLoadState.READY) {
            return TimedBreakClaimValidation.UNAVAILABLE
        }
        val durable = plannerStore.durableState.value
            ?: return TimedBreakClaimValidation.UNAVAILABLE
        val live = plannerStore.state.value
        return if (listOf(durable, live).all { state ->
                state.authoritativeTimedBreakNotificationIdentity()?.digest == expectedDigest &&
                    state.activeSession?.timedBreakEnded == true &&
                    state.lastBreakEndNotificationAttemptDigest == expectedDigest &&
                    state.acknowledgedBreakEndDigest != expectedDigest
            }
        ) {
            TimedBreakClaimValidation.CURRENT
        } else {
            TimedBreakClaimValidation.STALE
        }
    }

    /** Null means the selected state itself is not available yet. */
    private fun classify(
        state: DayWeaveUiState?,
        expectedDigest: String,
        requireClaimed: Boolean,
    ): TimedBreakPreparation? {
        state ?: return null
        val identity = state.authoritativeTimedBreakNotificationIdentity()
        return when {
            identity?.digest != expectedDigest -> TimedBreakPreparation.STALE
            nowEpochMillis() < identity.deadlineEpochMillis -> TimedBreakPreparation.NOT_DUE
            state.acknowledgedBreakEndDigest == expectedDigest ->
                TimedBreakPreparation.ALREADY_HANDLED
            !requireClaimed && state.lastBreakEndNotificationAttemptDigest == expectedDigest ->
                TimedBreakPreparation.ALREADY_HANDLED
            requireClaimed && (
                state.activeSession?.timedBreakEnded != true ||
                    state.lastBreakEndNotificationAttemptDigest != expectedDigest
                ) ->
                TimedBreakPreparation.STALE
            else -> TimedBreakPreparation.READY
        }
    }
}

internal enum class TimedBreakNotificationPostResult {
    POSTED,
    SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
}

data class TimedBreakNotificationSystemState(
    val runtimePermissionGranted: Boolean,
    val appNotificationsEnabled: Boolean,
    val channelEnabled: Boolean,
    val runtimePermissionPreviouslyRequested: Boolean,
) {
    companion object {
        val ENABLED = TimedBreakNotificationSystemState(
            runtimePermissionGranted = true,
            appNotificationsEnabled = true,
            channelEnabled = true,
            runtimePermissionPreviouslyRequested = false,
        )
    }
}

internal enum class TimedBreakReminderEnableAction {
    NONE,
    REQUEST_RUNTIME_PERMISSION,
    OPEN_NOTIFICATION_SETTINGS,
}

internal fun timedBreakReminderSystemEnableAction(
    sdkInt: Int,
    systemState: TimedBreakNotificationSystemState,
): TimedBreakReminderEnableAction = when {
    sdkInt >= Build.VERSION_CODES.TIRAMISU &&
        !systemState.runtimePermissionGranted &&
        !systemState.runtimePermissionPreviouslyRequested ->
        TimedBreakReminderEnableAction.REQUEST_RUNTIME_PERMISSION
    sdkInt >= Build.VERSION_CODES.TIRAMISU && !systemState.runtimePermissionGranted ->
        TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS
    !systemState.appNotificationsEnabled || !systemState.channelEnabled ->
        TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS
    else -> TimedBreakReminderEnableAction.NONE
}

/** Recovery affordance is derived from encrypted future-break truth, never an ephemeral event. */
internal fun timedBreakReminderEnableAction(
    durableState: DayWeaveUiState?,
    nowEpochMillis: Long,
    sdkInt: Int,
    systemState: TimedBreakNotificationSystemState,
): TimedBreakReminderEnableAction {
    val identity = durableState?.authoritativeTimedBreakNotificationIdentity()
        ?: return TimedBreakReminderEnableAction.NONE
    if (identity.deadlineEpochMillis <= nowEpochMillis) {
        return TimedBreakReminderEnableAction.NONE
    }
    return timedBreakReminderSystemEnableAction(sdkInt, systemState)
}

internal interface TimedBreakNotificationGateway {
    fun post(identityDigest: String): TimedBreakNotificationPostResult
    fun cancel()
}

internal enum class TimedBreakDeliveryCompletion {
    SUCCESS,
    RETRY,
}

/** Small deterministic orchestration seam shared by the production worker and unit tests. */
internal class TimedBreakNotificationDelivery(
    private val stateAccess: TimedBreakNotificationStateAccess,
    private val gateway: TimedBreakNotificationGateway,
) {
    suspend fun deliver(expectedDigest: String): TimedBreakDeliveryCompletion =
        withTimedBreakNotificationSideEffect { deliverLocked(expectedDigest) }

    private suspend fun deliverLocked(expectedDigest: String): TimedBreakDeliveryCompletion {
        return when (stateAccess.prepare(expectedDigest)) {
            TimedBreakPreparation.NOT_DUE -> TimedBreakDeliveryCompletion.RETRY
            TimedBreakPreparation.STALE,
            TimedBreakPreparation.ALREADY_HANDLED,
            TimedBreakPreparation.UNAVAILABLE,
            -> TimedBreakDeliveryCompletion.SUCCESS
            TimedBreakPreparation.READY -> {
                // The exact encrypted at-most-once claim is already durable. Revalidate once more
                // immediately before notify() so a transition that won after the claim suppresses
                // the external side effect even before WorkManager cancellation arrives.
                if (stateAccess.validateClaim(expectedDigest) != TimedBreakClaimValidation.CURRENT) {
                    return TimedBreakDeliveryCompletion.SUCCESS
                }
                try {
                    gateway.post(expectedDigest)
                } catch (error: CancellationException) {
                    gateway.cancel()
                    throw error
                } catch (_: RuntimeException) {
                    // The exact claim is already durable. A possibly permanent or ambiguous
                    // platform exception terminates with the in-app resolver instead of retrying.
                    return TimedBreakDeliveryCompletion.SUCCESS
                }
                try {
                    when (stateAccess.validateClaim(expectedDigest)) {
                        TimedBreakClaimValidation.CURRENT ->
                            TimedBreakDeliveryCompletion.SUCCESS
                        TimedBreakClaimValidation.STALE,
                        TimedBreakClaimValidation.UNAVAILABLE,
                        -> {
                            // Resume/end/defer/replacement may win around notify(). Removing the
                            // one fixed ID closes the running-worker cancellation race.
                            gateway.cancel()
                            TimedBreakDeliveryCompletion.SUCCESS
                        }
                    }
                } catch (error: CancellationException) {
                    // WorkManager cancellation can arrive immediately after notify(). Always erase
                    // the fixed ID before propagating cancellation to the scheduler.
                    gateway.cancel()
                    throw error
                }
            }
        }
    }

}

class TimedBreakEndedWorker(
    appContext: Context,
    workerParameters: WorkerParameters,
) : CoroutineWorker(appContext, workerParameters) {
    private var deliveryOverride: (suspend (String) -> TimedBreakDeliveryCompletion)? = null

    internal constructor(
        appContext: Context,
        workerParameters: WorkerParameters,
        delivery: suspend (String) -> TimedBreakDeliveryCompletion,
    ) : this(appContext, workerParameters) {
        deliveryOverride = delivery
    }

    override suspend fun doWork(): Result {
        val digest = inputData.getString(INPUT_IDENTITY_DIGEST)
        if (!isTimedBreakNotificationDigest(digest)) return Result.failure()
        val completion = try {
            deliveryOverride?.invoke(requireNotNull(digest)) ?: run {
                val application = applicationContext as? DayWeaveApplication
                    ?: return Result.success()
                TimedBreakNotificationDelivery(
                    stateAccess = PlannerTimedBreakNotificationStateAccess(
                        application.plannerStore,
                    ),
                    gateway = AndroidTimedBreakNotificationGateway(applicationContext),
                ).deliver(requireNotNull(digest))
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            TimedBreakDeliveryCompletion.RETRY
        }
        return when {
            completion == TimedBreakDeliveryCompletion.SUCCESS -> Result.success()
            shouldRetryTimedBreakNotificationWork(runAttemptCount) -> Result.retry()
            else -> Result.success()
        }
    }

    companion object {
        internal const val INPUT_IDENTITY_DIGEST = "timed_break_identity_digest"
    }
}

internal class AndroidTimedBreakNotificationGateway(
    private val context: Context,
) : TimedBreakNotificationGateway {
    private val notificationManager = NotificationManagerCompat.from(context)
    private val routeMailbox =
        (context.applicationContext as? DayWeaveApplication)?.timedBreakNotificationRoutes
            ?: TimedBreakNotificationRouteMailbox(context)

    @SuppressLint("MissingPermission")
    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        require(isTimedBreakNotificationDigest(identityDigest))
        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
        }
        if (!notificationManager.areNotificationsEnabled()) {
            return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
        }
        try {
            ensureTimedBreakNotificationChannel(context)
        } catch (_: SecurityException) {
            return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
        } catch (_: RuntimeException) {
            return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
        }
        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
            context.getSystemService(NotificationManager::class.java)
                ?.getNotificationChannel(TIMED_BREAK_NOTIFICATION_CHANNEL_ID)
                ?.importance == NotificationManager.IMPORTANCE_NONE
        ) {
            return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
        }
        return postTimedBreakNotificationWithIssuedRoute(
            routeMailbox = routeMailbox,
            identityDigest = identityDigest,
        ) {
            notificationManager.notify(
                TIMED_BREAK_NOTIFICATION_ID,
                buildTimedBreakEndedNotification(context, identityDigest),
            )
        }
    }

    override fun cancel() {
        val revoked = routeMailbox.revokeIssued()
        notificationManager.cancel(TIMED_BREAK_NOTIFICATION_ID)
        check(revoked) { "Timed-break notification route revocation was not durable" }
    }
}

/** Issue-before-notify capability fence; every failure revokes the exact still-current issue. */
internal fun postTimedBreakNotificationWithIssuedRoute(
    routeMailbox: TimedBreakNotificationRouteMailbox,
    identityDigest: String,
    notify: () -> Unit,
): TimedBreakNotificationPostResult {
    if (!routeMailbox.issue(identityDigest)) {
        return TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
    }
    return try {
        notify()
        TimedBreakNotificationPostResult.POSTED
    } catch (_: SecurityException) {
        routeMailbox.revokeIssued(identityDigest)
        TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
    } catch (_: RuntimeException) {
        // A platform failure after issue can be ambiguous, but the encrypted delivery claim keeps
        // the banner at-most-once and the in-app resolver available. Revoke any untapped route.
        routeMailbox.revokeIssued(identityDigest)
        TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL
    }
}

/** Only the pre-claim early-clock path retries, and even that is bounded. */
internal fun shouldRetryTimedBreakNotificationWork(runAttemptCount: Int): Boolean =
    runAttemptCount in 0 until MAX_TIMED_BREAK_WORK_ATTEMPTS - 1

internal fun ensureTimedBreakNotificationChannel(
    context: Context,
    name: String = context.getString(R.string.timed_break_notification_channel_name),
    channelDescription: String =
        context.getString(R.string.timed_break_notification_channel_description),
) {
    val manager = context.getSystemService(NotificationManager::class.java) ?: return
    val channel = NotificationChannel(
        TIMED_BREAK_NOTIFICATION_CHANNEL_ID,
        name,
        NotificationManager.IMPORTANCE_DEFAULT,
    ).apply {
        description = channelDescription
        lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    manager.createNotificationChannel(channel)
}

internal fun buildTimedBreakEndedNotification(
    context: Context,
    identityDigest: String,
    title: String = context.getString(R.string.timed_break_notification_title),
    body: String = context.getString(R.string.timed_break_notification_body),
): Notification {
    require(isTimedBreakNotificationDigest(identityDigest))
    val publicVersion = NotificationCompat.Builder(context, TIMED_BREAK_NOTIFICATION_CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_timed_break_notification)
        .setContentTitle(title)
        .setContentText(body)
        .setCategory(NotificationCompat.CATEGORY_REMINDER)
        .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        .build()
    return NotificationCompat.Builder(context, TIMED_BREAK_NOTIFICATION_CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_timed_break_notification)
        .setContentTitle(title)
        .setContentText(body)
        .setCategory(NotificationCompat.CATEGORY_REMINDER)
        .setPriority(NotificationCompat.PRIORITY_DEFAULT)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        .setPublicVersion(publicVersion)
        .setOnlyAlertOnce(true)
        .setAutoCancel(true)
        .setContentIntent(timedBreakResolutionPendingIntent(context, identityDigest))
        .build()
}

internal fun timedBreakResolutionPendingIntent(
    context: Context,
    identityDigest: String,
): PendingIntent {
    require(isTimedBreakNotificationDigest(identityDigest))
    val intent = Intent(context, MainActivity::class.java)
        .setAction(timedBreakNotificationAction(identityDigest))
        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        .putExtra(EXTRA_TIMED_BREAK_IDENTITY_DIGEST, identityDigest)
    return PendingIntent.getActivity(
        context,
        TIMED_BREAK_NOTIFICATION_PENDING_INTENT_REQUEST_CODE,
        intent,
        PendingIntent.FLAG_CANCEL_CURRENT or PendingIntent.FLAG_ONE_SHOT or
            PendingIntent.FLAG_IMMUTABLE,
    )
}

internal fun timedBreakNotificationDigest(intent: Intent?): String? {
    val digest = intent?.getStringExtra(EXTRA_TIMED_BREAK_IDENTITY_DIGEST)
        ?.takeIf(::isTimedBreakNotificationDigest)
        ?: return null
    return digest.takeIf { intent.action == timedBreakNotificationAction(it) }
}

internal enum class TimedBreakNotificationRouteConsumption {
    CONSUMED,
    REJECTED,
    ALREADY_CONSUMED,
    ALREADY_REJECTED,
    STALE,
    UNAVAILABLE,
}

internal enum class TimedBreakNotificationPresentationDecision {
    PRESENT_EXACT_BREAK,
    OFFER_CURRENT_BREAK_REVIEW,
    OFFER_CURRENT_BREAK_REVIEW_NON_MODAL,
    SUPPRESS_CURRENT_BREAK,
    RETRY_AFTER_STATE_SETTLES,
}

/** Stable UI transition decided only after the opaque route receipt reaches encrypted storage. */
internal fun timedBreakNotificationPresentationDecision(
    consumption: TimedBreakNotificationRouteConsumption,
    initiallyMatchedExactBreak: Boolean,
    currentEndedBreakKey: String?,
): TimedBreakNotificationPresentationDecision = when (consumption) {
    TimedBreakNotificationRouteConsumption.CONSUMED ->
        if (initiallyMatchedExactBreak) {
            TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK
        } else {
            TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK
        }
    TimedBreakNotificationRouteConsumption.REJECTED ->
        if (currentEndedBreakKey == null) {
            TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK
        } else {
            TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW
        }
    TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED ->
        if (initiallyMatchedExactBreak) {
            TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK
        } else if (currentEndedBreakKey != null) {
            TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW
        } else {
            TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK
        }
    TimedBreakNotificationRouteConsumption.ALREADY_REJECTED ->
        if (currentEndedBreakKey != null) {
            TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW_NON_MODAL
        } else {
            TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK
        }
    TimedBreakNotificationRouteConsumption.STALE,
    TimedBreakNotificationRouteConsumption.UNAVAILABLE,
    -> TimedBreakNotificationPresentationDecision.RETRY_AFTER_STATE_SETTLES
}

/** Persists the opaque consume-once receipt before Activity/task recreation can replay a tap. */
internal class PlannerTimedBreakNotificationRouteAccess(
    private val plannerStore: PlannerStore,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) {
    suspend fun consume(expectedDigest: String): TimedBreakNotificationRouteConsumption {
        require(isTimedBreakNotificationDigest(expectedDigest))
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) {
            return TimedBreakNotificationRouteConsumption.UNAVAILABLE
        }
        val durableBefore = plannerStore.durableState.value
            ?: return TimedBreakNotificationRouteConsumption.UNAVAILABLE
        val liveBefore = plannerStore.state.value
        val durableMatches = durableBefore.matchesTimedBreakResolution(
            expectedDigest,
            nowEpochMillis(),
        )
        val liveMatches = liveBefore.matchesTimedBreakResolution(
            expectedDigest,
            nowEpochMillis(),
        )
        if (durableMatches != liveMatches) {
            // Durable/live disagreement can never be classified as exact or stale authority.
            return TimedBreakNotificationRouteConsumption.UNAVAILABLE
        }
        val receipt = if (durableMatches) {
            if (durableBefore.lastConsumedBreakEndNotificationDigest == expectedDigest) {
                return TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED
            }
            if (liveBefore.lastConsumedBreakEndNotificationDigest == expectedDigest) {
                return if (awaitDurableRouteReceipt(expectedDigest, rejected = false)) {
                    TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED
                } else {
                    TimedBreakNotificationRouteConsumption.UNAVAILABLE
                }
            }
            plannerStore.recordTimedBreakNotificationRouteConsumption(expectedDigest)
        } else {
            if (durableBefore.lastRejectedBreakEndNotificationDigest == expectedDigest) {
                return TimedBreakNotificationRouteConsumption.ALREADY_REJECTED
            }
            if (liveBefore.lastRejectedBreakEndNotificationDigest == expectedDigest) {
                return if (awaitDurableRouteReceipt(expectedDigest, rejected = true)) {
                    TimedBreakNotificationRouteConsumption.ALREADY_REJECTED
                } else {
                    TimedBreakNotificationRouteConsumption.UNAVAILABLE
                }
            }
            plannerStore.recordTimedBreakNotificationRouteRejection(expectedDigest)
        } ?: return TimedBreakNotificationRouteConsumption.UNAVAILABLE
        if (!receipt.awaitDurable()) return TimedBreakNotificationRouteConsumption.UNAVAILABLE

        val durableAfter = plannerStore.durableState.value
            ?: return TimedBreakNotificationRouteConsumption.UNAVAILABLE
        val liveAfter = plannerStore.state.value
        return if (
            durableMatches && listOf(durableAfter, liveAfter).all { state ->
                state.matchesTimedBreakResolution(expectedDigest, nowEpochMillis())
                    && state.lastConsumedBreakEndNotificationDigest == expectedDigest
            }
        ) {
            TimedBreakNotificationRouteConsumption.CONSUMED
        } else if (
            !durableMatches && listOf(durableAfter, liveAfter).all { state ->
                !state.matchesTimedBreakResolution(expectedDigest, nowEpochMillis()) &&
                    state.lastRejectedBreakEndNotificationDigest == expectedDigest
            }
        ) {
            TimedBreakNotificationRouteConsumption.REJECTED
        } else {
            // Authority changed while a receipt was saving. Keep the payload pending until the
            // stable presentation keys restart this state machine; never retarget presentation.
            TimedBreakNotificationRouteConsumption.UNAVAILABLE
        }
    }

    private suspend fun awaitDurableRouteReceipt(
        expectedDigest: String,
        rejected: Boolean,
    ): Boolean {
        val settled = plannerStore.loadState.combine(plannerStore.durableState) {
                currentLoad, durable -> currentLoad to durable
            }.first { (currentLoad, durable) ->
                val receipt = if (rejected) {
                    durable?.lastRejectedBreakEndNotificationDigest
                } else {
                    durable?.lastConsumedBreakEndNotificationDigest
                }
                currentLoad != PlannerLoadState.READY || receipt == expectedDigest
            }
        if (settled.first != PlannerLoadState.READY) return false
        return if (rejected) {
            settled.second?.lastRejectedBreakEndNotificationDigest == expectedDigest
        } else {
            settled.second?.lastConsumedBreakEndNotificationDigest == expectedDigest
        }
    }
}

internal fun timedBreakNotificationAction(identityDigest: String): String =
    "$TIMED_BREAK_NOTIFICATION_ACTION_PREFIX.$identityDigest"

internal fun shouldOpenTimedBreakResolution(
    durableState: DayWeaveUiState,
    liveState: DayWeaveUiState,
    identityDigest: String,
    nowEpochMillis: Long = System.currentTimeMillis(),
): Boolean = isExactTimedBreakResolutionCurrent(
    durableState = durableState,
    liveState = liveState,
    identityDigest = identityDigest,
    nowEpochMillis = nowEpochMillis,
) && listOf(durableState, liveState).all { state ->
        state.lastConsumedBreakEndNotificationDigest != identityDigest
}

/** Distinguishes cold encrypted restore from a successfully restored empty execution snapshot. */
internal fun timedBreakNotificationRouteStateAvailable(
    durableState: DayWeaveUiState?,
): Boolean = durableState != null

/** Receipt-agnostic exact authority used to recover presentation after Activity interruption. */
internal fun isExactTimedBreakResolutionCurrent(
    durableState: DayWeaveUiState,
    liveState: DayWeaveUiState,
    identityDigest: String,
    nowEpochMillis: Long = System.currentTimeMillis(),
): Boolean = listOf(durableState, liveState).all { state ->
    state.matchesTimedBreakResolution(identityDigest, nowEpochMillis)
}

/**
 * A notification launch may present only the exact break its opaque route authorized. Rejecting
 * stale A suppresses the already-ended B for that launch, while a normal non-notification launch
 * and a later distinct break retain the ordinary clock-driven resolver behavior.
 */
internal fun shouldPresentTimedBreakResolution(
    endedBreakKey: String?,
    dismissedBreakKey: String?,
    pendingNotificationDigest: String?,
    authorizedNotificationDigest: String?,
    rejectedNotificationLaunchBreakKey: String?,
): Boolean {
    if (
        endedBreakKey == null || endedBreakKey == dismissedBreakKey ||
        endedBreakKey == rejectedNotificationLaunchBreakKey
    ) {
        return false
    }
    if (authorizedNotificationDigest != null) {
        return endedBreakKey == authorizedNotificationDigest &&
            (pendingNotificationDigest == null ||
                pendingNotificationDigest == authorizedNotificationDigest)
    }
    return pendingNotificationDigest == null
}

/**
 * A stale notification never opens a replacement break directly. Once the stale route has been
 * consumed, offer a content-free recovery step so the current break is not suppressed for the
 * lifetime of the Activity; only the user's explicit review action may reveal its resolver.
 */
internal fun shouldOfferCurrentTimedBreakReview(
    endedBreakKey: String?,
    pendingNotificationDigest: String?,
    rejectedNotificationLaunchBreakKey: String?,
    validatedRejectedRouteDigest: String? = null,
): Boolean = endedBreakKey != null &&
    rejectedNotificationLaunchBreakKey == endedBreakKey &&
    (
        pendingNotificationDigest == null ||
            pendingNotificationDigest == validatedRejectedRouteDigest
    )

/** A generic stale-route fence is released only after its exact durable mailbox CAS succeeds. */
internal fun clearValidatedRejectedNotificationRoute(
    pendingRejectedRouteDigest: String?,
    consume: (String) -> Boolean,
): Boolean = pendingRejectedRouteDigest == null || consume(pendingRejectedRouteDigest)

internal data class TimedBreakNotificationAuthorizationTransition(
    val authorizedDigest: String?,
    val changedBreakReviewKey: String?,
)

/** Exact A presentation authority cannot silently hide a directly replacing ended break B. */
internal fun reconcileTimedBreakNotificationAuthorization(
    authorizedDigest: String?,
    endedBreakKey: String?,
): TimedBreakNotificationAuthorizationTransition = if (
    authorizedDigest != null && endedBreakKey != authorizedDigest
) {
    TimedBreakNotificationAuthorizationTransition(
        authorizedDigest = null,
        changedBreakReviewKey = endedBreakKey,
    )
} else {
    TimedBreakNotificationAuthorizationTransition(
        authorizedDigest = authorizedDigest,
        changedBreakReviewKey = null,
    )
}

private fun DayWeaveUiState.matchesTimedBreakResolution(
    identityDigest: String,
    nowEpochMillis: Long,
): Boolean {
    val identity = authoritativeTimedBreakNotificationIdentity() ?: return false
    return identity.digest == identityDigest &&
        nowEpochMillis >= identity.deadlineEpochMillis &&
        activeSession?.timedBreakEnded == true &&
        acknowledgedBreakEndDigest != identityDigest
}

internal const val TIMED_BREAK_NOTIFICATION_CHANNEL_ID = "timed-breaks-v1"
internal const val TIMED_BREAK_NOTIFICATION_ID = 0x44574252
internal const val EXTRA_TIMED_BREAK_IDENTITY_DIGEST =
    "com.greengolddog.dayweave.extra.TIMED_BREAK_IDENTITY_DIGEST"
private const val TIMED_BREAK_NOTIFICATION_ACTION_PREFIX =
    "com.greengolddog.dayweave.action.OPEN_TIMED_BREAK_RESOLUTION"
private const val TIMED_BREAK_NOTIFICATION_PENDING_INTENT_REQUEST_CODE = 0x44574252
private const val MAX_TIMED_BREAK_WORK_ATTEMPTS = 3
