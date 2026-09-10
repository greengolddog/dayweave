package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitMissedExplicitActionSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.planningTestReadyState
import com.greengolddog.dayweave.model.planningDisplayCapsule
import com.greengolddog.dayweave.model.planningDisplaySnapshot
import com.greengolddog.dayweave.model.PLANNING_DISPLAY_NOW
import com.greengolddog.dayweave.model.routinePlanningStableInputFingerprint
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.HabitApiException
import com.greengolddog.dayweave.network.HabitTransport
import com.greengolddog.dayweave.network.RemoteHabitAnalytics
import com.greengolddog.dayweave.network.RemoteHabitAnalyticsBucket
import com.greengolddog.dayweave.network.RemoteHabitDeltaPage
import com.greengolddog.dayweave.network.RemoteHabitDeltaChange
import com.greengolddog.dayweave.network.RemoteHabitMissedReconcilePage
import com.greengolddog.dayweave.network.RemoteHabitMissedCancellationReason
import com.greengolddog.dayweave.network.RemoteHabitMissedPolicy
import com.greengolddog.dayweave.network.RemoteHabitMissedResolution
import com.greengolddog.dayweave.network.RemoteHabitMissedResolutionAction
import com.greengolddog.dayweave.network.RemoteHabitMissedResumeAction
import com.greengolddog.dayweave.network.RemoteHabitMutation
import com.greengolddog.dayweave.network.RemoteHabitOccurrence
import com.greengolddog.dayweave.network.RemoteHabitOccurrenceEvidence
import com.greengolddog.dayweave.network.RemoteHabitOccurrencePage
import com.greengolddog.dayweave.network.RemoteHabitOutcome
import com.greengolddog.dayweave.network.RemoteHabitOutcomeStatus
import com.greengolddog.dayweave.network.RemoteHabitPause
import com.greengolddog.dayweave.network.RemoteHabitSupportiveFactCode
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class HabitSyncManagerTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test
    fun rejectedAuthoritativeReadsWithdrawEncryptedCheckpointAcrossRestartWithoutDiscardingFixedInputs(): Unit = runBlocking {
        for (variant in listOf("decoder", "cursor", "immutable-evidence")) {
            val base = fixedHabitInputState()
            val capsule = requireNotNull(base.routinePlanningInputCapsule)
            val display = planningDisplaySnapshot(capsule)
            val initial = base.copy(routinePlanningDisplaySnapshot = display)
            val directory = temporary.root.toPath().resolve(variant)
            fun repository(prepare: Boolean) = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
                NativeConvergenceDisk(directory, "synthetic-habit-withdrawal", prepare))) { displayNowMillis }
            val persistence = repository(true)
            persistence.save(initial)
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val store = PlannerStore(initial, persistence, scope, nowEpochMillis = { displayNowMillis })
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
                val fence = requireNotNull(store.captureRoutinePlanningInputFence())
                assertTrue(store.admitRoutinePlanningDisplay(fence, display) { true })
                val before = store.state.value
                val transport = FakeHabitTransport().apply {
                    deltaHandler = { cursor ->
                        when (variant) {
                            "decoder" -> throw HabitApiException.InvalidResponse()
                            "cursor" -> RemoteHabitDeltaPage(emptyList(), requireNotNull(cursor), hasMore = true)
                            else -> RemoteHabitDeltaPage(listOf(RemoteHabitDeltaChange.OccurrenceUpsert(
                                remoteOccurrence().let { it.copy(evidence = it.evidence.copy(sourceItemRevision = 8)) })),
                                "cursor-rejected", hasMore = false)
                        }
                    }
                }
                assertEquals(HabitSyncOutcome.PROTOCOL_FAILURE, manager(store, transport, emptyList()).refresh())
                val after = store.state.value
                assertEquals(before.habitLedger.copy(deltaCaughtUp = false), after.habitLedger)
                assertEquals(capsule, after.routinePlanningInputCapsule)
                assertEquals(display, after.routinePlanningDisplaySnapshot)
                assertNull(after.routinePlanningDisplayAdmission)
                assertTrue(transport.missedReconcileBodies.isEmpty())
                val restored = requireNotNull(repository(false).load())
                assertEquals(after.habitLedger, restored.habitLedger)
                assertEquals(capsule, restored.routinePlanningInputCapsule)
                assertEquals(display, restored.routinePlanningDisplaySnapshot)
                val restarted = PlannerStore(restored, nowEpochMillis = { displayNowMillis })
                assertFalse(capsule.isReusableInput(restarted.state.value, displayNowMillis, true))
                assertNull(restarted.state.value.routinePlanningDisplayAdmission)
            } finally { scope.cancel() }
        }
    }

    @Test
    fun rejectedReadCannotWithdrawACompetingCheckpointOrAnEqualStateAfterABA(): Unit = runBlocking {
        for (aba in listOf(false, true)) {
            val store = PlannerStore(fixedHabitInputState(), nowEpochMillis = { displayNowMillis })
            val before = store.state.value
            var competitor: DayWeaveUiState? = null
            val transport = FakeHabitTransport().apply {
                deltaHandler = {
                    if (aba) {
                        store.navigate(AppDestination.CALENDAR)
                        store.navigate(before.destination)
                    } else {
                        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID,
                            listOf(HabitOccurrenceSnapshot.fromRemote(remoteOccurrence(outcome = completedOutcome()))),
                            emptyList(), "cursor-competitor", hasMore = false)
                    }
                    competitor = store.state.value
                    throw HabitApiException.InvalidResponse()
                }
            }
            assertEquals(HabitSyncOutcome.CONFIGURATION_CHANGED, manager(store, transport, emptyList()).refresh())
            assertSame(competitor, store.state.value)
            assertSame(competitor, store.durableState.value)
            assertTrue(store.state.value.habitLedger.deltaCaughtUp)
            assertTrue(transport.missedReconcileBodies.isEmpty())
        }
    }

    @Test
    fun rejectedReadAfterPrivacyRevocationCannotPersistIntoTheNewRuntimeGeneration(): Unit = runBlocking {
        val store = PlannerStore(fixedHabitInputState(), nowEpochMillis = { displayNowMillis })
        var revoked: DayWeaveUiState? = null
        val durable = store.durableState.value
        val transport = FakeHabitTransport().apply {
            deltaHandler = {
                store.invalidateRoutineOccurrenceAuthority()
                revoked = store.state.value
                throw HabitApiException.InvalidResponse()
            }
        }
        assertEquals(HabitSyncOutcome.CONFIGURATION_CHANGED, manager(store, transport, emptyList()).refresh())
        assertSame(revoked, store.state.value)
        assertSame(durable, store.durableState.value)
        assertTrue(transport.missedReconcileBodies.isEmpty())
    }

    @Test
    fun malformedLateReadAfterCredentialReplacementOrCancellationDoesNotWithdrawOldCustody(): Unit = runBlocking {
        val store = PlannerStore(fixedHabitInputState(), nowEpochMillis = { displayNowMillis })
        val before = store.state.value
        val credentials = GenerationBoundCredentialStore().apply { configurationId = CONFIGURATION_ID }
        val replaced = FakeHabitTransport().apply {
            deltaHandler = {
                credentials.configurationId = "synthetic-replaced-binding"
                throw HabitApiException.InvalidResponse()
            }
        }
        assertEquals(HabitSyncOutcome.CONFIGURATION_CHANGED,
            HabitSyncManager(store, credentials, replaced).refresh())
        assertSame(before, store.state.value)
        assertSame(before, store.durableState.value)

        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val cancelled = FakeHabitTransport().apply {
            deltaHandler = {
                withContext(NonCancellable) { entered.complete(Unit); release.await() }
                throw HabitApiException.InvalidResponse()
            }
        }
        val pending = async { manager(store, cancelled, emptyList()).refresh() }
        withTimeout(3_000) { entered.await() }
        pending.cancel()
        release.complete(Unit)
        withTimeout(3_000) { pending.join() }
        assertTrue(pending.isCancelled)
        assertSame(before, store.state.value)
        assertSame(before, store.durableState.value)
        assertTrue(replaced.missedReconcileBodies.isEmpty())
        assertTrue(cancelled.missedReconcileBodies.isEmpty())
    }

    @Test
    fun failedWithdrawalSaveRevokesRuntimeWithoutReloadRetryOrLosingRetainedBytes(): Unit = runBlocking {
        val base = fixedHabitInputState()
        val capsule = requireNotNull(base.routinePlanningInputCapsule)
        val display = planningDisplaySnapshot(capsule)
        val initial = base.copy(routinePlanningDisplaySnapshot = display)
        var loads = 0
        val attempts = mutableListOf<DayWeaveUiState>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState { loads++; return initial }
            override suspend fun save(state: DayWeaveUiState) {
                attempts += state
                // Refresh first confirms the existing binding with an exact durable save.
                // Fail the withdrawal itself, after the malformed response has been observed.
                if (!state.habitLedger.deltaCaughtUp) throw IOException("Synthetic exact withdrawal storage failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope, nowEpochMillis = { displayNowMillis })
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertTrue(store.admitRoutinePlanningDisplay(requireNotNull(store.captureRoutinePlanningInputFence()), display) { true })
            val transport = FakeHabitTransport().apply { deltaHandler = { throw HabitApiException.InvalidResponse() } }
            assertEquals(HabitSyncOutcome.LOCAL_STORAGE_FAILURE, manager(store, transport, emptyList()).refresh())
            assertEquals(1, loads); assertEquals(2, attempts.size)
            assertEquals(initial.habitLedger, attempts.first().habitLedger)
            assertFalse(attempts.last().habitLedger.deltaCaughtUp)
            assertEquals(listOf("cursor-0"), transport.deltaCursors)
            assertEquals(initial.habitLedger, store.durableState.value?.habitLedger)
            assertEquals(capsule, store.state.value.routinePlanningInputCapsule)
            assertEquals(display, store.state.value.routinePlanningDisplaySnapshot)
            assertNull(store.state.value.routinePlanningDisplayAdmission)
            assertNull(store.captureRoutinePlanningInputFence())
            assertTrue(transport.missedReconcileBodies.isEmpty())
        } finally { scope.cancel() }
    }

    @Test
    fun withdrawalChecksCancellationAgainBeforeItsExactStateMutation() {
        val store = boundStore()
        val before = store.state.value
        val read = store.captureHabitDeltaReadFence(ORIGIN, CONFIGURATION_ID)
        var calls = 0
        assertThrows(IllegalArgumentException::class.java) {
            store.rejectHabitDeltaRead(read) { ++calls < 3 }
        }
        assertEquals(3, calls)
        assertSame(before, store.state.value)
        assertSame(before, store.durableState.value)
    }

    @Test
    fun offlineForegroundReadKeepsTerminalHabitCacheAndSavedRoutineInputWithoutNewMissedJournal() = runBlocking {
        for (ledger in listOf(HabitLedgerSnapshot(syncOrigin = ORIGIN, configurationId = CONFIGURATION_ID,
                deltaCursor = "cursor-0", deltaCaughtUp = true), boundStore().state.value.habitLedger)) {
            val base = planningTestReadyState().let { it.copy(canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = CONFIGURATION_ID,
                routineOccurrenceLedger = it.routineOccurrenceLedger.copy(syncOrigin = ORIGIN, configurationId = CONFIGURATION_ID), habitLedger = ledger) }
            val capsule = planningDisplayCapsule(base)
            val store = PlannerStore(base.copy(routinePlanningInputCapsule = capsule))
            val before = store.state.value
            val fingerprint = before.routinePlanningStableInputFingerprint()
            val transport = FakeHabitTransport().apply { deltaHandler = { throw IOException("Synthetic offline read") } }
            assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager(store, transport, emptyList()).refresh())
            assertEquals(before, store.state.value)
            assertEquals(ledger, store.state.value.habitLedger)
            assertEquals(capsule, store.state.value.routinePlanningInputCapsule)
            assertEquals(fingerprint, store.state.value.routinePlanningStableInputFingerprint())
            assertTrue(transport.missedReconcileBodies.isEmpty())
            assertNull(store.state.value.habitLedger.pendingMissedReconcile)
            assertEquals(listOf("cursor-0"), transport.deltaCursors)
        }
    }
    @Test
    fun outcomeStageIsDurableBeforeSuccessAndDoesNotTouchTheNetwork() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport()
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.stageOutcome(
                habitId = HABIT_ID,
                occurrenceId = OCCURRENCE_ID,
                observedOutcomeRevision = 0,
                outcome = completedInput(),
            ),
        )

        val pending = store.durableState.value?.habitLedger?.pendingMutations?.single()
        assertEquals(OPERATION_ID, pending?.idempotencyKey)
        assertEquals(0L, pending?.expectedRevision)
        assertEquals(emptyList<String>(), transport.outcomeBodies)
    }

    @Test
    fun outcomeStageRejectsTheRevisionThatWasObservedBeforeABackgroundRefresh() = runBlocking {
        val store = boundStore(outcome = completedOutcome())
        val transport = FakeHabitTransport()
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.INVALID_LOCAL_STATE,
            manager.stageOutcome(
                habitId = HABIT_ID,
                occurrenceId = OCCURRENCE_ID,
                observedOutcomeRevision = 0,
                outcome = completedInput(),
            ),
        )

        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertTrue(transport.outcomeBodies.isEmpty())
        assertEquals(
            1L,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).outcome?.revision,
        )
    }

    @Test
    fun ambiguousOutcomeReplaysTheExactDurableBodyAndKey() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport()
        var attempts = 0
        transport.outcomeHandler = { _, _, key, body ->
            attempts += 1
            if (attempts == 1) throw IOException("synthetic response loss")
            assertEquals(OPERATION_ID, key)
            assertTrue(body.contains("\"progress_basis_points\":10000"))
            RemoteHabitMutation(
                remoteOccurrence(outcome = completedOutcome()),
                replayed = true,
            )
        }
        val first = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            first.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        val pending = store.state.value.habitLedger.pendingMutations.single()

        val relaunched = manager(store, transport, emptyList())
        assertEquals(HabitSyncOutcome.SUCCESS, relaunched.refresh())

        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.outcomeBodies)
        assertEquals(listOf(OPERATION_ID, OPERATION_ID), transport.outcomeKeys)
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertEquals(
            HabitOutcomeStatusSnapshot.COMPLETED,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).outcome?.status,
        )
    }

    @Test
    fun outcomeAcknowledgementPreservesAConcurrentMissedResolutionAdvance() = runBlocking {
        val store = boundStore(missedResolution = remoteMissedResolution())
        val advancedMissed = remoteMissedResolution(
            revision = 2,
            action = RemoteHabitMissedResolutionAction.Skip,
            updatedAt = "2026-09-01T09:02:00Z",
        )
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ ->
                RemoteHabitMutation(
                    remoteOccurrence(
                        outcome = completedOutcome(),
                        missedResolution = advancedMissed,
                    ),
                    replayed = false,
                )
            }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )

        val occurrence = store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
        assertEquals(1L, occurrence.outcome?.revision)
        assertEquals(2L, occurrence.missedResolution?.revision)
        assertEquals(
            com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Skip,
            occurrence.missedResolution?.action,
        )
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun deterministicConflictRemainsEncryptedForReviewAndIsNotReplayed() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw HabitApiException.Conflict() }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.CONFLICT,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertEquals(
            PendingHabitMutationDisposition.CONFLICT,
            store.state.value.habitLedger.pendingMutations.single().disposition,
        )
        assertEquals(HabitSyncOutcome.CONFLICT, manager.refresh())
        assertEquals(1, transport.outcomeBodies.size)

        assertEquals(HabitSyncOutcome.SUCCESS, manager.discardReviewedMutation(OPERATION_ID))
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun missedDecisionReplaysExactJournalAndAcceptsMatchingCancelledRace() = runBlocking {
        val store = boundStore(missedResolution = remoteMissedResolution())
        var attempts = 0
        val transport = FakeHabitTransport().apply {
            missedResolutionHandler = { _, _, key, body ->
                attempts += 1
                assertEquals(OPERATION_ID, key)
                assertEquals(
                    """{"operation_id":"$OPERATION_ID","expected_revision":1,"action":"carry"}""",
                    body,
                )
                if (attempts == 1) throw IOException("synthetic response loss")
                RemoteHabitMutation(
                    remoteMissedResolution(
                        revision = 2,
                        action = RemoteHabitMissedResolutionAction.Cancelled(
                            RemoteHabitMissedCancellationReason.SOURCE_COMPLETED,
                            RemoteHabitMissedResumeAction.CARRY,
                        ),
                        updatedAt = "2026-09-01T09:02:00Z",
                    ),
                    replayed = true,
                )
            }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.stageMissedResolution(
                HABIT_ID,
                OCCURRENCE_ID,
                observedMissedRevision = 1,
                action = HabitMissedExplicitActionSnapshot.CARRY,
            ),
        )
        val pending = store.durableState.value?.habitLedger?.pendingMutations?.single()
        assertEquals(OPERATION_ID, pending?.idempotencyKey)
        assertTrue(transport.missedResolutionBodies.isEmpty())
        assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.refresh())

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager(store, transport, emptyList()).refresh(),
        )
        assertEquals(listOf(OPERATION_ID, OPERATION_ID), transport.missedResolutionKeys)
        assertEquals(listOf(pending?.requestJson, pending?.requestJson), transport.missedResolutionBodies)
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        val action = store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
            .missedResolution?.action
        assertTrue(
            action is com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Cancelled,
        )
    }

    @Test
    fun confirmedMissedDecisionStaysDeltaIncompleteWhenFollowingPullFails() = runBlocking {
        val store = boundStore(missedResolution = remoteMissedResolution())
        val resolved = remoteMissedResolution(
            revision = 2,
            action = RemoteHabitMissedResolutionAction.Skip,
            updatedAt = "2026-09-01T09:02:00Z",
        )
        var deltaAttempts = 0
        val transport = FakeHabitTransport().apply {
            missedResolutionHandler = { _, _, _, _ ->
                RemoteHabitMutation(resolved, replayed = false)
            }
            deltaHandler = { cursor ->
                deltaAttempts += 1
                if (deltaAttempts == 1) throw IOException("synthetic post-confirmation loss")
                RemoteHabitDeltaPage(emptyList(), cursor ?: "cursor-0", hasMore = false)
            }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.stageMissedResolution(
                HABIT_ID,
                OCCURRENCE_ID,
                observedMissedRevision = 1,
                action = HabitMissedExplicitActionSnapshot.SKIP,
            ),
        )
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
        assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.refresh())
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
        assertEquals(
            com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Skip,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
                .missedResolution?.action,
        )

        assertEquals(HabitSyncOutcome.SUCCESS, manager.refresh())
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun missedReconcileResponseIsNonAuthoritativeUntilFollowingDeltaPull() = runBlocking {
        val store = boundStore()
        val skipped = remoteMissedResolution(
            configuredPolicy = RemoteHabitMissedPolicy.SKIP,
            action = RemoteHabitMissedResolutionAction.Skip,
        )
        var reconcileAttempt = 0
        var deltaAttempt = 0
        val transport = FakeHabitTransport().apply {
            missedReconcileHandler = { _, _, _ ->
                reconcileAttempt += 1
                if (reconcileAttempt == 1) {
                    RemoteHabitMissedReconcilePage(listOf(skipped), false, replayed = false)
                } else {
                    RemoteHabitMissedReconcilePage(emptyList(), false, replayed = false)
                }
            }
            deltaHandler = delta@ { cursor ->
                if (reconcileAttempt == 0) return@delta RemoteHabitDeltaPage(emptyList(), cursor ?: "cursor-0", hasMore = false)
                deltaAttempt += 1
                if (deltaAttempt == 1) throw IOException("delta unavailable")
                RemoteHabitDeltaPage(
                    changes = listOf(
                        com.greengolddog.dayweave.network.RemoteHabitDeltaChange.OccurrenceUpsert(
                            remoteOccurrence(missedResolution = skipped),
                        ),
                    ),
                    nextCursor = cursor ?: "cursor-0",
                    hasMore = false,
                )
            }
        }
        val manager = manager(
            store,
            transport,
            emptyList(),
            reconciliationUuids = listOf(RECONCILE_ID, SECOND_RECONCILE_ID),
        )

        assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, manager.refresh())
        assertNull(
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).missedResolution,
        )
        assertFalse(requireNotNull(store.durableState.value).habitLedger.deltaCaughtUp)
        assertEquals(HabitSyncOutcome.SUCCESS, manager.refresh())
        assertEquals(
            com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Skip,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
                .missedResolution?.action,
        )
        assertEquals(
            listOf(
                """{"operation_id":"$RECONCILE_ID"}""",
                """{"operation_id":"$SECOND_RECONCILE_ID"}""",
            ),
            transport.missedReconcileBodies,
        )
        assertEquals(listOf(RECONCILE_ID, SECOND_RECONCILE_ID), transport.missedReconcileKeys)
        assertTrue(transport.missedReconcileLimits.all { it == 25 })
    }

    @Test
    fun missedReconcileResponseLossKeepsCheckpointIncompleteUntilNextDeltaConverges() =
        runBlocking {
            val store = boundStore()
            val skipped = remoteMissedResolution(
                configuredPolicy = RemoteHabitMissedPolicy.SKIP,
                action = RemoteHabitMissedResolutionAction.Skip,
            )
            var reconciliationAttempt = 0
            val transport = FakeHabitTransport().apply {
                missedReconcileHandler = { _, _, _ ->
                    reconciliationAttempt += 1
                    if (reconciliationAttempt == 1) {
                        throw IOException("synthetic committed response loss")
                    }
                    RemoteHabitMissedReconcilePage(emptyList(), false, replayed = true)
                }
                deltaHandler = { cursor ->
                    RemoteHabitDeltaPage(
                        changes = listOf(
                            com.greengolddog.dayweave.network.RemoteHabitDeltaChange
                                .OccurrenceUpsert(
                                    remoteOccurrence(missedResolution = skipped),
                                ),
                        ),
                        nextCursor = cursor ?: "cursor-0",
                        hasMore = false,
                    )
                }
            }
            val first = manager(
                store,
                transport,
                emptyList(),
                reconciliationUuids = listOf(RECONCILE_ID),
            )

            assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, first.refresh())
            assertFalse(store.state.value.habitLedger.deltaCaughtUp)
            assertFalse(requireNotNull(store.durableState.value).habitLedger.deltaCaughtUp)
            assertEquals(
                RECONCILE_ID,
                requireNotNull(store.durableState.value).habitLedger
                    .pendingMissedReconcile?.idempotencyKey,
            )

            val relaunched = manager(
                store,
                transport,
                emptyList(),
                reconciliationUuids = listOf(SECOND_RECONCILE_ID),
            )
            assertEquals(HabitSyncOutcome.SUCCESS, relaunched.refresh())
            assertTrue(store.state.value.habitLedger.deltaCaughtUp)
            assertNull(store.state.value.habitLedger.pendingMissedReconcile)
            assertEquals(
                listOf(RECONCILE_ID, RECONCILE_ID, SECOND_RECONCILE_ID),
                transport.missedReconcileKeys,
            )
            assertEquals(
                listOf(
                    """{"operation_id":"$RECONCILE_ID"}""",
                    """{"operation_id":"$RECONCILE_ID"}""",
                    """{"operation_id":"$SECOND_RECONCILE_ID"}""",
                ),
                transport.missedReconcileBodies,
            )
            assertEquals(
                com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Skip,
                store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
                    .missedResolution?.action,
            )
        }

    @Test
    fun expiredAmbiguousMissedReconcileRotatesDurablyThenReplaysTheFreshRequest() = runBlocking {
        val store = boundStore()
        var attempt = 0
        val transport = FakeHabitTransport().apply {
            missedReconcileHandler = { _, _, _ ->
                attempt += 1
                if (attempt <= 2) throw IOException("synthetic committed response loss")
                RemoteHabitMissedReconcilePage(emptyList(), false, replayed = true)
            }
        }
        val first = manager(
            store,
            transport,
            emptyList(),
            times = listOf(NOW),
            reconciliationUuids = listOf(RECONCILE_ID),
        )

        assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, first.refresh())
        assertEquals(
            RECONCILE_ID,
            store.state.value.habitLedger.pendingMissedReconcile?.idempotencyKey,
        )

        val leaseBoundary = NOW.plus(Duration.ofHours(12))
        val rotated = manager(
            store,
            transport,
            emptyList(),
            times = listOf(leaseBoundary),
            reconciliationUuids = listOf(SECOND_RECONCILE_ID),
        )
        assertEquals(HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE, rotated.refresh())
        val freshJournal = requireNotNull(store.durableState.value).habitLedger
            .pendingMissedReconcile
        assertEquals(SECOND_RECONCILE_ID, freshJournal?.idempotencyKey)
        assertEquals(leaseBoundary.toString(), freshJournal?.createdAt)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
        assertFalse(requireNotNull(store.durableState.value).habitLedger.deltaCaughtUp)

        val relaunched = manager(
            store,
            transport,
            emptyList(),
            times = listOf(leaseBoundary.plus(Duration.ofHours(1))),
            reconciliationUuids = listOf(THIRD_RECONCILE_ID),
        )
        assertEquals(HabitSyncOutcome.SUCCESS, relaunched.refresh())
        assertEquals(
            listOf(
                RECONCILE_ID,
                SECOND_RECONCILE_ID,
                SECOND_RECONCILE_ID,
                THIRD_RECONCILE_ID,
            ),
            transport.missedReconcileKeys,
        )
        assertNull(store.state.value.habitLedger.pendingMissedReconcile)
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun cancellationDuringMissedReconcileCannotRestoreStaleCheckpointAuthority() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            missedReconcileHandler = { _, _, _ ->
                throw CancellationException("synthetic cancellation after request admission")
            }
        }
        val manager = manager(
            store,
            transport,
            emptyList(),
            reconciliationUuids = listOf(RECONCILE_ID),
        )
        var cancelled = false

        try {
            manager.refresh()
        } catch (_: CancellationException) {
            cancelled = true
        }

        assertTrue(cancelled)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
        assertFalse(requireNotNull(store.durableState.value).habitLedger.deltaCaughtUp)
        assertEquals(
            RECONCILE_ID,
            requireNotNull(store.durableState.value).habitLedger
                .pendingMissedReconcile?.idempotencyKey,
        )
    }

    @Test
    fun pauseStartAndResumeUseIndependentExactRevisionedCommands() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            startPauseHandler = { _, key, _ ->
                assertEquals(OPERATION_ID, key)
                RemoteHabitMutation(remotePause(revision = 1), replayed = false)
            }
            resumePauseHandler = { _, pauseId, key, _ ->
                assertEquals(PAUSE_ID, pauseId)
                assertEquals(SECOND_OPERATION_ID, key)
                RemoteHabitMutation(
                    remotePause(revision = 2, endedAt = RESUMED_AT),
                    replayed = false,
                )
            }
        }
        val manager = manager(
            store,
            transport,
            listOf(OPERATION_ID, PAUSE_ID, SECOND_OPERATION_ID),
            times = listOf(Instant.parse(PAUSED_AT), Instant.parse(RESUMED_AT)),
        )

        assertEquals(HabitSyncOutcome.SUCCESS, manager.startPause(HABIT_ID))
        assertNull(store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt)
        assertEquals(HabitSyncOutcome.SUCCESS, manager.resumePause(HABIT_ID, PAUSE_ID))
        assertEquals(RESUMED_AT, store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt)
        assertEquals(1, transport.startPauseBodies.size)
        assertEquals(1, transport.resumePauseBodies.size)
        assertTrue(transport.startPauseBodies.single().contains("\"expected_revision\":0"))
        assertTrue(transport.resumePauseBodies.single().contains("\"expected_revision\":1"))
    }

    @Test
    fun paginatedOccurrenceLoadMergesEveryPageWithoutMovingTheDeltaCursor() = runBlocking {
        val store = boundStore()
        val second = remoteOccurrence(id = SECOND_OCCURRENCE_ID)
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                occurrences = listOf(remoteOccurrence()),
                nextCursor = "page-2",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                occurrences = listOf(second),
                nextCursor = null,
                hasMore = false,
            )
        }
        val manager = manager(store, transport, emptyList())

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(setOf(OCCURRENCE_ID, SECOND_OCCURRENCE_ID),
            store.state.value.habitLedger.occurrences.keys)
        assertEquals("cursor-0", store.state.value.habitLedger.deltaCursor)
        assertEquals(listOf(null, "page-2"), transport.occurrenceCursors)
        assertEquals(listOf(25, 25), transport.occurrenceLimits)
    }

    @Test
    fun scopedHabitLoadDrainsWorkspaceDeltaAfterMissedReconciliation() = runBlocking {
        val base = remoteOccurrence(id = SECOND_OCCURRENCE_ID)
        val crossHabitResolution = remoteMissedResolution(
            configuredPolicy = RemoteHabitMissedPolicy.SKIP,
            action = RemoteHabitMissedResolutionAction.Skip,
        ).copy(
            occurrenceEvidenceId = base.evidence.id,
            habitId = OTHER_HABIT_ID,
            sourcePlannerOccurrenceId = base.evidence.plannerOccurrenceId,
        )
        val crossHabit = base.copy(
            evidence = base.evidence.copy(habitId = OTHER_HABIT_ID),
            missedResolution = crossHabitResolution,
        )
        val transport = FakeHabitTransport().apply {
            missedReconcileHandler = { _, _, _ ->
                RemoteHabitMissedReconcilePage(
                    resolutions = listOf(crossHabitResolution),
                    hasMore = false,
                    replayed = false,
                )
            }
            deltaHandler = { _ ->
                RemoteHabitDeltaPage(
                    changes = listOf(
                        com.greengolddog.dayweave.network.RemoteHabitDeltaChange.OccurrenceUpsert(
                            crossHabit,
                        ),
                    ),
                    nextCursor = "cursor-workspace",
                    hasMore = false,
                )
            }
            occurrencePages += RemoteHabitOccurrencePage(
                occurrences = listOf(remoteOccurrence()),
                nextCursor = null,
                hasMore = false,
            )
        }
        val store = boundStore()

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager(store, transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(listOf("cursor-0", "cursor-workspace"), transport.deltaCursors)
        assertEquals(
            com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot.Skip,
            store.state.value.habitLedger.occurrences.getValue(SECOND_OCCURRENCE_ID)
                .missedResolution?.action,
        )
    }

    @Test
    fun occurrencePaginationCanReachATerminalPageBeyondTheOldHundredPageLimit() = runBlocking {
        val transport = FakeHabitTransport().apply {
            repeat(101) { index ->
                occurrencePages += RemoteHabitOccurrencePage(
                    occurrences = emptyList(),
                    nextCursor = if (index == 100) null else "occurrence-page-${index + 1}",
                    hasMore = index < 100,
                )
            }
        }

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager(boundStore(), transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(101, transport.occurrenceCursors.size)
        assertTrue(transport.occurrenceLimits.all { it == 25 })
    }

    @Test
    fun unresolvedHabitWriteBlocksCredentialReplacement() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw IOException("offline") }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertTrue(store.hasCredentialReplacementBlocker())
        assertFalse(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun laterOfflineActionIsDurableBeforeAnOlderAmbiguousWriteIsRetried() = runBlocking {
        val store = boundStore().also { bound ->
            bound.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(HabitOccurrenceSnapshot.fromRemote(remoteOccurrence(SECOND_OCCURRENCE_ID))),
                emptyList(),
                "cursor-1",
            )
        }
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw IOException("offline") }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID, SECOND_OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, SECOND_OCCURRENCE_ID, 0, completedInput()),
        )

        assertEquals(
            setOf(OCCURRENCE_ID, SECOND_OCCURRENCE_ID),
            store.state.value.habitLedger.pendingMutations.mapTo(mutableSetOf()) { it.targetId },
        )
        assertEquals(
            listOf(OPERATION_ID, OPERATION_ID),
            transport.outcomeKeys,
        )
    }

    @Test
    fun rejectedDurableDeltaCursorIsClearedOnceAndReplayedFromGenesis() = runBlocking {
        val store = boundStore()
        var attempts = 0
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                attempts += 1
                if (attempts == 1) {
                    assertEquals("cursor-0", cursor)
                    throw HabitApiException.Validation(400)
                }
                if (attempts == 2) assertNull(cursor) else assertEquals("cursor-repaired", cursor)
                RemoteHabitDeltaPage(emptyList(), "cursor-repaired", hasMore = false)
            }
        }

        assertEquals(HabitSyncOutcome.SUCCESS, manager(store, transport, emptyList()).refresh())

        assertEquals(listOf("cursor-0", null, "cursor-repaired"), transport.deltaCursors)
        assertEquals(listOf(25, 25, 25), transport.deltaLimits)
        assertEquals("cursor-repaired", store.state.value.habitLedger.deltaCursor)
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun deltaPaginationCanReachATerminalPageBeyondTheOldHundredPageLimit() = runBlocking {
        val store = boundStore()
        var page = 0
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                assertEquals("cursor-$page", cursor)
                page += 1
                RemoteHabitDeltaPage(
                    changes = emptyList(),
                    nextCursor = "cursor-$page",
                    hasMore = page < 101,
                )
            }
        }

        assertEquals(HabitSyncOutcome.SUCCESS, manager(store, transport, emptyList()).refresh())
        assertEquals(102, transport.deltaCursors.size)
        assertTrue(transport.deltaLimits.all { it == 25 })
        assertEquals("cursor-102", store.state.value.habitLedger.deltaCursor)
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun intermediateDeltaCheckpointSurvivesPageTwoFailureButIsNotCaughtUp() = runBlocking {
        val store = boundStore()
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
        var failSecondPage = true
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                when (cursor) {
                    "cursor-0" -> RemoteHabitDeltaPage(
                        emptyList(),
                        "cursor-1",
                        hasMore = true,
                    )
                    "cursor-1" -> if (failSecondPage) {
                        throw IOException("synthetic page-two failure")
                    } else {
                        RemoteHabitDeltaPage(emptyList(), "cursor-2", hasMore = false)
                    }
                    else -> error("unexpected cursor")
                }
            }
        }

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        val durableIntermediate = requireNotNull(store.durableState.value)
        assertEquals("cursor-1", durableIntermediate.habitLedger.deltaCursor)
        assertFalse(durableIntermediate.habitLedger.deltaCaughtUp)

        failSecondPage = false
        val relaunchedStore = PlannerStore(durableIntermediate)
        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager(relaunchedStore, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cursor-1", "cursor-1"), transport.deltaCursors)
        assertEquals("cursor-2", relaunchedStore.state.value.habitLedger.deltaCursor)
        assertTrue(relaunchedStore.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun staleMissedDeltaStillValidatesIdentityAndTimestampOrdering() {
        val current = remoteMissedResolution(
            revision = 2,
            action = RemoteHabitMissedResolutionAction.Skip,
            updatedAt = "2026-09-01T09:02:00Z",
        )
        val invalidStaleCoordinates = listOf(
            remoteMissedResolution(
                revision = 1,
                configuredPolicy = RemoteHabitMissedPolicy.SKIP,
                action = RemoteHabitMissedResolutionAction.Skip,
            ),
            remoteMissedResolution(
                revision = 1,
                updatedAt = "2026-09-01T09:03:00Z",
            ),
        )

        invalidStaleCoordinates.forEachIndexed { index, invalid ->
            val store = boundStore(missedResolution = current)
            val before = store.state.value

            assertThrows(IllegalArgumentException::class.java) {
                store.applyHabitDeltaPage(
                    ORIGIN,
                    CONFIGURATION_ID,
                    occurrences = listOf(
                        HabitOccurrenceSnapshot.fromRemote(
                            remoteOccurrence(missedResolution = invalid),
                        ),
                    ),
                    pauses = emptyList(),
                    nextCursor = "invalid-stale-$index",
                    hasMore = false,
                )
            }
            assertEquals(before, store.state.value)
        }
    }

    @Test
    fun occurrencePaginationRejectsOpaqueCursorCyclesBeforeRepeatingARequest() = runBlocking {
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_A",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_B",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_A",
                hasMore = true,
            )
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(boundStore(), transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(listOf(null, "cycle_A", "cycle_B"), transport.occurrenceCursors)
    }

    @Test
    fun deltaPaginationRejectsOpaqueCursorCyclesBeforePersistingTheRepeat() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                RemoteHabitDeltaPage(
                    emptyList(),
                    when (cursor) {
                        "cursor-0" -> "cycle_A"
                        "cycle_A" -> "cycle_B"
                        "cycle_B" -> "cycle_A"
                        else -> error("unexpected cursor")
                    },
                    hasMore = true,
                )
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cycle_A", "cycle_B"), transport.deltaCursors)
        assertEquals("cycle_B", store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun occurrencePaginationRejectsAMissingContinuingCursorBeforeMerging() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence(id = SECOND_OCCURRENCE_ID)),
                nextCursor = null,
                hasMore = true,
            )
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(setOf(OCCURRENCE_ID), store.state.value.habitLedger.occurrences.keys)
    }

    @Test
    fun terminalDeltaCannotMoveTheDurableCursorBackward() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                when (cursor) {
                    "cursor-0" -> RemoteHabitDeltaPage(emptyList(), "cursor-a", hasMore = true)
                    "cursor-a" -> RemoteHabitDeltaPage(emptyList(), "cursor-0", hasMore = false)
                    else -> error("unexpected cursor")
                }
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cursor-a"), transport.deltaCursors)
        assertEquals("cursor-a", store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun analyticsResponseMustMatchTheRequestedIdentityBeforeCaching() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            analyticsHandler = { _, start, end, bucket ->
                remoteAnalytics(
                    habitId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    startDate = start,
                    endDate = end,
                    bucket = bucket,
                )
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refreshAnalytics(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-01"),
                com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot.DAY,
            ),
        )
        assertTrue(store.state.value.habitLedger.analytics.isEmpty())
    }

    @Test
    fun mutationTimeMustUseMicrosecondsAndTheServerWindow() = runBlocking {
        val store = boundStore()
        val manager = manager(store, FakeHabitTransport(), listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.INVALID_LOCAL_STATE,
            manager.recordOutcome(
                HABIT_ID,
                OCCURRENCE_ID,
                0,
                completedInput().copy(occurredAt = "2026-09-01T07:30:00.000000001Z"),
            ),
        )
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    private fun boundStore(
        outcome: RemoteHabitOutcome? = null,
        missedResolution: RemoteHabitMissedResolution? = null,
    ) = PlannerStore(DayWeaveUiState()).also { store ->
        store.bindHabitLedger(ORIGIN, CONFIGURATION_ID)
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(
                HabitOccurrenceSnapshot.fromRemote(
                    remoteOccurrence(outcome = outcome, missedResolution = missedResolution),
                ),
            ),
            emptyList(),
            "cursor-0",
            hasMore = false,
        )
    }

    private val displayNowMillis = Instant.parse(PLANNING_DISPLAY_NOW).toEpochMilli() + 1

    private fun fixedHabitInputState(): DayWeaveUiState {
        val base = planningTestReadyState().let { it.copy(canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = CONFIGURATION_ID,
            routineOccurrenceLedger = it.routineOccurrenceLedger.copy(syncOrigin = ORIGIN, configurationId = CONFIGURATION_ID),
            habitLedger = boundStore().state.value.habitLedger) }
        return base.copy(routinePlanningInputCapsule = planningDisplayCapsule(base))
    }

    private fun manager(
        store: PlannerStore,
        transport: FakeHabitTransport,
        uuids: List<String>,
        times: List<Instant> = listOf(NOW),
        reconciliationUuids: List<String> = emptyList(),
    ): HabitSyncManager {
        val uuidIterator = uuids.iterator()
        val timeIterator = times.iterator()
        val reconciliationIterator = reconciliationUuids.iterator()
        var lastTime = times.last()
        return HabitSyncManager(
            plannerStore = store,
            credentialStore = GenerationBoundCredentialStore().apply {
                configurationId = CONFIGURATION_ID
            },
            transport = transport,
            now = {
                if (timeIterator.hasNext()) lastTime = timeIterator.next()
                lastTime
            },
            newUuid = { UUID.fromString(uuidIterator.next()) },
            newReconciliationUuid = {
                UUID.fromString(
                    if (reconciliationIterator.hasNext()) {
                        reconciliationIterator.next()
                    } else {
                        UUID.randomUUID().toString()
                    },
                )
            },
        )
    }

    private fun completedInput() = HabitOutcomeInputSnapshot(
        status = HabitOutcomeStatusSnapshot.COMPLETED,
        progressBasisPoints = 10_000,
        quantity = 8,
        unit = "pages",
        actualSeconds = 600,
        note = "Finished",
        occurredAt = OCCURRED_AT,
    )

    private fun completedOutcome() = RemoteHabitOutcome(
        revision = 1,
        status = RemoteHabitOutcomeStatus.COMPLETED,
        progressBasisPoints = 10_000,
        quantity = 8,
        unit = "pages",
        actualSeconds = 600,
        note = "Finished",
        occurredAt = OCCURRED_AT,
        updatedAt = UPDATED_AT,
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-01T07:29:00Z")
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "configuration-a"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OTHER_HABIT_ID = "12121212-1212-4212-8212-121212121212"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_OCCURRENCE_ID = "88888888-8888-4888-8888-888888888888"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PAUSE_ID = "55555555-5555-4555-8555-555555555555"
        const val OPERATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SECOND_OPERATION_ID = "77777777-7777-4777-8777-777777777777"
        const val RECONCILE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val SECOND_RECONCILE_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val THIRD_RECONCILE_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val OCCURRED_AT = "2026-09-01T07:30:00Z"
        const val UPDATED_AT = "2026-09-01T07:31:00Z"
        const val PAUSED_AT = "2026-09-02T08:00:00Z"
        const val RESUMED_AT = "2026-09-03T08:00:00Z"
    }
}

private class FakeHabitTransport : HabitTransport {
    val occurrencePages = ArrayDeque<RemoteHabitOccurrencePage>()
    val occurrenceCursors = mutableListOf<String?>()
    val occurrenceLimits = mutableListOf<Int>()
    val outcomeKeys = mutableListOf<String>()
    val outcomeBodies = mutableListOf<String>()
    val startPauseBodies = mutableListOf<String>()
    val resumePauseBodies = mutableListOf<String>()
    val missedReconcileBodies = mutableListOf<String>()
    val missedReconcileKeys = mutableListOf<String>()
    val missedReconcileLimits = mutableListOf<Int>()
    val missedResolutionBodies = mutableListOf<String>()
    val missedResolutionKeys = mutableListOf<String>()
    val deltaCursors = mutableListOf<String?>()
    val deltaLimits = mutableListOf<Int>()

    var outcomeHandler: suspend (String, String, String, String) ->
        RemoteHabitMutation<RemoteHabitOccurrence> = { _, _, _, _ -> error("not configured") }
    var startPauseHandler: suspend (String, String, String) ->
        RemoteHabitMutation<RemoteHabitPause> = { _, _, _ -> error("not configured") }
    var resumePauseHandler: suspend (String, String, String, String) ->
        RemoteHabitMutation<RemoteHabitPause> = { _, _, _, _ -> error("not configured") }
    var missedResolutionHandler: suspend (String, String, String, String) ->
        RemoteHabitMutation<RemoteHabitMissedResolution> =
        { _, _, _, _ -> error("not configured") }
    var missedReconcileHandler: suspend (String, String, Int) ->
        RemoteHabitMissedReconcilePage = { _, _, _ ->
            RemoteHabitMissedReconcilePage(emptyList(), hasMore = false, replayed = false)
        }
    var deltaHandler: suspend (String?) -> RemoteHabitDeltaPage = { cursor ->
        RemoteHabitDeltaPage(emptyList(), cursor ?: "cursor-0", hasMore = false)
    }
    var analyticsHandler: suspend (
        String,
        LocalDate,
        LocalDate,
        RemoteHabitAnalyticsBucket,
    ) -> RemoteHabitAnalytics = { _, _, _, _ -> error("not configured") }

    override suspend fun listOccurrences(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        cursor: String?,
        limit: Int,
    ): RemoteHabitOccurrencePage {
        occurrenceCursors += cursor
        occurrenceLimits += limit
        return occurrencePages.removeFirst()
    }

    override suspend fun putOutcome(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitOccurrence> {
        outcomeKeys += idempotencyKey
        outcomeBodies += requestJson
        return outcomeHandler(habitId, occurrenceId, idempotencyKey, requestJson)
    }

    override suspend fun delta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
        limit: Int,
    ): RemoteHabitDeltaPage {
        deltaCursors += cursor
        deltaLimits += limit
        return deltaHandler(cursor)
    }

    override suspend fun reconcileMissed(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        requestJson: String,
        limit: Int,
    ): RemoteHabitMissedReconcilePage {
        missedReconcileBodies += requestJson
        missedReconcileKeys += idempotencyKey
        missedReconcileLimits += limit
        return missedReconcileHandler(idempotencyKey, requestJson, limit)
    }

    override suspend fun putMissedResolution(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitMissedResolution> {
        missedResolutionBodies += requestJson
        missedResolutionKeys += idempotencyKey
        return missedResolutionHandler(habitId, occurrenceId, idempotencyKey, requestJson)
    }

    override suspend fun startPause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        startPauseBodies += requestJson
        return startPauseHandler(habitId, idempotencyKey, requestJson)
    }

    override suspend fun resumePause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        pauseId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        resumePauseBodies += requestJson
        return resumePauseHandler(habitId, pauseId, idempotencyKey, requestJson)
    }

    override suspend fun analytics(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        bucket: RemoteHabitAnalyticsBucket,
    ): RemoteHabitAnalytics = analyticsHandler(habitId, startDate, endDate, bucket)
}

private fun remoteAnalytics(
    habitId: String,
    startDate: LocalDate,
    endDate: LocalDate,
    bucket: RemoteHabitAnalyticsBucket,
) = RemoteHabitAnalytics(
    habitId = habitId,
    startDate = startDate.toString(),
    endDate = endDate.toString(),
    bucket = bucket,
    expected = 0,
    eligible = 0,
    completed = 0,
    partial = 0,
    skipped = 0,
    missed = 0,
    excused = 0,
    unresolved = 0,
    adherenceBasisPoints = 0,
    actualSecondsTotal = 0,
    quantityTotals = emptyList(),
    currentStreak = 0,
    longestStreak = 0,
    trends = emptyList(),
    supportiveFactCodes = listOf(RemoteHabitSupportiveFactCode.NO_DATA),
)

private fun remoteOccurrence(
    id: String = "22222222-2222-4222-8222-222222222222",
    outcome: RemoteHabitOutcome? = null,
    missedResolution: RemoteHabitMissedResolution? = null,
) = RemoteHabitOccurrence(
    evidence = RemoteHabitOccurrenceEvidence(
        id = id,
        habitId = "11111111-1111-4111-8111-111111111111",
        plannerOccurrenceId = if (id == "22222222-2222-4222-8222-222222222222") {
            "33333333-3333-5333-8333-333333333333"
        } else {
            "99999999-9999-5999-8999-999999999999"
        },
        sourceScheduleRevisionId = "44444444-4444-4444-8444-444444444444",
        sourceItemRevision = 7,
        policyFingerprint = "sha256:${"a".repeat(64)}",
        identity = JsonObject(
            mapOf(
                "type" to JsonPrimitive("calendar_day"),
                "date" to JsonPrimitive("2026-09-01"),
                "bucket_ordinal" to JsonPrimitive(0),
            ),
        ),
        nominalStart = "2026-09-01T07:00:00Z",
        nominalEnd = "2026-09-01T07:30:00Z",
        windowStart = "2026-09-01T06:00:00Z",
        windowEnd = "2026-09-01T09:00:00Z",
        localDate = "2026-09-01",
        timezoneName = "Europe/Paris",
        expectedDurationSeconds = 1_800,
        expectedQuantity = 20,
        expectedUnit = "pages",
    ),
    outcome = outcome,
    missedResolution = missedResolution,
)

private fun remoteMissedResolution(
    revision: Long = 1,
    configuredPolicy: RemoteHabitMissedPolicy = RemoteHabitMissedPolicy.ASK,
    action: RemoteHabitMissedResolutionAction =
        RemoteHabitMissedResolutionAction.DecisionRequired,
    updatedAt: String = "2026-09-01T09:01:00Z",
) = RemoteHabitMissedResolution(
    occurrenceEvidenceId = "22222222-2222-4222-8222-222222222222",
    habitId = "11111111-1111-4111-8111-111111111111",
    sourcePlannerOccurrenceId = "33333333-3333-5333-8333-333333333333",
    revision = revision,
    configuredPolicy = configuredPolicy,
    action = action,
    createdAt = "2026-09-01T09:01:00Z",
    updatedAt = updatedAt,
)

private fun remotePause(
    revision: Long,
    endedAt: String? = null,
) = RemoteHabitPause(
    id = "55555555-5555-4555-8555-555555555555",
    habitId = "11111111-1111-4111-8111-111111111111",
    revision = revision,
    startedAt = "2026-09-02T08:00:00Z",
    endedAt = endedAt,
    preservesStreak = true,
    createdAt = "2026-09-02T08:00:00Z",
    updatedAt = endedAt ?: "2026-09-02T08:00:00Z",
)
