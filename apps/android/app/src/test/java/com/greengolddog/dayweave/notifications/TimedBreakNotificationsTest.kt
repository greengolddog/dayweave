package com.greengolddog.dayweave.notifications

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import androidx.work.NetworkType
import androidx.work.Configuration
import androidx.work.Data
import androidx.work.ListenableWorker
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.WorkerFactory
import androidx.work.WorkerParameters
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import com.greengolddog.dayweave.DayWeaveLauncherActivity
import com.greengolddog.dayweave.MainActivity
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.TimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.After
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TimedBreakNotificationsTest {
    @Test
    fun identityAndWorkMetadataBindExactAuthorityWithoutRawIdsOrContent() {
        val identity = timedBreakState().authoritativeTimedBreakNotificationIdentity()!!
        val request = buildTimedBreakNotificationWorkRequest(
            identity = identity,
            nowEpochMillis = DEADLINE - 90_000L,
        )

        assertEquals(TimeUnit.SECONDS.toMillis(90), request.workSpec.initialDelay)
        assertEquals(NetworkType.NOT_REQUIRED, request.workSpec.constraints.requiredNetworkType)
        assertEquals(
            identity.digest,
            request.workSpec.input.getString(TimedBreakEndedWorker.INPUT_IDENTITY_DIGEST),
        )
        val serializedMetadata = buildString {
            append(request.workSpec.input.keyValueMap)
            append(request.tags)
        }
        assertFalse(serializedMetadata.contains(SESSION_ID))
        assertFalse(serializedMetadata.contains(SENSITIVE_CANARY))
        assertTrue(request.tags.contains(WorkManagerTimedBreakNotificationBackend.WORK_TAG))
        assertEquals(WorkManagerTimedBreakNotificationBackend.EXISTING_WORK_POLICY,
            androidx.work.ExistingWorkPolicy.REPLACE)
    }

    @Test
    fun expiredRestoredBreakUsesZeroDelayAndReplacementChangesDigest() {
        val first = timedBreakState().authoritativeTimedBreakNotificationIdentity()!!
        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
        ).authoritativeTimedBreakNotificationIdentity()!!

        val restoredRequest = buildTimedBreakNotificationWorkRequest(first, DEADLINE + 1L)

        assertEquals(0L, restoredRequest.workSpec.initialDelay)
        assertNotEquals(first.digest, replacement.digest)
    }

    @Test
    fun coordinatorDeduplicatesAndCancelsResumeOrExactReplacement() = runBlocking {
        val backend = RecordingBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val first = timedBreakState()

        assertTrue(coordinator.reconcile(first))
        assertTrue(coordinator.reconcile(first))
        assertEquals(1, backend.ensures.size)
        assertTrue(backend.ensures.single().second)

        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
        )
        assertTrue(coordinator.reconcile(replacement))
        assertEquals(2, backend.ensures.size)
        assertTrue(backend.ensures.last().second)

        assertTrue(coordinator.reconcile(replacement.copy(canonicalExecutionSession = null)))
        assertEquals(1, backend.fullCancels)
    }

    @Test
    fun restoredHandledIdentityLeavesVisibleNotificationButSchedulesSafeSuppressionCheck() =
        runBlocking {
        val backend = RecordingBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val state = timedBreakState(timedBreakEnded = true)
        val digest = state.authoritativeTimedBreakNotificationIdentity()!!.digest

        assertTrue(
            coordinator.reconcile(state.copy(lastBreakEndNotificationAttemptDigest = digest)),
        )

        assertEquals(1, backend.ensures.size)
        assertFalse(backend.ensures.single().second)
    }

    @Test
    fun durableKeepPausedAcknowledgementCancelsDelayedWorkIncludingAfterColdRestart() = runBlocking {
        val state = timedBreakState(timedBreakEnded = true)
        val digest = state.authoritativeTimedBreakNotificationIdentity()!!.digest
        val acknowledged = state.copy(acknowledgedBreakEndDigest = digest)
        val backend = RecordingBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)

        assertTrue(coordinator.reconcile(state))
        assertTrue(coordinator.reconcile(acknowledged))
        assertEquals(1, backend.fullCancels)

        val coldBackend = RecordingBackend()
        assertTrue(TimedBreakNotificationCoordinator(coldBackend).reconcile(acknowledged))
        assertEquals(1, coldBackend.fullCancels)
        assertEquals(0, coldBackend.ensures.size)
    }

    @Test
    fun failedBackendOperationIsNotMemoizedAndSameDurableGenerationRetries() = runBlocking {
        val backend = RecordingBackend(failEnsureAttempts = 1)
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val state = timedBreakState()

        reconcileTimedBreakNotificationStates(
            durableStates = flowOf(state),
            coordinator = coordinator,
            retryDelayMillis = 0L,
        )

        assertEquals(2, backend.ensureAttempts)
        assertEquals(1, backend.ensures.size)
    }

    @Test
    fun authoritativeTransitionAwaitsCancellationAndFailedTransitionCanRestoreSameDigest() =
        runBlocking {
            val backend = AwaitedCancelBackend()
            val coordinator = TimedBreakNotificationCoordinator(backend)
            val state = timedBreakState()
            assertTrue(coordinator.reconcile(state))

            val cancellation = async(Dispatchers.Default) {
                coordinator.cancelForAuthoritativeTransition()
            }
            withTimeout(3_000) { backend.cancelEntered.await() }
            assertFalse(cancellation.isCompleted)
            backend.allowCancel.complete(Unit)
            assertTrue(withTimeout(3_000) { cancellation.await() })

            // Simulate a rejected/destructive transition: unchanged durable truth is explicitly
            // reconciled and receives a fresh OS job only after cancellation finished.
            assertTrue(coordinator.reconcile(state))
            assertEquals(2, backend.ensures)
            assertEquals(1, backend.cancels)
        }

    @Test
    fun partialCancellationFailureInvalidatesMemoSoUnchangedDigestIsReenqueued() = runBlocking {
        val backend = RecordingBackend(failCancelAttempts = 1)
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val state = timedBreakState()
        var retryRequests = 0

        assertTrue(coordinator.reconcile(state))
        assertFalse(
            cancelTimedBreakNotificationAndRestoreOnFailure(
                coordinator = coordinator,
                unchangedDurableState = state,
                queueReconciliationRetry = { retryRequests += 1 },
            ),
        )
        assertEquals(1, backend.fullCancels)

        // The backend performed its destructive side effect before throwing. The application
        // wrapper restores this exact reminder synchronously without waiting for another state
        // emission or process restart.
        assertEquals(2, backend.ensures.size)
        assertEquals(0, retryRequests)
    }

    @Test
    fun failedImmediateRestoreQueuesReconciliationWithoutAllowingTransition() = runBlocking {
        val backend = PartialCancelAndRestoreFailureBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val state = timedBreakState()
        var retryRequests = 0
        assertTrue(coordinator.reconcile(state))

        assertFalse(
            cancelTimedBreakNotificationAndRestoreOnFailure(
                coordinator = coordinator,
                unchangedDurableState = state,
                queueReconciliationRetry = { retryRequests += 1 },
            ),
        )

        assertEquals(2, backend.ensureAttempts)
        assertEquals(1, retryRequests)
    }

    @Test
    fun cancellationFailureRestoresReminderAndPropagatesStructuredCancellation() = runBlocking {
        val backend = CancellationDuringCancelBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val state = timedBreakState()
        assertTrue(coordinator.reconcile(state))

        val failure = runCatching {
            cancelTimedBreakNotificationAndRestoreOnFailure(
                coordinator = coordinator,
                unchangedDurableState = state,
                queueReconciliationRetry = {},
            )
        }.exceptionOrNull()

        assertTrue(failure is kotlinx.coroutines.CancellationException)
        assertEquals(2, backend.ensureAttempts)
    }

    @Test
    fun cancelledPartialEnsureInvalidatesMemoAndSameReplacementRetries() = runBlocking {
        val backend = CancellationDuringSecondEnsureBackend()
        val coordinator = TimedBreakNotificationCoordinator(backend)
        val first = timedBreakState()
        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
        )
        assertTrue(coordinator.reconcile(first))

        assertTrue(
            runCatching { coordinator.reconcile(replacement) }.exceptionOrNull() is
                kotlinx.coroutines.CancellationException,
        )
        assertTrue(coordinator.reconcile(replacement))
        assertEquals(3, backend.ensureAttempts)
    }

    @Test
    fun staleDuplicateAndUnreadableStateNeverPost() = runBlocking {
        listOf(
            TimedBreakPreparation.STALE,
            TimedBreakPreparation.ALREADY_HANDLED,
            TimedBreakPreparation.UNAVAILABLE,
        ).forEach { preparation ->
            val stateAccess = RecordingStateAccess(preparation)
            val gateway = RecordingGateway()

            val completion = TimedBreakNotificationDelivery(stateAccess, gateway)
                .deliver(DIGEST_A)

            assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
            assertEquals(0, gateway.posts)
            assertEquals(0, stateAccess.validationCalls)
        }
    }

    @Test
    fun permissionDenialUsesDurableClaimAndCompletesWithoutRetryStorm() = runBlocking {
        val stateAccess = RecordingStateAccess(TimedBreakPreparation.READY)
        val gateway = RecordingGateway(
            result = TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
        )

        val completion = TimedBreakNotificationDelivery(stateAccess, gateway).deliver(DIGEST_A)

        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
        assertEquals(1, gateway.posts)
        assertEquals(2, stateAccess.validationCalls)
    }

    @Test
    fun postedOrSuppressedUnavailablePostValidationCancelsAndDoesNotRetry() = runBlocking {
        listOf(
            TimedBreakNotificationPostResult.POSTED,
            TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
        ).forEach { postResult ->
            val stateAccess = RecordingStateAccess(
                preparation = TimedBreakPreparation.READY,
                validations = listOf(
                    TimedBreakClaimValidation.CURRENT,
                    TimedBreakClaimValidation.UNAVAILABLE,
                ),
            )
            val gateway = RecordingGateway(result = postResult)

            val completion = TimedBreakNotificationDelivery(stateAccess, gateway)
                .deliver(DIGEST_A)

            assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
            assertEquals(1, gateway.posts)
            assertEquals(2, stateAccess.validationCalls)
            assertEquals(1, gateway.cancels)
        }
    }

    @Test
    fun staleValidationAfterPostOrSuppressionCancelsFixedNotificationAndCompletes() = runBlocking {
        listOf(
            TimedBreakNotificationPostResult.POSTED,
            TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
        ).forEach { postResult ->
            val gateway = RecordingGateway(result = postResult)
            val completion = TimedBreakNotificationDelivery(
                stateAccess = RecordingStateAccess(
                    preparation = TimedBreakPreparation.READY,
                    validations = listOf(
                        TimedBreakClaimValidation.CURRENT,
                        TimedBreakClaimValidation.STALE,
                    ),
                ),
                gateway = gateway,
            ).deliver(DIGEST_A)

            assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
            assertEquals(1, gateway.posts)
            assertEquals(1, gateway.cancels)
        }
    }

    @Test
    fun overlappingSameDigestDeliveriesProduceOnlyOnePost() = runBlocking {
        val access = ExclusiveRecordingStateAccess()
        val gateway = BlockingRecordingGateway()
        val first = async(Dispatchers.Default) {
            TimedBreakNotificationDelivery(access, gateway).deliver(DIGEST_A)
        }
        assertTrue(gateway.postEntered.await(3, TimeUnit.SECONDS))
        val second = async(Dispatchers.Default) {
            TimedBreakNotificationDelivery(access, gateway).deliver(DIGEST_A)
        }

        delay(75)
        assertFalse(second.isCompleted)
        gateway.allowPost.countDown()

        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, withTimeout(3_000) { first.await() })
        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, withTimeout(3_000) { second.await() })
        assertEquals(1, gateway.posts.get())
        assertEquals(1, access.claims.get())
    }

    @Test
    fun authoritativeCancellationJoinsDelayedPostThenRemovesTheFixedNotification() = runBlocking {
        val access = ExclusiveRecordingStateAccess()
        val gateway = BlockingVisibleGateway()
        val delivery = async(Dispatchers.Default) {
            TimedBreakNotificationDelivery(access, gateway).deliver(DIGEST_A)
        }
        assertTrue(gateway.postEntered.await(3, TimeUnit.SECONDS))

        val workCancellationRequested = CompletableDeferred<Unit>()
        val cancellation = async(Dispatchers.Default) {
            cancelTimedBreakNotificationWorkAndDisplayedAlert(
                cancelWork = { workCancellationRequested.complete(Unit) },
                cancelDisplayedAlert = gateway::cancel,
            )
        }
        withTimeout(3_000) { workCancellationRequested.await() }
        delay(75)
        assertFalse(cancellation.isCompleted)
        assertFalse(gateway.visible.get())

        gateway.allowPost.countDown()
        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, withTimeout(3_000) { delivery.await() })
        withTimeout(3_000) { cancellation.await() }

        // A transition may safely terminate immediately after its awaited barrier: even though
        // notify() was already in flight, no stale fixed-ID notification survives.
        assertEquals(1, gateway.posts.get())
        assertEquals(1, gateway.cancels.get())
        assertFalse(gateway.visible.get())
    }

    @Test
    fun onlyEarlyClockRetriesAndRetryBudgetIsFinite() = runBlocking {
        val early = TimedBreakNotificationDelivery(
            RecordingStateAccess(TimedBreakPreparation.NOT_DUE),
            RecordingGateway(),
        ).deliver(DIGEST_A)
        val permanentPostFailure = TimedBreakNotificationDelivery(
            RecordingStateAccess(TimedBreakPreparation.READY),
            ThrowingGateway(),
        ).deliver(DIGEST_A)

        assertEquals(TimedBreakDeliveryCompletion.RETRY, early)
        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, permanentPostFailure)
        assertTrue(shouldRetryTimedBreakNotificationWork(runAttemptCount = 0))
        assertTrue(shouldRetryTimedBreakNotificationWork(runAttemptCount = 1))
        assertFalse(shouldRetryTimedBreakNotificationWork(runAttemptCount = 2))
        assertFalse(shouldRetryTimedBreakNotificationWork(runAttemptCount = 100))
    }

    @Test
    fun realWorkerMapsSuccessRetryAndBoundedAttemptResults() = runBlocking {
        assertEquals(
            ListenableWorker.Result.success(),
            worker(runAttemptCount = 0) { TimedBreakDeliveryCompletion.SUCCESS }.doWork(),
        )
        assertEquals(
            ListenableWorker.Result.retry(),
            worker(runAttemptCount = 0) { TimedBreakDeliveryCompletion.RETRY }.doWork(),
        )
        assertEquals(
            ListenableWorker.Result.retry(),
            worker(runAttemptCount = 1) { TimedBreakDeliveryCompletion.RETRY }.doWork(),
        )
        assertEquals(
            ListenableWorker.Result.success(),
            worker(runAttemptCount = 2) { TimedBreakDeliveryCompletion.RETRY }.doWork(),
        )
        assertEquals(
            ListenableWorker.Result.failure(),
            worker(
                digest = "not-an-opaque-digest",
                runAttemptCount = 0,
            ) { error("invalid input must not reach delivery") }.doWork(),
        )
    }

    @Test
    fun durableClaimPreventsSecondBannerAfterPostCrashWithoutMutatingExecution() = runBlocking {
        var now = DEADLINE + 1L
        val store = PlannerStore(timedBreakState(), nowEpochMillis = { now })
        val stateAccess = PlannerTimedBreakNotificationStateAccess(store) { now }
        val digest = store.state.value.authoritativeTimedBreakNotificationIdentity()!!.digest
        val firstGateway = RecordingGateway()

        // Simulate process death after the banner but before any post-return logic.
        assertEquals(TimedBreakPreparation.READY, stateAccess.prepare(digest))
        assertEquals(
            TimedBreakNotificationPostResult.POSTED,
            firstGateway.post(digest),
        )
        val executionBeforeRetry = store.state.value.canonicalExecutionSession

        val retryGateway = RecordingGateway()
        val completion = TimedBreakNotificationDelivery(stateAccess, retryGateway).deliver(digest)

        assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
        assertEquals(0, retryGateway.posts)
        assertEquals(digest, store.state.value.lastBreakEndNotificationAttemptDigest)
        assertEquals(executionBeforeRetry, store.state.value.canonicalExecutionSession)
        assertTrue(store.state.value.activeSession!!.isPaused)
        assertTrue(store.state.value.activeSession!!.timedBreakEnded)

        val duplicateGateway = RecordingGateway()
        TimedBreakNotificationDelivery(stateAccess, duplicateGateway).deliver(digest)
        assertEquals(0, duplicateGateway.posts)
    }

    @Test
    fun resumedEndedDeferredAndReplacementTruthAllSuppressStaleWorker() = runBlocking {
        val paused = timedBreakState()
        val oldDigest = paused.authoritativeTimedBreakNotificationIdentity()!!.digest
        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
        )
        val staleStates = listOf(
            paused.copy(
                canonicalExecutionSession = paused.canonicalExecutionSession!!.copy(status = "active"),
                activeSession = paused.activeSession!!.copy(isPaused = false),
            ),
            paused.copy(
                canonicalExecutionSession = paused.canonicalExecutionSession!!.copy(status = "completed"),
                activeSession = null,
            ),
            paused.copy(
                canonicalExecutionSession = paused.canonicalExecutionSession!!.copy(status = "deferred"),
                activeSession = null,
            ),
            replacement,
        )

        staleStates.forEach { stale ->
            val store = PlannerStore(stale, nowEpochMillis = { DEADLINE + 700_000L })
            val access = PlannerTimedBreakNotificationStateAccess(store) { DEADLINE + 700_000L }
            val executionBefore = store.state.value.canonicalExecutionSession

            assertEquals(TimedBreakPreparation.STALE, access.prepare(oldDigest))
            assertEquals(executionBefore, store.state.value.canonicalExecutionSession)
        }
    }

    @Test
    fun notificationIsGenericActionFreeAndTapCarriesOnlyOpaqueDigest() {
        val context = RuntimeEnvironment.getApplication() as Context
        ensureTimedBreakNotificationChannel(context, "Break reminders", "Timed break reminders")

        val notification = buildTimedBreakEndedNotification(
            context,
            DIGEST_A,
            title = "Break ended",
            body = "Open DayWeave to choose what happens next.",
        )
        val tapIntent = shadowOf(notification.contentIntent).savedIntent

        assertEquals("Break ended", notification.extras.getCharSequence(Notification.EXTRA_TITLE))
        assertEquals(
            "Open DayWeave to choose what happens next.",
            notification.extras.getCharSequence(Notification.EXTRA_TEXT),
        )
        assertEquals(0, notification.actions?.size ?: 0)
        assertFalse(notification.extras.toString().contains(SENSITIVE_CANARY))
        assertEquals(DIGEST_A, timedBreakNotificationDigest(tapIntent))
        assertEquals(setOf(EXTRA_TIMED_BREAK_IDENTITY_DIGEST), tapIntent.extras!!.keySet())
        assertEquals(
            MainActivity::class.java.name,
            tapIntent.component!!.className,
        )
        assertNull(tapIntent.data)
        assertEquals(
            "Break ended",
            notification.publicVersion.extras.getCharSequence(Notification.EXTRA_TITLE),
        )
    }

    @Test
    @Config(sdk = [32])
    fun repeatedPostReplacesOneVisibleNotificationInsteadOfDuplicatingIt() {
        val context = RuntimeEnvironment.getApplication() as Context
        ensureTimedBreakNotificationChannel(context, "Break reminders", "Timed break reminders")
        val manager = context.getSystemService(NotificationManager::class.java)

        val first = buildTimedBreakEndedNotification(
            context,
            DIGEST_A,
            title = "Break ended",
            body = "Open DayWeave to choose what happens next.",
        )
        val retry = buildTimedBreakEndedNotification(
            context,
            DIGEST_A,
            title = "Break ended",
            body = "Open DayWeave to choose what happens next.",
        )
        manager.notify(TIMED_BREAK_NOTIFICATION_ID, first)
        manager.notify(TIMED_BREAK_NOTIFICATION_ID, retry)

        assertEquals(1, shadowOf(manager).allNotifications.size)
    }

    @Test
    fun trustedRouteMailboxPersistsBeforeLaunchAndConsumesExactlyOnceAcrossRestart() {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "restore", reset = true)
        val mailbox = TimedBreakNotificationRouteMailbox(preferences)
        val launch = routeIntent(context, DIGEST_A)

        assertFalse(mailbox.acceptTrusted(launch))
        assertTrue(mailbox.issue(DIGEST_A))
        assertTrue(mailbox.acceptTrusted(launch))
        assertEquals(DIGEST_A, mailbox.pendingDigest.value)
        // A fresh process-owned instance restores the synchronously committed opaque digest.
        val restored = TimedBreakNotificationRouteMailbox(preferences)
        assertEquals(DIGEST_A, restored.pendingDigest.value)
        assertTrue(restored.consume(DIGEST_A))
        assertNull(TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value)
        assertFalse(restored.consume(DIGEST_A))
    }

    @Test
    fun acceptedRouteIsSanitizedAndCannotReopenAfterActivityOrRawTaskReplay() {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "activity-replay", reset = true)
        val mailbox = TimedBreakNotificationRouteMailbox(preferences)
        val rawRoute = routeIntent(context, DIGEST_A)

        assertTrue(mailbox.issue(DIGEST_A))
        val storedActivityIntent = admitTrustedTimedBreakRouteAndSanitizeMainIntent(
            context = context,
            candidate = rawRoute,
            mailbox = mailbox,
        )
        assertEquals(DIGEST_A, mailbox.pendingDigest.value)
        assertNull(timedBreakNotificationDigest(storedActivityIntent))
        assertEquals(Intent.ACTION_MAIN, storedActivityIntent.action)
        assertTrue(mailbox.consume(DIGEST_A))

        // Configuration recreation sees only the sanitized Activity intent. Even if Android
        // restores the original task-base intent after process death, the durable handled token
        // prevents the already-cleared one-shot route from being admitted again.
        val recreated = TimedBreakNotificationRouteMailbox(preferences)
        admitTrustedTimedBreakRouteAndSanitizeMainIntent(context, storedActivityIntent, recreated)
        assertNull(recreated.pendingDigest.value)
        admitTrustedTimedBreakRouteAndSanitizeMainIntent(context, rawRoute, recreated)
        assertNull(recreated.pendingDigest.value)

        assertTrue(recreated.issue(DIGEST_B))
        assertTrue(recreated.acceptTrusted(routeIntent(context, DIGEST_B)))
        assertTrue(recreated.consume(DIGEST_B))
        // B replacing the issue fence cannot make the old A task-base route valid again.
        assertFalse(recreated.acceptTrusted(rawRoute))
        assertNull(recreated.pendingDigest.value)
    }

    @Test
    fun staleConsumeCannotEraseNewerDurableTrustedRoute() {
        val context = RuntimeEnvironment.getApplication() as Context
        val mailbox = TimedBreakNotificationRouteMailbox(
            routePreferences(context, "newer", reset = true),
        )
        val old = routeIntent(context, DIGEST_A)
        val newer = routeIntent(context, DIGEST_B)

        assertTrue(mailbox.issue(DIGEST_A))
        assertTrue(mailbox.acceptTrusted(old))
        assertTrue(mailbox.issue(DIGEST_B))
        assertTrue(mailbox.acceptTrusted(newer))

        assertFalse(mailbox.consume(DIGEST_A))
        assertEquals(DIGEST_B, mailbox.pendingDigest.value)
        assertEquals(
            DIGEST_B,
            TimedBreakNotificationRouteMailbox(
                routePreferences(context, "newer"),
            ).pendingDigest.value,
        )
    }

    @Test
    fun failedMailboxClearKeepsStaleReviewFenceAndNewerRouteCasSafe() {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "failed-clear", reset = true)
        val writable = TimedBreakNotificationRouteMailbox(preferences)
        assertTrue(writable.issue(DIGEST_A))
        assertTrue(writable.acceptTrusted(routeIntent(context, DIGEST_A)))
        val failing = TimedBreakNotificationRouteMailbox(preferences) { false }

        assertFalse(failing.consume(DIGEST_A))
        assertFalse(
            clearValidatedRejectedNotificationRoute(DIGEST_A, failing::consume),
        )
        assertEquals(
            DIGEST_A,
            TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value,
        )

        // A trusted newer tap replaces A. A stale A choice cannot clear or acknowledge B.
        assertTrue(writable.issue(DIGEST_B))
        assertTrue(writable.acceptTrusted(routeIntent(context, DIGEST_B)))
        assertFalse(failing.consume(DIGEST_A))
        assertEquals(
            DIGEST_B,
            TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value,
        )
    }

    @Test
    fun issuedCapabilityCommitsBeforeNotifyAndPostFailureRevokesWithoutRetryAuthority() {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "issue-post", reset = true)
        val mailbox = TimedBreakNotificationRouteMailbox(preferences)
        var notifyCalls = 0

        assertEquals(
            TimedBreakNotificationPostResult.POSTED,
            postTimedBreakNotificationWithIssuedRoute(mailbox, DIGEST_A) {
                notifyCalls += 1
                assertEquals(
                    DIGEST_A,
                    preferences.getString(
                        TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY,
                        null,
                    ),
                )
            },
        )
        assertEquals(1, notifyCalls)
        assertTrue(mailbox.revokeIssued(DIGEST_A))

        assertEquals(
            TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
            postTimedBreakNotificationWithIssuedRoute(mailbox, DIGEST_B) {
                notifyCalls += 1
                error("synthetic NotificationManager failure")
            },
        )
        assertEquals(2, notifyCalls)
        assertFalse(preferences.contains(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY))

        val failingIssue = TimedBreakNotificationRouteMailbox(preferences) { false }
        assertEquals(
            TimedBreakNotificationPostResult.SUPPRESSED_BY_PERMISSION_OR_CHANNEL,
            postTimedBreakNotificationWithIssuedRoute(failingIssue, DIGEST_A) {
                notifyCalls += 1
            },
        )
        assertEquals(2, notifyCalls)
    }

    @Test
    fun processWideIssuedFenceSerializesAcceptReplacementAndStaleRevocation() = runBlocking {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "route-race", reset = true)
        val issuer = TimedBreakNotificationRouteMailbox(preferences)
        assertTrue(issuer.issue(DIGEST_A))
        val acceptCommitEntered = CountDownLatch(1)
        val allowAcceptCommit = CountDownLatch(1)
        val accepting = TimedBreakNotificationRouteMailbox(preferences) { editor ->
            acceptCommitEntered.countDown()
            assertTrue(allowAcceptCommit.await(3, TimeUnit.SECONDS))
            editor.commit()
        }

        val acceptA = async(Dispatchers.IO) {
            accepting.acceptTrusted(routeIntent(context, DIGEST_A))
        }
        assertTrue(acceptCommitEntered.await(3, TimeUnit.SECONDS))
        val issueB = async(Dispatchers.IO) { issuer.issue(DIGEST_B) }
        assertFalse(issueB.isCompleted)
        allowAcceptCommit.countDown()

        assertTrue(withTimeout(3_000) { acceptA.await() })
        assertTrue(withTimeout(3_000) { issueB.await() })
        assertEquals(DIGEST_A, accepting.pendingDigest.value)
        assertEquals(
            DIGEST_B,
            preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY, null),
        )
        // A delayed stale cancellation/choice cannot erase the newly issued B capability.
        assertTrue(issuer.revokeIssued(DIGEST_A))
        assertEquals(
            DIGEST_B,
            preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY, null),
        )
        assertTrue(issuer.acceptTrusted(routeIntent(context, DIGEST_B)))
        assertFalse(accepting.consume(DIGEST_A))
        assertEquals(DIGEST_B, issuer.pendingDigest.value)
    }

    @Test
    fun exportedLauncherDiscardsForgedRouteAndStartsNonExportedMainWithoutExtras() {
        val context = RuntimeEnvironment.getApplication() as Context
        val controller = Robolectric.buildActivity(
            DayWeaveLauncherActivity::class.java,
            Intent(context, DayWeaveLauncherActivity::class.java)
                .setAction(timedBreakNotificationAction(DIGEST_A))
                .putExtra(EXTRA_TIMED_BREAK_IDENTITY_DIGEST, DIGEST_A),
        ).create()
        val launcherActivity = controller.get()

        assertTrue(launcherActivity.isFinishing)
        val mainLaunch = shadowOf(launcherActivity).nextStartedActivity
        assertEquals(MainActivity::class.java.name, mainLaunch.component!!.className)
        assertEquals(Intent.ACTION_MAIN, mainLaunch.action)
        assertNull(timedBreakNotificationDigest(mainLaunch))
        assertNull(mainLaunch.extras)
    }

    @Test
    fun corruptDurableTrustedRouteIsDroppedFailClosed() {
        val context = RuntimeEnvironment.getApplication() as Context
        val preferences = routePreferences(context, "corrupt", reset = true)
        assertTrue(
            preferences.edit()
                .putString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, "not-a-digest")
                .putString(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY, "also-invalid")
                .commit(),
        )

        assertNull(TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value)
        assertFalse(preferences.contains(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY))
        assertFalse(preferences.contains(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY))

        assertTrue(
            preferences.edit()
                .putString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, DIGEST_A)
                .putString(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY, DIGEST_A)
                .commit(),
        )
        assertEquals(
            DIGEST_A,
            TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value,
        )
        assertFalse(preferences.contains(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY))
    }

    @Test
    fun coldStaleRouteWaitsForEncryptedStateThenIsDurablyConsumedWhenNoBreakExists() =
        runBlocking {
            val context = RuntimeEnvironment.getApplication() as Context
            val preferences = routePreferences(context, "cold-empty", reset = true)
            val mailbox = TimedBreakNotificationRouteMailbox(preferences)
            assertTrue(mailbox.issue(DIGEST_A))
            assertTrue(mailbox.acceptTrusted(routeIntent(context, DIGEST_A)))

            // READY can compose one frame before the SQLCipher-backed StateFlow publishes its
            // empty snapshot. This availability key must change even though both route keys are
            // otherwise the same "no break" value.
            assertFalse(timedBreakNotificationRouteStateAvailable(null))
            val restored = DayWeaveUiState()
            assertTrue(timedBreakNotificationRouteStateAvailable(restored))

            val consumption = PlannerTimedBreakNotificationRouteAccess(
                PlannerStore(restored),
            ).consume(DIGEST_A)
            assertEquals(TimedBreakNotificationRouteConsumption.REJECTED, consumption)
            assertEquals(
                TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK,
                timedBreakNotificationPresentationDecision(
                    consumption = consumption,
                    initiallyMatchedExactBreak = false,
                    currentEndedBreakKey = null,
                ),
            )
            assertTrue(mailbox.consume(DIGEST_A))
            assertNull(TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value)
        }

    @Test
    fun routeRequiresExactCurrentEndedLeaseAndRejectsReplacement() {
        val ended = timedBreakState(timedBreakEnded = true)
        val digest = ended.authoritativeTimedBreakNotificationIdentity()!!.digest
        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
            timedBreakEnded = true,
        )

        assertTrue(shouldOpenTimedBreakResolution(ended, ended, digest, DEADLINE + 1L))
        assertFalse(shouldOpenTimedBreakResolution(ended, ended, DIGEST_B, DEADLINE + 1L))
        assertFalse(
            shouldOpenTimedBreakResolution(ended, replacement, digest, DEADLINE + 700_000L),
        )
        assertFalse(
            shouldOpenTimedBreakResolution(
                ended.copy(activeSession = ended.activeSession!!.copy(timedBreakEnded = false)),
                ended,
                digest,
                DEADLINE + 1L,
            ),
        )
        assertFalse(
            shouldOpenTimedBreakResolution(
                ended,
                ended.copy(lastConsumedBreakEndNotificationDigest = digest),
                digest,
                DEADLINE + 1L,
            ),
        )
    }

    @Test
    fun staleNotificationAHasNoPresentationAuthorityOverCurrentBreakB() {
        assertFalse(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = DIGEST_A,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = null,
            ),
        )
        assertTrue(
            shouldOfferCurrentTimedBreakReview(
                endedBreakKey = DIGEST_B,
                pendingNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
            ),
        )
        assertFalse(
            shouldOfferCurrentTimedBreakReview(
                endedBreakKey = DIGEST_B,
                pendingNotificationDigest = DIGEST_A,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
            ),
        )
        assertTrue(
            shouldOfferCurrentTimedBreakReview(
                endedBreakKey = DIGEST_B,
                pendingNotificationDigest = DIGEST_A,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
                validatedRejectedRouteDigest = DIGEST_A,
            ),
        )
        assertFalse(
            shouldOfferCurrentTimedBreakReview(
                endedBreakKey = DIGEST_B,
                pendingNotificationDigest = DIGEST_A,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
                validatedRejectedRouteDigest = "c".repeat(64),
            ),
        )
        // The generic recovery prompt's explicit Review action clears the rejection. Only that
        // second step allows the ordinary B resolver to appear.
        assertTrue(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = null,
            ),
        )
        assertFalse(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
            ),
        )
        assertTrue(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = DIGEST_B,
                authorizedNotificationDigest = DIGEST_B,
                rejectedNotificationLaunchBreakKey = null,
            ),
        )
        assertTrue(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = null,
            ),
        )
        assertTrue(
            shouldPresentTimedBreakResolution(
                endedBreakKey = "local:new-break",
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
            ),
        )
    }

    @Test
    fun staleAToDurableLiveBIsRejectedOnceSanitizedAcrossRecreationAndExplicitlyReviewable() =
        runBlocking {
            val now = DEADLINE + 700_000L
            val currentB = timedBreakState(
                executionRevision = 9,
                sessionRevision = 4,
                deadline = DEADLINE + 600_000L,
                timedBreakEnded = true,
            )
            val digestB = currentB.authoritativeTimedBreakNotificationIdentity()!!.digest
            val store = PlannerStore(currentB, nowEpochMillis = { now })
            val routeAccess = PlannerTimedBreakNotificationRouteAccess(store) { now }
            val context = RuntimeEnvironment.getApplication() as Context
            val staleLaunchA = routeIntent(context, DIGEST_A)
            val preferences = routePreferences(context, "stale-route", reset = true)
            val firstMailbox = TimedBreakNotificationRouteMailbox(preferences).apply {
                assertTrue(issue(DIGEST_A))
                assertTrue(acceptTrusted(staleLaunchA))
            }

            val firstConsumption = routeAccess.consume(DIGEST_A)

            assertEquals(TimedBreakNotificationRouteConsumption.REJECTED, firstConsumption)
            assertEquals(DIGEST_A, store.durableState.value
                ?.lastRejectedBreakEndNotificationDigest)
            assertNull(store.durableState.value?.lastConsumedBreakEndNotificationDigest)
            assertEquals(
                TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW,
                timedBreakNotificationPresentationDecision(
                    firstConsumption,
                    initiallyMatchedExactBreak = false,
                    currentEndedBreakKey = digestB,
                ),
            )
            assertFalse(
                shouldPresentTimedBreakResolution(
                    endedBreakKey = digestB,
                    dismissedBreakKey = null,
                    pendingNotificationDigest = null,
                    authorizedNotificationDigest = null,
                    rejectedNotificationLaunchBreakKey = digestB,
                ),
            )
            assertTrue(
                shouldOfferCurrentTimedBreakReview(
                    endedBreakKey = digestB,
                    pendingNotificationDigest = DIGEST_A,
                    rejectedNotificationLaunchBreakKey = digestB,
                    validatedRejectedRouteDigest = DIGEST_A,
                ),
            )
            // The prompt's explicit Review action clears the rejection and only then reveals B.
            assertTrue(
                shouldPresentTimedBreakResolution(
                    endedBreakKey = digestB,
                    dismissedBreakKey = null,
                    pendingNotificationDigest = null,
                    authorizedNotificationDigest = null,
                    rejectedNotificationLaunchBreakKey = null,
                ),
            )

            // Crash after the encrypted rejection receipt but before the user chooses Review or
            // Not now. The opaque trusted mailbox remains durable, while a restored encrypted
            // planner snapshot retains only the rejection receipt and current B.
            assertEquals(DIGEST_A, firstMailbox.pendingDigest.value)
            val recreatedMailbox = TimedBreakNotificationRouteMailbox(preferences)
            assertEquals(DIGEST_A, recreatedMailbox.pendingDigest.value)
            val restoredStore = PlannerStore(
                store.durableState.value!!,
                nowEpochMillis = { now },
            )
            val replayConsumption = PlannerTimedBreakNotificationRouteAccess(restoredStore) { now }
                .consume(DIGEST_A)
            assertEquals(TimedBreakNotificationRouteConsumption.ALREADY_REJECTED, replayConsumption)
            assertEquals(
                TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW_NON_MODAL,
                timedBreakNotificationPresentationDecision(
                    replayConsumption,
                    initiallyMatchedExactBreak = false,
                    currentEndedBreakKey = digestB,
                ),
            )
            // Only the user's explicit generic-review choice clears the durable route fence.
            assertTrue(recreatedMailbox.consume(DIGEST_A))
            assertNull(TimedBreakNotificationRouteMailbox(preferences).pendingDigest.value)

            val exactStore = PlannerStore(currentB, nowEpochMillis = { now })
            val exactConsumption = PlannerTimedBreakNotificationRouteAccess(exactStore) { now }
                .consume(digestB)
            assertEquals(TimedBreakNotificationRouteConsumption.CONSUMED, exactConsumption)
            assertEquals(
                TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK,
                timedBreakNotificationPresentationDecision(
                    exactConsumption,
                    initiallyMatchedExactBreak = true,
                    currentEndedBreakKey = digestB,
                ),
            )
            val exactReplay = PlannerTimedBreakNotificationRouteAccess(exactStore) { now }
                .consume(digestB)
            assertEquals(TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED, exactReplay)
            assertTrue(
                isExactTimedBreakResolutionCurrent(
                    durableState = exactStore.durableState.value!!,
                    liveState = exactStore.state.value,
                    identityDigest = digestB,
                    nowEpochMillis = now,
                ),
            )
            assertEquals(
                TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK,
                timedBreakNotificationPresentationDecision(
                    exactReplay,
                    initiallyMatchedExactBreak = true,
                    currentEndedBreakKey = digestB,
                ),
            )

            val priorExactStore = PlannerStore(
                currentB.copy(lastConsumedBreakEndNotificationDigest = digestB),
                nowEpochMillis = { now },
            )
            assertEquals(
                TimedBreakNotificationRouteConsumption.REJECTED,
                PlannerTimedBreakNotificationRouteAccess(priorExactStore) { now }
                    .consume(DIGEST_A),
            )
            assertEquals(
                digestB,
                priorExactStore.durableState.value?.lastConsumedBreakEndNotificationDigest,
            )
            assertEquals(
                DIGEST_A,
                priorExactStore.durableState.value?.lastRejectedBreakEndNotificationDigest,
            )
        }

    @Test
    fun exactAuthorizationAChangingDirectlyToEndedBRequiresGenericReviewThenAllowsLaterBreak() {
        val transition = reconcileTimedBreakNotificationAuthorization(
            authorizedDigest = DIGEST_A,
            endedBreakKey = DIGEST_B,
        )

        assertNull(transition.authorizedDigest)
        assertEquals(DIGEST_B, transition.changedBreakReviewKey)
        assertFalse(
            shouldPresentTimedBreakResolution(
                endedBreakKey = DIGEST_B,
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = transition.authorizedDigest,
                rejectedNotificationLaunchBreakKey = transition.changedBreakReviewKey,
            ),
        )
        assertTrue(
            shouldOfferCurrentTimedBreakReview(
                endedBreakKey = DIGEST_B,
                pendingNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = transition.changedBreakReviewKey,
            ),
        )
        assertTrue(
            shouldPresentTimedBreakResolution(
                endedBreakKey = "local:later-break",
                dismissedBreakKey = null,
                pendingNotificationDigest = null,
                authorizedNotificationDigest = null,
                rejectedNotificationLaunchBreakKey = DIGEST_B,
            ),
        )
    }

    private fun routeIntent(context: Context, digest: String): Intent =
        Intent(context, MainActivity::class.java)
            .setAction(timedBreakNotificationAction(digest))
            .putExtra(EXTRA_TIMED_BREAK_IDENTITY_DIGEST, digest)

    private fun routePreferences(
        context: Context,
        suffix: String,
        reset: Boolean = false,
    ) = context.getSharedPreferences("test-timed-break-route-$suffix", Context.MODE_PRIVATE).also {
        if (reset) assertTrue(it.edit().clear().commit())
    }

    private fun worker(
        digest: String = DIGEST_A,
        runAttemptCount: Int,
        delivery: suspend (String) -> TimedBreakDeliveryCompletion,
    ): TimedBreakEndedWorker {
        val context = RuntimeEnvironment.getApplication() as Context
        return TestListenableWorkerBuilder
            .from(context, TimedBreakEndedWorker::class.java)
            .setInputData(
                Data.Builder()
                    .putString(TimedBreakEndedWorker.INPUT_IDENTITY_DIGEST, digest)
                    .build(),
            )
            .setRunAttemptCount(runAttemptCount)
            .setWorkerFactory(TimedBreakWorkerFactory(delivery))
            .build()
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TimedBreakWorkManagerPolicyTest {
    @After
    fun tearDown() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun realBackendKeepsExactActiveJobAcrossColdReconciliationThenReplacesAndCancels() =
        runBlocking {
        val context = RuntimeEnvironment.getApplication() as Context
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().build(),
        )
        val workManager = WorkManager.getInstance(context)
        val backend = WorkManagerTimedBreakNotificationBackend(context) { DEADLINE - 60_000L }
        val first = timedBreakState().authoritativeTimedBreakNotificationIdentity()!!
        val replacement = timedBreakState(
            executionRevision = 9,
            sessionRevision = 4,
            deadline = DEADLINE + 600_000L,
        ).authoritativeTimedBreakNotificationIdentity()!!

        backend.ensure(first, clearDisplayedBeforeSchedule = true)
        val firstWork = activeTimedBreakWork(workManager).single()
        // A cold coordinator/backend must recognize its own already-enqueued exact work rather
        // than REPLACE it and reset the OS deadline.
        WorkManagerTimedBreakNotificationBackend(context) { DEADLINE - 30_000L }
            .ensure(first, clearDisplayedBeforeSchedule = true)
        val retainedWork = activeTimedBreakWork(workManager).single()
        assertEquals(firstWork.id, retainedWork.id)

        backend.ensure(replacement, clearDisplayedBeforeSchedule = true)
        val replacementWork = activeTimedBreakWork(workManager).single()

        assertNotEquals(firstWork.id, replacementWork.id)
        backend.cancelWorkAndNotification()
        assertTrue(activeTimedBreakWork(workManager).isEmpty())
    }

    @Test
    fun realTestDriverRunningWorkIsCancelledAndAwaited() = runBlocking {
        val context = RuntimeEnvironment.getApplication() as Context
        val workerStarted = CompletableDeferred<Unit>()
        val keepWorkerRunning = CompletableDeferred<Unit>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder()
                .setWorkerFactory(
                    TimedBreakWorkerFactory {
                        workerStarted.complete(Unit)
                        keepWorkerRunning.await()
                        TimedBreakDeliveryCompletion.SUCCESS
                    },
                )
                .build(),
        )
        val workManager = WorkManager.getInstance(context)
        val backend = WorkManagerTimedBreakNotificationBackend(context) { DEADLINE + 1L }
        val identity = timedBreakState().authoritativeTimedBreakNotificationIdentity()!!

        backend.ensure(identity, clearDisplayedBeforeSchedule = true)
        val work = activeTimedBreakWork(workManager).single()
        requireNotNull(WorkManagerTestInitHelper.getTestDriver(context))
            .setInitialDelayMet(work.id)
        withTimeout(3_000) { workerStarted.await() }
        assertEquals(
            WorkInfo.State.RUNNING,
            requireNotNull(workManager.getWorkInfoById(work.id).get()).state,
        )

        backend.cancelWorkAndNotification()

        assertTrue(activeTimedBreakWork(workManager).isEmpty())
        keepWorkerRunning.complete(Unit)
        Unit
    }

    private fun activeTimedBreakWork(workManager: WorkManager): List<WorkInfo> =
        workManager.getWorkInfosForUniqueWork(
            WorkManagerTimedBreakNotificationBackend.UNIQUE_WORK_NAME,
        ).get().filterNot { it.state.isFinished }
}

private class TimedBreakWorkerFactory(
    private val delivery: suspend (String) -> TimedBreakDeliveryCompletion,
) : WorkerFactory() {
    override fun createWorker(
        appContext: Context,
        workerClassName: String,
        workerParameters: WorkerParameters,
    ): ListenableWorker? = if (workerClassName == TimedBreakEndedWorker::class.java.name) {
        TimedBreakEndedWorker(appContext, workerParameters, delivery)
    } else {
        null
    }
}

private class RecordingBackend : TimedBreakNotificationWorkBackend {
    constructor(failEnsureAttempts: Int = 0, failCancelAttempts: Int = 0) {
        failuresRemaining = failEnsureAttempts
        cancelFailuresRemaining = failCancelAttempts
    }

    val ensures = mutableListOf<Pair<TimedBreakNotificationIdentity, Boolean>>()
    var fullCancels = 0
    var ensureAttempts = 0
    private var failuresRemaining = 0
    private var cancelFailuresRemaining = 0

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        ensureAttempts += 1
        if (failuresRemaining > 0) {
            failuresRemaining -= 1
            error("synthetic backend failure")
        }
        ensures += identity to clearDisplayedBeforeSchedule
    }

    override suspend fun cancelWorkAndNotification() {
        fullCancels += 1
        if (cancelFailuresRemaining > 0) {
            cancelFailuresRemaining -= 1
            error("synthetic partial cancellation failure")
        }
    }
}

private class AwaitedCancelBackend : TimedBreakNotificationWorkBackend {
    val cancelEntered = CompletableDeferred<Unit>()
    val allowCancel = CompletableDeferred<Unit>()
    var ensures = 0
    var cancels = 0

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        ensures += 1
    }

    override suspend fun cancelWorkAndNotification() {
        cancelEntered.complete(Unit)
        allowCancel.await()
        cancels += 1
    }
}

private class PartialCancelAndRestoreFailureBackend : TimedBreakNotificationWorkBackend {
    var ensureAttempts = 0

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        ensureAttempts += 1
        if (ensureAttempts == 2) error("synthetic restore scheduling failure")
    }

    override suspend fun cancelWorkAndNotification() {
        error("synthetic partial cancellation failure")
    }
}

private class CancellationDuringCancelBackend : TimedBreakNotificationWorkBackend {
    var ensureAttempts = 0

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        ensureAttempts += 1
    }

    override suspend fun cancelWorkAndNotification() {
        throw kotlinx.coroutines.CancellationException("synthetic cancellation after side effect")
    }
}

private class CancellationDuringSecondEnsureBackend : TimedBreakNotificationWorkBackend {
    var ensureAttempts = 0

    override suspend fun ensure(
        identity: TimedBreakNotificationIdentity,
        clearDisplayedBeforeSchedule: Boolean,
    ) {
        ensureAttempts += 1
        if (ensureAttempts == 2) {
            throw kotlinx.coroutines.CancellationException(
                "synthetic cancellation after partial ensure",
            )
        }
    }

    override suspend fun cancelWorkAndNotification() = Unit
}

private class ExclusiveRecordingStateAccess : TimedBreakNotificationStateAccess {
    private val handled = AtomicBoolean(false)
    val claims = AtomicInteger(0)

    override suspend fun prepare(expectedDigest: String): TimedBreakPreparation =
        if (handled.compareAndSet(false, true)) {
            claims.incrementAndGet()
            TimedBreakPreparation.READY
        } else {
            TimedBreakPreparation.ALREADY_HANDLED
        }

    override suspend fun validateClaim(expectedDigest: String): TimedBreakClaimValidation =
        if (handled.get()) TimedBreakClaimValidation.CURRENT else TimedBreakClaimValidation.STALE
}

private class BlockingRecordingGateway : TimedBreakNotificationGateway {
    val postEntered = CountDownLatch(1)
    val allowPost = CountDownLatch(1)
    val posts = AtomicInteger(0)

    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        postEntered.countDown()
        check(allowPost.await(3, TimeUnit.SECONDS))
        posts.incrementAndGet()
        return TimedBreakNotificationPostResult.POSTED
    }

    override fun cancel() = Unit
}

private class BlockingVisibleGateway : TimedBreakNotificationGateway {
    val postEntered = CountDownLatch(1)
    val allowPost = CountDownLatch(1)
    val posts = AtomicInteger(0)
    val cancels = AtomicInteger(0)
    val visible = AtomicBoolean(false)

    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        postEntered.countDown()
        check(allowPost.await(3, TimeUnit.SECONDS))
        visible.set(true)
        posts.incrementAndGet()
        return TimedBreakNotificationPostResult.POSTED
    }

    override fun cancel() {
        visible.set(false)
        cancels.incrementAndGet()
    }
}

private class RecordingStateAccess(
    private val preparation: TimedBreakPreparation,
    private val validations: List<TimedBreakClaimValidation> =
        listOf(TimedBreakClaimValidation.CURRENT),
) : TimedBreakNotificationStateAccess {
    var validationCalls = 0

    override suspend fun prepare(expectedDigest: String): TimedBreakPreparation = preparation

    override suspend fun validateClaim(expectedDigest: String): TimedBreakClaimValidation {
        val result = validations[validationCalls.coerceAtMost(validations.lastIndex)]
        validationCalls += 1
        return result
    }
}

private class ThrowingGateway : TimedBreakNotificationGateway {
    override fun post(identityDigest: String): TimedBreakNotificationPostResult =
        error("permanent notification backend failure")

    override fun cancel() = Unit
}

private class RecordingGateway(
    private val result: TimedBreakNotificationPostResult = TimedBreakNotificationPostResult.POSTED,
) : TimedBreakNotificationGateway {
    var posts = 0
    var cancels = 0

    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        posts += 1
        return result
    }

    override fun cancel() {
        cancels += 1
    }
}

private fun timedBreakState(
    executionRevision: Long = 7L,
    sessionRevision: Long = 3L,
    deadline: Long = DEADLINE,
    timedBreakEnded: Boolean = false,
): DayWeaveUiState {
    val deadlineInstant = Instant.ofEpochMilli(deadline).toString()
    val session = CanonicalExecutionSessionSnapshot(
        id = SESSION_ID,
        itemId = ITEM_ID,
        itemRevision = 2,
        sessionIndex = 0,
        plannedBlockId = BLOCK_ID,
        sourceDeviceId = DEVICE_ID,
        status = "paused",
        revision = sessionRevision,
        accumulatedSeconds = 300,
        startedAt = "2026-09-01T06:00:00Z",
        pausedAt = "2026-09-01T06:05:00Z",
        pauseUntil = deadlineInstant,
        createdAt = "2026-09-01T06:00:00Z",
        updatedAt = "2026-09-01T06:05:00Z",
    )
    return DayWeaveUiState(
        canonicalExecutionRevision = executionRevision,
        canonicalExecutionSession = session,
        activeSession = ActiveSession(
            itemId = BLOCK_ID,
            elapsedMinutes = 5,
            isPaused = true,
            accumulatedSeconds = 300,
            pauseUntilEpochMillis = deadline,
            timedBreakEnded = timedBreakEnded,
            canonicalExecutionSessionId = SESSION_ID,
        ),
    )
}

private const val SESSION_ID = "11111111-1111-4111-8111-111111111111"
private const val ITEM_ID = "22222222-2222-4222-8222-222222222222"
private const val BLOCK_ID = "33333333-3333-4333-8333-333333333333"
private const val DEVICE_ID = "44444444-4444-4444-8444-444444444444"
private val DEADLINE = Instant.parse("2026-09-01T06:10:00Z").toEpochMilli()
private const val DIGEST_A =
    "sha256:28447e15abbd7cd6272e45f8ef320098eff063147e830c6c6aa7114b39986754"
private const val DIGEST_B =
    "sha256:b8447e15abbd7cd6272e45f8ef320098eff063147e830c6c6aa7114b39986755"
private const val SENSITIVE_CANARY = "PRIVATE dentist appointment with Alice"
