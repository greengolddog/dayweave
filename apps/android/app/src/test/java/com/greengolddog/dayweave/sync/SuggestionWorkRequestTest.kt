package com.greengolddog.dayweave.sync

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.NetworkType
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.WorkerFactory
import androidx.work.WorkerParameters
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SuggestionWorkRequestTest {
    private val policy = SuggestionSyncWorkPolicy()

    @Test
    fun realPeriodicRequestHasConnectedConstraintDelayFlexAndExponentialBackoff() {
        val request = buildSuggestionPeriodicWorkRequest(policy)
        val spec = request.workSpec

        assertEquals(TimeUnit.HOURS.toMillis(12), spec.intervalDuration)
        assertEquals(TimeUnit.HOURS.toMillis(2), spec.flexDuration)
        assertEquals(TimeUnit.HOURS.toMillis(12), spec.initialDelay)
        assertEquals(NetworkType.CONNECTED, spec.constraints.requiredNetworkType)
        assertEquals(BackoffPolicy.EXPONENTIAL, spec.backoffPolicy)
        assertEquals(TimeUnit.MINUTES.toMillis(30), spec.backoffDelayDuration)
        assertTrue(request.tags.contains(WorkManagerSuggestionSyncBackend.WORK_TAG))
        assertTrue(spec.input.keyValueMap.isEmpty())
    }

    @Test
    fun realImmediateRequestRunsWithoutDelayButRetainsConstraintAndBackoff() {
        val request = buildSuggestionImmediateWorkRequest(policy)
        val spec = request.workSpec

        assertEquals(0L, spec.initialDelay)
        assertEquals(NetworkType.CONNECTED, spec.constraints.requiredNetworkType)
        assertEquals(BackoffPolicy.EXPONENTIAL, spec.backoffPolicy)
        assertEquals(TimeUnit.MINUTES.toMillis(30), spec.backoffDelayDuration)
        assertTrue(request.tags.contains(WorkManagerSuggestionSyncBackend.WORK_TAG))
        assertTrue(spec.input.keyValueMap.isEmpty())
    }

    @Test
    fun workTestingBuildsRealWorkerAndPreservesOutcomeContract() = runBlocking {
        val expected = mapOf(
            SuggestionRefreshOutcome.SUCCESS to ListenableWorker.Result.success(),
            SuggestionRefreshOutcome.TRANSIENT_NETWORK_FAILURE to ListenableWorker.Result.retry(),
            SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE to ListenableWorker.Result.retry(),
            SuggestionRefreshOutcome.AUTH_REQUIRED to ListenableWorker.Result.failure(),
            SuggestionRefreshOutcome.CONFIGURATION_ERROR to ListenableWorker.Result.failure(),
            SuggestionRefreshOutcome.PROTOCOL_FAILURE to ListenableWorker.Result.failure(),
        )

        expected.forEach { (outcome, result) ->
            val worker = worker { outcome }
            assertEquals(result, worker.doWork())
        }
    }

    @Test
    fun realWorkerFailsUnexpectedErrorsWithoutLeakingThemIntoOutputData() = runBlocking {
        val worker = worker { throw IOException("synthetic transport failure") }

        val result = worker.doWork()

        assertEquals(ListenableWorker.Result.failure(), result)
        assertTrue(result.outputData.keyValueMap.isEmpty())
    }

    @Test
    fun oldQueuedWorkerIsASuccessfulNoOpBeforePrivacyAcknowledgement() = runBlocking {
        var refreshCalls = 0
        val worker = worker(workAllowed = false) {
            refreshCalls += 1
            SuggestionRefreshOutcome.SUCCESS
        }

        assertEquals(ListenableWorker.Result.success(), worker.doWork())
        assertEquals(0, refreshCalls)
    }

    private fun worker(
        workAllowed: Boolean = true,
        refresh: suspend () -> SuggestionRefreshOutcome,
    ): SuggestionRefreshWorker {
        val context = RuntimeEnvironment.getApplication()
        return TestListenableWorkerBuilder
            .from(context, SuggestionRefreshWorker::class.java)
            .setWorkerFactory(
                RefreshWorkerFactory(
                    workAllowed = workAllowed,
                    refresh = refresh,
                ),
            )
            .build()
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SuggestionWorkManagerPolicyTest {
    @After
    fun tearDown() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun realUniqueWorkReplacesEveryImmediateGenerationAndAwaitsCancellation() = runBlocking {
        val context = RuntimeEnvironment.getApplication()
        val workerStarted = CompletableDeferred<Unit>()
        val keepWorkerRunning = CompletableDeferred<Unit>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder()
                .setWorkerFactory(
                    RefreshWorkerFactory {
                        workerStarted.complete(Unit)
                        keepWorkerRunning.await()
                        SuggestionRefreshOutcome.SUCCESS
                    },
                )
                .build(),
        )
        val workManager = WorkManager.getInstance(context)
        val backend = WorkManagerSuggestionSyncBackend(context)

        backend.ensurePeriodic(SuggestionSyncWorkPolicy())
        backend.enqueueStartupRefresh(SuggestionSyncWorkPolicy())
        val firstId = activeWork(workManager, WorkManagerSuggestionSyncBackend.IMMEDIATE_WORK_NAME)
            .single()
            .id
        requireNotNull(WorkManagerTestInitHelper.getTestDriver(context))
            .setAllConstraintsMet(firstId)
        withTimeout(3_000) { workerStarted.await() }
        assertEquals(
            WorkInfo.State.RUNNING,
            requireNotNull(workManager.getWorkInfoById(firstId).get()).state,
        )

        backend.enqueueStartupRefresh(SuggestionSyncWorkPolicy())
        val startupReplacement = activeWork(
            workManager,
            WorkManagerSuggestionSyncBackend.IMMEDIATE_WORK_NAME,
        ).single()
        assertTrue(startupReplacement.id != firstId)

        backend.replaceConfigurationRefresh(SuggestionSyncWorkPolicy())
        val replacement = activeWork(
            workManager,
            WorkManagerSuggestionSyncBackend.IMMEDIATE_WORK_NAME,
        ).single()
        assertTrue(replacement.id != startupReplacement.id)

        backend.cancelAllAndAwait()
        assertTrue(
            activeWork(workManager, WorkManagerSuggestionSyncBackend.IMMEDIATE_WORK_NAME).isEmpty(),
        )
        assertTrue(
            activeWork(workManager, WorkManagerSuggestionSyncBackend.PERIODIC_WORK_NAME).isEmpty(),
        )
    }

    private fun activeWork(workManager: WorkManager, name: String): List<WorkInfo> =
        workManager.getWorkInfosForUniqueWork(name).get().filterNot { it.state.isFinished }
}

private class RefreshWorkerFactory(
    private val workAllowed: Boolean = true,
    private val refresh: suspend () -> SuggestionRefreshOutcome,
) : WorkerFactory() {
    override fun createWorker(
        appContext: Context,
        workerClassName: String,
        workerParameters: WorkerParameters,
    ): ListenableWorker? = if (workerClassName == SuggestionRefreshWorker::class.java.name) {
        SuggestionRefreshWorker(
            appContext = appContext,
            workerParameters = workerParameters,
            refresh = refresh,
            workAllowed = { workAllowed },
        )
    } else {
        null
    }
}
