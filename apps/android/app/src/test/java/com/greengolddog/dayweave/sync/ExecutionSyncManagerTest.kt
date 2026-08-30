package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ExecutionApiException
import com.greengolddog.dayweave.network.ExecutionTransport
import com.greengolddog.dayweave.network.RemoteExecutionMutation
import com.greengolddog.dayweave.network.RemoteExecutionHistoryPage
import com.greengolddog.dayweave.network.RemoteExecutionSession
import com.greengolddog.dayweave.network.RemoteExecutionSnapshot
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ExecutionSyncManagerTest {
    @Test
    fun delayedOldBindingCommandCannotRestorePendingOrSessionAfterFence() = runBlocking {
        val store = plannerStore(configurationId = "configuration-a")
        val credentials = GenerationBoundCredentialStore()
        val responseStarted = CompletableDeferred<Unit>()
        val releaseResponse = CompletableDeferred<Unit>()
        val changed = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            commandHandler = { _, _ ->
                responseStarted.complete(Unit)
                releaseResponse.await()
                RemoteExecutionMutation(1, changed, changed, replayed = false)
            }
        }
        val manager = manager(store, transport, credentials)

        val oldCommand = async { manager.start(BLOCK_ID) }
        withTimeout(3_000) { responseStarted.await() }
        assertNotNull(store.state.value.pendingExecutionCommand)
        val fence = async {
            credentials.invalidateBeforeQuarantine {
                val cleared = store.abandonCanonicalConnection()?.awaitDurable() == true
                if (cleared) manager.quarantineBindingState()
                cleared
            }
        }
        yield()
        releaseResponse.complete(Unit)

        assertEquals(ExecutionSyncOutcome.SUCCESS, withTimeout(3_000) { oldCommand.await() })
        assertTrue(withTimeout(3_000) { fence.await() })
        assertNull(store.state.value.pendingExecutionCommand)
        assertNull(store.state.value.canonicalExecutionSession)
        assertNull(store.state.value.activeSession)
        assertTrue(store.state.value.schedule.isEmpty())
        assertEquals(CanonicalSyncPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun readerCreatedDuringWriterCannotSendOrPersistOldExecutionCommand() = runBlocking {
        val store = plannerStore(configurationId = "configuration-a")
        val credentials = GenerationBoundCredentialStore()
        val writerEntered = CompletableDeferred<Unit>()
        val releaseWriter = CompletableDeferred<Unit>()
        val configurationObserved = CompletableDeferred<Unit>()
        credentials.configurationObserved = { configurationObserved.complete(Unit) }
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
        }
        val manager = manager(store, transport, credentials)

        val fence = async {
            credentials.invalidateBeforeQuarantine {
                writerEntered.complete(Unit)
                releaseWriter.await()
                val cleared = store.abandonCanonicalConnection()?.awaitDurable() == true
                if (cleared) manager.quarantineBindingState()
                cleared
            }
        }
        withTimeout(3_000) { writerEntered.await() }
        val command = async { manager.start(BLOCK_ID) }
        withTimeout(3_000) { configurationObserved.await() }

        assertTrue(credentials.enabled)
        assertEquals(0, transport.snapshotCalls)
        assertNull(store.state.value.pendingExecutionCommand)
        releaseWriter.complete(Unit)

        assertTrue(withTimeout(3_000) { fence.await() })
        assertEquals(ExecutionSyncOutcome.NOT_CONFIGURED, withTimeout(3_000) { command.await() })
        assertEquals(0, transport.snapshotCalls)
        assertNull(store.state.value.pendingExecutionCommand)
        assertNull(store.state.value.canonicalExecutionSession)
        assertEquals(CanonicalSyncPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun ambiguousStartIsRetriedWithExactBodyAndKeyAfterRelaunch() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport()
        val changed = activeSession(sessionId = SESSION_ID)
        transport.snapshotResult = RemoteExecutionSnapshot(0, null)
        var attempts = 0
        transport.commandHandler = { _, _ ->
            attempts += 1
            transport.snapshotResult = RemoteExecutionSnapshot(1, changed)
            if (attempts == 1) throw IOException("response lost")
            RemoteExecutionMutation(1, changed, changed, replayed = true)
        }
        val first = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, first.start(BLOCK_ID))
        val pending = requireNotNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.single().status)
        assertNull(store.state.value.activeSession)

        val relaunched = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, relaunched.refresh())

        assertEquals(2, transport.commandBodies.size)
        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.commandBodies)
        assertEquals(listOf(pending.idempotencyKey, pending.idempotencyKey), transport.commandKeys)
        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
        assertEquals(SESSION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
    }

    @Test
    fun processRestartRestoresDurablePendingCommandBeforeExactReplay() = runBlocking {
        val saved = AtomicReference<DayWeaveUiState?>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState? = saved.get()
            override suspend fun save(state: DayWeaveUiState) {
                saved.set(state)
            }
        }
        val firstScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val transport = FakeExecutionTransport()
        val changed = activeSession(sessionId = SESSION_ID)
        var attempts = 0
        transport.commandHandler = { _, _ ->
            attempts += 1
            transport.snapshotResult = RemoteExecutionSnapshot(1, changed)
            if (attempts == 1) throw IOException("response lost")
            RemoteExecutionMutation(1, changed, changed, replayed = true)
        }
        val initial = plannerStore().state.value
        val firstStore = PlannerStore(initial, repository, firstScope)
        withTimeout(3_000) { firstStore.loadState.first { it == PlannerLoadState.READY } }
        try {
            assertEquals(
                ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
                manager(firstStore, transport).start(BLOCK_ID),
            )
            val durable = requireNotNull(saved.get())
            assertNotNull(durable.pendingExecutionCommand)
            val exactBody = durable.pendingExecutionCommand?.requestJson
            val exactKey = durable.pendingExecutionCommand?.idempotencyKey

            firstScope.cancel()
            val secondScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val restoredStore = PlannerStore(initial, repository, secondScope)
                withTimeout(3_000) {
                    restoredStore.loadState.first { it == PlannerLoadState.READY }
                }
                assertNotNull(restoredStore.state.value.pendingExecutionCommand)

                assertEquals(ExecutionSyncOutcome.SUCCESS, manager(restoredStore, transport).refresh())
                assertEquals(exactBody, transport.commandBodies.last())
                assertEquals(exactKey, transport.commandKeys.last())
                assertNull(restoredStore.state.value.pendingExecutionCommand)
                assertEquals(SESSION_ID, restoredStore.state.value.activeSession?.canonicalExecutionSessionId)
            } finally {
                secondScope.cancel()
            }
        } finally {
            firstScope.cancel()
        }
    }

    @Test
    fun offlineCanonicalStartDoesNotCreateDivergentLocalExecution() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotError = IOException("offline")
        }

        val outcome = manager(store, transport).start(BLOCK_ID)

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, outcome)
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.single().status)
        assertNull(store.state.value.activeSession)
        assertNull(store.state.value.pendingExecutionCommand)
        assertNotNull(store.state.value.executionDeviceId)
        assertTrue(transport.commandBodies.isEmpty())
    }

    @Test
    fun canonicalStartRejectsMissingServerSessionIndexWithoutSendingCommand() = runBlocking {
        val published = plannerStore().state.value
        val store = PlannerStore(
            published.copy(
                schedule = listOf(published.schedule.single().copy(sessionIndex = null)),
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val transport = FakeExecutionTransport()

        val outcome = manager(store, transport).start(BLOCK_ID)

        assertEquals(ExecutionSyncOutcome.INVALID_LOCAL_STATE, outcome)
        assertNull(store.state.value.pendingExecutionCommand)
        assertTrue(transport.commandBodies.isEmpty())
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.single().status)
    }

    @Test
    fun canonicalStartCannotReplaceAnExistingDeviceLocalFocusSession() = runBlocking {
        val local = ScheduleItem(
            id = "local-focus",
            title = "Local focus",
            kind = ItemKind.TASK,
            startMinute = 8 * 60,
            durationMinutes = 30,
            status = ItemStatus.ACTIVE,
        )
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                schedule = listOf(scheduleItem(BLOCK_ID, 0), local),
                activeSession = ActiveSession(
                    itemId = local.id,
                    elapsedMinutes = 3,
                    isPaused = false,
                ),
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
        }

        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            manager(store, transport).start(BLOCK_ID),
        )
        assertEquals("local-focus", store.state.value.activeSession?.itemId)
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.first().status)
        assertTrue(transport.commandBodies.isEmpty())
    }

    @Test
    fun crossDeviceConflictClearsFenceAndRestoresRemoteLease() = runBlocking {
        val store = plannerStore()
        val otherSession = activeSession(
            sessionId = OTHER_SESSION_ID,
            deviceId = OTHER_DEVICE_ID,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            commandHandler = { _, _ ->
                snapshotResult = RemoteExecutionSnapshot(1, otherSession)
                throw ExecutionApiException.Conflict()
            }
        }

        val outcome = manager(store, transport).start(BLOCK_ID)

        assertEquals(ExecutionSyncOutcome.CONFLICT, outcome)
        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals(OTHER_SESSION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
    }

    @Test
    fun authenticationFailureRetainsExactPendingCommand() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            commandHandler = { _, _ -> throw ExecutionApiException.Authentication() }
        }

        val outcome = manager(store, transport).start(BLOCK_ID)

        assertEquals(ExecutionSyncOutcome.AUTH_REQUIRED, outcome)
        assertNotNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.single().status)
        assertNull(store.state.value.activeSession)
    }

    @Test
    fun expiredTimedBreakNeverAutoResumesAndCanBeExtended() = runBlocking {
        val store = plannerStore()
        val expired = pausedSession(
            pauseUntil = "2026-09-01T06:59:00Z",
            revision = 2,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, expired)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.activeSession?.isPaused == true)
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)

        val extended = pausedSession(
            pauseUntil = "2026-09-01T07:10:00Z",
            revision = 3,
        ).copy(updatedAt = NOW.toString())
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            assertEquals("pause", command.getValue("type").jsonPrimitive.content)
            assertEquals("600", command.getValue("duration_seconds").jsonPrimitive.content)
            transport.snapshotResult = RemoteExecutionSnapshot(3, extended)
            RemoteExecutionMutation(3, extended, extended, replayed = false)
        }

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.pause(BLOCK_ID, 600))
        val active = requireNotNull(store.state.value.activeSession)
        assertTrue(active.isPaused)
        assertFalse(active.timedBreakEnded)
        assertEquals(Instant.parse("2026-09-01T07:10:00Z").toEpochMilli(), active.pauseUntilEpochMillis)
    }

    @Test
    fun completingOneSplitSessionDoesNotCompleteItsSiblingOrParent() = runBlocking {
        val store = plannerStore(split = true)
        val running = activeSession(sessionId = SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        val completed = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 300,
            actualSeconds = 300,
            runningSince = null,
            endedAt = "2026-09-01T07:05:00Z",
            updatedAt = "2026-09-01T07:05:00Z",
        )
        transport.commandHandler = { _, _ ->
            transport.snapshotResult = RemoteExecutionSnapshot(2, null)
            RemoteExecutionMutation(2, null, completed, replayed = false)
        }

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.complete(BLOCK_ID))

        val schedule = store.state.value.schedule.associateBy(ScheduleItem::id)
        assertEquals(ItemStatus.COMPLETED, schedule.getValue(BLOCK_ID).status)
        assertEquals(ItemStatus.SCHEDULED, schedule.getValue(SECOND_BLOCK_ID).status)
        assertNull(store.state.value.activeSession)
        assertEquals(7L, schedule.getValue(SECOND_BLOCK_ID).canonicalRevision)
    }

    @Test
    fun openEndedPauseResumeAndSkipRemainServerCommands() = runBlocking {
        val store = plannerStore()
        var serverSession = activeSession(sessionId = SESSION_ID)
        var globalRevision = 1L
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(globalRevision, serverSession)
        }
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            globalRevision += 1
            serverSession = when (command.getValue("type").jsonPrimitive.content) {
                "pause" -> serverSession.copy(
                    status = "paused",
                    revision = 2,
                    accumulatedSeconds = 0,
                    runningSince = null,
                    pausedAt = NOW.toString(),
                    pauseUntil = null,
                    updatedAt = NOW.toString(),
                )
                "resume" -> serverSession.copy(
                    status = "active",
                    revision = 3,
                    runningSince = NOW.toString(),
                    pausedAt = null,
                    pauseUntil = null,
                    updatedAt = NOW.toString(),
                )
                "skip" -> serverSession.copy(
                    status = "skipped",
                    revision = 4,
                    accumulatedSeconds = 0,
                    actualSeconds = 0,
                    runningSince = null,
                    pausedAt = null,
                    endedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                )
                else -> error("Unexpected command")
            }
            val active = serverSession.takeIf { it.status in setOf("active", "paused") }
            transport.snapshotResult = RemoteExecutionSnapshot(globalRevision, active)
            RemoteExecutionMutation(globalRevision, active, serverSession, replayed = false)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.pause(BLOCK_ID))
        assertTrue(store.state.value.activeSession?.isPaused == true)
        assertNull(store.state.value.activeSession?.pauseUntilEpochMillis)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.resume(BLOCK_ID))
        assertFalse(requireNotNull(store.state.value.activeSession).isPaused)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.skip(BLOCK_ID))
        assertEquals(ItemStatus.SKIPPED, store.state.value.schedule.single().status)
        assertNull(store.state.value.activeSession)
    }

    @Test
    fun protocolAheadPrivateElapsedAnchorAcceptsPauseAndCompleteResponses() = runBlocking {
        val publicStartedAt = Instant.parse("2026-09-01T06:59:59.900Z")
        val publicChangedAt = Instant.parse("2026-09-01T07:00:04.100Z")
        assertEquals(4L, Duration.between(publicStartedAt, publicChangedAt).seconds)

        listOf("pause", "complete").forEach { requestedTransition ->
            val store = plannerStore()
            lateinit var serverSession: RemoteExecutionSession
            var globalRevision = 0L
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(0, null)
                commandHandler = { _, body ->
                    val command = Json.parseToJsonElement(body).jsonObject
                        .getValue("command").jsonObject
                    val type = command.getValue("type").jsonPrimitive.content
                    globalRevision += 1
                    serverSession = when (type) {
                        "start" -> activeSession(
                            sessionId = command.getValue("session_id").jsonPrimitive.content,
                            deviceId = command.getValue("device_id").jsonPrimitive.content,
                        ).copy(
                            startedAt = publicStartedAt.toString(),
                            runningSince = publicStartedAt.toString(),
                            createdAt = publicStartedAt.toString(),
                            updatedAt = publicStartedAt.toString(),
                        )
                        "pause" -> {
                            assertEquals("pause", requestedTransition)
                            serverSession.copy(
                                status = "paused",
                                revision = 2,
                                accumulatedSeconds = 5,
                                runningSince = null,
                                pausedAt = publicChangedAt.toString(),
                                updatedAt = publicChangedAt.toString(),
                            )
                        }
                        "complete" -> {
                            assertEquals("complete", requestedTransition)
                            serverSession.copy(
                                status = "completed",
                                revision = 2,
                                accumulatedSeconds = 5,
                                actualSeconds = 5,
                                runningSince = null,
                                endedAt = publicChangedAt.toString(),
                                updatedAt = publicChangedAt.toString(),
                            )
                        }
                        else -> error("Unexpected command: $type")
                    }
                    val active = serverSession.takeIf { it.status in setOf("active", "paused") }
                    snapshotResult = RemoteExecutionSnapshot(globalRevision, active)
                    RemoteExecutionMutation(
                        revision = globalRevision,
                        activeSession = active,
                        changedSession = serverSession,
                        replayed = false,
                    )
                }
            }
            val manager = manager(store, transport)

            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.start(BLOCK_ID))
            assertNull(store.state.value.pendingExecutionCommand)
            val transitionOutcome = if (requestedTransition == "pause") {
                manager.pause(BLOCK_ID)
            } else {
                manager.complete(BLOCK_ID)
            }

            assertEquals(ExecutionSyncOutcome.SUCCESS, transitionOutcome)
            assertNull(store.state.value.pendingExecutionCommand)
            if (requestedTransition == "pause") {
                assertEquals(5L, store.state.value.canonicalExecutionSession?.accumulatedSeconds)
                assertTrue(requireNotNull(store.state.value.activeSession).isPaused)
            } else {
                val closed = store.state.value.terminalExecutionOutcomes.getValue(SESSION_ID).session
                assertEquals(5L, closed.accumulatedSeconds)
                assertEquals(5L, closed.actualSeconds)
                assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
            }
        }
    }

    @Test
    fun absolutePauseEndIsSentWithoutACompetingDuration() = runBlocking {
        val store = plannerStore()
        val running = activeSession(sessionId = SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
        }
        val until = NOW.plusSeconds(30 * 60L)
        val paused = running.copy(
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            runningSince = null,
            pausedAt = NOW.toString(),
            pauseUntil = until.toString(),
            updatedAt = NOW.toString(),
        )
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            assertEquals(until.toString(), command.getValue("pause_until").jsonPrimitive.content)
            assertFalse("duration_seconds" in command)
            transport.snapshotResult = RemoteExecutionSnapshot(2, paused)
            RemoteExecutionMutation(2, paused, paused, replayed = false)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.SUCCESS,
            manager.pause(BLOCK_ID, pauseUntil = until),
        )
        assertEquals(until.toEpochMilli(), store.state.value.activeSession?.pauseUntilEpochMillis)
    }

    @Test
    fun canonicalDeferProducerRemainsDisabledInCompatibilityFoundation() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(ExecutionSyncOutcome.INVALID_LOCAL_STATE, manager.doLater(BLOCK_ID))
        assertEquals(SESSION_ID, store.state.value.canonicalExecutionSession?.id)
        assertTrue(transport.commandBodies.isEmpty())
    }

    @Test
    fun stableDeviceIdIsReusedAcrossDistinctSessions() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
        }
        val manager = manager(store, transport)
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            val session = activeSession(
                sessionId = command.getValue("session_id").jsonPrimitive.content,
                deviceId = command.getValue("device_id").jsonPrimitive.content,
            )
            transport.snapshotResult = RemoteExecutionSnapshot(1, session)
            RemoteExecutionMutation(1, session, session, replayed = false)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.start(BLOCK_ID))
        val firstDevice = requireNotNull(store.state.value.executionDeviceId)
        val firstSession = requireNotNull(store.state.value.canonicalExecutionSession)
        assertEquals(firstDevice, firstSession.sourceDeviceId)
        assertNotEquals(firstDevice, firstSession.id)
    }

    @Test
    fun commandResponsesCannotSubstituteRevisionDurationOrCorrectedActual() = runBlocking {
        run {
            val store = plannerStore()
            val impossible = activeSession(SESSION_ID).copy(revision = 2)
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(0, null)
                commandHandler = { _, _ ->
                    RemoteExecutionMutation(1, impossible, impossible, replayed = false)
                }
            }
            assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager(store, transport).start(BLOCK_ID))
            assertNotNull(store.state.value.pendingExecutionCommand)
            assertNull(store.state.value.canonicalExecutionSession)
        }

        run {
            val store = plannerStore()
            val running = activeSession(SESSION_ID)
            val wrongDuration = running.copy(
                status = "paused",
                revision = 2,
                runningSince = null,
                pausedAt = NOW.toString(),
                pauseUntil = NOW.plusSeconds(300).toString(),
                updatedAt = NOW.toString(),
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(1, running)
                commandHandler = { _, _ ->
                    RemoteExecutionMutation(2, wrongDuration, wrongDuration, replayed = false)
                }
            }
            val manager = manager(store, transport)
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
            assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager.pause(BLOCK_ID, 900))
            assertNotNull(store.state.value.pendingExecutionCommand)
        }

        run {
            val store = plannerStore()
            val running = activeSession(SESSION_ID)
            val wrongActual = running.copy(
                status = "completed",
                revision = 2,
                actualSeconds = 599,
                runningSince = null,
                endedAt = NOW.toString(),
                updatedAt = NOW.toString(),
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(1, running)
                commandHandler = { _, _ ->
                    RemoteExecutionMutation(2, null, wrongActual, replayed = false)
                }
            }
            val manager = manager(store, transport)
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
            assertEquals(
                ExecutionSyncOutcome.PROTOCOL_FAILURE,
                manager.complete(BLOCK_ID, actualSeconds = 600),
            )
            assertNotNull(store.state.value.pendingExecutionCommand)
        }
    }

    @Test
    fun nullableAuthoritativeBlockIdentitySurvivesPauseResumeAndSkip() = runBlocking {
        val store = plannerStore()
        var serverSession = activeSession(SESSION_ID).copy(plannedBlockId = null)
        var globalRevision = 1L
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(globalRevision, serverSession)
        }
        transport.commandHandler = { _, body ->
            val pending = requireNotNull(store.state.value.pendingExecutionCommand)
            assertNull(pending.plannedBlockId)
            assertEquals(DEVICE_ID, pending.sourceDeviceId)
            val type = Json.parseToJsonElement(body).jsonObject
                .getValue("command").jsonObject.getValue("type").jsonPrimitive.content
            globalRevision += 1
            serverSession = when (type) {
                "pause" -> serverSession.copy(
                    status = "paused",
                    revision = 2,
                    runningSince = null,
                    pausedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                )
                "resume" -> serverSession.copy(
                    status = "active",
                    revision = 3,
                    runningSince = NOW.toString(),
                    pausedAt = null,
                    updatedAt = NOW.toString(),
                )
                "skip" -> serverSession.copy(
                    status = "skipped",
                    revision = 4,
                    actualSeconds = 0,
                    accumulatedSeconds = 0,
                    runningSince = null,
                    pausedAt = null,
                    endedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                )
                else -> error("Unexpected command: $type")
            }
            val active = serverSession.takeIf { it.status in setOf("active", "paused") }
            transport.snapshotResult = RemoteExecutionSnapshot(globalRevision, active)
            RemoteExecutionMutation(globalRevision, active, serverSession, replayed = false)
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.pause(BLOCK_ID))
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.resume(BLOCK_ID))
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.skip(BLOCK_ID))
        assertEquals(ItemStatus.SKIPPED, store.state.value.schedule.single().status)
    }

    @Test
    fun historicalPlannedBlockIdentityIsUsedForCompleteInsteadOfCurrentLocalBlock() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID).copy(plannedBlockId = OLD_BLOCK_ID)
        val completed = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 0,
            actualSeconds = 0,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            commandHandler = { _, _ ->
                assertEquals(
                    OLD_BLOCK_ID,
                    store.state.value.pendingExecutionCommand?.plannedBlockId,
                )
                snapshotResult = RemoteExecutionSnapshot(2, null)
                RemoteExecutionMutation(2, null, completed, replayed = false)
            }
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.complete(BLOCK_ID))
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
    }

    @Test
    fun foregroundRefreshConvergesRemoteCompleteAndSkipFromStableBoundedHistory() = runBlocking {
        listOf(
            "completed" to ItemStatus.COMPLETED,
            "skipped" to ItemStatus.SKIPPED,
        ).forEach { (terminalStatus, expectedStatus) ->
            val store = plannerStore()
            val running = activeSession(SESSION_ID)
            val terminal = running.copy(
                status = terminalStatus,
                revision = 2,
                accumulatedSeconds = 75,
                actualSeconds = 75,
                runningSince = null,
                endedAt = NOW.toString(),
                updatedAt = NOW.toString(),
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(1, running)
            }
            val manager = manager(store, transport)
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

            transport.snapshotResult = RemoteExecutionSnapshot(2, null)
            transport.historyResult = listOf(terminal)
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

            assertEquals(expectedStatus, store.state.value.schedule.single().status)
            assertNull(store.state.value.activeSession)
            assertEquals(2, transport.historyCalls)
            assertEquals(
                ExecutionSyncOutcome.INVALID_LOCAL_STATE,
                manager.start(BLOCK_ID),
            )
            assertTrue(transport.commandBodies.isEmpty())
        }
    }

    @Test
    fun steadyForegroundPollDoesNotRewriteAnIdenticalTerminalLedgerRow() = runBlocking {
        val initial = plannerStore().state.value
        val saved = AtomicReference<DayWeaveUiState?>(initial)
        val saveCount = AtomicInteger()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState? = saved.get()
            override suspend fun save(state: DayWeaveUiState) {
                saved.set(state)
                saveCount.incrementAndGet()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val newer = activeSession(SESSION_ID).copy(
                status = "completed",
                revision = 2,
                accumulatedSeconds = 75,
                actualSeconds = 75,
                runningSince = null,
                endedAt = NOW.toString(),
                updatedAt = NOW.toString(),
            )
            val older = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
                status = "skipped",
                revision = 2,
                accumulatedSeconds = 30,
                actualSeconds = 30,
                startedAt = NOW.minusSeconds(120).toString(),
                runningSince = null,
                endedAt = NOW.minusSeconds(1).toString(),
                createdAt = NOW.minusSeconds(120).toString(),
                updatedAt = NOW.minusSeconds(1).toString(),
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(4, null)
                historyResult = listOf(newer, older)
            }
            val manager = manager(store, transport)

            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
            assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
            assertEquals(2, store.state.value.terminalExecutionOutcomes.size)
            val afterFirstPoll = saveCount.get()
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

            // Fence, snapshot, and history-window generations remain exact; the already durable
            // terminal row itself must not create a fourth SQLCipher write on every 30s poll.
            assertEquals(3, saveCount.get() - afterFirstPoll)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun newerActiveSessionShadowsOlderTerminalPresentationWithoutLedgerRewrite() = runBlocking {
        val initial = plannerStore().state.value
        val saved = AtomicReference<DayWeaveUiState?>(initial)
        val saveCount = AtomicInteger()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState? = saved.get()
            override suspend fun save(state: DayWeaveUiState) {
                saved.set(state)
                saveCount.incrementAndGet()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val olderTerminal = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
                status = "completed",
                revision = 2,
                accumulatedSeconds = 30,
                actualSeconds = 30,
                startedAt = NOW.minusSeconds(120).toString(),
                runningSince = null,
                endedAt = NOW.minusSeconds(60).toString(),
                createdAt = NOW.minusSeconds(120).toString(),
                updatedAt = NOW.minusSeconds(60).toString(),
            )
            val newerActive = activeSession(SESSION_ID)
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(3, newerActive)
                historyResult = listOf(newerActive, olderTerminal)
            }
            val manager = manager(store, transport)

            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
            assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
            assertEquals(SESSION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
            assertTrue(OTHER_SESSION_ID in store.state.value.terminalExecutionOutcomes)
            val afterFirstPoll = saveCount.get()

            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
            assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
            assertEquals(SESSION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
            // History verification fence, snapshot, and history window only: the shadowed terminal
            // fact is ledger-only and must not create an extra durable presentation write.
            assertEquals(3, saveCount.get() - afterFirstPoll)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun durableTerminalFenceRejectsStartEvenIfAStoredPlanRegressesToScheduled() = runBlocking {
        val canonicalItem = CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "task",
            status = "planned",
            title = "Write test plan",
            timezoneName = "Europe/Madrid",
            durationSeconds = 1_800,
            flexibleConstraintsJson = "{}",
            splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 50,
            urgency = 50,
            siblingOrder = 0,
            isExecutable = true,
            revision = 7,
            createdAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem),
                canonicalSyncOrigin = "https://api.example.test/",
                schedule = listOf(scheduleItem(BLOCK_ID, 0)),
            ),
        )
        val running = activeSession(SESSION_ID).toSnapshot()
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = "https://api.example.test/",
                configurationId = null,
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = "https://api.example.test/",
                configurationId = null,
                revision = 2,
                activeSession = null,
                changedSession = running.copy(
                    status = "completed",
                    revision = 2,
                    accumulatedSeconds = 120,
                    actualSeconds = 120,
                    runningSince = null,
                    endedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                ),
                message = "Completed",
            ),
        )
        val regressed = PlannerStore(
            store.state.value.copy(
                schedule = listOf(scheduleItem(BLOCK_ID, 0).copy(status = ItemStatus.SCHEDULED)),
            ),
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, null)
            historyResult = listOf(
                activeSession(SESSION_ID).copy(
                    status = "completed",
                    revision = 2,
                    accumulatedSeconds = 120,
                    actualSeconds = 120,
                    runningSince = null,
                    endedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                ),
            )
        }

        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            manager(regressed, transport, ExecutionCredentialStore(null)).start(BLOCK_ID),
        )
        assertTrue(transport.commandBodies.isEmpty())
        assertEquals(ItemStatus.COMPLETED, regressed.state.value.schedule.single().status)
    }

    @Test
    fun lostCompleteWithExpiredIdempotencyReconcilesExactTerminalHistory() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val completed = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 120,
            actualSeconds = 120,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            commandHandler = { _, _ ->
                attempts += 1
                snapshotResult = RemoteExecutionSnapshot(2, null)
                historyResult = listOf(completed)
                if (attempts == 1) throw IOException("response lost")
                throw ExecutionApiException.Conflict()
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.complete(BLOCK_ID))
        assertNotNull(store.state.value.pendingExecutionCommand)

        assertEquals(ExecutionSyncOutcome.CONFLICT, manager.refresh())

        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertEquals(2, transport.commandBodies.size)
    }

    @Test
    fun rejectedRetryRetainsFenceWhenHistoryCannotBeRead() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            commandHandler = { _, _ ->
                attempts += 1
                if (attempts == 1) throw IOException("response lost")
                throw ExecutionApiException.Conflict()
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.complete(BLOCK_ID))
        val pendingKey = store.state.value.pendingExecutionCommand?.idempotencyKey
        transport.historyError = IOException("history offline")

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.refresh())
        assertEquals(pendingKey, store.state.value.pendingExecutionCommand?.idempotencyKey)
        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
    }

    @Test
    fun rejectedRestartRetryFindsPendingLeaseOnLaterHistoryPage() = runBlocking {
        val baseline = (0 until 150).map { index ->
            activeSession(UUID(0L, index.toLong() + 5_000L).toString()).copy(
                itemId = OTHER_ITEM_ID,
                plannedBlockId = null,
                status = "completed",
                revision = 2,
                accumulatedSeconds = index.toLong(),
                actualSeconds = index.toLong(),
                startedAt = NOW.minusSeconds(index.toLong() + 300L).toString(),
                runningSince = null,
                endedAt = NOW.minusSeconds(index.toLong()).toString(),
                createdAt = NOW.minusSeconds(index.toLong() + 300L).toString(),
                updatedAt = NOW.minusSeconds(index.toLong()).toString(),
            )
        }
        var serverHistory = baseline
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(300, null)
            historyPageHandler = { limit, offset ->
                val start = offset.toInt()
                val end = minOf(start + limit, serverHistory.size)
                RemoteExecutionHistoryPage(
                    sessions = serverHistory.subList(start, end),
                    nextOffset = end.toLong().takeIf { end < serverHistory.size },
                )
            }
            commandHandler = { _, body ->
                attempts += 1
                if (attempts == 1) {
                    val command = Json.parseToJsonElement(body).jsonObject
                        .getValue("command").jsonObject
                    val accepted = activeSession(
                        sessionId = command.getValue("session_id").jsonPrimitive.content,
                        deviceId = command.getValue("device_id").jsonPrimitive.content,
                    ).copy(
                        // A workspace wall-clock rollback can put a newly accepted session after
                        // the newest page even though its global revision is current.
                        startedAt = NOW.minusSeconds(2_000).toString(),
                        runningSince = NOW.minusSeconds(2_000).toString(),
                        createdAt = NOW.minusSeconds(2_000).toString(),
                        updatedAt = NOW.minusSeconds(2_000).toString(),
                    )
                    serverHistory = baseline + accepted
                    snapshotResult = RemoteExecutionSnapshot(301, accepted)
                    throw IOException("accepted response lost")
                }
                throw ExecutionApiException.Conflict()
            }
        }
        val store = plannerStore()
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.canonicalExecutionHistoryContinuityEstablished)

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.start(BLOCK_ID))
        assertNotNull(store.state.value.pendingExecutionCommand)
        val callsBeforeRecovery = transport.historyCalls

        assertEquals(ExecutionSyncOutcome.CONFLICT, manager.refresh())
        assertTrue(transport.historyCalls - callsBeforeRecovery >= 2)
        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals(301L, store.state.value.canonicalExecutionRevision)
        assertNotNull(store.state.value.canonicalExecutionSession)
    }

    @Test
    fun sameSessionIdWithDifferentImmutableHistoryIdentityCannotClearFence() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val mismatched = running.copy(
            sourceDeviceId = OTHER_DEVICE_ID,
            status = "completed",
            revision = 2,
            actualSeconds = 20,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            commandHandler = { _, _ ->
                attempts += 1
                if (attempts == 1) {
                    snapshotResult = RemoteExecutionSnapshot(2, null)
                    historyResult = listOf(mismatched)
                    throw IOException("response lost")
                }
                throw ExecutionApiException.Conflict()
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.complete(BLOCK_ID))

        assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager.refresh())
        assertNotNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
    }

    @Test
    fun racingSnapshotHistoryReadRetainsAmbiguousFence() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val completed = running.copy(
            status = "completed",
            revision = 2,
            actualSeconds = 20,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            commandHandler = { _, _ ->
                attempts += 1
                if (attempts == 1) {
                    historyResult = listOf(completed)
                    throw IOException("response lost")
                }
                throw ExecutionApiException.Conflict()
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.complete(BLOCK_ID))
        val race = listOf(
            RemoteExecutionSnapshot(2, null),
            RemoteExecutionSnapshot(3, activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID)),
            RemoteExecutionSnapshot(4, null),
        ).iterator()
        transport.snapshotHandler = { race.next() }

        assertEquals(ExecutionSyncOutcome.CONFLICT, manager.refresh())
        assertNotNull(store.state.value.pendingExecutionCommand)
        assertEquals(4, transport.historyCalls)
    }

    @Test
    fun unmatchedRemoteLeasePausesLocalFocusAndPreservesRecordedMinutes() = runBlocking {
        val local = ScheduleItem(
            id = "local-focus",
            title = "Local focus",
            kind = ItemKind.TASK,
            startMinute = 8 * 60,
            durationMinutes = 30,
            status = ItemStatus.ACTIVE,
        )
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                schedule = listOf(scheduleItem(BLOCK_ID, 0), local),
                activeSession = ActiveSession(
                    itemId = local.id,
                    elapsedMinutes = 3,
                    isPaused = false,
                ),
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val remote = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
            itemId = OTHER_ITEM_ID,
            plannedBlockId = null,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, remote)
        }

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(store, transport).refresh())

        val schedule = store.state.value.schedule.associateBy(ScheduleItem::id)
        assertEquals(ItemStatus.PAUSED, schedule.getValue(local.id).status)
        assertEquals(3, schedule.getValue(local.id).actualMinutes)
        assertEquals(ItemStatus.ACTIVE, schedule.getValue(OTHER_SESSION_ID).status)
        assertEquals(1, schedule.values.count { it.status == ItemStatus.ACTIVE })
        assertEquals(
            OTHER_SESSION_ID,
            store.state.value.activeSession?.canonicalExecutionSessionId,
        )
        assertEquals(OTHER_SESSION_ID, store.state.value.canonicalExecutionSession?.id)
    }

    @Test
    fun deferredRemoteClosureIsRetainedWithoutAnyTerminalOrRecurrenceProjection() = runBlocking {
        val canonicalItem = CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "habit",
            status = "planned",
            title = "Practice",
            timezoneName = "Europe/Madrid",
            durationSeconds = 1_800,
            recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
            flexibleConstraintsJson = "{}",
            splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 50,
            urgency = 50,
            siblingOrder = 0,
            isExecutable = true,
            revision = 7,
            createdAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val block = scheduleItem(BLOCK_ID, 0).copy(
            kind = ItemKind.HABIT,
            occurrenceId = OCCURRENCE_ID,
        )
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                schedule = listOf(block),
                canonicalItems = listOf(canonicalItem),
                canonicalSyncOrigin = "https://api.example.test/",
                canonicalConfigurationId = DEFAULT_CONFIGURATION_ID,
                occurrenceSeriesItemIds = mapOf(OCCURRENCE_ID to ITEM_ID),
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val running = activeSession(SESSION_ID).copy(occurrenceId = OCCURRENCE_ID)
        val deferred = running.copy(
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 135,
            actualSeconds = 135,
            runningSince = null,
            endedAt = NOW.toString(),
            moveStart = NOW.plusSeconds(3_600).toString(),
            moveEnd = NOW.plusSeconds(7_200).toString(),
            updatedAt = NOW.toString(),
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(SESSION_ID, store.state.value.canonicalExecutionSession?.id)

        transport.snapshotResult = RemoteExecutionSnapshot(2, null)
        transport.historyResult = listOf(deferred)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        val state = store.state.value
        assertNull(state.canonicalExecutionSession)
        assertNull(state.activeSession)
        assertEquals(ItemStatus.SCHEDULED, state.schedule.single().status)
        assertEquals("planned", state.canonicalItems.single().status)
        val deferredOutcome = state.terminalExecutionOutcomes.getValue(deferred.id)
        assertEquals(deferred.toSnapshot(), deferredOutcome.session)
        assertFalse(deferredOutcome.requiresCanonicalItemProjection)
        assertNull(deferredOutcome.canonicalProjectionRevision)
        assertNull(deferredOutcome.canonicalProjectionResolution)
        assertNull(deferredOutcome.canonicalProjectionConflict)
        assertNull(deferredOutcome.canonicalProjectionRetryAuthorizedAt)
        assertTrue(state.recurrenceOutcomes.isEmpty())
        assertTrue(state.recurrenceCompletionAnchors.isEmpty())
        assertEquals(deferred.toSnapshot(), state.canonicalExecutionHistoryWindow.single())
        assertEquals(2L, state.canonicalExecutionHistoryWindowRevision)
        assertTrue(state.canonicalExecutionHistoryVerified)
        assertTrue(store.isCanonicalExecutionStartBlocked(BLOCK_ID))
        assertTrue(transport.commandBodies.isEmpty())

        val restarted = PlannerStore(state)
        val retained = restarted.state.value.canonicalExecutionHistoryWindow.single()
        assertEquals("deferred", retained.status)
        assertEquals(135L, retained.actualSeconds)
        assertEquals(deferred.moveStart, retained.moveStart)
        assertEquals(deferred.moveEnd, retained.moveEnd)
        assertEquals(deferredOutcome, restarted.state.value.terminalExecutionOutcomes[deferred.id])
        assertTrue(restarted.isCanonicalExecutionStartBlocked(BLOCK_ID))

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(restarted, transport).refresh())
        assertEquals(deferredOutcome, restarted.state.value.terminalExecutionOutcomes[deferred.id])
        assertTrue(restarted.isCanonicalExecutionStartBlocked(BLOCK_ID))
    }

    @Test
    fun coldRefreshLetsNewerDeferredClosureShadowOlderTerminalPresentation() = runBlocking {
        val olderCompleted = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            startedAt = NOW.minusSeconds(120).toString(),
            runningSince = null,
            endedAt = NOW.minusSeconds(60).toString(),
            createdAt = NOW.minusSeconds(120).toString(),
            updatedAt = NOW.minusSeconds(60).toString(),
        )
        val newerDeferred = activeSession(SESSION_ID).copy(
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = NOW.toString(),
            moveStart = NOW.plusSeconds(3_600).toString(),
            moveEnd = NOW.plusSeconds(7_200).toString(),
            updatedAt = NOW.toString(),
        )
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(4, null)
            historyResult = listOf(newerDeferred, olderCompleted)
        }

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(store, transport).refresh())

        val state = store.state.value
        assertEquals(ItemStatus.SCHEDULED, state.schedule.single().status)
        assertNull(state.schedule.single().actualMinutes)
        assertEquals(
            setOf(newerDeferred.id, olderCompleted.id),
            state.terminalExecutionOutcomes.keys,
        )
        assertFalse(
            state.terminalExecutionOutcomes.getValue(newerDeferred.id)
                .requiresCanonicalItemProjection,
        )
        assertTrue(state.recurrenceOutcomes.isEmpty())
        assertTrue(state.recurrenceCompletionAnchors.isEmpty())
        assertTrue(store.isCanonicalExecutionStartBlocked(BLOCK_ID))
    }

    @Test
    fun authoritativeLegacyClockActiveLeaseSuppressesNewerTimestampTerminalRecurrence() =
        runBlocking {
            val block = scheduleItem(BLOCK_ID, 0).copy(
                kind = ItemKind.HABIT,
                occurrenceId = OCCURRENCE_ID,
            )
            val store = PlannerStore(
                initialState = DayWeaveUiState(
                    schedule = listOf(block),
                    canonicalSyncOrigin = "https://api.example.test/",
                    occurrenceSeriesItemIds = mapOf(OCCURRENCE_ID to ITEM_ID),
                ),
                nowEpochMillis = { NOW.toEpochMilli() },
            )
            val oldTerminalWithLaterClock = activeSession(
                OTHER_SESSION_ID,
                OTHER_DEVICE_ID,
            ).copy(
                occurrenceId = OCCURRENCE_ID,
                status = "completed",
                revision = 2,
                accumulatedSeconds = 60,
                actualSeconds = 60,
                runningSince = null,
                endedAt = NOW.plusSeconds(3_600).toString(),
                updatedAt = NOW.plusSeconds(3_600).toString(),
            )
            val authoritativeActiveWithEarlierClock = activeSession(SESSION_ID).copy(
                occurrenceId = OCCURRENCE_ID,
                startedAt = NOW.minusSeconds(3_600).toString(),
                runningSince = NOW.minusSeconds(3_600).toString(),
                createdAt = NOW.minusSeconds(3_600).toString(),
                updatedAt = NOW.minusSeconds(3_600).toString(),
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(3, authoritativeActiveWithEarlierClock)
                historyResult = listOf(
                    oldTerminalWithLaterClock,
                    authoritativeActiveWithEarlierClock,
                )
            }

            assertEquals(ExecutionSyncOutcome.SUCCESS, manager(store, transport).refresh())

            val state = store.state.value
            assertEquals(SESSION_ID, state.canonicalExecutionSession?.id)
            assertEquals(ItemStatus.ACTIVE, state.schedule.single().status)
            assertEquals(SESSION_ID, state.activeSession?.canonicalExecutionSessionId)
            assertTrue(state.recurrenceOutcomes.isEmpty())
            assertTrue(state.recurrenceCompletionAnchors.isEmpty())
            assertTrue(state.terminalExecutionOutcomes.containsKey(OTHER_SESSION_ID))
        }

    @Test
    fun malformedDeferredWindowsAndMoveFieldsOnOtherStatusesFailClosed() = runBlocking {
        val valid = activeSession(SESSION_ID).copy(
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = NOW.toString(),
            moveStart = NOW.plusSeconds(60).toString(),
            moveEnd = NOW.plusSeconds(120).toString(),
            updatedAt = NOW.toString(),
        )
        val invalidRows = listOf(
            valid.copy(actualSeconds = null),
            valid.copy(endedAt = null),
            valid.copy(moveStart = null),
            valid.copy(moveEnd = null),
            valid.copy(moveStart = NOW.toString()),
            valid.copy(moveEnd = valid.moveStart),
            valid.copy(moveEnd = NOW.plusSeconds(60 + 24 * 60 * 60 + 1L).toString()),
            valid.copy(status = "completed"),
        )

        invalidRows.forEachIndexed { index, invalid ->
            val store = plannerStore()
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(2, null)
                historyResult = listOf(invalid)
            }

            assertEquals(
                "invalid fixture $index",
                ExecutionSyncOutcome.PROTOCOL_FAILURE,
                manager(store, transport).refresh(),
            )
            assertTrue(store.state.value.canonicalExecutionHistoryWindow.isEmpty())
            assertTrue(store.state.value.terminalExecutionOutcomes.isEmpty())
            assertNull(store.state.value.canonicalExecutionSession)
        }
    }

    @Test
    fun nullToNullPollStillReconcilesEveryUnseenTerminalSession() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            historyResult = emptyList()
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        val older = activeSession(OTHER_SESSION_ID).copy(
            status = "skipped",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            startedAt = NOW.minusSeconds(120).toString(),
            runningSince = null,
            endedAt = NOW.minusSeconds(60).toString(),
            createdAt = NOW.minusSeconds(120).toString(),
            updatedAt = NOW.minusSeconds(60).toString(),
        )
        val newer = activeSession(SESSION_ID).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        transport.snapshotResult = RemoteExecutionSnapshot(4, null)
        transport.historyResult = listOf(newer, older)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(
            setOf(SESSION_ID, OTHER_SESSION_ID),
            store.state.value.terminalExecutionOutcomes.keys,
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertTrue(store.state.value.canonicalExecutionHistoryVerified)
    }

    @Test
    fun sameOriginCredentialReplacementCannotRebindPendingCommandAfterRestart() = runBlocking {
        val store = plannerStore(configurationId = "configuration-a")
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            historyResult = emptyList()
            commandHandler = { _, _ -> throw IOException("response outcome unknown") }
        }
        val credentialA = ExecutionCredentialStore("configuration-a")
        assertEquals(
            ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(store, transport, credentialA).start(BLOCK_ID),
        )
        val pending = requireNotNull(store.state.value.pendingExecutionCommand)
        assertEquals("configuration-a", pending.configurationId)

        val restarted = PlannerStore(store.state.value)
        val credentialB = ExecutionCredentialStore("configuration-b")
        val commandAttempts = transport.commandBodies.size
        assertEquals(
            ExecutionSyncOutcome.CONFIGURATION_CHANGED,
            manager(restarted, transport, credentialB).refresh(),
        )
        assertEquals(pending, restarted.state.value.pendingExecutionCommand)
        assertEquals("configuration-a", restarted.state.value.canonicalExecutionConfigurationId)
        assertEquals(commandAttempts, transport.commandBodies.size)
    }

    @Test
    fun mutatedConfirmedTerminalHistoryFailsClosedWithoutReplacingLedger() = runBlocking {
        val original = activeSession(SESSION_ID).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, null)
            historyResult = listOf(original)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        transport.historyResult = listOf(
            original.copy(
                actualSeconds = 91,
                updatedAt = NOW.plusSeconds(1).toString(),
            ),
        )
        assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager.refresh())
        assertEquals(
            90L,
            store.state.value.terminalExecutionOutcomes.getValue(SESSION_ID)
                .session.actualSeconds,
        )
        assertFalse(store.state.value.canonicalExecutionHistoryVerified)
    }

    @Test
    fun duplicateOrSnapshotIncoherentHistoryFailsBeforeAnyFenceCanClear() = runBlocking {
        val terminal = activeSession(SESSION_ID).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, null)
            historyResult = listOf(terminal, terminal)
        }
        assertEquals(
            ExecutionSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refresh(),
        )
        assertTrue(store.state.value.terminalExecutionOutcomes.isEmpty())
        assertFalse(store.state.value.canonicalExecutionHistoryVerified)

        val older = terminal.copy(
            id = OTHER_SESSION_ID,
            updatedAt = NOW.minusSeconds(1).toString(),
            endedAt = NOW.minusSeconds(1).toString(),
        )
        transport.snapshotResult = RemoteExecutionSnapshot(4, null)
        transport.historyResult = listOf(older, terminal)
        assertEquals(
            ExecutionSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refresh(),
        )
        assertTrue(store.state.value.terminalExecutionOutcomes.isEmpty())

        transport.snapshotResult = RemoteExecutionSnapshot(2, null)
        transport.historyResult = listOf(terminal.copy(revision = 3))
        assertEquals(
            ExecutionSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refresh(),
        )
        assertTrue(store.state.value.terminalExecutionOutcomes.isEmpty())
    }

    @Test
    fun historyNetworkFailureRevokesPriorStartAdmissionDurably() = runBlocking {
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(0, null)
            historyResult = emptyList()
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.canonicalExecutionHistoryVerified)

        transport.historyError = IOException("history unavailable")
        assertEquals(
            ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.start(BLOCK_ID),
        )
        assertFalse(store.state.value.canonicalExecutionHistoryVerified)
        assertTrue(transport.commandBodies.isEmpty())
    }

    @Test
    fun freshClientPaginatesCompleteHistoryAndCanStartAfterMoreThanOneHundredSessions() = runBlocking {
        val completeHistory = (0 until 150).map { index ->
            activeSession(UUID(0L, index.toLong() + 100L).toString()).copy(
                itemId = OTHER_ITEM_ID,
                plannedBlockId = null,
                status = "completed",
                revision = 2,
                accumulatedSeconds = index.toLong(),
                actualSeconds = index.toLong(),
                startedAt = NOW.minusSeconds(index.toLong()).toString(),
                runningSince = null,
                endedAt = NOW.minusSeconds(index.toLong()).toString(),
                createdAt = NOW.minusSeconds(index.toLong()).toString(),
                updatedAt = NOW.minusSeconds(index.toLong()).toString(),
            )
        }
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(300, null)
            historyPageHandler = { limit, offset ->
                val start = offset.toInt()
                val end = minOf(start + limit, completeHistory.size)
                RemoteExecutionHistoryPage(
                    sessions = completeHistory.subList(start, end),
                    nextOffset = end.toLong().takeIf { end < completeHistory.size },
                )
            }
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(100, store.state.value.canonicalExecutionHistoryWindow.size)
        assertEquals(150, store.state.value.terminalExecutionOutcomes.size)
        assertTrue(store.state.value.canonicalExecutionHistoryContinuityEstablished)
        assertTrue(store.state.value.canonicalExecutionHistoryVerified)

        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            val started = activeSession(
                sessionId = command.getValue("session_id").jsonPrimitive.content,
                deviceId = command.getValue("device_id").jsonPrimitive.content,
            )
            transport.snapshotResult = RemoteExecutionSnapshot(301, started)
            RemoteExecutionMutation(301, started, started, replayed = false)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.start(BLOCK_ID))
        assertTrue(transport.commandBodies.isNotEmpty())
    }

    @Test
    fun deferredLifetimeFenceSurvivesHistoryWindowEvictionAndRestartRefresh() = runBlocking {
        val deferredId = UUID(0L, 9_999L).toString()
        val completeHistory = (0..100).map { index ->
            val endedAt = NOW.minusSeconds(index.toLong())
            val base = activeSession(UUID(0L, index.toLong() + 5_000L).toString()).copy(
                itemId = OTHER_ITEM_ID,
                plannedBlockId = null,
                status = "completed",
                revision = 2,
                accumulatedSeconds = index.toLong(),
                actualSeconds = index.toLong(),
                startedAt = endedAt.minusSeconds(120).toString(),
                runningSince = null,
                endedAt = endedAt.toString(),
                createdAt = endedAt.minusSeconds(120).toString(),
                updatedAt = endedAt.toString(),
            )
            if (index == 100) {
                base.copy(
                    id = deferredId,
                    itemId = ITEM_ID,
                    plannedBlockId = BLOCK_ID,
                    status = "deferred",
                    moveStart = endedAt.plusSeconds(60).toString(),
                    moveEnd = endedAt.plusSeconds(3_660).toString(),
                )
            } else {
                base
            }
        }
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(202, null)
            historyPageHandler = { limit, offset ->
                val start = offset.toInt()
                val end = minOf(start + limit, completeHistory.size)
                RemoteExecutionHistoryPage(
                    sessions = completeHistory.subList(start, end),
                    nextOffset = end.toLong().takeIf { end < completeHistory.size },
                )
            }
        }
        val store = plannerStore()

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(store, transport).refresh())
        val state = store.state.value
        assertEquals(100, state.canonicalExecutionHistoryWindow.size)
        assertTrue(state.canonicalExecutionHistoryWindow.none { it.id == deferredId })
        assertEquals(101, state.terminalExecutionOutcomes.size)
        assertEquals(
            "deferred",
            state.terminalExecutionOutcomes.getValue(deferredId).session.status,
        )
        assertTrue(store.isCanonicalExecutionStartBlocked(BLOCK_ID))

        val restarted = PlannerStore(state)
        assertTrue(restarted.isCanonicalExecutionStartBlocked(BLOCK_ID))
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(restarted, transport).refresh())
        assertTrue(
            restarted.state.value.canonicalExecutionHistoryWindow.none { it.id == deferredId },
        )
        assertEquals(
            "deferred",
            restarted.state.value.terminalExecutionOutcomes.getValue(deferredId).session.status,
        )
        assertTrue(restarted.isCanonicalExecutionStartBlocked(BLOCK_ID))
    }

    @Test
    fun provenHundredRowHistoryRollsForwardWithoutLifetimeDeadlock() = runBlocking {
        val completeWindow = (0 until 100).map { index ->
            activeSession(UUID(0L, index.toLong() + 1_000L).toString()).copy(
                itemId = OTHER_ITEM_ID,
                plannedBlockId = null,
                status = "completed",
                revision = 2,
                accumulatedSeconds = index.toLong(),
                actualSeconds = index.toLong(),
                startedAt = NOW.minusSeconds(index.toLong() + 120L).toString(),
                runningSince = null,
                endedAt = NOW.minusSeconds(index.toLong()).toString(),
                createdAt = NOW.minusSeconds(index.toLong() + 120L).toString(),
                updatedAt = NOW.minusSeconds(index.toLong()).toString(),
            )
        }
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(200, null)
            historyResult = completeWindow
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.canonicalExecutionHistoryContinuityEstablished)
        assertEquals(200L, store.state.value.canonicalExecutionHistoryWindowRevision)

        val newTerminal = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
            itemId = OTHER_ITEM_ID,
            plannedBlockId = null,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = NOW.plusSeconds(1).toString(),
            updatedAt = NOW.plusSeconds(1).toString(),
        )
        transport.snapshotResult = RemoteExecutionSnapshot(202, null)
        transport.historyResult = listOf(newTerminal) + completeWindow.take(99)

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.canonicalExecutionHistoryVerified)
        assertTrue(store.state.value.canonicalExecutionHistoryContinuityEstablished)
        assertEquals(202L, store.state.value.canonicalExecutionHistoryWindowRevision)

        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            val started = activeSession(
                sessionId = command.getValue("session_id").jsonPrimitive.content,
                deviceId = command.getValue("device_id").jsonPrimitive.content,
            )
            transport.snapshotResult = RemoteExecutionSnapshot(203, started)
            RemoteExecutionMutation(203, started, started, replayed = false)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.start(BLOCK_ID))
        assertTrue(transport.commandBodies.isNotEmpty())
    }

    @Test
    fun shortHistoryWithMissingRevisionMassNeverUnlocksStarts() = runBlocking {
        val omittedCommandEvidence = activeSession(OTHER_SESSION_ID, OTHER_DEVICE_ID).copy(
            itemId = OTHER_ITEM_ID,
            plannedBlockId = null,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val store = plannerStore()
        val transport = FakeExecutionTransport().apply {
            // Revision three proves at least one command is absent from this revision-two row.
            snapshotResult = RemoteExecutionSnapshot(3, null)
            historyResult = listOf(omittedCommandEvidence)
        }
        val manager = manager(store, transport)

        assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager.refresh())
        assertFalse(store.state.value.canonicalExecutionHistoryVerified)
        assertFalse(store.state.value.canonicalExecutionHistoryContinuityEstablished)
        assertEquals(1, store.state.value.canonicalExecutionHistoryWindow.size)

        assertEquals(ExecutionSyncOutcome.PROTOCOL_FAILURE, manager.start(BLOCK_ID))
        assertTrue(transport.commandBodies.isEmpty())
    }

    private fun manager(
        store: PlannerStore,
        transport: FakeExecutionTransport,
        credentialStore: ApiCredentialStore = ExecutionCredentialStore(),
    ) = ExecutionSyncManager(
        plannerStore = store,
        credentialStore = credentialStore,
        transport = transport,
        now = { NOW },
        newUuid = UUIDS.iterator().let { iterator -> { iterator.next() } },
    )

    private fun plannerStore(
        split: Boolean = false,
        configurationId: String = DEFAULT_CONFIGURATION_ID,
    ): PlannerStore {
        val blocks = mutableListOf(scheduleItem(BLOCK_ID, 0))
        if (split) blocks += scheduleItem(SECOND_BLOCK_ID, 1)
        val revision = publishedRevision()
        return PlannerStore(
            initialState = DayWeaveUiState(
                schedule = blocks,
                canonicalItems = listOf(canonicalItem()),
                canonicalSyncOrigin = "https://api.example.test/",
                canonicalConfigurationId = configurationId,
                publishedScheduleRevision = revision,
                publishedScheduleProof = PublishedScheduleProofSnapshot(
                    schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                    syncOrigin = "https://api.example.test/",
                    configurationId = configurationId,
                    revision = revision,
                    asOf = NOW.toString(),
                    blocks = blocks.map(::publishedBlockProof),
                ),
                scheduleInputDigest = revision.inputDigest,
                scheduleGeneratedAt = NOW.toString(),
                schedulePlanningZoneId = "Europe/Madrid",
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
    }

    private fun canonicalItem() = CanonicalItemSnapshot(
        id = ITEM_ID,
        kind = "task",
        status = "planned",
        title = "Write test plan",
        timezoneName = "Europe/Madrid",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = NOW.toString(),
        updatedAt = NOW.toString(),
    )

    private fun publishedRevision() = PublishedScheduleRevisionSnapshot(
        id = PUBLISHED_REVISION_ID,
        revision = "1:$PUBLISHED_REVISION_ID",
        revisionNumber = 1uL,
        inputDigest = "sha256:${"a".repeat(64)}",
        horizonStart = "2026-09-01T00:00:00Z",
        horizonEnd = "2026-09-02T00:00:00Z",
        timezoneName = "Europe/Madrid",
        publishedAt = NOW.toString(),
    )

    private fun publishedBlockProof(block: ScheduleItem) =
        PublishedScheduleBlockProofSnapshot(
            id = block.id,
            itemId = requireNotNull(block.canonicalItemId),
            itemRevision = requireNotNull(block.canonicalRevision),
            occurrenceId = block.occurrenceId,
            sessionIndex = requireNotNull(block.sessionIndex),
            start = requireNotNull(block.absoluteStartAt),
            end = requireNotNull(block.absoluteEndAt),
            kind = requireNotNull(block.canonicalBlockKind),
        )

    private fun scheduleItem(id: String, sessionIndex: Int) = ScheduleItem(
        id = id,
        title = "Write test plan",
        kind = ItemKind.TASK,
        startMinute = 9 * 60 + sessionIndex * 30,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        isSplittable = sessionIndex > 0,
        canonicalItemId = ITEM_ID,
        canonicalRevision = 7,
        sessionIndex = sessionIndex,
        absoluteStartAt = "2026-09-01T07:${if (sessionIndex == 0) "00" else "30"}:00Z",
        absoluteEndAt = "2026-09-01T07:${if (sessionIndex == 0) "30" else "59"}:00Z",
        planningZoneId = "Europe/Madrid",
        canonicalBlockKind = "planned",
    )

    private fun activeSession(
        sessionId: String,
        deviceId: String = DEVICE_ID,
    ) = RemoteExecutionSession(
        id = sessionId,
        itemId = ITEM_ID,
        itemRevision = 7,
        occurrenceId = null,
        sessionIndex = 0,
        plannedBlockId = BLOCK_ID,
        sourceDeviceId = deviceId,
        status = "active",
        revision = 1,
        accumulatedSeconds = 0,
        actualSeconds = null,
        startedAt = "2026-09-01T07:00:00Z",
        runningSince = "2026-09-01T07:00:00Z",
        pausedAt = null,
        pauseUntil = null,
        pauseReason = null,
        endedAt = null,
        createdAt = "2026-09-01T07:00:00Z",
        updatedAt = "2026-09-01T07:00:00Z",
    )

    private fun RemoteExecutionSession.toSnapshot() = CanonicalExecutionSessionSnapshot(
        id = id,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId,
        status = status,
        revision = revision,
        accumulatedSeconds = accumulatedSeconds,
        actualSeconds = actualSeconds,
        startedAt = startedAt,
        runningSince = runningSince,
        pausedAt = pausedAt,
        pauseUntil = pauseUntil,
        pauseReason = pauseReason,
        endedAt = endedAt,
        moveStart = moveStart,
        moveEnd = moveEnd,
        createdAt = createdAt,
        updatedAt = updatedAt,
    )

    private fun pausedSession(
        pauseUntil: String,
        revision: Long,
    ) = RemoteExecutionSession(
        id = SESSION_ID,
        itemId = ITEM_ID,
        itemRevision = 7,
        occurrenceId = null,
        sessionIndex = 0,
        plannedBlockId = BLOCK_ID,
        sourceDeviceId = DEVICE_ID,
        status = "paused",
        revision = revision,
        accumulatedSeconds = 120,
        actualSeconds = null,
        startedAt = "2026-09-01T06:45:00Z",
        runningSince = null,
        pausedAt = "2026-09-01T06:50:00Z",
        pauseUntil = pauseUntil,
        pauseReason = null,
        endedAt = null,
        createdAt = "2026-09-01T06:45:00Z",
        updatedAt = "2026-09-01T06:50:00Z",
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-01T07:00:00Z")
        val UUIDS = listOf(
            UUID.fromString(DEVICE_ID),
            UUID.fromString(SESSION_ID),
            UUID.fromString("77777777-7777-4777-8777-777777777777"),
            UUID.fromString("88888888-8888-4888-8888-888888888888"),
            UUID.fromString("99999999-9999-4999-8999-999999999999"),
        )
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_BLOCK_ID = "33333333-3333-4333-8333-333333333333"
        const val OLD_BLOCK_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val OTHER_ITEM_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val SESSION_ID = "44444444-4444-4444-8444-444444444444"
        const val DEVICE_ID = "55555555-5555-4555-8555-555555555555"
        const val OTHER_SESSION_ID = "66666666-6666-4666-8666-666666666666"
        const val OCCURRENCE_ID = "77777777-7777-4777-8777-777777777777"
        const val OTHER_DEVICE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val PUBLISHED_REVISION_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val DEFAULT_CONFIGURATION_ID = "configuration-1"
    }
}

private class ExecutionCredentialStore(
    private val configurationId: String? = "configuration-1",
) : ApiCredentialStore {
    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        configurationId?.let {
            AuthenticatedApiConfiguration.createBound(
                "https://api.example.test/",
                "test-secret",
                it,
            )
        } ?: AuthenticatedApiConfiguration.create("https://api.example.test/", "test-secret")

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeExecutionTransport : ExecutionTransport {
    var snapshotResult = RemoteExecutionSnapshot(0, null)
    var snapshotError: Throwable? = null
    var snapshotHandler: (suspend () -> RemoteExecutionSnapshot)? = null
    var commandHandler: suspend (String, String) -> RemoteExecutionMutation = { _, _ ->
        error("No command response configured")
    }
    var historyResult: List<RemoteExecutionSession>? = null
    var historyError: Throwable? = null
    var historyHandler: (suspend (Int) -> List<RemoteExecutionSession>)? = null
    var historyPageHandler: (suspend (Int, Long) -> RemoteExecutionHistoryPage)? = null
    val commandKeys = mutableListOf<String>()
    val commandBodies = mutableListOf<String>()
    var snapshotCalls = 0
    var historyCalls = 0

    override suspend fun snapshot(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteExecutionSnapshot {
        snapshotCalls += 1
        snapshotHandler?.let { return it() }
        snapshotError?.let { throw it }
        return snapshotResult
    }

    override suspend fun command(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteExecutionMutation {
        commandKeys += idempotencyKey
        commandBodies += requestJson
        return commandHandler(idempotencyKey, requestJson)
    }

    override suspend fun history(
        configuration: AuthenticatedApiConfiguration,
        limit: Int,
        offset: Long,
    ): RemoteExecutionHistoryPage {
        historyCalls += 1
        historyPageHandler?.let { return it(limit, offset) }
        historyHandler?.let { return RemoteExecutionHistoryPage(it(limit), null) }
        historyError?.let { throw it }
        return RemoteExecutionHistoryPage(
            historyResult ?: listOfNotNull(snapshotResult.activeSession),
            null,
        )
    }
}
