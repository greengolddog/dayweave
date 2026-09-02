package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.assistant.AssistantTurnRequest
import com.greengolddog.dayweave.assistant.AssistantTurnResponse
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AssistantTransport
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeviceAuthenticationChangedException
import com.greengolddog.dayweave.network.DeviceAuthenticationRequiredException
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import java.io.IOException
import java.time.Instant
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AssistantManagerTest {
    @Test
    fun providerCannotStartBeforeExactEncryptedUserSnapshotIsDurable() = runBlocking {
        val initial = DayWeaveUiState()
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
            val planner = PlannerStore(initial, repository, scope)
            withTimeout(3_000) {
                planner.loadState.first { it == PlannerLoadState.READY }
            }
            val transport = FakeAssistantTransport { request ->
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "Durably anchored reply",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope)

            assertTrue(manager.send("Persist me first"))
            val staged = withTimeoutOrNull(3_000) { saveStarted.await() } ?: error(
                "Assistant did not stage the user message; state=${manager.state.value}, " +
                    "requests=${transport.requests.size}",
            )
            assertEquals("Persist me first", staged.messages.single().text)
            assertTrue(transport.requests.isEmpty())

            allowSave.complete(Unit)
            withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.READY && it.model != null }
            }
            assertEquals(1, transport.requests.size)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun admittedTurnStoresUserBeforeNetworkAndOnlyCompletedBoundReplyAfterward() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { request ->
                assertEquals(ChatRole.USER, planner.state.value.messages.single().role)
                assertEquals(request.message, planner.state.value.messages.single().text)
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "Protect the focus block, then take a short break.",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope)

            assertTrue(manager.send("How should I handle today?"))
            val completed = withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.READY && it.model != null }
            }

            assertEquals("test-model", completed.model)
            assertEquals(
                listOf(ChatRole.USER, ChatRole.ASSISTANT),
                planner.state.value.messages.map(ChatMessage::role),
            )
            assertEquals(1, transport.requests.size)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun networkFailureKeepsDurableUserTurnAndNeverRetriesAutomatically() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { throw IOException("synthetic offline") }
            val manager = manager(planner, transport, scope)

            assertTrue(manager.send("Review the week"))
            val failed = withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.OFFLINE }
            }

            assertTrue(failed.message.contains("not be resent"))
            assertEquals(listOf(ChatRole.USER), planner.state.value.messages.map(ChatMessage::role))
            assertEquals(1, transport.requests.size)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun privacyGenerationDropsLateNonCancellableResponseAcrossUnlock() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val allowed = AtomicBoolean(true)
            val started = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { request ->
                started.complete(Unit)
                withContext(NonCancellable) { release.await() }
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "Late private reply",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope, allowed::get)

            assertTrue(manager.send("Private turn"))
            withTimeout(3_000) { started.await() }
            allowed.set(false)
            manager.cancelForPrivacyBoundary()
            allowed.set(true)
            manager.restoreForegroundState()
            release.complete(Unit)

            withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.READY }
            }
            assertEquals(listOf(ChatRole.USER), planner.state.value.messages.map(ChatMessage::role))
            assertFalse(planner.state.value.messages.any { it.text == "Late private reply" })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun turnIsSingleFlightAndUnprovenStoredTranscriptIsNeverSent() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val started = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            val oldMessages = (0 until 30).map { index ->
                ChatMessage(
                    id = "history-$index",
                    role = if (index % 2 == 0) ChatRole.USER else ChatRole.ASSISTANT,
                    text = "history $index",
                )
            }
            val planner = PlannerStore(DayWeaveUiState(messages = oldMessages))
            val transport = FakeAssistantTransport { request ->
                started.complete(Unit)
                release.await()
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "Done",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope)

            assertTrue(manager.send("First"))
            withTimeout(3_000) { started.await() }
            assertFalse(manager.send("Second"))
            assertFalse(manager.send("spoofed\u202Etext"))
            assertEquals(AssistantPhase.SENDING, manager.state.value.phase)
            assertTrue(transport.requests.single().history.isEmpty())
            release.complete(Unit)
            withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.READY && it.model != null }
            }
            assertEquals(1, transport.requests.size)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun onlyCompletedPairsFromTheCurrentNativeBindingBecomeBoundedHistory() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(
                DayWeaveUiState(
                    messages = listOf(
                        ChatMessage("legacy-user", ChatRole.USER, "old account question"),
                        ChatMessage("legacy-reply", ChatRole.ASSISTANT, "old account answer"),
                    ),
                ),
            )
            val replyIndex = AtomicInteger()
            val transport = FakeAssistantTransport { request ->
                val index = replyIndex.getAndIncrement()
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "reply-$index",
                    model = "test-model",
                    generatedAt = Instant.parse("2026-09-03T10:00:01Z")
                        .plusSeconds(index.toLong())
                        .toString(),
                )
            }
            val manager = manager(planner, transport, scope)

            repeat(12) { index ->
                assertTrue(awaitAdmission(manager, "turn-$index"))
                withTimeout(3_000) {
                    manager.state.first {
                        it.completedAt == Instant.parse("2026-09-03T10:00:01Z")
                            .plusSeconds(index.toLong())
                            .toString()
                    }
                }
            }

            assertTrue(transport.requests.first().history.isEmpty())
            val bounded = transport.requests.last().history
            assertEquals(20, bounded.size)
            assertEquals("turn-1", bounded.first().content)
            assertEquals("reply-10", bounded.last().content)
            assertFalse(bounded.any { it.content.contains("old account") })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun failedPromptsAndPriorBindingsNeverEnterLaterHistory() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(DayWeaveUiState())
            val credentials = FakeAssistantCredentials()
            val transport = FakeAssistantTransport { request ->
                if (request.message == "failed prompt") throw IOException("synthetic offline")
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "reply to ${request.message}",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope, credentials = credentials)

            assertTrue(manager.send("first binding turn"))
            withTimeout(3_000) { planner.state.first { it.messages.size == 2 } }
            assertTrue(awaitAdmission(manager, "failed prompt"))
            withTimeout(3_000) { manager.state.first { it.phase == AssistantPhase.OFFLINE } }
            assertTrue(awaitAdmission(manager, "after failure"))
            withTimeout(3_000) { planner.state.first { it.messages.size == 5 } }

            assertEquals(
                listOf("first binding turn", "reply to first binding turn"),
                transport.requests[2].history.map { it.content },
            )
            assertFalse(transport.requests[2].history.any { it.content == "failed prompt" })

            credentials.switchBinding("assistant-binding-b")
            assertTrue(awaitAdmission(manager, "new binding turn"))
            withTimeout(3_000) { planner.state.first { it.messages.size == 7 } }
            assertTrue(transport.requests[3].history.isEmpty())
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun privacyBoundaryBetweenResponseValidationAndCommitRejectsTheReply() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val allowed = AtomicBoolean(true)
            val commitWindowEntered = CompletableDeferred<Unit>()
            val releaseCommitWindow = CompletableDeferred<Unit>()
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { request ->
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "must not cross the boundary",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(
                planner = planner,
                transport = transport,
                scope = scope,
                allowed = allowed::get,
                beforeReplyCommit = {
                    commitWindowEntered.complete(Unit)
                    withContext(NonCancellable) { releaseCommitWindow.await() }
                },
            )

            assertTrue(manager.send("race the lock"))
            withTimeout(3_000) { commitWindowEntered.await() }
            allowed.set(false)
            manager.cancelForPrivacyBoundary()
            releaseCommitWindow.complete(Unit)
            allowed.set(true)
            manager.restoreForegroundState()

            withTimeout(3_000) { manager.state.first { it.phase == AssistantPhase.READY } }
            assertEquals(listOf(ChatRole.USER), planner.state.value.messages.map(ChatMessage::role))
            assertFalse(planner.state.value.messages.any {
                it.text == "must not cross the boundary"
            })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun invalidInputAndMissingConnectionNeverCreateMessagesOrProviderCalls() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { error("must not be called") }
            val disconnected = FakeAssistantCredentials(configured = false)
            val manager = AssistantManager(planner, disconnected, transport, scope)

            assertFalse(manager.send(" "))
            assertFalse(manager.send("x".repeat(8 * 1024 + 1)))
            assertFalse(manager.send("spoofed\u202Etext"))
            assertFalse(manager.send("unpaired \uD800 surrogate"))
            assertFalse(manager.send("Valid but disconnected"))
            assertTrue(planner.state.value.messages.isEmpty())
            assertTrue(transport.requests.isEmpty())
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun validSupplementaryUnicodeRoundTripsThroughTheCompletedTurn() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { request ->
                AssistantTurnResponse(
                    requestId = request.requestId,
                    reply = "Protect the creative block ✨",
                    model = "test-model",
                    generatedAt = "2026-09-03T10:00:01Z",
                )
            }
            val manager = manager(planner, transport, scope)

            assertTrue(manager.send("Plan around my run 🏃🏽‍♂️"))
            withTimeout(3_000) {
                manager.state.first { it.phase == AssistantPhase.READY && it.model != null }
            }

            assertEquals("Plan around my run 🏃🏽‍♂️", transport.requests.single().message)
            assertEquals(
                listOf("Plan around my run 🏃🏽‍♂️", "Protect the creative block ✨"),
                planner.state.value.messages.map(ChatMessage::text),
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun deviceAuthenticationFailuresAreNotMisreportedAsOffline() = runBlocking {
        for ((failure, expectedPhase) in listOf(
            DeviceAuthenticationRequiredException() to AssistantPhase.AUTH_REQUIRED,
            DeviceAuthenticationChangedException() to AssistantPhase.READY,
        )) {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val planner = PlannerStore(DayWeaveUiState())
                val transport = FakeAssistantTransport { throw failure }
                val manager = manager(planner, transport, scope)

                assertTrue(manager.send("authentication test"))
                val result = withTimeout(3_000) {
                    manager.state.first {
                        it.phase == expectedPhase &&
                            (expectedPhase != AssistantPhase.READY ||
                                it.message.contains("changed", ignoreCase = true))
                    }
                }

                assertEquals(expectedPhase, result.phase)
                assertFalse(result.phase == AssistantPhase.OFFLINE)
            } finally {
                scope.cancel()
            }
        }
    }

    @Test
    fun closedForegroundGateRejectsNewTurnsAndCannotRestoreReadyState() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val allowed = AtomicBoolean(false)
            val planner = PlannerStore(DayWeaveUiState())
            val transport = FakeAssistantTransport { error("must not be called") }
            val manager = manager(planner, transport, scope, allowed::get)

            assertFalse(manager.send("background turn"))
            assertEquals(AssistantPhase.NOT_CONFIGURED, manager.state.value.phase)

            allowed.set(true)
            manager.restoreForegroundState()
            assertEquals(AssistantPhase.READY, manager.state.value.phase)

            allowed.set(false)
            manager.cancelForPrivacyBoundary()
            manager.restoreForegroundState()
            assertEquals(AssistantPhase.NOT_CONFIGURED, manager.state.value.phase)
            assertFalse(manager.send("still background"))
            assertTrue(transport.requests.isEmpty())
        } finally {
            scope.cancel()
        }
    }

    private fun manager(
        planner: PlannerStore,
        transport: FakeAssistantTransport,
        scope: CoroutineScope,
        allowed: () -> Boolean = { true },
        credentials: FakeAssistantCredentials = FakeAssistantCredentials(),
        beforeReplyCommit: suspend () -> Unit = {},
    ) = AssistantManager(
        plannerStore = planner,
        credentialStore = credentials,
        transport = transport,
        scope = scope,
        operationAllowed = allowed,
        now = { Instant.parse("2026-09-03T10:00:00Z") },
        newUuid = sequenceUuids(),
        beforeReplyCommit = beforeReplyCommit,
    )

    private suspend fun awaitAdmission(manager: AssistantManager, message: String): Boolean =
        withTimeout(3_000) {
            while (!manager.send(message)) yield()
            true
        }

    private fun sequenceUuids(): () -> UUID {
        val counter = AtomicInteger(1)
        return {
            UUID(0x1000L, counter.getAndIncrement().toLong())
        }
    }

    private class FakeAssistantTransport(
        private val response: suspend (AssistantTurnRequest) -> AssistantTurnResponse,
    ) : AssistantTransport {
        val requests = CopyOnWriteArrayList<AssistantTurnRequest>()

        override suspend fun turn(
            configuration: AuthenticatedApiConfiguration,
            request: AssistantTurnRequest,
        ): AssistantTurnResponse {
            requests += request
            return response(request)
        }
    }

    private class FakeAssistantCredentials(
        private val configured: Boolean = true,
    ) : ApiCredentialStore {
        @Volatile
        private var bindingId = "assistant-binding-a"

        override fun snapshot() = ApiConnectionSnapshot(
            baseUrl = if (configured) "https://dayweave.invalid/" else null,
            hasBearerToken = configured,
            lastSuccessfulSyncEpochMillis = null,
            configurationId = bindingId.takeIf { configured },
        )

        override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
            AuthenticatedApiConfiguration.createBound(
                baseUrl = "https://dayweave.invalid/",
                bearerToken = "synthetic-assistant-token",
                configurationId = bindingId,
            ).takeIf { configured }

        fun switchBinding(nextBindingId: String) {
            bindingId = nextBindingId
        }

        override fun update(baseUrl: String, bearerToken: String?) = Unit

        override fun clear() = Unit

        override fun recordSuccessfulSync(epochMillis: Long) = Unit
    }
}
