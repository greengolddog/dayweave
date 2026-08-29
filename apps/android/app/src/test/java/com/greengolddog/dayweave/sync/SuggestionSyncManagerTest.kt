package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.RemoteSuggestion
import com.greengolddog.dayweave.network.SuggestionApiException
import com.greengolddog.dayweave.network.SuggestionsTransport
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SuggestionSyncManagerTest {
    private val now = Instant.parse("2026-08-29T09:00:00Z").toEpochMilli()

    @Test
    fun cancelledBackgroundRefreshRestoresAUsableNonBusyState() = runBlocking {
        val started = CompletableDeferred<Unit>()
        val neverFinish = CompletableDeferred<Unit>()
        val transport = FakeSuggestionsTransport().apply {
            listStarted = started
            listGate = neverFinish
        }
        val manager = manager(PlannerStore(DayWeaveUiState()), transport)

        val refresh = async { manager.refresh() }
        withTimeout(3_000) { started.await() }
        assertEquals(SuggestionSyncPhase.SYNCING, manager.state.value.phase)

        refresh.cancelAndJoin()

        assertEquals(SuggestionSyncPhase.READY, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun refreshCannotReportSuccessBeforeItsExactEncryptedSnapshotIsDurable() = runBlocking {
        val initial = DayWeaveUiState(suggestions = emptyList())
        val saveStarted = CompletableDeferred<Unit>()
        val allowSave = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveStarted.complete(Unit)
                allowSave.await()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val plannerStore = PlannerStore(initial, repository, scope)
            withTimeout(3_000) {
                plannerStore.loadState.first { it == PlannerLoadState.READY }
            }
            val transport = FakeSuggestionsTransport().apply {
                listed = listOf(remoteSuggestion())
            }
            val manager = manager(plannerStore, transport)

            val outcome = async { manager.refresh() }
            withTimeout(3_000) { saveStarted.await() }

            assertTrue(!outcome.isCompleted)
            assertEquals(SuggestionSyncPhase.SYNCING, manager.state.value.phase)
            allowSave.complete(Unit)

            assertEquals(SuggestionRefreshOutcome.SUCCESS, withTimeout(3_000) { outcome.await() })
            assertEquals(SuggestionSyncPhase.CONNECTED, manager.state.value.phase)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun refreshReportsLocalFailureWhenExactEncryptedSnapshotCannotBeSaved() = runBlocking {
        val initial = DayWeaveUiState(suggestions = emptyList())
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw IllegalStateException("synthetic encrypted save failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val plannerStore = PlannerStore(initial, repository, scope)
            withTimeout(3_000) {
                plannerStore.loadState.first { it == PlannerLoadState.READY }
            }
            val transport = FakeSuggestionsTransport().apply {
                listed = listOf(remoteSuggestion())
            }

            val outcome = manager(plannerStore, transport).refresh()

            assertEquals(SuggestionRefreshOutcome.LOCAL_STORAGE_FAILURE, outcome)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun remoteDecisionCannotFinishBeforeItsReconciledSnapshotIsDurable() = runBlocking {
        val initial = DayWeaveUiState(suggestions = listOf(cachedSuggestion(1)))
        val saveStarted = CompletableDeferred<DayWeaveUiState>()
        val allowSave = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveStarted.complete(state)
                allowSave.await()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val plannerStore = PlannerStore(initial, repository, scope)
            withTimeout(3_000) {
                plannerStore.loadState.first { it == PlannerLoadState.READY }
            }
            val transport = FakeSuggestionsTransport().apply {
                acceptResult = remoteSuggestion(revision = 2, status = "accepted")
            }
            val manager = manager(plannerStore, transport)

            val decision = async { manager.accept("proposal-id") }
            val exactSnapshot = withTimeout(3_000) { saveStarted.await() }

            assertTrue(!decision.isCompleted)
            assertEquals(SuggestionSyncPhase.SYNCING, manager.state.value.phase)
            assertEquals(
                SuggestionDisposition.APPROVED_FOR_INBOX,
                exactSnapshot.suggestions.single().disposition,
            )
            assertTrue(exactSnapshot.inbox.any { it.id == "proposal-proposal-id" })
            allowSave.complete(Unit)

            withTimeout(3_000) { decision.await() }
            assertEquals(SuggestionSyncPhase.CONNECTED, manager.state.value.phase)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun refreshAndAcceptReconcileRevisionIntoEncryptedStateWithoutChangingSchedule() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState.preview().copy(suggestions = emptyList()))
        val scheduleBefore = plannerStore.state.value.schedule
        val transport = FakeSuggestionsTransport().apply {
            listed = listOf(remoteSuggestion())
            acceptResult = remoteSuggestion(revision = 2, status = "accepted")
        }
        val manager = manager(plannerStore, transport)

        val refreshOutcome = manager.refresh()
        manager.accept("proposal-id")

        assertEquals(SuggestionRefreshOutcome.SUCCESS, refreshOutcome)
        assertEquals(1L, transport.acceptedRevision)
        assertEquals(scheduleBefore, plannerStore.state.value.schedule)
        val suggestion = plannerStore.state.value.suggestions.single { it.id == "proposal-id" }
        assertEquals(2L, suggestion.remoteRevision)
        assertEquals(SuggestionDisposition.APPROVED_FOR_INBOX, suggestion.disposition)
        val draft = plannerStore.state.value.inbox.first { it.id == "proposal-proposal-id" }
        assertEquals(InboxSource.EXTERNAL_PROPOSAL, draft.source)
        assertTrue(draft.requiresReview)
        assertTrue(draft.detail.contains("start_minute"))
        assertEquals(SuggestionSyncPhase.CONNECTED, manager.state.value.phase)
        assertEquals(now, manager.state.value.lastSuccessfulSyncEpochMillis)
    }

    @Test
    fun acceptedServerStateIsRecoveredAsDraftAfterAnInterruptedClientDecision() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState.preview().copy(suggestions = emptyList()))
        val scheduleBefore = plannerStore.state.value.schedule
        val transport = FakeSuggestionsTransport().apply {
            listed = listOf(remoteSuggestion(revision = 2, status = "accepted"))
        }

        val outcome = manager(plannerStore, transport).refresh()

        assertEquals(SuggestionRefreshOutcome.SUCCESS, outcome)
        assertEquals(scheduleBefore, plannerStore.state.value.schedule)
        assertTrue(plannerStore.state.value.inbox.any { it.id == "proposal-proposal-id" })
    }

    @Test
    fun editAndRejectUseLatestOptimisticRevision() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState(suggestions = listOf(cachedSuggestion(4))))
        val transport = FakeSuggestionsTransport().apply {
            editResult = remoteSuggestion(revision = 5, title = "Edited")
            rejectResult = remoteSuggestion(revision = 6, title = "Edited", status = "rejected")
        }
        val manager = manager(plannerStore, transport)

        manager.edit("proposal-id", " Edited ", " Better explanation ")
        manager.reject("proposal-id")

        assertEquals(listOf(4L, 5L), transport.mutationRevisions)
        val result = plannerStore.state.value.suggestions.single()
        assertEquals(6L, result.remoteRevision)
        assertEquals(SuggestionDisposition.REJECTED, result.disposition)
        assertTrue(plannerStore.state.value.inbox.isEmpty())
    }

    @Test
    fun ioFailureKeepsEncryptedCacheAndReportsOffline() = runBlocking {
        val cached = cachedSuggestion(3)
        val plannerStore = PlannerStore(DayWeaveUiState(suggestions = listOf(cached)))
        val transport = FakeSuggestionsTransport().apply {
            listFailure = IOException("synthetic offline failure")
        }
        val manager = manager(plannerStore, transport)

        val outcome = manager.refresh()

        assertEquals(SuggestionRefreshOutcome.TRANSIENT_NETWORK_FAILURE, outcome)
        assertEquals(listOf(cached), plannerStore.state.value.suggestions)
        assertEquals(SuggestionSyncPhase.OFFLINE, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("cached Inbox"))
    }

    @Test
    fun authenticationFailureDoesNotApplyAProposalLocally() = runBlocking {
        val cached = cachedSuggestion(3)
        val plannerStore = PlannerStore(DayWeaveUiState(suggestions = listOf(cached)))
        val transport = FakeSuggestionsTransport().apply {
            acceptFailure = SuggestionApiException.Authentication()
        }
        val manager = manager(plannerStore, transport)

        manager.accept("proposal-id")

        assertEquals(SuggestionDisposition.PENDING, plannerStore.state.value.suggestions.single().disposition)
        assertTrue(plannerStore.state.value.inbox.isEmpty())
        assertEquals(SuggestionSyncPhase.AUTH_REQUIRED, manager.state.value.phase)
    }

    @Test
    fun refreshClassifiesServerAndProtocolFailuresForBackgroundRetry() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeSuggestionsTransport()
        val manager = manager(plannerStore, transport)

        transport.listFailure = SuggestionApiException.Http(503)
        assertEquals(SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE, manager.refresh())

        transport.listFailure = SuggestionApiException.Http(408)
        assertEquals(SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE, manager.refresh())

        transport.listFailure = SuggestionApiException.Http(429)
        assertEquals(SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE, manager.refresh())

        transport.listFailure = SuggestionApiException.Http(400)
        assertEquals(SuggestionRefreshOutcome.PERMANENT_SERVER_FAILURE, manager.refresh())

        transport.listFailure = SuggestionApiException.InvalidResponse()
        assertEquals(SuggestionRefreshOutcome.PROTOCOL_FAILURE, manager.refresh())

        transport.listFailure = SuggestionApiException.Authentication()
        assertEquals(SuggestionRefreshOutcome.AUTH_REQUIRED, manager.refresh())
    }

    private fun manager(
        plannerStore: PlannerStore,
        transport: FakeSuggestionsTransport,
    ): SuggestionSyncManager = SuggestionSyncManager(
        plannerStore = plannerStore,
        credentialStore = FakeApiCredentialStore(),
        transport = transport,
        nowEpochMillis = { now },
    )

    private fun cachedSuggestion(revision: Long) = PlanningSuggestion(
        id = "proposal-id",
        title = "Protect recovery time",
        summary = "Keep a protected hour after deep work",
        source = "Codex",
        kind = SuggestionKind.SCHEDULE_CHANGE,
        expiresInDays = 7,
        remoteRevision = revision,
        remotePayloadJson = "{\"start_minute\":1020}",
    )

    private fun remoteSuggestion(
        revision: Long = 1,
        status: String = "pending",
        title: String = "Protect recovery time",
    ) = RemoteSuggestion(
        id = "proposal-id",
        revision = revision,
        submittedBy = "token:fingerprint",
        source = "codex",
        sourceReference = "conversation-42",
        kind = "schedule_plan",
        status = status,
        title = title,
        explanation = "Keep a protected hour after deep work",
        payload = buildJsonObject { put("start_minute", 1020) },
        createdAt = "2026-08-29T09:00:00Z",
        updatedAt = "2026-08-29T09:00:00Z",
        expiresAt = "2026-09-05T09:00:00Z",
    )
}

private class FakeApiCredentialStore : ApiCredentialStore {
    private var baseUrl = "https://api.example.test/"
    private var token: String? = "test-secret"
    private var lastSync: Long? = null

    override fun snapshot() = ApiConnectionSnapshot(baseUrl, token != null, lastSync)

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        token?.let { AuthenticatedApiConfiguration.create(baseUrl, it) }

    override fun update(baseUrl: String, bearerToken: String?) {
        val effectiveToken = bearerToken ?: token
        if (effectiveToken == null) {
            AuthenticatedApiConfiguration.create(baseUrl, "validation-placeholder")
            this.baseUrl = baseUrl
        } else {
            val validated = AuthenticatedApiConfiguration.create(baseUrl, effectiveToken)
            this.baseUrl = validated.baseUrl.toString()
            token = effectiveToken
        }
    }

    override fun clear() {
        baseUrl = ""
        token = null
        lastSync = null
    }

    override fun recordSuccessfulSync(epochMillis: Long) {
        lastSync = epochMillis
    }
}

private class FakeSuggestionsTransport : SuggestionsTransport {
    var listed: List<RemoteSuggestion> = emptyList()
    var listFailure: Exception? = null
    var listStarted: CompletableDeferred<Unit>? = null
    var listGate: CompletableDeferred<Unit>? = null
    var editResult: RemoteSuggestion? = null
    var acceptResult: RemoteSuggestion? = null
    var rejectResult: RemoteSuggestion? = null
    var acceptFailure: Exception? = null
    var acceptedRevision: Long? = null
    val mutationRevisions = mutableListOf<Long>()

    override suspend fun list(
        configuration: AuthenticatedApiConfiguration,
    ): List<RemoteSuggestion> {
        listStarted?.complete(Unit)
        listGate?.await()
        listFailure?.let { throw it }
        return listed
    }

    override suspend fun edit(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
        title: String,
        explanation: String,
    ): RemoteSuggestion {
        mutationRevisions += expectedRevision
        return requireNotNull(editResult)
    }

    override suspend fun accept(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion {
        acceptedRevision = expectedRevision
        mutationRevisions += expectedRevision
        acceptFailure?.let { throw it }
        return requireNotNull(acceptResult)
    }

    override suspend fun reject(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion {
        mutationRevisions += expectedRevision
        return requireNotNull(rejectResult)
    }
}
