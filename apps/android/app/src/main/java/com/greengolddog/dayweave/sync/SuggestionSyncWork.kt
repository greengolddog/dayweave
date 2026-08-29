package com.greengolddog.dayweave.sync

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.PeriodicWorkRequest
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.await
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.network.ApiCredentialStore
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

data class SuggestionSyncWorkPolicy(
    val repeatIntervalHours: Long = 12,
    val flexIntervalHours: Long = 2,
    val retryBackoffMinutes: Long = 30,
    val requiresConnectedNetwork: Boolean = true,
) {
    init {
        require(repeatIntervalHours >= 1) { "Repeat interval must be positive" }
        require(flexIntervalHours in 1..repeatIntervalHours) {
            "Flex interval must be positive and no longer than the repeat interval"
        }
        require(retryBackoffMinutes >= 10) { "Retry backoff must be at least 10 minutes" }
        require(requiresConnectedNetwork) { "Suggestion sync must require network connectivity" }
    }
}

interface SuggestionSyncWorkBackend {
    fun ensurePeriodic(policy: SuggestionSyncWorkPolicy)
    fun enqueueStartupRefresh(policy: SuggestionSyncWorkPolicy)
    fun replaceConfigurationRefresh(policy: SuggestionSyncWorkPolicy)
    fun cancelAll()
    suspend fun cancelAllAndAwait() = cancelAll()
}

/** Decides whether any work should exist without reading or exposing the bearer token. */
class SuggestionSyncSchedulingCoordinator(
    private val credentialStore: ApiCredentialStore,
    private val backend: SuggestionSyncWorkBackend,
    private val policy: SuggestionSyncWorkPolicy = SuggestionSyncWorkPolicy(),
) {
    fun onAppStart() = reconcileAndRefresh(configurationChanged = false)

    fun onConfigurationSaved() = reconcileAndRefresh(configurationChanged = true)

    suspend fun cancelBeforeCredentialClear() = backend.cancelAllAndAwait()

    private fun reconcileAndRefresh(configurationChanged: Boolean) {
        val connection = credentialStore.snapshot()
        if (connection.baseUrl != null && connection.hasBearerToken) {
            backend.ensurePeriodic(policy)
            if (configurationChanged) {
                backend.replaceConfigurationRefresh(policy)
            } else {
                backend.enqueueStartupRefresh(policy)
            }
        } else {
            backend.cancelAll()
        }
    }
}

/** Serializes settings changes so work can never be re-enqueued between cancellation and clear. */
class SuggestionConnectionController(
    private val syncManager: SuggestionSyncManager,
    private val schedulingCoordinator: SuggestionSyncSchedulingCoordinator,
    private val canonicalSyncManager: CanonicalSyncManager? = null,
) {
    private val settingsMutex = Mutex()

    suspend fun update(baseUrl: String, bearerToken: String?): Boolean = settingsMutex.withLock {
        try {
            withCanonicalConfigurationUpdateLock(baseUrl) {
                if (!syncManager.updateConnection(baseUrl, bearerToken)) {
                    return@withCanonicalConfigurationUpdateLock false
                }
                schedulingCoordinator.onConfigurationSaved()
                true
            }
        } catch (_: CanonicalConfigurationChangeBlockedException) {
            false
        } catch (_: CanonicalAbandonmentPersistenceException) {
            false
        }
    }

    suspend fun forget(): Boolean = settingsMutex.withLock {
        try {
            canonicalSyncManager?.forgetConfiguration(
                cancelBackgroundWork = {
                    try {
                        schedulingCoordinator.cancelBeforeCredentialClear()
                        true
                    } catch (error: CancellationException) {
                        throw error
                    } catch (error: Exception) {
                        syncManager.reportCredentialClearBlocked()
                        false
                    }
                },
                clearCredentials = syncManager::clearConnection,
            ) ?: forgetWithoutCanonicalManager()
        } catch (_: CanonicalConfigurationChangeBlockedException) {
            false
        } catch (_: CanonicalAbandonmentPersistenceException) {
            false
        }
    }

    private suspend fun <T> withCanonicalConfigurationUpdateLock(
        baseUrl: String,
        block: suspend () -> T,
    ): T = canonicalSyncManager?.withConfigurationUpdateLock(baseUrl, block) ?: block()

    private suspend fun forgetWithoutCanonicalManager(): Boolean {
        return try {
            schedulingCoordinator.cancelBeforeCredentialClear()
            syncManager.clearConnection()
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            syncManager.reportCredentialClearBlocked()
            false
        }
    }
}

class WorkManagerSuggestionSyncBackend(
    context: Context,
) : SuggestionSyncWorkBackend {
    private val workManager = WorkManager.getInstance(context.applicationContext)

    override fun ensurePeriodic(policy: SuggestionSyncWorkPolicy) {
        workManager.enqueueUniquePeriodicWork(
            PERIODIC_WORK_NAME,
            PERIODIC_EXISTING_WORK_POLICY,
            buildSuggestionPeriodicWorkRequest(policy),
        )
    }

    override fun enqueueStartupRefresh(policy: SuggestionSyncWorkPolicy) {
        workManager.enqueueUniqueWork(
            IMMEDIATE_WORK_NAME,
            STARTUP_IMMEDIATE_EXISTING_WORK_POLICY,
            buildSuggestionImmediateWorkRequest(policy),
        )
    }

    override fun replaceConfigurationRefresh(policy: SuggestionSyncWorkPolicy) {
        workManager.enqueueUniqueWork(
            IMMEDIATE_WORK_NAME,
            CONFIGURATION_IMMEDIATE_EXISTING_WORK_POLICY,
            buildSuggestionImmediateWorkRequest(policy),
        )
    }

    override fun cancelAll() {
        cancelOperations()
    }

    override suspend fun cancelAllAndAwait() {
        cancelOperations().forEach { operation -> operation.await() }
    }

    private fun cancelOperations() = listOf(
        workManager.cancelUniqueWork(PERIODIC_WORK_NAME),
        workManager.cancelUniqueWork(IMMEDIATE_WORK_NAME),
    )

    companion object {
        internal const val PERIODIC_WORK_NAME = "dayweave-suggestion-refresh-periodic-v1"
        internal const val IMMEDIATE_WORK_NAME = "dayweave-suggestion-refresh-immediate-v1"
        internal const val WORK_TAG = "dayweave-suggestion-refresh"
        internal val PERIODIC_EXISTING_WORK_POLICY = ExistingPeriodicWorkPolicy.UPDATE
        internal val STARTUP_IMMEDIATE_EXISTING_WORK_POLICY = ExistingWorkPolicy.KEEP
        internal val CONFIGURATION_IMMEDIATE_EXISTING_WORK_POLICY = ExistingWorkPolicy.REPLACE
    }
}

internal fun buildSuggestionPeriodicWorkRequest(
    policy: SuggestionSyncWorkPolicy,
): PeriodicWorkRequest = PeriodicWorkRequest.Builder(
    SuggestionRefreshWorker::class.java,
    policy.repeatIntervalHours,
    TimeUnit.HOURS,
    policy.flexIntervalHours,
    TimeUnit.HOURS,
)
    .setInitialDelay(policy.repeatIntervalHours, TimeUnit.HOURS)
    .setConstraints(policy.constraints())
    .setBackoffCriteria(
        BackoffPolicy.EXPONENTIAL,
        policy.retryBackoffMinutes,
        TimeUnit.MINUTES,
    )
    .addTag(WorkManagerSuggestionSyncBackend.WORK_TAG)
    .build()

internal fun buildSuggestionImmediateWorkRequest(
    policy: SuggestionSyncWorkPolicy,
): OneTimeWorkRequest = OneTimeWorkRequest.Builder(SuggestionRefreshWorker::class.java)
    .setConstraints(policy.constraints())
    .setBackoffCriteria(
        BackoffPolicy.EXPONENTIAL,
        policy.retryBackoffMinutes,
        TimeUnit.MINUTES,
    )
    .addTag(WorkManagerSuggestionSyncBackend.WORK_TAG)
    .build()

private fun SuggestionSyncWorkPolicy.constraints(): Constraints = Constraints.Builder()
    .setRequiredNetworkType(
        if (requiresConnectedNetwork) NetworkType.CONNECTED else NetworkType.NOT_REQUIRED,
    )
    .build()

enum class SuggestionWorkerCompletion {
    SUCCESS,
    RETRY,
    FAILURE,
}

internal fun SuggestionRefreshOutcome.toWorkerCompletion(): SuggestionWorkerCompletion = when (this) {
    SuggestionRefreshOutcome.SUCCESS -> SuggestionWorkerCompletion.SUCCESS
    SuggestionRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
    SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE,
    -> SuggestionWorkerCompletion.RETRY
    SuggestionRefreshOutcome.NOT_CONFIGURED,
    SuggestionRefreshOutcome.AUTH_REQUIRED,
    SuggestionRefreshOutcome.CONFIGURATION_ERROR,
    SuggestionRefreshOutcome.PERMANENT_SERVER_FAILURE,
    SuggestionRefreshOutcome.PROTOCOL_FAILURE,
    SuggestionRefreshOutcome.LOCAL_STORAGE_FAILURE,
    SuggestionRefreshOutcome.UNEXPECTED_FAILURE,
    -> SuggestionWorkerCompletion.FAILURE
}

class SuggestionRefreshWorker(
    appContext: Context,
    workerParameters: WorkerParameters,
) : CoroutineWorker(appContext, workerParameters) {
    private var refreshOverride: (suspend () -> SuggestionRefreshOutcome)? = null

    internal constructor(
        appContext: Context,
        workerParameters: WorkerParameters,
        refresh: suspend () -> SuggestionRefreshOutcome,
    ) : this(appContext, workerParameters) {
        refreshOverride = refresh
    }

    override suspend fun doWork(): Result {
        val completion = try {
            val outcome = refreshOverride?.invoke() ?: run {
                val application = applicationContext as? DayWeaveApplication
                    ?: return Result.failure()
                application.suggestionSyncManager.refresh()
            }
            outcome.toWorkerCompletion()
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            SuggestionWorkerCompletion.FAILURE
        }
        return when (completion) {
            SuggestionWorkerCompletion.SUCCESS -> Result.success()
            SuggestionWorkerCompletion.RETRY -> Result.retry()
            SuggestionWorkerCompletion.FAILURE -> Result.failure()
        }
    }
}
