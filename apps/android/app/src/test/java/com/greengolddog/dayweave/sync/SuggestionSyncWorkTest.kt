package com.greengolddog.dayweave.sync

import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.RemoteSuggestion
import com.greengolddog.dayweave.network.SuggestionsTransport
import com.greengolddog.dayweave.state.PlannerStore
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SuggestionSyncWorkTest {
    @Test
    fun configuredStartupSchedulesConservativePeriodicAndImmediateWork() {
        val backend = RecordingWorkBackend()
        val coordinator = SuggestionSyncSchedulingCoordinator(
            credentialStore = SchedulingCredentialStore(configured = true),
            backend = backend,
        )

        coordinator.onAppStart()

        val periodic = requireNotNull(backend.periodicPolicy)
        assertEquals(12L, periodic.repeatIntervalHours)
        assertEquals(2L, periodic.flexIntervalHours)
        assertEquals(30L, periodic.retryBackoffMinutes)
        assertTrue(periodic.requiresConnectedNetwork)
        assertEquals(periodic, backend.startupPolicy)
        assertEquals(null, backend.configurationPolicy)
        assertFalse(backend.cancelled)
    }

    @Test
    fun unconfiguredStartupCancelsStaleWorkAndNeverEnqueues() {
        val backend = RecordingWorkBackend()
        val coordinator = SuggestionSyncSchedulingCoordinator(
            credentialStore = SchedulingCredentialStore(configured = false),
            backend = backend,
        )

        coordinator.onAppStart()

        assertTrue(backend.cancelled)
        assertEquals(null, backend.periodicPolicy)
        assertEquals(null, backend.startupPolicy)
        assertEquals(null, backend.configurationPolicy)
    }

    @Test
    fun configurationSaveReplacesEvenAnExistingStartupRefresh() = runBlocking {
        val credentials = SchedulingCredentialStore(configured = false)
        val backend = RecordingWorkBackend()
        val coordinator = SuggestionSyncSchedulingCoordinator(credentials, backend)

        credentials.configured = true
        coordinator.onAppStart()
        coordinator.onConfigurationSaved()
        coordinator.cancelBeforeCredentialClear()

        assertTrue(backend.periodicPolicy != null)
        assertTrue(backend.startupPolicy != null)
        assertTrue(backend.configurationPolicy != null)
        assertEquals(listOf("periodic", "startup", "periodic", "replace", "cancel"), backend.events)
        assertTrue(backend.cancelled)
    }

    @Test
    fun consentReleaseCancellationIsAwaitedBeforeStartupBootstrap() = runBlocking {
        val backend = AwaitedConsentReleaseWorkBackend()
        val coordinator = SuggestionSyncSchedulingCoordinator(
            credentialStore = SchedulingCredentialStore(configured = true),
            backend = backend,
        )

        val cancellation = async(Dispatchers.Default) {
            coordinator.cancelBeforeConsentRelease()
        }
        withTimeout(3_000) { backend.cancelEntered.await() }
        assertFalse(cancellation.isCompleted)

        backend.allowCancellation.complete(Unit)
        withTimeout(3_000) { cancellation.await() }
        coordinator.onAppStart()

        assertEquals(
            listOf("cancel-start", "cancel-finished", "periodic", "startup"),
            backend.events,
        )
    }

    @Test
    fun forgetCancelsAndAwaitsBeforeAControlStorePartiallyFailsToClear() = runBlocking {
        val events = mutableListOf<String>()
        val credentials = SchedulingCredentialStore(
            configured = true,
            events = events,
            failAfterClear = true,
        )
        val backend = RecordingWorkBackend(events)
        val manager = SuggestionSyncManager(
            plannerStore = PlannerStore(),
            credentialStore = credentials,
            transport = NeverSuggestionsTransport,
        )
        val controller = SuggestionConnectionController(
            syncManager = manager,
            schedulingCoordinator = SuggestionSyncSchedulingCoordinator(credentials, backend),
        )

        val cleared = controller.forget()

        assertFalse(cleared)
        assertEquals(listOf("cancel", "clear"), events)
        assertTrue(backend.cancelled)
    }

    @Test
    fun cancellationFailureLeavesCredentialsIntactInsteadOfRacingAWorker() = runBlocking {
        val events = mutableListOf<String>()
        val credentials = SchedulingCredentialStore(configured = true, events = events)
        val backend = RecordingWorkBackend(events, failCancellation = true)
        val manager = SuggestionSyncManager(
            plannerStore = PlannerStore(),
            credentialStore = credentials,
            transport = NeverSuggestionsTransport,
        )
        val controller = SuggestionConnectionController(
            syncManager = manager,
            schedulingCoordinator = SuggestionSyncSchedulingCoordinator(credentials, backend),
        )

        val cleared = controller.forget()

        assertFalse(cleared)
        assertEquals(listOf("cancel"), events)
        assertTrue(credentials.configured)
        assertEquals(SuggestionSyncPhase.ERROR, manager.state.value.phase)
    }

    @Test
    fun workNamesAndPoliciesAreUniqueAndIdempotent() {
        assertNotEquals(
            WorkManagerSuggestionSyncBackend.PERIODIC_WORK_NAME,
            WorkManagerSuggestionSyncBackend.IMMEDIATE_WORK_NAME,
        )
        assertEquals(
            ExistingPeriodicWorkPolicy.UPDATE,
            WorkManagerSuggestionSyncBackend.PERIODIC_EXISTING_WORK_POLICY,
        )
        assertEquals(
            ExistingWorkPolicy.REPLACE,
            WorkManagerSuggestionSyncBackend.STARTUP_IMMEDIATE_EXISTING_WORK_POLICY,
        )
        assertEquals(
            ExistingWorkPolicy.REPLACE,
            WorkManagerSuggestionSyncBackend.CONFIGURATION_IMMEDIATE_EXISTING_WORK_POLICY,
        )
    }

    @Test
    fun workerRetriesOnlyTransientNetworkAndServerFailures() {
        val retryable = setOf(
            SuggestionRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE,
        )

        SuggestionRefreshOutcome.entries.forEach { outcome ->
            val expected = when {
                outcome == SuggestionRefreshOutcome.SUCCESS -> SuggestionWorkerCompletion.SUCCESS
                outcome in retryable -> SuggestionWorkerCompletion.RETRY
                else -> SuggestionWorkerCompletion.FAILURE
            }
            assertEquals(expected, outcome.toWorkerCompletion())
        }
    }
}

private class AwaitedConsentReleaseWorkBackend : SuggestionSyncWorkBackend {
    val cancelEntered = CompletableDeferred<Unit>()
    val allowCancellation = CompletableDeferred<Unit>()
    val events = mutableListOf<String>()

    override fun ensurePeriodic(policy: SuggestionSyncWorkPolicy) {
        events += "periodic"
    }

    override fun enqueueStartupRefresh(policy: SuggestionSyncWorkPolicy) {
        events += "startup"
    }

    override fun replaceConfigurationRefresh(policy: SuggestionSyncWorkPolicy) {
        events += "configuration"
    }

    override fun cancelAll() {
        events += "cancel"
    }

    override suspend fun cancelAllAndAwait() {
        events += "cancel-start"
        cancelEntered.complete(Unit)
        allowCancellation.await()
        events += "cancel-finished"
    }
}

private class SchedulingCredentialStore(
    var configured: Boolean,
    private val events: MutableList<String>? = null,
    private val failAfterClear: Boolean = false,
) : ApiCredentialStore {
    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = if (configured) "https://api.example.test/" else null,
        hasBearerToken = configured,
        lastSuccessfulSyncEpochMillis = null,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? = null

    override fun update(baseUrl: String, bearerToken: String?) = Unit

    override fun clear() {
        events?.add("clear")
        configured = false
        if (failAfterClear) throw IllegalStateException("synthetic partial clear failure")
    }

    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class RecordingWorkBackend(
    val events: MutableList<String> = mutableListOf(),
    private val failCancellation: Boolean = false,
) : SuggestionSyncWorkBackend {
    var periodicPolicy: SuggestionSyncWorkPolicy? = null
    var startupPolicy: SuggestionSyncWorkPolicy? = null
    var configurationPolicy: SuggestionSyncWorkPolicy? = null
    var cancelled = false

    override fun ensurePeriodic(policy: SuggestionSyncWorkPolicy) {
        periodicPolicy = policy
        events += "periodic"
    }

    override fun enqueueStartupRefresh(policy: SuggestionSyncWorkPolicy) {
        startupPolicy = policy
        events += "startup"
    }

    override fun replaceConfigurationRefresh(policy: SuggestionSyncWorkPolicy) {
        configurationPolicy = policy
        events += "replace"
    }

    override fun cancelAll() {
        cancelled = true
        events += "cancel"
    }

    override suspend fun cancelAllAndAwait() {
        cancelAll()
        if (failCancellation) throw IllegalStateException("synthetic cancellation failure")
    }
}

private object NeverSuggestionsTransport : SuggestionsTransport {
    override suspend fun list(
        configuration: AuthenticatedApiConfiguration,
    ): List<RemoteSuggestion> = error("Not used")

    override suspend fun edit(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
        title: String,
        explanation: String,
    ): RemoteSuggestion = error("Not used")

    override suspend fun accept(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion = error("Not used")

    override suspend fun reject(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion = error("Not used")
}
