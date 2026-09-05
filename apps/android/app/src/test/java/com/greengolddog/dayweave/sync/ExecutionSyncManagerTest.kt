package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.deferredExecutionRecompositionNeeded
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferViolationSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PendingExecutionDeferIntent
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeferAssessmentHttpRequest
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
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlinx.serialization.json.put
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

        val relaunched = manager(
            store = store,
            transport = transport,
            cancelTimedBreakNotification = {
                error("a restored start command must not enter the timed-break barrier")
            },
        )
        assertEquals(ExecutionSyncOutcome.SUCCESS, relaunched.refresh())

        assertEquals(2, transport.commandBodies.size)
        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.commandBodies)
        assertEquals(listOf(pending.idempotencyKey, pending.idempotencyKey), transport.commandKeys)
        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
        assertEquals(SESSION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
    }

    @Test
    fun restoredResumeCancelsReminderBeforeExactReplayAndReconcilesAfterward() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = "2026-09-01T06:59:00Z",
            revision = 2,
        )
        val resumed = paused.copy(
            status = "active",
            revision = 3,
            pausedAt = null,
            pauseUntil = null,
            runningSince = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        var attempts = 0
        val events = mutableListOf<String>()
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            commandHandler = { _, _ ->
                attempts += 1
                events += "network"
                if (attempts == 1) throw IOException("response lost")
                snapshotResult = RemoteExecutionSnapshot(3, resumed)
                RemoteExecutionMutation(3, resumed, resumed, replayed = true)
            }
        }
        val first = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, first.refresh())
        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, first.resume(BLOCK_ID))
        assertEquals("resume", store.state.value.pendingExecutionCommand?.commandType)
        events.clear()

        val relaunched = manager(
            store = store,
            transport = transport,
            cancelTimedBreakNotification = {
                events += "cancel"
                true
            },
            reconcileTimedBreakNotification = { events += "reconcile" },
        )

        assertEquals(ExecutionSyncOutcome.SUCCESS, relaunched.refresh())
        assertEquals(listOf("cancel", "network", "reconcile"), events)
        assertNull(store.state.value.pendingExecutionCommand)
        assertEquals("active", store.state.value.canonicalExecutionSession?.status)
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
    fun timedBreakTransitionAwaitsCancellationAndReconcilesAfterDefinitiveFailure() = runBlocking {
        val store = plannerStore()
        val expired = pausedSession(
            pauseUntil = "2026-09-01T06:59:00Z",
            revision = 2,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, expired)
        }
        val events = mutableListOf<String>()
        val manager = manager(
            store = store,
            transport = transport,
            cancelTimedBreakNotification = {
                events += "cancel"
                true
            },
            reconcileTimedBreakNotification = { events += "reconcile" },
        )
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        transport.commandHandler = { _, _ -> throw ExecutionApiException.Validation(422) }

        assertEquals(ExecutionSyncOutcome.VALIDATION_FAILURE, manager.pause(BLOCK_ID, 600))

        assertEquals(listOf("cancel", "reconcile"), events)
        assertNull(store.state.value.pendingExecutionCommand)
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
        assertTrue(store.state.value.activeSession?.isPaused == true)
    }

    @Test
    fun failedNotificationCancellationBlocksTimedBreakTransitionBeforeJournalOrNetwork() =
        runBlocking {
            val store = plannerStore()
            val expired = pausedSession(
                pauseUntil = "2026-09-01T06:59:00Z",
                revision = 2,
            )
            val transport = FakeExecutionTransport().apply {
                snapshotResult = RemoteExecutionSnapshot(2, expired)
            }
            var cancellations = 0
            val manager = manager(
                store = store,
                transport = transport,
                cancelTimedBreakNotification = {
                    cancellations += 1
                    false
                },
            )
            assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

            assertEquals(ExecutionSyncOutcome.LOCAL_STORAGE_FAILURE, manager.resume(BLOCK_ID))

            assertEquals(1, cancellations)
            assertTrue(transport.commandBodies.isEmpty())
            assertNull(store.state.value.pendingExecutionCommand)
            assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
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
    fun stalePublishedHeadStillAllowsOpenEndedPauseResumeAndSkipServerCommands() = runBlocking {
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
        val hintReceipt = requireNotNull(
            store.recordPublishedScheduleRevisionHint(
                "https://api.example.test/",
                DEFAULT_CONFIGURATION_ID,
                2uL,
            ),
        )
        assertTrue(hintReceipt.awaitDurable())
        assertFalse(
            store.state.value.hasPublishedExecutionAuthority(store.state.value.schedule.single()),
        )

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
    fun confirmedActiveCanonicalDeferPausesThenMovesExactRemainingPublishedDuration() = runBlocking {
        val moveStart = NOW.plusSeconds(3_600)
        val moveEnd = moveStart.plusSeconds(1_800)
        val original = plannerStore()
        val pinnedSource = original.state.value.schedule.single().copy(
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        val hardBlock = ScheduleItem(
            id = "99999999-9999-4999-8999-999999999999",
            title = "Fixed appointment",
            kind = ItemKind.EVENT,
            startMinute = 10 * 60 + 10,
            durationMinutes = 10,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            absoluteStartAt = moveStart.plusSeconds(10 * 60L).toString(),
            absoluteEndAt = moveStart.plusSeconds(20 * 60L).toString(),
            planningZoneId = "Europe/Madrid",
            canonicalBlockKind = "external_fixed",
            sessionIndex = 0,
        )
        val store = PlannerStore(
            original.state.value.withPublishedSchedule(listOf(pinnedSource, hardBlock)),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        var serverSession = activeSession(SESSION_ID)
        var globalRevision = 1L
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(globalRevision, serverSession)
            historyResult = listOf(serverSession)
        }
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            globalRevision += 1
            serverSession = when (command.getValue("type").jsonPrimitive.content) {
                "pause" -> serverSession.copy(
                    status = "paused",
                    revision = 2,
                    runningSince = null,
                    pausedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                )
                "defer" -> {
                    assertEquals("0", command.getValue("actual_seconds").jsonPrimitive.content)
                    assertEquals(moveStart.toString(), command.getValue("move_start").jsonPrimitive.content)
                    assertEquals(moveEnd.toString(), command.getValue("move_end").jsonPrimitive.content)
                    serverSession.copy(
                        status = "deferred",
                        revision = 3,
                        accumulatedSeconds = 0,
                        actualSeconds = 0,
                        runningSince = null,
                        pauseUntil = null,
                        pauseReason = null,
                        moveStart = moveStart.toString(),
                        moveEnd = moveEnd.toString(),
                        endedAt = NOW.toString(),
                        updatedAt = NOW.toString(),
                    )
                }
                else -> error("Unexpected execution command")
            }
            val active = serverSession.takeIf { it.status in setOf("active", "paused") }
            transport.snapshotResult = RemoteExecutionSnapshot(globalRevision, active)
            transport.historyResult = listOf(serverSession)
            RemoteExecutionMutation(globalRevision, active, serverSession, replayed = false)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(
            ExecutionSyncOutcome.SUCCESS,
            manager.doLater(BLOCK_ID, moveStart),
        )

        assertEquals(listOf("pause", "defer"), transport.commandBodies.map { body ->
            Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
                .getValue("type").jsonPrimitive.content
        })
        assertNull(store.state.value.canonicalExecutionSession)
        assertNull(store.state.value.activeSession)
        assertEquals("deferred", store.state.value.terminalExecutionOutcomes
            .getValue(SESSION_ID).session.status)
        assertNull(store.state.value.publishedScheduleProof)
        assertNull(store.state.value.publishedScheduleRevision)
        assertNull(store.state.value.scheduleInputDigest)
    }

    @Test
    fun executionDeferRejectsTooCloseOrMisalignedProgrammaticTargetsBeforePausing() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            manager.doLater(BLOCK_ID, NOW.plusSeconds(5 * 60L)),
        )
        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            manager.doLater(BLOCK_ID, NOW.plusSeconds(10 * 60L + 1)),
        )
        assertTrue(transport.commandBodies.isEmpty())
        assertTrue(transport.assessmentRequests.isEmpty())
        assertNull(store.state.value.pendingExecutionDeferIntent)
    }

    @Test
    fun executionDeferAcceptsExactTtlPlusOneSlotBoundary() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
            assessmentHandler = { request -> warningAssessment(this, request) }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, NOW.plusSeconds(10 * 60L)),
        )
        assertEquals(NOW.plusSeconds(10 * 60L).toString(), transport.assessmentRequests.single().moveStart)
        assertNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
    }

    @Test
    fun delayedRestartClearsUnassessableTargetBeforeChangingActiveLease() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(store, transport).refresh())
        val block = store.state.value.schedule.single { it.id == BLOCK_ID }
        val moveStart = NOW.plusSeconds(3_600)
        val intent = PendingExecutionDeferIntent(
            schemaVersion = 1,
            syncOrigin = requireNotNull(store.state.value.canonicalExecutionSyncOrigin),
            configurationId = store.state.value.canonicalExecutionConfigurationId,
            sessionId = running.id,
            itemId = running.itemId,
            itemRevision = running.itemRevision,
            occurrenceId = running.occurrenceId,
            sessionIndex = running.sessionIndex,
            plannedBlockId = requireNotNull(running.plannedBlockId),
            sourceDeviceId = running.sourceDeviceId,
            focusedBlockId = block.id,
            sourceStart = requireNotNull(block.absoluteStartAt),
            sourceEnd = requireNotNull(block.absoluteEndAt),
            moveStart = moveStart.toString(),
            stagedAt = NOW.toString(),
        )
        assertTrue(requireNotNull(store.stageExecutionDeferIntent(intent)).awaitDurable())
        val delayedNow = moveStart.minusSeconds(5 * 60L)
        val relaunched = PlannerStore(
            store.state.value,
            nowEpochMillis = { delayedNow.toEpochMilli() },
        )
        val relaunchedManager = manager(relaunched, transport, currentNow = { delayedNow })

        assertEquals(ExecutionSyncOutcome.INVALID_LOCAL_STATE, relaunchedManager.refresh())
        assertNull(relaunched.state.value.pendingExecutionDeferIntent)
        assertEquals("active", relaunched.state.value.canonicalExecutionSession?.status)
        assertTrue(transport.commandBodies.isEmpty())
        assertTrue(transport.assessmentRequests.isEmpty())
        assertTrue(relaunchedManager.state.value.message.contains("at least ten minutes"))
        val secondRelaunch = PlannerStore(
            relaunched.state.value,
            nowEpochMillis = { delayedNow.toEpochMilli() },
        )
        assertNull(secondRelaunch.state.value.pendingExecutionDeferIntent)
        assertEquals("active", secondRelaunch.state.value.canonicalExecutionSession?.status)
    }

    @Test
    fun delayedRestartPreservesFreshAssessmentInsideOriginalSafetyMargin() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(10 * 60L)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request ->
            warningAssessment(transport, request).copy(
                expiresAt = NOW.plusSeconds(5 * 60L).toString(),
            )
        }
        val initialManager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, initialManager.refresh())
        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            initialManager.doLater(BLOCK_ID, moveStart),
        )
        val persisted = requireNotNull(store.state.value.pendingExecutionDeferIntent)
        val assessment = requireNotNull(persisted.assessment)
        val delayedNow = NOW.plusSeconds(4 * 60L)
        val relaunched = PlannerStore(
            store.state.value,
            nowEpochMillis = { delayedNow.toEpochMilli() },
        )
        val deferred = paused.copy(
            status = "deferred",
            revision = 3,
            actualSeconds = assessment.actualSeconds,
            pauseUntil = null,
            pauseReason = null,
            moveStart = assessment.moveStart,
            moveEnd = assessment.moveEnd,
            endedAt = delayedNow.toString(),
            updatedAt = delayedNow.toString(),
        )
        transport.commandHandler = { _, _ ->
            transport.snapshotResult = RemoteExecutionSnapshot(3, null)
            transport.historyResult = listOf(deferred)
            RemoteExecutionMutation(3, null, deferred, replayed = false)
        }
        val relaunchedManager = manager(relaunched, transport, currentNow = { delayedNow })

        assertEquals(ExecutionSyncOutcome.APPROVAL_REQUIRED, relaunchedManager.refresh())
        assertEquals(assessment, relaunched.state.value.pendingExecutionDeferIntent?.assessment)
        assertEquals(1, transport.assessmentRequests.size)
        assertEquals(
            ExecutionSyncOutcome.SUCCESS,
            relaunchedManager.approveDefer(assessment.assessmentDigest),
        )
        assertEquals(1, transport.assessmentRequests.size)
        assertNull(relaunched.state.value.pendingExecutionDeferIntent)
        assertEquals("deferred", relaunched.state.value.terminalExecutionOutcomes
            .getValue(SESSION_ID).session.status)
    }

    @Test
    fun assessmentExpiringAfterEntryClearsUnsafeTargetWithoutRetryLockout() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(10 * 60L)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request ->
            warningAssessment(transport, request).copy(
                expiresAt = NOW.plusSeconds(5 * 60L).toString(),
            )
        }
        val initialManager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, initialManager.refresh())
        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            initialManager.doLater(BLOCK_ID, moveStart),
        )
        val assessment = requireNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
        val beforeExpiry = NOW.plusSeconds(4 * 60L)
        val afterExpiry = NOW.plusSeconds(5 * 60L + 1)
        val relaunched = PlannerStore(
            store.state.value,
            nowEpochMillis = { beforeExpiry.toEpochMilli() },
        )
        val clockCalls = AtomicInteger()
        val expiringManager = manager(
            relaunched,
            transport,
            currentNow = {
                if (clockCalls.incrementAndGet() <= 2) beforeExpiry else afterExpiry
            },
        )

        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            expiringManager.approveDefer(assessment.assessmentDigest),
        )
        assertNull(relaunched.state.value.pendingExecutionDeferIntent)
        assertNull(relaunched.state.value.pendingExecutionCommand)
        assertEquals("paused", relaunched.state.value.canonicalExecutionSession?.status)
        assertTrue(transport.commandBodies.isEmpty())
        assertEquals(1, transport.assessmentRequests.size)
        assertTrue(expiringManager.state.value.message.contains("at least ten minutes"))
        val secondRelaunch = PlannerStore(
            relaunched.state.value,
            nowEpochMillis = { afterExpiry.toEpochMilli() },
        )
        assertNull(secondRelaunch.state.value.pendingExecutionDeferIntent)
        assertEquals("paused", secondRelaunch.state.value.canonicalExecutionSession?.status)
    }

    @Test
    fun republishedActiveSourceAcceptsAssessmentBoundToImmutableOriginRevision() = runBlocking {
        val original = plannerStore()
        val republishedId = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        val republishedRevision = requireNotNull(original.state.value.publishedScheduleRevision).copy(
            id = republishedId,
            revision = "2:$republishedId",
            revisionNumber = 2uL,
            inputDigest = "sha256:${"f".repeat(64)}",
        )
        val store = PlannerStore(
            original.state.value.copy(
                publishedScheduleRevision = republishedRevision,
                publishedScheduleProof = requireNotNull(
                    original.state.value.publishedScheduleProof,
                ).copy(revision = republishedRevision),
                publishedScheduleRevisionHint = requireNotNull(
                    original.state.value.publishedScheduleRevisionHint,
                ).copy(revisionNumber = republishedRevision.revisionNumber),
                scheduleInputDigest = republishedRevision.inputDigest,
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(3_600)
        var assessedSourceRevisionId: String? = null
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
            assessmentHandler = { request ->
                cleanAssessment(request).also { assessment ->
                    assessedSourceRevisionId = assessment.sourceScheduleRevisionId
                }
            }
        }
        transport.commandHandler = { _, body ->
            val command = Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
            assertEquals("defer", command.getValue("type").jsonPrimitive.content)
            val assessment = transport.cleanAssessment(transport.assessmentRequests.single())
            val deferred = paused.copy(
                status = "deferred",
                revision = 3,
                actualSeconds = assessment.actualSeconds,
                pauseUntil = null,
                moveStart = assessment.moveStart,
                moveEnd = assessment.moveEnd,
                endedAt = NOW.toString(),
                updatedAt = NOW.toString(),
            )
            transport.snapshotResult = RemoteExecutionSnapshot(3, null)
            transport.historyResult = listOf(deferred)
            RemoteExecutionMutation(3, null, deferred, replayed = false)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.doLater(BLOCK_ID, moveStart))
        assertEquals(PUBLISHED_REVISION_ID, assessedSourceRevisionId)
        assertNull(store.state.value.pendingExecutionDeferIntent)
    }

    @Test
    fun serverAssessmentSupersedesChangedLocalConflictEstimate() = runBlocking {
        val moveStart = NOW.plusSeconds(3_600)
        fun hardBlock(id: String, title: String, minuteOffset: Long) = ScheduleItem(
            id = id,
            title = title,
            kind = ItemKind.EVENT,
            startMinute = 10 * 60,
            durationMinutes = 10,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            absoluteStartAt = moveStart.plusSeconds(minuteOffset * 60L).toString(),
            absoluteEndAt = moveStart.plusSeconds((minuteOffset + 10L) * 60L).toString(),
            planningZoneId = "Europe/Madrid",
            canonicalBlockKind = "external_fixed",
            sessionIndex = 0,
        )
        val reviewedConflict = hardBlock(
            "88888888-8888-4888-8888-888888888888",
            "Reviewed conflict",
            5,
        )
        val reviewedStore = PlannerStore(
            plannerStore().state.value.let { state ->
                state.withPublishedSchedule(state.schedule + reviewedConflict)
            },
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val active = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, active)
            historyResult = listOf(active)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(reviewedStore, transport).refresh())
        val newlyArrivedConflict = hardBlock(
            "99999999-9999-4999-8999-999999999999",
            "New conflict",
            20,
        )
        val mutationStore = PlannerStore(
            reviewedStore.state.value.let { state ->
                state.withPublishedSchedule(state.schedule + newlyArrivedConflict)
            },
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val paused = active.copy(
            status = "paused",
            revision = 2,
            runningSince = null,
            pausedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        transport.commandHandler = { _, _ ->
            transport.snapshotResult = RemoteExecutionSnapshot(2, paused)
            transport.historyResult = listOf(paused)
            RemoteExecutionMutation(2, paused, paused, replayed = false)
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val mutationManager = manager(mutationStore, transport)

        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            mutationManager.doLater(BLOCK_ID, moveStart),
        )

        assertEquals(1, transport.commandBodies.size)
        assertNotNull(mutationStore.state.value.pendingExecutionDeferIntent?.assessment)
        assertNull(
            mutationStore.state.value.pendingExecutionDeferIntent?.approvedAssessmentDigest,
        )
        assertEquals("paused", mutationStore.state.value.canonicalExecutionSession?.status)
    }

    @Test
    fun activeCanonicalDeferRequiresApprovalBeforeOverlappingHardTime() = runBlocking {
        val original = plannerStore()
        val hardBlock = ScheduleItem(
            id = "99999999-9999-4999-8999-999999999999",
            title = "Fixed appointment",
            kind = ItemKind.EVENT,
            startMinute = 10 * 60 + 10,
            durationMinutes = 20,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            absoluteStartAt = "2026-09-01T08:10:00Z",
            absoluteEndAt = "2026-09-01T08:30:00Z",
            planningZoneId = "Europe/Madrid",
            canonicalBlockKind = "external_fixed",
            sessionIndex = 0,
        )
        val store = PlannerStore(
            original.state.value.withPublishedSchedule(
                original.state.value.schedule + hardBlock,
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val running = activeSession(SESSION_ID)
        val paused = running.copy(
            status = "paused",
            revision = 2,
            runningSince = null,
            pausedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
            commandHandler = { _, _ ->
                snapshotResult = RemoteExecutionSnapshot(2, paused)
                historyResult = listOf(paused)
                RemoteExecutionMutation(2, paused, paused, replayed = false)
            }
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, NOW.plusSeconds(3_600)),
        )

        assertEquals(1, transport.commandBodies.size)
        assertNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
        assertEquals("paused", store.state.value.canonicalExecutionSession?.status)
    }

    @Test
    fun conflictedAssessmentRestoresThenApprovalStagesItsExactServerWindow() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(3_600)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, moveStart),
        )
        val persisted = requireNotNull(store.state.value.pendingExecutionDeferIntent)
        val assessment = requireNotNull(persisted.assessment)
        assertNull(persisted.approvedAssessmentDigest)
        assertEquals(120L, assessment.actualSeconds)
        assertEquals(moveStart.plusSeconds(1_680).toString(), assessment.moveEnd)

        val restored = PlannerStore(
            store.state.value,
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        assertEquals(assessment, restored.state.value.pendingExecutionDeferIntent?.assessment)
        val deferred = paused.copy(
            status = "deferred",
            revision = 3,
            actualSeconds = assessment.actualSeconds,
            pauseUntil = null,
            pauseReason = null,
            moveStart = assessment.moveStart,
            moveEnd = assessment.moveEnd,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        transport.commandHandler = { key, body ->
            val journal = requireNotNull(restored.state.value.pendingExecutionCommand)
            assertEquals(key, journal.idempotencyKey)
            assertEquals(body, journal.requestJson)
            assertEquals(
                assessment.assessmentDigest,
                restored.state.value.pendingExecutionDeferIntent?.approvedAssessmentDigest,
            )
            val command = Json.parseToJsonElement(body).jsonObject
                .getValue("command").jsonObject
            assertEquals(assessment.moveEnd, command.getValue("move_end").jsonPrimitive.content)
            assertEquals(
                assessment.actualSeconds,
                command.getValue("actual_seconds").jsonPrimitive.long,
            )
            assertEquals(
                assessment.assessmentDigest,
                command.getValue("assessment_digest").jsonPrimitive.content,
            )
            assertEquals(
                assessment.assessmentDigest,
                command.getValue("approved_assessment_digest").jsonPrimitive.content,
            )
            transport.snapshotResult = RemoteExecutionSnapshot(3, null)
            transport.historyResult = listOf(deferred)
            RemoteExecutionMutation(3, null, deferred, replayed = false)
        }

        assertEquals(
            ExecutionSyncOutcome.SUCCESS,
            manager(restored, transport).approveDefer(assessment.assessmentDigest),
        )
        assertNull(restored.state.value.pendingExecutionCommand)
        assertNull(restored.state.value.pendingExecutionDeferIntent)
        assertEquals(
            assessment.moveEnd,
            restored.state.value.terminalExecutionOutcomes[SESSION_ID]?.session?.moveEnd,
        )
    }

    @Test
    fun replacementAssessmentNeverInheritsApprovalFromOlderDigest() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, NOW.plusSeconds(3_600)),
        )
        val first = requireNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
        assertTrue(
            requireNotNull(
                store.approveExecutionDeferAssessment(SESSION_ID, first.assessmentDigest),
            ).awaitDurable(),
        )
        assertEquals(
            first.assessmentDigest,
            store.state.value.pendingExecutionDeferIntent?.approvedAssessmentDigest,
        )
        assertTrue(
            requireNotNull(
                store.clearExecutionDeferAssessment(SESSION_ID, "synthetic stale evidence"),
            ).awaitDurable(),
        )
        val replacement = first.copy(
            environmentDigest = "sha256:${"d".repeat(64)}",
            assessmentDigest = "sha256:${"e".repeat(64)}",
        )
        assertTrue(
            requireNotNull(
                store.recordExecutionDeferAssessment(SESSION_ID, replacement),
            ).awaitDurable(),
        )

        assertEquals(replacement, store.state.value.pendingExecutionDeferIntent?.assessment)
        assertNull(store.state.value.pendingExecutionDeferIntent?.approvedAssessmentDigest)
    }

    @Test
    fun newerPausedRevisionClearsApprovalAndRequestsFreshAssessmentForSavedTarget() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(3_600)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, moveStart),
        )
        val first = requireNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
        assertTrue(
            requireNotNull(
                store.approveExecutionDeferAssessment(SESSION_ID, first.assessmentDigest),
            ).awaitDurable(),
        )
        val newerPaused = paused.copy(
            revision = 3,
            updatedAt = NOW.plusSeconds(1).toString(),
        )
        transport.snapshotResult = RemoteExecutionSnapshot(3, newerPaused)
        transport.historyResult = listOf(newerPaused)
        transport.assessmentHandler = { request ->
            warningAssessment(transport, request).copy(
                environmentDigest = "sha256:${"d".repeat(64)}",
                assessmentDigest = "sha256:${"e".repeat(64)}",
            )
        }

        assertEquals(ExecutionSyncOutcome.APPROVAL_REQUIRED, manager.refresh())

        val refreshed = requireNotNull(store.state.value.pendingExecutionDeferIntent)
        assertEquals(moveStart.toString(), refreshed.moveStart)
        assertEquals("sha256:${"e".repeat(64)}", refreshed.assessment?.assessmentDigest)
        assertNull(refreshed.approvedAssessmentDigest)
        assertEquals(listOf(2L, 3L), transport.assessmentRequests.map { it.expectedRevision })
        assertEquals("paused", store.state.value.canonicalExecutionSession?.status)
    }

    @Test
    fun exactJournalReplaysUnchangedAfterAssessmentExpiry() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(3_600)
        val clock = AtomicReference(NOW)
        var attempts = 0
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        transport.assessmentHandler = { request ->
            transport.cleanAssessment(request).copy(expiresAt = NOW.plusSeconds(5).toString())
        }
        val assessmentEnd = moveStart.plusSeconds(1_680).toString()
        val deferred = paused.copy(
            status = "deferred",
            revision = 3,
            actualSeconds = 120,
            pauseUntil = null,
            pauseReason = null,
            moveStart = moveStart.toString(),
            moveEnd = assessmentEnd,
            endedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        transport.commandHandler = { _, _ ->
            attempts += 1
            transport.snapshotResult = RemoteExecutionSnapshot(3, null)
            transport.historyResult = listOf(deferred)
            if (attempts == 1) throw IOException("synthetic lost defer response")
            RemoteExecutionMutation(3, null, deferred, replayed = true)
        }
        val manager = manager(store, transport, currentNow = clock::get)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.doLater(BLOCK_ID, moveStart),
        )
        val pending = requireNotNull(store.state.value.pendingExecutionCommand)
        assertEquals(1, transport.assessmentRequests.size)
        clock.set(NOW.plusSeconds(10))
        val restored = PlannerStore(
            store.state.value,
            nowEpochMillis = { clock.get().toEpochMilli() },
        )
        assertNull(restored.state.value.pendingExecutionDeferIntent?.assessment)
        assertEquals(pending.requestJson, restored.state.value.pendingExecutionCommand?.requestJson)

        assertEquals(
            ExecutionSyncOutcome.SUCCESS,
            manager(restored, transport, currentNow = clock::get).refresh(),
        )

        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.commandBodies)
        assertEquals(listOf(pending.idempotencyKey, pending.idempotencyKey), transport.commandKeys)
        assertEquals(1, transport.assessmentRequests.size)
        assertNull(restored.state.value.pendingExecutionCommand)
        assertEquals("deferred", restored.state.value.terminalExecutionOutcomes
            .getValue(SESSION_ID).session.status)
    }

    @Test
    fun exactPauseCannotExpandMoveBeyondTheConflictEnvelopeUserReviewed() = runBlocking {
        val managerNow = NOW.plusSeconds(15 * 60L)
        val moveStart = managerNow.plusSeconds(60 * 60L)
        val original = plannerStore()
        val hardBlock = ScheduleItem(
            id = "99999999-9999-4999-8999-999999999999",
            title = "Fixed appointment",
            kind = ItemKind.EVENT,
            startMinute = 10 * 60 + 35,
            durationMinutes = 10,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            absoluteStartAt = moveStart.plusSeconds(20 * 60L).toString(),
            absoluteEndAt = moveStart.plusSeconds(30 * 60L).toString(),
            planningZoneId = "Europe/Madrid",
            canonicalBlockKind = "external_fixed",
            sessionIndex = 0,
        )
        val store = PlannerStore(
            original.state.value.withPublishedSchedule(
                original.state.value.schedule + hardBlock,
            ),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val running = activeSession(SESSION_ID)
        val paused = running.copy(
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            runningSince = null,
            pausedAt = managerNow.toString(),
            updatedAt = managerNow.toString(),
        )
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
            commandHandler = { _, body ->
                val command = Json.parseToJsonElement(body).jsonObject
                    .getValue("command").jsonObject
                assertEquals("pause", command.getValue("type").jsonPrimitive.content)
                snapshotResult = RemoteExecutionSnapshot(2, paused)
                historyResult = listOf(paused)
                RemoteExecutionMutation(2, paused, paused, replayed = false)
            }
        }
        transport.assessmentHandler = { request -> warningAssessment(transport, request) }
        val manager = manager(store, transport, currentNow = { managerNow })
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        // The wall-clock estimate ends before the appointment, but the exact server Pause proves
        // 30 minutes remain and would overlap it. A Defer may not silently broaden the warning.
        assertEquals(
            ExecutionSyncOutcome.APPROVAL_REQUIRED,
            manager.doLater(BLOCK_ID, moveStart),
        )

        assertEquals(listOf("pause"), transport.commandBodies.map { body ->
            Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
                .getValue("type").jsonPrimitive.content
        })
        assertEquals("paused", store.state.value.canonicalExecutionSession?.status)
        assertNotNull(store.state.value.pendingExecutionDeferIntent?.assessment)
        assertNull(store.state.value.pendingExecutionDeferIntent?.approvedAssessmentDigest)
    }

    @Test
    fun invalidPublishedDurationIsRejectedBeforeTheActiveLeaseIsPaused() = runBlocking {
        val original = plannerStore()
        val running = activeSession(SESSION_ID)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
        }
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(original, transport).refresh())
        val malformed = PlannerStore(
            original.state.value.copy(
                schedule = original.state.value.schedule.map { block ->
                    block.copy(absoluteEndAt = block.absoluteStartAt)
                },
            ),
        )

        assertEquals(
            ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            manager(malformed, transport).doLater(BLOCK_ID, NOW.plusSeconds(3_600)),
        )

        assertTrue(transport.commandBodies.isEmpty())
        assertEquals("active", malformed.state.value.canonicalExecutionSession?.status)
        assertNull(malformed.state.value.pendingExecutionDeferIntent)
    }

    @Test
    fun lostPauseResponseRetainsMoveIntentAndRelaunchCompletesExactDefer() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val paused = running.copy(
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            runningSince = null,
            pausedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        val moveStart = NOW.plusSeconds(3_600)
        val moveEnd = moveStart.plusSeconds(1_800)
        var firstPauseResponse = true
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
            commandHandler = { _, body ->
                val type = Json.parseToJsonElement(body).jsonObject.getValue("command")
                    .jsonObject.getValue("type").jsonPrimitive.content
                when (type) {
                    "pause" -> {
                        snapshotResult = RemoteExecutionSnapshot(2, paused)
                        historyResult = listOf(paused)
                        if (firstPauseResponse) {
                            firstPauseResponse = false
                            throw IOException("synthetic lost pause response")
                        }
                        RemoteExecutionMutation(2, paused, paused, replayed = true)
                    }
                    "defer" -> {
                        val deferred = paused.copy(
                            status = "deferred",
                            revision = 3,
                            actualSeconds = 0,
                            moveStart = moveStart.toString(),
                            moveEnd = moveEnd.toString(),
                            endedAt = NOW.toString(),
                            updatedAt = NOW.toString(),
                        )
                        snapshotResult = RemoteExecutionSnapshot(3, null)
                        historyResult = listOf(deferred)
                        RemoteExecutionMutation(3, null, deferred, replayed = false)
                    }
                    else -> error("Unexpected execution command")
                }
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.doLater(BLOCK_ID, moveStart),
        )
        assertEquals(moveStart.toString(), store.state.value.pendingExecutionDeferIntent?.moveStart)
        assertEquals("pause", store.state.value.pendingExecutionCommand?.commandType)

        val relaunched = PlannerStore(
            store.state.value,
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager(relaunched, transport).refresh())

        assertNull(relaunched.state.value.pendingExecutionCommand)
        assertNull(relaunched.state.value.pendingExecutionDeferIntent)
        assertEquals("deferred", relaunched.state.value.terminalExecutionOutcomes
            .getValue(SESSION_ID).session.status)
        assertEquals(listOf("pause", "pause", "defer"), transport.commandBodies.map { body ->
            Json.parseToJsonElement(body).jsonObject.getValue("command").jsonObject
                .getValue("type").jsonPrimitive.content
        })
    }

    @Test
    fun expiredSavedMoveIsClearedAfterPauseRecoveryAndLeavesLeasePaused() = runBlocking {
        val store = plannerStore()
        val running = activeSession(SESSION_ID)
        val paused = running.copy(
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            runningSince = null,
            pausedAt = NOW.toString(),
            updatedAt = NOW.toString(),
        )
        var firstPauseResponse = true
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(1, running)
            historyResult = listOf(running)
            commandHandler = { _, _ ->
                snapshotResult = RemoteExecutionSnapshot(2, paused)
                historyResult = listOf(paused)
                if (firstPauseResponse) {
                    firstPauseResponse = false
                    throw IOException("synthetic lost pause response")
                }
                RemoteExecutionMutation(2, paused, paused, replayed = true)
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        val selected = NOW.plusSeconds(600)
        assertEquals(
            ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.doLater(BLOCK_ID, selected),
        )

        val relaunched = PlannerStore(
            store.state.value,
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        val afterExpiry = manager(
            relaunched,
            transport,
            currentNow = { selected.plusSeconds(1) },
        )
        assertEquals(ExecutionSyncOutcome.INVALID_LOCAL_STATE, afterExpiry.refresh())

        assertNull(relaunched.state.value.pendingExecutionCommand)
        assertNull(relaunched.state.value.pendingExecutionDeferIntent)
        assertEquals("paused", relaunched.state.value.canonicalExecutionSession?.status)
        assertTrue(afterExpiry.state.value.message.contains("choose a new time", ignoreCase = true))
        assertEquals(2, transport.commandBodies.size)
    }

    @Test
    fun pausedCanonicalDeferUsesConfirmedAccumulationAndRejectsMutatedMoveResponse() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val moveStart = NOW.plusSeconds(3 * 3_600)
        val expectedMoveEnd = moveStart.plusSeconds(1_680)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
            commandHandler = { _, body ->
                val command = Json.parseToJsonElement(body).jsonObject
                    .getValue("command").jsonObject
                assertEquals("defer", command.getValue("type").jsonPrimitive.content)
                assertEquals("120", command.getValue("actual_seconds").jsonPrimitive.content)
                assertEquals(
                    expectedMoveEnd.toString(),
                    command.getValue("move_end").jsonPrimitive.content,
                )
                val malformed = paused.copy(
                    status = "deferred",
                    revision = 3,
                    actualSeconds = 120,
                    pauseUntil = null,
                    moveStart = moveStart.plusSeconds(60).toString(),
                    moveEnd = expectedMoveEnd.plusSeconds(60).toString(),
                    endedAt = NOW.toString(),
                    updatedAt = NOW.toString(),
                )
                RemoteExecutionMutation(3, null, malformed, replayed = false)
            }
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())

        assertEquals(
            ExecutionSyncOutcome.PROTOCOL_FAILURE,
            manager.doLater(BLOCK_ID, moveStart),
        )

        assertNotNull(store.state.value.pendingExecutionCommand)
        assertEquals("paused", store.state.value.canonicalExecutionSession?.status)
        assertEquals(1, transport.commandBodies.size)
    }

    @Test
    fun legacyDeferJournalWithoutAssessmentDigestFailsClosedBeforeReplay() = runBlocking {
        val store = plannerStore()
        val paused = pausedSession(
            pauseUntil = NOW.plusSeconds(600).toString(),
            revision = 2,
        )
        val oldMoveStart = NOW.plusSeconds(3_600)
        val oldMoveEnd = oldMoveStart.plusSeconds(1_680)
        val newlySelectedStart = NOW.plusSeconds(3 * 3_600)
        val transport = FakeExecutionTransport().apply {
            snapshotResult = RemoteExecutionSnapshot(2, paused)
            historyResult = listOf(paused)
        }
        val manager = manager(store, transport)
        assertEquals(ExecutionSyncOutcome.SUCCESS, manager.refresh())
        val requestJson = buildJsonObject {
            put("expected_revision", 2)
            put(
                "command",
                buildJsonObject {
                    put("type", "defer")
                    put("session_id", SESSION_ID)
                    put("move_start", oldMoveStart.toString())
                    put("move_end", oldMoveEnd.toString())
                    put("actual_seconds", 120)
                },
            )
        }.toString()
        val legacyPending = PendingExecutionCommand(
            idempotencyKey = "99999999-9999-4999-8999-999999999999",
            syncOrigin = "https://api.example.test/",
            configurationId = DEFAULT_CONFIGURATION_ID,
            expectedRevision = 2,
            sessionId = SESSION_ID,
            itemId = ITEM_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            commandType = "defer",
            requestJson = requestJson,
            focusedBlockId = BLOCK_ID,
            startedAt = NOW.toString(),
        )
        val legacyStore = PlannerStore(
            store.state.value.copy(pendingExecutionCommand = legacyPending),
            nowEpochMillis = { NOW.toEpochMilli() },
        )
        assertEquals(
            ExecutionSyncOutcome.PROTOCOL_FAILURE,
            manager(legacyStore, transport).doLater(BLOCK_ID, newlySelectedStart),
        )

        assertTrue(transport.commandBodies.isEmpty())
        assertEquals(legacyPending, legacyStore.state.value.pendingExecutionCommand)
        assertTrue(legacyStore.state.value.terminalExecutionOutcomes.isEmpty())
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
        assertTrue(state.deferredExecutionRecompositionNeeded())
        assertEquals(deferred.toSnapshot(), state.canonicalExecutionHistoryWindow.single())
        assertEquals(2L, state.canonicalExecutionHistoryWindowRevision)
        assertTrue(state.canonicalExecutionHistoryVerified)
        assertTrue(store.isCanonicalExecutionStartBlocked(BLOCK_ID))
        assertTrue(transport.commandBodies.isEmpty())
        assertFalse(
            state.copy(schedule = emptyList(), publishedScheduleProof = null)
                .deferredExecutionRecompositionNeeded(),
        )

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
            valid.copy(moveEnd = NOW.plusSeconds(120).plusNanos(1).toString()),
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
        currentNow: () -> Instant = { NOW },
        cancelTimedBreakNotification: suspend () -> Boolean = { true },
        reconcileTimedBreakNotification: suspend () -> Unit = {},
    ) = ExecutionSyncManager(
        plannerStore = store,
        credentialStore = credentialStore,
        transport = transport,
        now = currentNow,
        newUuid = UUIDS.iterator().let { iterator -> { iterator.next() } },
        cancelTimedBreakNotification = cancelTimedBreakNotification,
        reconcileTimedBreakNotification = reconcileTimedBreakNotification,
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
                publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(
                    syncOrigin = "https://api.example.test/",
                    configurationId = configurationId,
                    revisionNumber = revision.revisionNumber,
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
        PublishedScheduleBlockProofSnapshot.from(block)

    private fun DayWeaveUiState.withPublishedSchedule(
        publishedSchedule: List<ScheduleItem>,
    ): DayWeaveUiState = copy(
        schedule = publishedSchedule,
        publishedScheduleProof = requireNotNull(publishedScheduleProof).copy(
            blocks = publishedSchedule
                .filter {
                    it.canonicalBlockKind != null &&
                        it.canonicalBlockKind != "remote_execution_lease"
                }
                .map(::publishedBlockProof),
        ),
    )

    private fun warningAssessment(
        transport: FakeExecutionTransport,
        request: DeferAssessmentHttpRequest,
    ): ExecutionDeferAssessmentSnapshot {
        val clean = transport.cleanAssessment(request)
        return clean.copy(
            assessmentDigest = "sha256:${"c".repeat(64)}",
            approvalRequired = true,
            violations = listOf(
                ExecutionDeferViolationSnapshot(
                    code = "outside_availability",
                    itemIds = listOf(ITEM_ID),
                    occurrenceIds = emptyList(),
                    conflictingBlockIds = emptyList(),
                    conflictingBlocks = emptyList(),
                    start = clean.moveStart,
                    end = clean.moveEnd,
                    message = "The requested placement is outside an allowed availability window.",
                ),
            ),
        )
    }

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
    var assessmentHandler: suspend (DeferAssessmentHttpRequest) ->
        ExecutionDeferAssessmentSnapshot = ::cleanAssessment

    fun cleanAssessment(request: DeferAssessmentHttpRequest): ExecutionDeferAssessmentSnapshot {
        val paused = requireNotNull(snapshotResult.activeSession)
        val plannedSeconds = 1_800L
        val creditedSeconds = request.actualSeconds.coerceAtMost(plannedSeconds)
        val remainingSeconds = plannedSeconds - creditedSeconds
        return ExecutionDeferAssessmentSnapshot(
            sessionId = request.sessionId,
            executionRevision = request.expectedRevision,
            sessionRevision = paused.revision,
            itemId = paused.itemId,
            itemRevision = paused.itemRevision,
            occurrenceId = paused.occurrenceId,
            sourceSessionIndex = paused.sessionIndex,
            replacementSessionIndex = paused.sessionIndex + 1,
            sourceScheduleRevisionId = "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            sourceBlockId = requireNotNull(paused.plannedBlockId),
            actualSeconds = request.actualSeconds,
            creditedSourceSeconds = creditedSeconds,
            plannedDurationSeconds = plannedSeconds,
            remainingDurationSeconds = remainingSeconds,
            moveStart = request.moveStart,
            moveEnd = Instant.parse(request.moveStart).plusSeconds(remainingSeconds).toString(),
            environmentDigest = "sha256:${"a".repeat(64)}",
            assessmentDigest = "sha256:${"b".repeat(64)}",
            approvalRequired = false,
            violations = emptyList(),
            expiresAt = Instant.parse(request.moveStart).minusSeconds(1).toString(),
        )
    }
    var historyResult: List<RemoteExecutionSession>? = null
    var historyError: Throwable? = null
    var historyHandler: (suspend (Int) -> List<RemoteExecutionSession>)? = null
    var historyPageHandler: (suspend (Int, Long) -> RemoteExecutionHistoryPage)? = null
    val commandKeys = mutableListOf<String>()
    val commandBodies = mutableListOf<String>()
    val assessmentRequests = mutableListOf<DeferAssessmentHttpRequest>()
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

    override suspend fun assessDefer(
        configuration: AuthenticatedApiConfiguration,
        request: DeferAssessmentHttpRequest,
    ): ExecutionDeferAssessmentSnapshot {
        assessmentRequests += request
        return assessmentHandler(request)
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
