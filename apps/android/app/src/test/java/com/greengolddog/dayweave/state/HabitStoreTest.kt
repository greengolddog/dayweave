package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitMissedCancellationReasonSnapshot
import com.greengolddog.dayweave.model.HabitMissedExplicitActionSnapshot
import com.greengolddog.dayweave.model.HabitMissedPolicySnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionSnapshot
import com.greengolddog.dayweave.model.HabitMissedResolveCommandSnapshot
import com.greengolddog.dayweave.model.HabitMissedResumeActionSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.HabitPauseResumeCommandSnapshot
import com.greengolddog.dayweave.model.HabitPauseSnapshot
import com.greengolddog.dayweave.model.HabitPauseStartCommandSnapshot
import com.greengolddog.dayweave.model.HabitQuantityTotalSnapshot
import com.greengolddog.dayweave.model.HabitSupportiveFactCodeSnapshot
import com.greengolddog.dayweave.model.HabitTrendBucketSnapshot
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipProofSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceStateSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.RecurrenceOutcomeSnapshot
import com.greengolddog.dayweave.model.habitPolicyFingerprintOrNull
import java.time.Instant
import java.time.LocalDate
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitStoreTest {
    @Test
    fun missedDecisionJournalIsExactAndDirectConfirmationPreservesOutcomeCoordinate() {
        val initial = occurrence(
            outcome = completedOutcome(),
            missedResolution = missedResolution(),
        )
        val store = boundStore()
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(initial), emptyList(), "1")
        val command = HabitMissedResolveCommandSnapshot(
            operationId = OPERATION_ID,
            expectedRevision = 1,
            action = HabitMissedExplicitActionSnapshot.CARRY,
        )
        store.stageHabitMutation(
            pending(
                OPERATION_ID,
                PendingHabitMutationKind.MISSED_RESOLUTION,
                OCCURRENCE_ID,
                1,
                command.encoded(),
            ),
        )

        val cancelled = missedResolution(
            revision = 2,
            action = HabitMissedResolutionActionSnapshot.Cancelled(
                HabitMissedCancellationReasonSnapshot.SOURCE_COMPLETED,
                HabitMissedResumeActionSnapshot.CARRY,
            ),
            updatedAt = "2026-09-01T10:00:00Z",
        )
        store.reconcileHabitMissedResolution(OPERATION_ID, cancelled)

        val result = store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
        assertEquals(completedOutcome(), result.outcome)
        assertEquals(cancelled, result.missedResolution)
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun outcomeAcknowledgementMergesIndependentMissedAdvanceAndFailsClosedOnCorruption() {
        val acknowledgedOutcome = HabitOutcomeSnapshot(
            revision = 1,
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressBasisPoints = 10_000,
            quantity = 8,
            unit = "pages",
            actualSeconds = 600,
            note = "Finished",
            occurredAt = "2026-09-01T07:30:00Z",
            updatedAt = "2026-09-01T07:31:00Z",
        )
        fun stagedStore(): PlannerStore = boundStore().also { store ->
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(occurrence(missedResolution = missedResolution())),
                emptyList(),
                "1",
            )
            store.stageHabitMutation(
                pendingOutcome(
                    operationId = OPERATION_ID,
                    expectedRevision = 0,
                    status = HabitOutcomeStatusSnapshot.COMPLETED,
                    progressBasisPoints = 10_000,
                    note = "Finished",
                ),
            )
        }

        val advancedMissed = missedResolution(
            revision = 2,
            action = HabitMissedResolutionActionSnapshot.Skip,
            updatedAt = "2026-09-01T10:00:00Z",
        )
        val accepted = stagedStore()
        accepted.reconcileHabitOccurrence(
            OPERATION_ID,
            occurrence(acknowledgedOutcome, advancedMissed),
        )

        val merged = accepted.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
        assertEquals(acknowledgedOutcome, merged.outcome)
        assertEquals(advancedMissed, merged.missedResolution)
        assertTrue(accepted.state.value.habitLedger.pendingMutations.isEmpty())

        val rejectedMissedCoordinates = listOf(
            // Equal revisions must contain identical decoded content.
            missedResolution(updatedAt = "2026-09-01T09:02:00Z"),
            // ASK cannot remain decision-required on the next server revision.
            missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.DecisionRequired,
                updatedAt = "2026-09-01T10:00:00Z",
            ),
            // A resolution cannot detach from the enclosing occurrence authority.
            advancedMissed.copy(habitId = OTHER_HABIT_ID),
        )
        rejectedMissedCoordinates.forEach { rejectedMissed ->
            val rejected = stagedStore()
            val before = rejected.state.value
            assertThrows(IllegalArgumentException::class.java) {
                rejected.reconcileHabitOccurrence(
                    OPERATION_ID,
                    occurrence(acknowledgedOutcome, rejectedMissed),
                )
            }
            assertEquals(before, rejected.state.value)
            assertEquals(
                OPERATION_ID,
                rejected.state.value.habitLedger.pendingMutations.single().idempotencyKey,
            )
        }
    }

    @Test
    fun occurrenceMergeAdvancesCrossedCoordinatesAndRejectsMutatedAuthority() {
        val store = boundStore()
        val first = occurrence(completedOutcome(), missedResolution())
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(first), emptyList(), "1")
        val outcomeAdvanced = first.copy(outcome = completedOutcome(revision = 2))
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(outcomeAdvanced),
            emptyList(),
            "2",
        )
        val bothAdvanced = outcomeAdvanced.copy(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.Skip,
                updatedAt = "2026-09-01T10:00:00Z",
            ),
        )
        store.mergeHabitOccurrencePage(ORIGIN, CONFIGURATION_ID, HABIT_ID, listOf(bothAdvanced))

        assertThrows(IllegalArgumentException::class.java) {
            store.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    bothAdvanced.copy(
                        missedResolution = bothAdvanced.missedResolution?.copy(
                            configuredPolicy = HabitMissedPolicySnapshot.CARRY,
                            action = HabitMissedResolutionActionSnapshot.Carry(
                                windowStart = "2026-09-01T10:00:00Z",
                                windowEnd = "2026-09-01T11:00:00Z",
                            ),
                        ),
                    ),
                ),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(
                    bothAdvanced.copy(
                        missedResolution = bothAdvanced.missedResolution?.copy(
                            action = HabitMissedResolutionActionSnapshot.ReductionPending,
                        ),
                    ),
                ),
                emptyList(),
                "4",
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    bothAdvanced.copy(
                        missedResolution = bothAdvanced.missedResolution?.copy(
                            revision = 3,
                            createdAt = "2026-09-01T09:02:00Z",
                            updatedAt = "2026-09-01T10:01:00Z",
                        ),
                    ),
                ),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(
                    bothAdvanced.copy(
                        missedResolution = bothAdvanced.missedResolution?.copy(
                            revision = 3,
                            updatedAt = "2026-09-01T09:59:00Z",
                        ),
                    ),
                ),
                emptyList(),
                "5",
            )
        }
        val newerOutcomeWithStaleMissed = bothAdvanced.copy(
            outcome = completedOutcome(revision = 3),
            missedResolution = missedResolution(),
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(newerOutcomeWithStaleMissed),
            emptyList(),
            "6",
        )
        assertEquals(
            completedOutcome(revision = 3),
            store.state.value.habitLedger.occurrences[OCCURRENCE_ID]?.outcome,
        )
        assertEquals(
            bothAdvanced.missedResolution,
            store.state.value.habitLedger.occurrences[OCCURRENCE_ID]?.missedResolution,
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    bothAdvanced.copy(
                        missedResolution = missedResolution(
                            revision = 4,
                            action = HabitMissedResolutionActionSnapshot.DecisionRequired,
                            updatedAt = "2026-09-01T10:01:00Z",
                        ),
                    ),
                ),
            )
        }
        val staleOutcomeWithNewerMissed = bothAdvanced.copy(
            missedResolution = missedResolution(
                revision = 4,
                action = HabitMissedResolutionActionSnapshot.Skip,
                updatedAt = "2026-09-01T10:01:00Z",
            ),
        )
        store.mergeHabitOccurrencePage(
            ORIGIN,
            CONFIGURATION_ID,
            HABIT_ID,
            listOf(staleOutcomeWithNewerMissed),
        )
        val merged = store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID)
        assertEquals(completedOutcome(revision = 3), merged.outcome)
        assertEquals(staleOutcomeWithNewerMissed.missedResolution, merged.missedResolution)

        assertThrows(IllegalArgumentException::class.java) {
            store.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    staleOutcomeWithNewerMissed.copy(
                        missedResolution = missedResolution(
                            revision = 5,
                            action = HabitMissedResolutionActionSnapshot.Skip,
                            updatedAt = "2026-09-01T10:02:00Z",
                        ),
                    ),
                ),
            )
        }

        val parityStore = boundStore()
        val skipAtTwo = occurrence(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.Skip,
                updatedAt = "2026-09-01T10:00:00Z",
            ),
        )
        parityStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(skipAtTwo),
            emptyList(),
            "1",
        )
        assertThrows(IllegalArgumentException::class.java) {
            parityStore.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    skipAtTwo.copy(
                        missedResolution = missedResolution(
                            revision = 5,
                            action = HabitMissedResolutionActionSnapshot.Skip,
                            updatedAt = "2026-09-01T10:03:00Z",
                        ),
                    ),
                ),
            )
        }
        parityStore.mergeHabitOccurrencePage(
            ORIGIN,
            CONFIGURATION_ID,
            HABIT_ID,
            listOf(
                skipAtTwo.copy(
                    missedResolution = missedResolution(
                        revision = 4,
                        action = HabitMissedResolutionActionSnapshot.Skip,
                        updatedAt = "2026-09-01T10:02:00Z",
                    ),
                ),
            ),
        )

        val carryStore = boundStore()
        val carryAtTwo = occurrence(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.Carry(
                    "2026-09-01T10:00:00Z",
                    "2026-09-01T11:00:00Z",
                ),
                updatedAt = "2026-09-01T10:00:00Z",
            ),
        )
        carryStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(carryAtTwo),
            emptyList(),
            "1",
        )
        assertThrows(IllegalArgumentException::class.java) {
            carryStore.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(
                    carryAtTwo.copy(
                        missedResolution = missedResolution(
                            revision = 4,
                            action = HabitMissedResolutionActionSnapshot.DecisionRequired,
                            updatedAt = "2026-09-01T10:02:00Z",
                        ),
                    ),
                ),
            )
        }
        carryStore.mergeHabitOccurrencePage(
            ORIGIN,
            CONFIGURATION_ID,
            HABIT_ID,
            listOf(
                carryAtTwo.copy(
                    missedResolution = missedResolution(
                        revision = 4,
                        action = HabitMissedResolutionActionSnapshot.Carry(
                            "2026-09-01T10:02:00Z",
                            "2026-09-01T11:02:00Z",
                        ),
                        updatedAt = "2026-09-01T10:02:00Z",
                    ),
                ),
            ),
        )
    }

    @Test
    fun habitPolicyFingerprintMatchesServerCanonicalBytesAndIgnoresEditorialChanges() {
        val item = canonicalHabit()
        val expected = "sha256:0269d214d7e721505b580bfe4bb45a3b349701eaec018152fb34b2653033b968"

        assertEquals(expected, item.habitPolicyFingerprintOrNull())
        assertEquals(
            expected,
            item.copy(
                revision = 99,
                title = "A harmless rename",
                importance = 100,
                urgency = 1,
                updatedAt = "2026-09-02T06:00:00Z",
            ).habitPolicyFingerprintOrNull(),
        )
        assertFalse(
            expected == item.copy(
                recurrenceJson = """{"type":"daily","times_per_day":2}""",
            ).habitPolicyFingerprintOrNull(),
        )
        assertNull(item.copy(splitPolicyJson = "{}").habitPolicyFingerprintOrNull())

        val frozenServerVector = item.copy(
            id = "00112233-4455-6677-8899-aabbccddeeff",
            durationSeconds = 2_400,
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1_200,
            durationMaxSeconds = 3_600,
            durationSource = CanonicalDurationSource.USER,
            recurrenceJson =
                """{"type":"custom","rrule":"FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=8"}""",
            flexibleConstraintsJson =
                """{"preserves_streak_when_paused":false,"habit_target":{"unit":"reps","amount":12},"habit_missed_policy":"reduce_frequency","habit_minimum_spacing_minutes":45}""",
            splitPolicyJson =
                """{"maximum_chunk_seconds":1800,"type":"splittable","minimum_chunk_seconds":600}""",
            hasExplicitStructuralMetadata = true,
        )
        assertEquals(
            "sha256:4bfc50898f2b4f24cda17d040b21647e4d5ba5fe7fab7e7409024217c8249ebf",
            frozenServerVector.habitPolicyFingerprintOrNull(),
        )
    }

    @Test
    fun transientCrossCoordinateStatesAreAcceptedAndTerminalOutcomeTakesPrecedence() {
        val store = boundStore()
        val terminalWithActiveResolution = occurrence(
            completedOutcome(),
            missedResolution(),
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(terminalWithActiveResolution),
            emptyList(),
            "1",
        )
        assertEquals(
            ItemStatus.COMPLETED,
            store.state.value.recurrenceOutcomes[PLANNER_OCCURRENCE_ID]?.status,
        )

        val cancelled = missedResolution(
            revision = 2,
            action = HabitMissedResolutionActionSnapshot.Cancelled(
                HabitMissedCancellationReasonSnapshot.SOURCE_COMPLETED,
                HabitMissedResumeActionSnapshot.DECISION_REQUIRED,
            ),
            updatedAt = "2026-09-01T10:00:00Z",
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(terminalWithActiveResolution.copy(missedResolution = cancelled)),
            emptyList(),
            "2",
        )
        val correctedUnresolved = terminalWithActiveResolution.copy(
            outcome = HabitOutcomeSnapshot(
                revision = 2,
                status = HabitOutcomeStatusSnapshot.UNRESOLVED,
                progressBasisPoints = 0,
                quantity = null,
                unit = null,
                actualSeconds = null,
                note = null,
                occurredAt = "2026-09-01T10:01:00Z",
                updatedAt = "2026-09-01T10:01:00Z",
            ),
            missedResolution = cancelled,
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(correctedUnresolved),
            emptyList(),
            "3",
        )
        assertFalse(PLANNER_OCCURRENCE_ID in store.state.value.recurrenceOutcomes)
    }

    @Test
    fun effectiveMissedSkipAndReductionProjectOnlyWithoutTerminalOutcomeOrPause() {
        val skipStore = boundStore()
        val skipped = occurrence(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.Skip,
                updatedAt = "2026-09-01T09:02:00Z",
            ),
        )
        skipStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(skipped),
            emptyList(),
            "1",
        )
        assertEquals(
            ItemStatus.SKIPPED,
            skipStore.state.value.recurrenceOutcomes[PLANNER_OCCURRENCE_ID]?.status,
        )

        val harmlessEditStore = boundStore(
            canonicalHabit().copy(
                revision = 8,
                title = "Read something",
                importance = 90,
                updatedAt = "2026-09-01T06:01:00Z",
            ),
        )
        harmlessEditStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(skipped),
            emptyList(),
            "1",
        )
        assertEquals(
            ItemStatus.SKIPPED,
            harmlessEditStore.state.value.recurrenceOutcomes[PLANNER_OCCURRENCE_ID]?.status,
        )
        val changedPolicyStore = boundStore(
            canonicalHabit().copy(
                revision = 8,
                recurrenceJson = """{"type":"daily","times_per_day":2}""",
                updatedAt = "2026-09-01T06:01:00Z",
            ),
        )
        changedPolicyStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(skipped),
            emptyList(),
            "1",
        )
        assertFalse(PLANNER_OCCURRENCE_ID in changedPolicyStore.state.value.recurrenceOutcomes)

        val reduced = occurrence(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.ReduceFrequency(
                    listOf(REDUCED_PLANNER_OCCURRENCE_ID),
                ),
                updatedAt = "2026-09-01T09:03:00Z",
            ),
        )
        val target = reducedTargetOccurrence()
        val missingTargetStore = boundStore()
        missingTargetStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(reduced),
            emptyList(),
            "1",
        )
        assertFalse(
            REDUCED_PLANNER_OCCURRENCE_ID in
                missingTargetStore.state.value.recurrenceOutcomes,
        )
        val reduceStore = boundStore(publishedOccurrences = listOf(reduced, target))
        reduceStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(reduced, target),
            emptyList(),
            "1",
        )
        assertFalse(PLANNER_OCCURRENCE_ID in reduceStore.state.value.recurrenceOutcomes)
        assertEquals(
            ItemStatus.SKIPPED,
            reduceStore.state.value.recurrenceOutcomes[REDUCED_PLANNER_OCCURRENCE_ID]?.status,
        )

        val targetPartial = HabitOutcomeSnapshot(
            revision = 1,
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressBasisPoints = 5_000,
            quantity = 10,
            unit = "pages",
            actualSeconds = 900,
            note = null,
            occurredAt = "2026-09-02T07:30:00Z",
            updatedAt = "2026-09-02T07:31:00Z",
        )
        reduceStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(target.copy(outcome = targetPartial)),
            emptyList(),
            "2",
        )
        assertFalse(REDUCED_PLANNER_OCCURRENCE_ID in reduceStore.state.value.recurrenceOutcomes)

        reduceStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(target.copy(outcome = completedOutcome(revision = 2).copy(
                occurredAt = "2026-09-02T07:30:00Z",
                updatedAt = "2026-09-02T07:31:00Z",
            ))),
            emptyList(),
            "3",
        )
        assertEquals(
            ItemStatus.COMPLETED,
            reduceStore.state.value.recurrenceOutcomes[REDUCED_PLANNER_OCCURRENCE_ID]?.status,
        )

        val pausedTargetStore = boundStore(publishedOccurrences = listOf(reduced, target))
        pausedTargetStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(reduced, target),
            listOf(pause()),
            "1",
        )
        assertFalse(
            REDUCED_PLANNER_OCCURRENCE_ID in pausedTargetStore.state.value.recurrenceOutcomes,
        )
    }

    @Test
    fun inactiveReductionSourceCannotMaskTargetsOwnMissedAction() {
        val reduction = occurrence(
            missedResolution = missedResolution(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.ReduceFrequency(
                    listOf(REDUCED_PLANNER_OCCURRENCE_ID),
                ),
                updatedAt = "2026-09-01T09:03:00Z",
            ),
        )
        val targetResolutionUpdatedAt = "2026-09-02T09:02:00Z"
        val targetWithOwnSkip = reducedTargetOccurrence().copy(
            missedResolution = HabitMissedResolutionSnapshot(
                occurrenceEvidenceId = REDUCED_OCCURRENCE_ID,
                habitId = HABIT_ID,
                sourcePlannerOccurrenceId = REDUCED_PLANNER_OCCURRENCE_ID,
                revision = 2,
                configuredPolicy = HabitMissedPolicySnapshot.ASK,
                action = HabitMissedResolutionActionSnapshot.Skip,
                createdAt = "2026-09-02T09:01:00Z",
                updatedAt = targetResolutionUpdatedAt,
            ),
        )

        fun assertTargetOwnSkip(
            source: HabitOccurrenceSnapshot,
            pauses: List<HabitPauseSnapshot> = emptyList(),
        ) {
            val store = boundStore()
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(source, targetWithOwnSkip),
                pauses,
                "1",
            )
            val projected = store.state.value.recurrenceOutcomes
                .getValue(REDUCED_PLANNER_OCCURRENCE_ID)
            assertEquals(ItemStatus.SKIPPED, projected.status)
            assertEquals(targetResolutionUpdatedAt, projected.resolvedAt)
        }

        assertTargetOwnSkip(reduction.copy(outcome = completedOutcome()))
        assertTargetOwnSkip(
            reduction,
            pauses = listOf(HabitPauseSnapshot(
                id = PAUSE_ID,
                habitId = HABIT_ID,
                revision = 1,
                startedAt = "2026-09-01T06:30:00Z",
                endedAt = "2026-09-01T08:00:00Z",
                preservesStreak = true,
                createdAt = "2026-09-01T06:30:00Z",
                updatedAt = "2026-09-01T08:00:00Z",
            )),
        )
        assertTargetOwnSkip(
            reduction.copy(
                evidence = reduction.evidence.copy(
                    policyFingerprint = "sha256:${"f".repeat(64)}",
                ),
            ),
        )
        assertTargetOwnSkip(
            reduction.copy(
                evidence = reduction.evidence.copy(
                    sourceItemRevision = canonicalHabit().revision + 1,
                ),
            ),
        )

        fun assertInvalidTargetDoesNotSuppress(target: HabitOccurrenceSnapshot) {
            val store = boundStore()
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(reduction, target),
                emptyList(),
                "1",
            )
            assertFalse(
                REDUCED_PLANNER_OCCURRENCE_ID in store.state.value.recurrenceOutcomes,
            )
            assertEquals(
                target,
                store.state.value.habitLedger.occurrences[target.evidence.id],
            )
        }
        assertInvalidTargetDoesNotSuppress(
            reducedTargetOccurrence().copy(
                evidence = reducedTargetOccurrence().evidence.copy(
                    policyFingerprint = "sha256:${"e".repeat(64)}",
                ),
            ),
        )
        assertInvalidTargetDoesNotSuppress(
            reducedTargetOccurrence().copy(
                evidence = reducedTargetOccurrence().evidence.copy(
                    sourceItemRevision = canonicalHabit().revision + 1,
                ),
            ),
        )

        listOf(
            canonicalHabit().copy(status = "blocked"),
            canonicalHabit().copy(status = "future_status"),
            canonicalHabit().copy(isExecutable = false),
        ).forEach { inactiveHabit ->
            val store = boundStore(inactiveHabit)
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(reduction, targetWithOwnSkip),
                emptyList(),
                "1",
            )
            assertFalse(
                REDUCED_PLANNER_OCCURRENCE_ID in store.state.value.recurrenceOutcomes,
            )
        }
    }

    @Test
    fun futureOccurrenceEvidenceStaysCachedButInertUntilCanonicalCatchesUp() {
        val futureRevision = canonicalHabit().revision + 1
        val futureCompleted = occurrence(outcome = completedOutcome()).copy(
            evidence = occurrence().evidence.copy(sourceItemRevision = futureRevision),
        )

        val staleCanonicalStore = boundStore()
        staleCanonicalStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(futureCompleted),
            emptyList(),
            "1",
        )
        assertEquals(
            futureCompleted,
            staleCanonicalStore.state.value.habitLedger.occurrences[OCCURRENCE_ID],
        )
        assertFalse(
            PLANNER_OCCURRENCE_ID in staleCanonicalStore.state.value.recurrenceOutcomes,
        )
        assertFalse(HABIT_ID in staleCanonicalStore.state.value.recurrenceCompletionAnchors)

        val currentCanonicalStore = boundStore(canonicalHabit().copy(revision = futureRevision))
        currentCanonicalStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(futureCompleted),
            emptyList(),
            "1",
        )
        assertEquals(
            ItemStatus.COMPLETED,
            currentCanonicalStore.state.value.recurrenceOutcomes
                .getValue(PLANNER_OCCURRENCE_ID).status,
        )
        assertEquals(
            completedOutcome().occurredAt,
            currentCanonicalStore.state.value.recurrenceCompletionAnchors[HABIT_ID],
        )
    }

    @Test
    fun reductionChainsAlternateAndRestoreTheNextEdgeWhenTheFirstSourceEnds() {
        val firstTarget = reducedTargetOccurrence()
        val finalTarget = firstTarget.copy(
            evidence = firstTarget.evidence.copy(
                id = CHAIN_TARGET_OCCURRENCE_ID,
                plannerOccurrenceId = CHAIN_TARGET_PLANNER_OCCURRENCE_ID,
                identity = JsonObject(
                    mapOf(
                        "type" to JsonPrimitive("calendar_day"),
                        "date" to JsonPrimitive("2026-09-03"),
                        "bucket_ordinal" to JsonPrimitive(0),
                    ),
                ),
                nominalStart = "2026-09-03T07:00:00Z",
                nominalEnd = "2026-09-03T07:30:00Z",
                windowStart = "2026-09-03T06:00:00Z",
                windowEnd = "2026-09-03T09:00:00Z",
                localDate = "2026-09-03",
            ),
        )
        fun reductionFor(
            evidence: HabitOccurrenceEvidenceSnapshot,
            targetPlannerId: String,
        ) = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = evidence.id,
            habitId = evidence.habitId,
            sourcePlannerOccurrenceId = evidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.ReduceFrequency(listOf(targetPlannerId)),
            createdAt = evidence.windowEnd,
            updatedAt = evidence.windowEnd,
        )
        val firstSource = occurrence(
            missedResolution = reductionFor(
                occurrence().evidence,
                firstTarget.evidence.plannerOccurrenceId,
            ),
        )
        val middleSource = firstTarget.copy(
            missedResolution = reductionFor(
                firstTarget.evidence,
                finalTarget.evidence.plannerOccurrenceId,
            ),
        )

        val alternating = boundStore(
            publishedOccurrences = listOf(finalTarget, middleSource, firstSource),
        )
        alternating.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(finalTarget, middleSource, firstSource),
            emptyList(),
            "1",
        )
        assertEquals(
            ItemStatus.SKIPPED,
            alternating.state.value.recurrenceOutcomes
                .getValue(firstTarget.evidence.plannerOccurrenceId).status,
        )
        assertFalse(
            finalTarget.evidence.plannerOccurrenceId in
                alternating.state.value.recurrenceOutcomes,
        )

        val restored = boundStore(
            publishedOccurrences = listOf(finalTarget, middleSource, firstSource),
        )
        restored.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(finalTarget, middleSource, firstSource.copy(outcome = completedOutcome())),
            emptyList(),
            "1",
        )
        assertFalse(
            firstTarget.evidence.plannerOccurrenceId in restored.state.value.recurrenceOutcomes,
        )
        assertEquals(
            ItemStatus.SKIPPED,
            restored.state.value.recurrenceOutcomes
                .getValue(finalTarget.evidence.plannerOccurrenceId).status,
        )
    }

    @Test
    fun deltaCatchUpIsFalseUntilATerminalPageAndResetClearsIt() {
        val store = boundStore()
        assertNull(store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = emptyList(),
            pauses = emptyList(),
            nextCursor = "page_1",
            hasMore = true,
        )
        assertEquals("page_1", store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = emptyList(),
            pauses = emptyList(),
            nextCursor = "page_2",
            hasMore = false,
        )
        assertEquals("page_2", store.state.value.habitLedger.deltaCursor)
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)

        store.resetHabitDeltaCursor(ORIGIN, CONFIGURATION_ID)
        assertNull(store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun outcomeLifecycleUsesLedgerIdentityAndProjectsPlannerIdentity() {
        val store = boundStore()
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(occurrence()),
            pauses = emptyList(),
            nextCursor = "1",
        )
        assertEquals(OCCURRENCE_ID, store.state.value.habitLedger.occurrences.keys.single())

        val completion = pendingOutcome(
            operationId = OPERATION_ID,
            expectedRevision = 0,
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressBasisPoints = 10_000,
            note = "Finished",
        )
        store.stageHabitMutation(completion)
        assertEquals(completion, store.state.value.habitLedger.pendingMutations.single())

        store.reconcileHabitOccurrence(
            OPERATION_ID,
            occurrence(
                HabitOutcomeSnapshot(
                    revision = 1,
                    status = HabitOutcomeStatusSnapshot.COMPLETED,
                    progressBasisPoints = 10_000,
                    quantity = 8,
                    unit = "pages",
                    actualSeconds = 600,
                    note = "Finished",
                    occurredAt = "2026-09-01T07:30:00Z",
                    updatedAt = "2026-09-01T07:31:00Z",
                ),
            ),
        )
        val completed = store.state.value
        assertTrue(completed.habitLedger.pendingMutations.isEmpty())
        assertEquals(ItemStatus.COMPLETED, completed.recurrenceOutcomes[PLANNER_OCCURRENCE_ID]?.status)
        assertEquals(
            "2026-09-01T07:30:00Z",
            completed.recurrenceCompletionAnchors[HABIT_ID],
        )

        val correction = pendingOutcome(
            operationId = SECOND_OPERATION_ID,
            expectedRevision = 1,
            status = HabitOutcomeStatusSnapshot.SKIPPED,
            progressBasisPoints = 0,
            note = "Travel day",
            occurredAt = "2026-09-01T08:00:00Z",
        )
        store.stageHabitMutation(correction)
        store.reconcileHabitOccurrence(
            SECOND_OPERATION_ID,
            occurrence(
                HabitOutcomeSnapshot(
                    revision = 2,
                    status = HabitOutcomeStatusSnapshot.SKIPPED,
                    progressBasisPoints = 0,
                    quantity = null,
                    unit = null,
                    actualSeconds = null,
                    note = "Travel day",
                    occurredAt = "2026-09-01T08:00:00Z",
                    updatedAt = "2026-09-01T08:01:00Z",
                ),
            ),
        )
        val corrected = store.state.value
        assertEquals(ItemStatus.SKIPPED, corrected.recurrenceOutcomes[PLANNER_OCCURRENCE_ID]?.status)
        assertNull(corrected.recurrenceCompletionAnchors[HABIT_ID])
    }

    @Test
    fun pauseLifecycleIsDurableAndQuarantineRemovesDerivedHabitAuthority() {
        val store = boundStore()
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(occurrence(completedOutcome())),
            pauses = emptyList(),
            nextCursor = "1",
        )
        assertTrue(PLANNER_OCCURRENCE_ID in store.state.value.recurrenceOutcomes)

        val start = HabitPauseStartCommandSnapshot(
            operationId = OPERATION_ID,
            pauseId = PAUSE_ID,
            expectedRevision = 0,
            startedAt = "2026-09-02T08:00:00Z",
        )
        store.stageHabitMutation(
            pending(
                operationId = OPERATION_ID,
                kind = PendingHabitMutationKind.START_PAUSE,
                targetId = PAUSE_ID,
                expectedRevision = 0,
                requestJson = start.encoded(),
            ),
        )
        store.reconcileHabitPause(OPERATION_ID, pause())
        assertNull(store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt)

        val resume = HabitPauseResumeCommandSnapshot(
            operationId = SECOND_OPERATION_ID,
            expectedRevision = 1,
            endedAt = "2026-09-03T08:00:00Z",
        )
        store.stageHabitMutation(
            pending(
                operationId = SECOND_OPERATION_ID,
                kind = PendingHabitMutationKind.RESUME_PAUSE,
                targetId = PAUSE_ID,
                expectedRevision = 1,
                requestJson = resume.encoded(),
            ),
        )
        store.reconcileHabitPause(
            SECOND_OPERATION_ID,
            pause(revision = 2, endedAt = "2026-09-03T08:00:00Z"),
        )
        assertEquals(
            "2026-09-03T08:00:00Z",
            store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt,
        )

        store.quarantineHabitLedger()
        assertFalse(store.state.value.habitLedger.isBound)
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())
    }

    @Test
    fun staleDeltaCannotRollBackOutcomeAndEqualRevisionMustBeIdentical() {
        val store = boundStore()
        val latest = occurrence(completedOutcome(revision = 2))
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(latest),
            emptyList(),
            "2",
        )

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(occurrence(completedOutcome(revision = 1))),
            emptyList(),
            "3",
        )
        assertEquals(2L, store.state.value.habitLedger.occurrences
            .getValue(OCCURRENCE_ID).outcome?.revision)

        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(latest.copy(outcome = latest.outcome?.copy(note = "tampered"))),
                emptyList(),
                "4",
            )
        }
    }

    @Test
    fun occurrenceRetentionAdvancesPastCeilingAndKeepsPendingAndAnchorEvidence() {
        val rawPage = (1..10_000).map { index ->
            val occurrence = retentionOccurrence(
                index = index,
                outcome = if (index == 2) retentionCompletedOutcome(index) else null,
            )
            if (index >= 4) {
                occurrence.copy(missedResolution = retentionSkippedResolution(occurrence))
            } else {
                occurrence
            }
        }
        val reductionSource = rawPage[0].copy(
            missedResolution = HabitMissedResolutionSnapshot(
                occurrenceEvidenceId = rawPage[0].evidence.id,
                habitId = HABIT_ID,
                sourcePlannerOccurrenceId = rawPage[0].evidence.plannerOccurrenceId,
                revision = 2,
                configuredPolicy = HabitMissedPolicySnapshot.ASK,
                action = HabitMissedResolutionActionSnapshot.ReduceFrequency(
                    listOf(rawPage[2].evidence.plannerOccurrenceId),
                ),
                createdAt = "1990-01-02T09:01:00Z",
                updatedAt = "1990-01-02T09:01:00Z",
            ),
        )
        val reductionTarget = rawPage[2].copy(
            outcome = retentionCompletedOutcome(3).copy(
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 5_000,
            ),
        )
        val page = rawPage.toMutableList().apply {
            this[0] = reductionSource
            this[2] = reductionTarget
        }
        val store = boundStore(
            nowEpochMillis = {
                java.time.Instant.parse("1990-01-02T12:00:00Z").toEpochMilli()
            },
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = page,
            pauses = emptyList(),
            nextCursor = "retained_10000",
        )
        val pendingTarget = store.state.value.habitLedger.occurrences.values
            .filter { it.evidence.id != numberedUuid(2, version = 4) }
            .minBy { it.evidence.nominalStart }
        val pending = pendingOutcome(
            operationId = OPERATION_ID,
            expectedRevision = 0,
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressBasisPoints = 4_000,
        ).copy(targetId = pendingTarget.evidence.id)
        store.stageHabitMutation(pending)
        val newest = retentionOccurrence(10_001)

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(newest),
            pauses = emptyList(),
            nextCursor = "retained_10001",
        )

        val retained = store.state.value.habitLedger.occurrences
        assertTrue(retained.size <= 10_000)
        assertTrue(pendingTarget.evidence.id in retained)
        assertTrue(numberedUuid(2, version = 4) in retained)
        assertTrue(reductionSource.evidence.id in retained)
        assertTrue(reductionTarget.evidence.id in retained)
        assertTrue(newest.evidence.id in retained)
        assertTrue(retained.size < page.size + 1)
    }

    @Test
    fun occurrenceRetentionPreservesTransitiveReductionDependenciesAcrossRestart() {
        val page = (1..10_001).map(::retentionOccurrence).toMutableList()
        val target = page.last()
        val middle = retentionReduction(page[1], target)
        val upstream = retentionReduction(page[0], middle)
        page[0] = upstream
        page[1] = middle
        val store = boundStore(
            nowEpochMillis = { Instant.parse(target.evidence.nominalStart).toEpochMilli() },
        )

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = page,
            pauses = emptyList(),
            nextCursor = "transitive_reduction_retained",
        )

        val retained = store.state.value.habitLedger.occurrences
        assertTrue(retained.size <= 10_000)
        assertEquals("transitive_reduction_retained", store.state.value.habitLedger.deltaCursor)
        assertTrue(upstream.evidence.id in retained)
        assertTrue(middle.evidence.id in retained)
        assertTrue(target.evidence.id in retained)
        assertFalse(page[2].evidence.id in retained)

        val restarted = PlannerStore(store.state.value).state.value.habitLedger.occurrences
        assertTrue(upstream.evidence.id in restarted)
        assertTrue(middle.evidence.id in restarted)
        assertTrue(target.evidence.id in restarted)
    }

    @Test
    fun occurrenceRetentionKeepsDormantReductionForLaterTargetCorrection() {
        val page = (1..10_001).map(::retentionOccurrence).toMutableList()
        val skippedTarget = page[1].copy(
            outcome = retentionCompletedOutcome(2).copy(
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
                quantity = null,
                unit = null,
                actualSeconds = null,
            ),
        )
        val reductionSource = retentionReduction(page[0], skippedTarget)
        page[0] = reductionSource
        page[1] = skippedTarget
        val newest = page.last()
        val store = boundStore(
            nowEpochMillis = { Instant.parse(newest.evidence.nominalStart).toEpochMilli() },
        )

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = page,
            pauses = emptyList(),
            nextCursor = "dormant_reduction_retained",
        )

        val retained = store.state.value.habitLedger.occurrences
        assertTrue(reductionSource.evidence.id in retained)
        assertTrue(skippedTarget.evidence.id in retained)
        assertEquals("dormant_reduction_retained", store.state.value.habitLedger.deltaCursor)

        val correctedTarget = skippedTarget.copy(
            outcome = requireNotNull(skippedTarget.outcome).copy(
                revision = 2,
                status = HabitOutcomeStatusSnapshot.UNRESOLVED,
                progressBasisPoints = 0,
                note = null,
                occurredAt = newest.evidence.nominalStart,
                updatedAt = newest.evidence.nominalStart,
            ),
        )
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(correctedTarget),
            pauses = emptyList(),
            nextCursor = "target_corrected",
        )
        assertTrue(
            reductionSource.evidence.id in store.state.value.habitLedger.occurrences,
        )
    }

    @Test
    fun duplicatePlannerIdentityAtTheCacheCeilingFailsBeforePruningOrCursorAdvance() {
        val store = boundStore()
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = (1..10_000).map(::retentionOccurrence),
            pauses = emptyList(),
            nextCursor = "before_duplicate",
        )
        val oldestRetained = store.state.value.habitLedger.occurrences.values.minBy {
            it.evidence.nominalStart
        }
        val incomingBase = retentionOccurrence(10_001)
        val duplicate = incomingBase.copy(
            evidence = incomingBase.evidence.copy(
                plannerOccurrenceId = oldestRetained.evidence.plannerOccurrenceId,
            ),
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = listOf(duplicate),
                pauses = emptyList(),
                nextCursor = "must_not_advance",
            )
        }

        assertEquals("before_duplicate", store.state.value.habitLedger.deltaCursor)
        assertTrue(oldestRetained.evidence.id in store.state.value.habitLedger.occurrences)
    }

    @Test
    fun overflowThenLatestCompletionCorrectionKeepsOlderAuthoritativeAnchor() {
        val olderCompletion = retentionOccurrence(1, retentionCompletedOutcome(1))
        val latestCompletion = retentionOccurrence(2, retentionCompletedOutcome(2))
        val page = (1..10_001).map { index ->
            when (index) {
                1 -> olderCompletion
                2 -> latestCompletion
                else -> retentionOccurrence(index)
            }
        }
        val overflowStore = boundStore()
        overflowStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = page,
            pauses = emptyList(),
            nextCursor = "overflow",
        )

        val overflowed = overflowStore.state.value
        assertTrue(overflowed.habitLedger.occurrences.size <= 10_000)
        assertTrue(overflowed.habitLedger.occurrences.size < page.size)
        assertTrue(olderCompletion.evidence.id in overflowed.habitLedger.occurrences)
        assertTrue(latestCompletion.evidence.id in overflowed.habitLedger.occurrences)
        assertEquals(
            latestCompletion.outcome?.occurredAt,
            overflowed.recurrenceCompletionAnchors[HABIT_ID],
        )

        listOf(
            HabitOutcomeStatusSnapshot.PARTIAL to 5_000,
            HabitOutcomeStatusSnapshot.SKIPPED to 0,
        ).forEach { (status, progressBasisPoints) ->
            val correctedStore = PlannerStore(overflowed)
            val correctedOutcome = requireNotNull(latestCompletion.outcome).copy(
                revision = 2,
                status = status,
                progressBasisPoints = progressBasisPoints,
                quantity = if (status == HabitOutcomeStatusSnapshot.PARTIAL) 10 else null,
                unit = if (status == HabitOutcomeStatusSnapshot.PARTIAL) "pages" else null,
                actualSeconds = if (status == HabitOutcomeStatusSnapshot.PARTIAL) 900 else null,
                occurredAt = "1990-01-03T08:00:00Z",
                updatedAt = "1990-01-03T08:01:00Z",
            )

            correctedStore.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = listOf(latestCompletion.copy(outcome = correctedOutcome)),
                pauses = emptyList(),
                nextCursor = "corrected_${status.name.lowercase()}",
            )

            val corrected = correctedStore.state.value
            assertEquals(
                olderCompletion.outcome?.occurredAt,
                corrected.recurrenceCompletionAnchors[HABIT_ID],
            )
            assertTrue(olderCompletion.evidence.id in corrected.habitLedger.occurrences)
            assertEquals(
                status,
                corrected.habitLedger.occurrences
                    .getValue(latestCompletion.evidence.id).outcome?.status,
            )
        }
    }

    @Test
    fun pauseRetentionAdvancesPastCeilingAndKeepsOpenPause() {
        val cached = (1..2_000).associate { index ->
            val pause = retentionPause(index, open = index == 1)
            pause.id to pause
        }
        val store = PlannerStore(
            DayWeaveUiState(
                habitLedger = HabitLedgerSnapshot(
                    syncOrigin = ORIGIN,
                    configurationId = CONFIGURATION_ID,
                    pauses = cached,
                ),
            ),
        )
        val newest = retentionPause(2_001, open = false)

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = emptyList(),
            pauses = listOf(newest),
            nextCursor = "retained_2001",
        )

        val retained = store.state.value.habitLedger.pauses
        assertEquals(2_000, retained.size)
        assertTrue(numberedUuid(1, version = 4) in retained)
        assertTrue(newest.id in retained)
        assertFalse(numberedUuid(2, version = 4) in retained)
    }

    @Test
    fun openPauseRetentionFailsClosedBeforeAdvancingTheDeltaCursor() {
        val openPauses = (1..2_001).map { index ->
            retentionPause(index, open = true).copy(
                habitId = numberedUuid(index + 10_000, version = 4),
            )
        }
        val store = boundStore()

        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = emptyList(),
                pauses = openPauses,
                nextCursor = "open_pause_authority_overflow",
            )
        }

        assertTrue(store.state.value.habitLedger.pauses.isEmpty())
        assertNull(store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun closedPauseOverlappingRetainedScheduleAuthoritySurvivesThePauseCeiling() {
        val source = retentionOccurrence(1)
        val protectedPause = HabitPauseSnapshot(
            id = numberedUuid(30_001, version = 4),
            habitId = HABIT_ID,
            revision = 2,
            startedAt = "1990-01-02T06:30:00Z",
            endedAt = "1990-01-02T08:30:00Z",
            preservesStreak = true,
            createdAt = "1990-01-02T06:30:00Z",
            updatedAt = "1990-01-02T08:30:00Z",
        )
        val newerPauses = (1..2_000).map { retentionPause(it, open = false) }
        val store = boundStore()

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(source),
            pauses = listOf(protectedPause) + newerPauses,
            nextCursor = "closed_pause_retained",
        )

        val retained = store.state.value.habitLedger.pauses
        assertEquals(2_000, retained.size)
        assertTrue(protectedPause.id in retained)
        assertEquals("closed_pause_retained", store.state.value.habitLedger.deltaCursor)
        assertTrue(source.evidence.id in store.state.value.habitLedger.occurrences)
    }

    @Test
    fun overlappingPauseAtTheCeilingFailsBeforePruningOrCursorAdvance() {
        val older = HabitPauseSnapshot(
            id = numberedUuid(30_002, version = 4),
            habitId = HABIT_ID,
            revision = 2,
            startedAt = "1990-01-02T06:00:00Z",
            endedAt = "1990-01-02T08:00:00Z",
            preservesStreak = true,
            createdAt = "1990-01-02T06:00:00Z",
            updatedAt = "1990-01-02T08:00:00Z",
        )
        val cached = (1..1_999).map { retentionPause(it, open = false) } + older
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalHabit()),
                canonicalSyncOrigin = ORIGIN,
                canonicalConfigurationId = CONFIGURATION_ID,
                habitLedger = HabitLedgerSnapshot(
                    syncOrigin = ORIGIN,
                    configurationId = CONFIGURATION_ID,
                    pauses = cached.associateBy(HabitPauseSnapshot::id),
                ),
            ),
        )
        val overlapping = older.copy(
            id = numberedUuid(30_003, version = 4),
            startedAt = "1990-01-02T07:00:00Z",
            endedAt = "1990-01-02T09:00:00Z",
            createdAt = "1990-01-02T07:00:00Z",
            updatedAt = "1990-01-02T09:00:00Z",
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = emptyList(),
                pauses = listOf(overlapping),
                nextCursor = "must_not_advance",
            )
        }

        assertEquals(2_000, store.state.value.habitLedger.pauses.size)
        assertNull(store.state.value.habitLedger.deltaCursor)
    }

    @Test
    fun occurrenceRetentionAlsoBoundsAggregatePrivateContent() {
        val largeNote = "n".repeat(10_000)
        val page = (1..900).map { index ->
            retentionOccurrence(
                index,
                retentionCompletedOutcome(index).copy(
                    status = HabitOutcomeStatusSnapshot.PARTIAL,
                    progressBasisPoints = 5_000,
                    note = largeNote,
                ),
            )
        }
        val store = boundStore()

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = page,
            pauses = emptyList(),
            nextCursor = "private_budget",
        )

        val retained = store.state.value.habitLedger.occurrences
        assertTrue(retained.size < page.size)
        assertTrue(page.last().evidence.id in retained)
        store.state.value.habitLedger.requireValid()
    }

    @Test
    fun completedOccurrenceRetentionFailsClosedWhenPrivateAuthorityExceedsBudget() {
        val largeNote = "n".repeat(10_000)
        val page = (1..900).map { index ->
            retentionOccurrence(
                index,
                retentionCompletedOutcome(index).copy(note = largeNote),
            )
        }
        val store = boundStore()

        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = page,
                pauses = emptyList(),
                nextCursor = "private_authority_overflow",
            )
        }

        assertTrue(store.state.value.habitLedger.occurrences.isEmpty())
        assertNull(store.state.value.habitLedger.deltaCursor)
    }

    @Test
    fun orderedDeltaPageAcceptsMultipleRevisionsOfTheSameOccurrenceAndPause() {
        val store = boundStore()
        val firstOutcome = completedOutcome(revision = 1)
        val correctedOutcome = firstOutcome.copy(
            revision = 2,
            status = HabitOutcomeStatusSnapshot.SKIPPED,
            progressBasisPoints = 0,
            quantity = null,
            unit = null,
            actualSeconds = null,
            occurredAt = "2026-09-01T08:00:00Z",
            updatedAt = "2026-09-01T08:01:00Z",
        )

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(occurrence(firstOutcome), occurrence(correctedOutcome)),
            pauses = listOf(
                pause(revision = 1),
                pause(revision = 2, endedAt = "2026-09-03T08:00:00Z"),
            ),
            nextCursor = "2",
        )

        assertEquals(
            correctedOutcome,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).outcome,
        )
        assertEquals(2L, store.state.value.habitLedger.pauses.getValue(PAUSE_ID).revision)
        assertEquals(
            "2026-09-03T08:00:00Z",
            store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt,
        )
    }

    @Test
    fun genesisReplayIgnoresStaleOpenPauseBeforeCheckingClosedImmutability() {
        val store = boundStore()
        val closed = pause(revision = 2, endedAt = "2026-09-03T08:00:00Z")
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = emptyList(),
            pauses = listOf(closed),
            nextCursor = "2",
        )

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = emptyList(),
            pauses = listOf(pause(revision = 1), closed),
            nextCursor = "3",
        )

        assertEquals(closed, store.state.value.habitLedger.pauses.getValue(PAUSE_ID))
        assertThrows(IllegalArgumentException::class.java) {
            store.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                occurrences = emptyList(),
                pauses = listOf(pause(revision = 1).copy(habitId = OTHER_HABIT_ID)),
                nextCursor = "4",
            )
        }
    }

    @Test
    fun firstPartialHabitEvidenceClearsLegacyTerminalProjectionAndAnchor() {
        val legacy = DayWeaveUiState(
            recurrenceOutcomes = mapOf(
                PLANNER_OCCURRENCE_ID to RecurrenceOutcomeSnapshot(
                    itemId = HABIT_ID,
                    status = ItemStatus.COMPLETED,
                    resolvedAt = "2026-09-01T07:30:00Z",
                ),
            ),
            recurrenceCompletionAnchors = mapOf(HABIT_ID to "2026-09-01T07:30:00Z"),
        )
        val store = PlannerStore(legacy)
        store.bindHabitLedger(ORIGIN, CONFIGURATION_ID)
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            occurrences = listOf(
                occurrence(
                    HabitOutcomeSnapshot(
                        revision = 1,
                        status = HabitOutcomeStatusSnapshot.PARTIAL,
                        progressBasisPoints = 4_000,
                        quantity = 8,
                        unit = "pages",
                        actualSeconds = 600,
                        note = null,
                        occurredAt = "2026-09-01T07:30:00Z",
                        updatedAt = "2026-09-01T07:31:00Z",
                    ),
                ),
            ),
            pauses = emptyList(),
            nextCursor = "1",
        )

        assertFalse(PLANNER_OCCURRENCE_ID in store.state.value.recurrenceOutcomes)
        assertFalse(HABIT_ID in store.state.value.recurrenceCompletionAnchors)
    }

    @Test
    fun higherPauseRevisionCannotChangeIdentityPolicyOrReopen() {
        val closed = pause(revision = 2, endedAt = "2026-09-03T08:00:00Z")
        val invalidRevisions = listOf(
            closed.copy(revision = 3, habitId = OTHER_HABIT_ID),
            closed.copy(revision = 3, startedAt = "2026-09-01T08:00:00Z"),
            closed.copy(revision = 3, createdAt = "2026-09-02T08:01:00Z"),
            closed.copy(revision = 3, preservesStreak = false),
            closed.copy(revision = 3, endedAt = null, updatedAt = "2026-09-03T09:00:00Z"),
            closed.copy(
                revision = 3,
                endedAt = "2026-09-03T09:00:00Z",
                updatedAt = "2026-09-03T09:00:00Z",
            ),
        )
        invalidRevisions.forEach { invalid ->
            val store = boundStore()
            store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, emptyList(), listOf(closed), "1")
            assertThrows(IllegalArgumentException::class.java) {
                store.applyHabitDeltaPage(
                    ORIGIN,
                    CONFIGURATION_ID,
                    emptyList(),
                    listOf(invalid),
                    "2",
                )
            }
            assertEquals(closed, store.state.value.habitLedger.pauses.getValue(PAUSE_ID))
        }
    }

    @Test
    fun ledgerRejectsOverlappingPausesForOneHabit() {
        val first = pause(revision = 2, endedAt = "2026-09-03T08:00:00Z")
        val overlapping = HabitPauseSnapshot(
            id = SECOND_PAUSE_ID,
            habitId = HABIT_ID,
            revision = 1,
            startedAt = "2026-09-03T07:59:00Z",
            endedAt = "2026-09-04T08:00:00Z",
            preservesStreak = true,
            createdAt = "2026-09-03T07:59:00Z",
            updatedAt = "2026-09-04T08:00:00Z",
        )

        assertThrows(IllegalArgumentException::class.java) {
            HabitLedgerSnapshot(
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                pauses = mapOf(first.id to first, overlapping.id to overlapping),
            ).requireValid()
        }
    }

    @Test
    fun ledgerValidatesOnlyPendingReplayAuthorityAgainstAuthoritativeTargets() {
        val unresolved = occurrence()
        val outcomeMutation = pendingOutcome(
            operationId = OPERATION_ID,
            expectedRevision = 0,
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressBasisPoints = 4_000,
        )
        val outcomeLedger = HabitLedgerSnapshot(
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            occurrences = mapOf(OCCURRENCE_ID to unresolved),
            pendingMutations = listOf(outcomeMutation),
        ).also(HabitLedgerSnapshot::requireValid)

        assertThrows(IllegalArgumentException::class.java) {
            outcomeLedger.copy(occurrences = emptyMap()).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            outcomeLedger.copy(
                occurrences = mapOf(OCCURRENCE_ID to occurrence(completedOutcome())),
            ).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            outcomeLedger.copy(
                pendingMutations = listOf(outcomeMutation.copy(habitId = OTHER_HABIT_ID)),
            ).requireValid()
        }
        listOf(
            PendingHabitMutationDisposition.CONFLICT,
            PendingHabitMutationDisposition.NOT_FOUND,
            PendingHabitMutationDisposition.REJECTED,
        ).forEach { disposition ->
            outcomeLedger.copy(
                occurrences = emptyMap(),
                pendingMutations = listOf(outcomeMutation.copy(disposition = disposition)),
            ).requireValid()
        }

        val startCommand = HabitPauseStartCommandSnapshot(
            operationId = OPERATION_ID,
            pauseId = PAUSE_ID,
            expectedRevision = 0,
            startedAt = "2026-09-02T08:00:00Z",
        )
        val startMutation = pending(
            OPERATION_ID,
            PendingHabitMutationKind.START_PAUSE,
            PAUSE_ID,
            0,
            startCommand.encoded(),
        )
        HabitLedgerSnapshot(
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            pendingMutations = listOf(startMutation),
        ).requireValid()
        assertThrows(IllegalArgumentException::class.java) {
            HabitLedgerSnapshot(
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                pauses = mapOf(PAUSE_ID to pause()),
                pendingMutations = listOf(startMutation),
            ).requireValid()
        }

        val resumeCommand = HabitPauseResumeCommandSnapshot(
            operationId = SECOND_OPERATION_ID,
            expectedRevision = 1,
            endedAt = "2026-09-03T08:00:00Z",
        )
        val resumeMutation = pending(
            SECOND_OPERATION_ID,
            PendingHabitMutationKind.RESUME_PAUSE,
            PAUSE_ID,
            1,
            resumeCommand.encoded(),
        )
        val resumeLedger = HabitLedgerSnapshot(
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            pauses = mapOf(PAUSE_ID to pause()),
            pendingMutations = listOf(resumeMutation),
        ).also(HabitLedgerSnapshot::requireValid)
        assertThrows(IllegalArgumentException::class.java) {
            resumeLedger.copy(
                pauses = mapOf(PAUSE_ID to pause(revision = 2)),
            ).requireValid()
        }

        val duplicateOutcome = pendingOutcome(
            operationId = SECOND_OPERATION_ID,
            expectedRevision = 0,
            status = HabitOutcomeStatusSnapshot.SKIPPED,
            progressBasisPoints = 0,
        )
        assertThrows(IllegalArgumentException::class.java) {
            outcomeLedger.copy(
                pendingMutations = listOf(outcomeMutation, duplicateOutcome),
            ).requireValid()
        }

        val secondStartCommand = HabitPauseStartCommandSnapshot(
            operationId = SECOND_OPERATION_ID,
            pauseId = SECOND_PAUSE_ID,
            expectedRevision = 0,
            startedAt = "2026-09-02T09:00:00Z",
        )
        assertThrows(IllegalArgumentException::class.java) {
            HabitLedgerSnapshot(
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                pendingMutations = listOf(
                    startMutation,
                    pending(
                        SECOND_OPERATION_ID,
                        PendingHabitMutationKind.START_PAUSE,
                        SECOND_PAUSE_ID,
                        0,
                        secondStartCommand.encoded(),
                    ),
                ),
            ).requireValid()
        }
    }

    @Test
    fun occurrenceWindowMergeCannotOverwriteAConflictingEqualRevision() {
        val store = boundStore()
        val latest = occurrence(completedOutcome(revision = 2))
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(latest), emptyList(), "2")

        assertThrows(IllegalArgumentException::class.java) {
            store.mergeHabitOccurrencePage(
                ORIGIN,
                CONFIGURATION_ID,
                HABIT_ID,
                listOf(latest.copy(outcome = latest.outcome?.copy(note = "different"))),
            )
        }
        assertEquals(latest, store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID))
    }

    @Test
    fun outcomeQuantityMustUseTheImmutableOccurrenceTargetUnit() {
        val store = boundStore()
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(occurrence()),
            emptyList(),
            "1",
        )
        val command = HabitOutcomeCommandSnapshot(
            operationId = OPERATION_ID,
            expectedRevision = 0,
            outcome = HabitOutcomeInputSnapshot(
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 4_000,
                quantity = 8,
                unit = "minutes",
                actualSeconds = 600,
                note = null,
                occurredAt = "2026-09-01T07:30:00Z",
            ),
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.stageHabitMutation(
                pending(
                    OPERATION_ID,
                    PendingHabitMutationKind.OUTCOME,
                    OCCURRENCE_ID,
                    0,
                    command.encoded(),
                ),
            )
        }
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun occurrenceDeltaInvalidatesAnalyticsOnlyForAnAuthoritativeChange() {
        val store = boundStore()
        val unresolved = occurrence()
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(unresolved), emptyList(), "1")
        val analytics = emptyAnalytics()
        val unrelatedAnalytics = emptyAnalytics(OTHER_HABIT_ID)
        store.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        store.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, unrelatedAnalytics)

        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(unresolved), emptyList(), "2")
        assertEquals(analytics, store.state.value.habitLedger.analytics[analytics.cacheKey])

        val completed = occurrence(completedOutcome())
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(completed), emptyList(), "3")
        assertEquals(
            mapOf(unrelatedAnalytics.cacheKey to unrelatedAnalytics),
            store.state.value.habitLedger.analytics,
        )

        store.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(unresolved), emptyList(), "4")
        assertEquals(analytics, store.state.value.habitLedger.analytics[analytics.cacheKey])
    }

    @Test
    fun stagingOutcomeOrPauseImmediatelyInvalidatesOnlyThatHabitsAnalytics() {
        val analytics = emptyAnalytics()
        val unrelatedAnalytics = emptyAnalytics(OTHER_HABIT_ID)
        val outcomeStore = boundStore()
        outcomeStore.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(occurrence()),
            emptyList(),
            "1",
        )
        outcomeStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        outcomeStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, unrelatedAnalytics)

        outcomeStore.stageHabitMutation(
            pendingOutcome(
                operationId = OPERATION_ID,
                expectedRevision = 0,
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
            ),
        )

        assertEquals(
            mapOf(unrelatedAnalytics.cacheKey to unrelatedAnalytics),
            outcomeStore.state.value.habitLedger.analytics,
        )

        val pauseStore = boundStore()
        pauseStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        pauseStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, unrelatedAnalytics)
        val start = HabitPauseStartCommandSnapshot(
            operationId = OPERATION_ID,
            pauseId = PAUSE_ID,
            expectedRevision = 0,
            startedAt = "2026-09-02T08:00:00Z",
        )

        pauseStore.stageHabitMutation(
            pending(OPERATION_ID, PendingHabitMutationKind.START_PAUSE, PAUSE_ID, 0, start.encoded()),
        )

        assertEquals(
            mapOf(unrelatedAnalytics.cacheKey to unrelatedAnalytics),
            pauseStore.state.value.habitLedger.analytics,
        )
    }

    @Test
    fun occurrenceWindowMergeInvalidatesAnalyticsOnlyWhenItsReplicaChanges() {
        val store = boundStore()
        val first = occurrence(completedOutcome())
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, listOf(first), emptyList(), "1")
        val analytics = emptyAnalytics()
        store.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)

        store.mergeHabitOccurrencePage(ORIGIN, CONFIGURATION_ID, HABIT_ID, listOf(first))
        assertEquals(analytics, store.state.value.habitLedger.analytics[analytics.cacheKey])

        val correction = first.copy(
            outcome = first.outcome?.copy(
                revision = 2,
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
                quantity = null,
                unit = null,
                actualSeconds = null,
                updatedAt = "2026-09-01T07:32:00Z",
            ),
        )
        store.mergeHabitOccurrencePage(ORIGIN, CONFIGURATION_ID, HABIT_ID, listOf(correction))
        assertTrue(store.state.value.habitLedger.analytics.isEmpty())
    }

    @Test
    fun pauseDeltaAndMutationReconciliationInvalidateAnalyticsButExactReplayDoesNot() {
        val store = boundStore()
        val analytics = emptyAnalytics()
        val open = pause()
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, emptyList(), listOf(open), "1")
        store.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)

        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, emptyList(), listOf(open), "2")
        assertEquals(analytics, store.state.value.habitLedger.analytics[analytics.cacheKey])
        val closed = pause(revision = 2, endedAt = "2026-09-03T08:00:00Z")
        store.applyHabitDeltaPage(ORIGIN, CONFIGURATION_ID, emptyList(), listOf(closed), "3")
        assertTrue(store.state.value.habitLedger.analytics.isEmpty())

        val secondStore = boundStore()
        secondStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        val start = HabitPauseStartCommandSnapshot(
            operationId = OPERATION_ID,
            pauseId = PAUSE_ID,
            expectedRevision = 0,
            startedAt = "2026-09-02T08:00:00Z",
        )
        secondStore.stageHabitMutation(
            pending(OPERATION_ID, PendingHabitMutationKind.START_PAUSE, PAUSE_ID, 0, start.encoded()),
        )
        secondStore.reconcileHabitPause(OPERATION_ID, open)
        assertTrue(secondStore.state.value.habitLedger.analytics.isEmpty())

        secondStore.cacheHabitAnalytics(ORIGIN, CONFIGURATION_ID, analytics)
        val resume = HabitPauseResumeCommandSnapshot(
            operationId = SECOND_OPERATION_ID,
            expectedRevision = 1,
            endedAt = "2026-09-03T08:00:00Z",
        )
        secondStore.stageHabitMutation(
            pending(
                SECOND_OPERATION_ID,
                PendingHabitMutationKind.RESUME_PAUSE,
                PAUSE_ID,
                1,
                resume.encoded(),
            ),
        )
        secondStore.reconcileHabitPause(SECOND_OPERATION_ID, closed)
        assertTrue(secondStore.state.value.habitLedger.analytics.isEmpty())
    }

    @Test
    fun analyticsRequireCanonicalBucketsAndExactTrendAggregates() {
        val first = HabitTrendBucketSnapshot(
            startDate = "2026-09-01",
            endDate = "2026-09-06",
            expected = 1,
            eligible = 1,
            completed = 1,
            partial = 0,
            skipped = 0,
            missed = 0,
            excused = 0,
            unresolved = 0,
            adherenceBasisPoints = 10_000,
            actualSecondsTotal = 60,
            quantityTotals = listOf(HabitQuantityTotalSnapshot("pages", 1)),
        )
        val second = first.copy(
            startDate = "2026-09-07",
            endDate = "2026-09-07",
        )
        val analytics = HabitAnalyticsSnapshot(
            habitId = HABIT_ID,
            startDate = "2026-09-01",
            endDate = "2026-09-07",
            bucket = HabitAnalyticsBucketSnapshot.WEEK,
            expected = 2,
            eligible = 2,
            completed = 2,
            partial = 0,
            skipped = 0,
            missed = 0,
            excused = 0,
            unresolved = 0,
            adherenceBasisPoints = 10_000,
            actualSecondsTotal = 120,
            quantityTotals = listOf(HabitQuantityTotalSnapshot("pages", 2)),
            currentStreak = 2,
            longestStreak = 2,
            trends = listOf(first, second),
            supportiveFactCodes = listOf(
                HabitSupportiveFactCodeSnapshot.ACTIVE_STREAK,
                HabitSupportiveFactCodeSnapshot.STRONG_ADHERENCE,
            ),
        )
        analytics.requireValid()

        assertThrows(IllegalArgumentException::class.java) {
            analytics.copy(trends = listOf(first.copy(endDate = "2026-09-07"), second))
                .requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            analytics.copy(actualSecondsTotal = 121).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            analytics.copy(
                quantityTotals = listOf(HabitQuantityTotalSnapshot("pages", 3)),
            ).requireValid()
        }
    }

    @Test
    fun habitWireTimestampsRejectNanosecondPrecision() {
        val input = HabitOutcomeInputSnapshot(
            status = HabitOutcomeStatusSnapshot.SKIPPED,
            progressBasisPoints = 0,
            quantity = null,
            unit = null,
            actualSeconds = null,
            note = null,
            occurredAt = "2026-09-01T07:30:00.0000001Z",
        )

        assertThrows(IllegalArgumentException::class.java) { input.requireValid() }
    }

    @Test
    fun conflictedMutationRemainsReviewableAndPendingUncertaintyCannotBeDiscarded() {
        val store = boundStore()
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(occurrence()),
            emptyList(),
            "1",
            hasMore = false,
        )
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
        store.stageHabitMutation(
            pendingOutcome(
                operationId = OPERATION_ID,
                expectedRevision = 0,
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 4_000,
            ),
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.discardReviewedHabitMutation(OPERATION_ID)
        }
        store.markHabitMutationForReview(
            OPERATION_ID,
            PendingHabitMutationDisposition.CONFLICT,
        )
        assertEquals(
            PendingHabitMutationDisposition.CONFLICT,
            store.state.value.habitLedger.pendingMutations.single().disposition,
        )
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
        store.discardReviewedHabitMutation(OPERATION_ID)
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)

        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            emptyList(),
            emptyList(),
            "1",
            hasMore = false,
        )
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    private fun boundStore(
        item: CanonicalItemSnapshot = canonicalHabit(),
        nowEpochMillis: () -> Long = System::currentTimeMillis,
        publishedOccurrences: List<HabitOccurrenceSnapshot>? = null,
    ): PlannerStore {
        val revision = publishedOccurrences?.let {
            PublishedScheduleRevisionSnapshot(
                id = SCHEDULE_REVISION_ID,
                revision = "1:$SCHEDULE_REVISION_ID",
                revisionNumber = 1uL,
                inputDigest = "sha256:${"a".repeat(64)}",
                horizonStart = "2026-09-01T00:00:00Z",
                horizonEnd = "2026-09-04T00:00:00Z",
                timezoneName = "Europe/Paris",
                publishedAt = "2026-09-01T06:00:00Z",
            )
        }
        return PlannerStore(
            DayWeaveUiState(
            canonicalItems = listOf(item),
            canonicalSyncOrigin = ORIGIN,
            canonicalConfigurationId = CONFIGURATION_ID,
            publishedOccurrenceMembershipProof = revision?.let { publishedRevision ->
                PublishedOccurrenceMembershipProofSnapshot(
                    schemaVersion =
                        PublishedOccurrenceMembershipProofSnapshot.CURRENT_SCHEMA_VERSION,
                    syncOrigin = ORIGIN,
                    configurationId = CONFIGURATION_ID,
                    revision = publishedRevision,
                    occurrences = requireNotNull(publishedOccurrences).map { occurrence ->
                        PublishedOccurrenceMembershipSnapshot(
                            plannerOccurrenceId = occurrence.evidence.plannerOccurrenceId,
                            seriesItemId = occurrence.evidence.habitId,
                            state = PublishedOccurrenceStateSnapshot.GENERATED,
                        )
                    }.sortedWith(
                        compareBy<PublishedOccurrenceMembershipSnapshot> {
                            it.plannerOccurrenceId
                        }.thenBy { it.seriesItemId },
                    ),
                )
            },
            publishedScheduleRevisionHint = revision?.let { publishedRevision ->
                PublishedScheduleRevisionHintSnapshot(
                    syncOrigin = ORIGIN,
                    configurationId = CONFIGURATION_ID,
                    revisionNumber = publishedRevision.revisionNumber,
                )
            },
        ),
        nowEpochMillis = nowEpochMillis,
        ).also {
            it.bindHabitLedger(ORIGIN, CONFIGURATION_ID)
        }
    }

    private fun canonicalHabit() = CanonicalItemSnapshot(
        id = HABIT_ID,
        kind = "habit",
        status = "planned",
        title = "Read",
        timezoneName = "Europe/Paris",
        durationSeconds = 1_800,
        recurrenceJson = """{"type":"daily","times_per_day":1}""",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-09-01T06:00:00Z",
        updatedAt = "2026-09-01T06:00:00Z",
    )

    private fun occurrence(
        outcome: HabitOutcomeSnapshot? = null,
        missedResolution: HabitMissedResolutionSnapshot? = null,
    ) = HabitOccurrenceSnapshot(
        evidence = HabitOccurrenceEvidenceSnapshot(
            id = OCCURRENCE_ID,
            habitId = HABIT_ID,
            plannerOccurrenceId = PLANNER_OCCURRENCE_ID,
            sourceScheduleRevisionId = SCHEDULE_REVISION_ID,
            sourceItemRevision = 7,
            policyFingerprint = requireNotNull(canonicalHabit().habitPolicyFingerprintOrNull()),
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

    private fun missedResolution(
        revision: Long = 1,
        action: HabitMissedResolutionActionSnapshot =
            HabitMissedResolutionActionSnapshot.DecisionRequired,
        updatedAt: String = "2026-09-01T09:01:00Z",
    ) = HabitMissedResolutionSnapshot(
        occurrenceEvidenceId = OCCURRENCE_ID,
        habitId = HABIT_ID,
        sourcePlannerOccurrenceId = PLANNER_OCCURRENCE_ID,
        revision = revision,
        configuredPolicy = HabitMissedPolicySnapshot.ASK,
        action = action,
        createdAt = "2026-09-01T09:01:00Z",
        updatedAt = updatedAt,
    )

    private fun reducedTargetOccurrence(
        outcome: HabitOutcomeSnapshot? = null,
    ): HabitOccurrenceSnapshot {
        val source = occurrence()
        return source.copy(
            evidence = source.evidence.copy(
                id = REDUCED_OCCURRENCE_ID,
                plannerOccurrenceId = REDUCED_PLANNER_OCCURRENCE_ID,
                identity = JsonObject(
                    mapOf(
                        "type" to JsonPrimitive("calendar_day"),
                        "date" to JsonPrimitive("2026-09-02"),
                        "bucket_ordinal" to JsonPrimitive(0),
                    ),
                ),
                nominalStart = "2026-09-02T07:00:00Z",
                nominalEnd = "2026-09-02T07:30:00Z",
                windowStart = "2026-09-02T06:00:00Z",
                windowEnd = "2026-09-02T09:00:00Z",
                localDate = "2026-09-02",
            ),
            outcome = outcome,
        )
    }

    private fun completedOutcome(revision: Long = 1) = HabitOutcomeSnapshot(
        revision = revision,
        status = HabitOutcomeStatusSnapshot.COMPLETED,
        progressBasisPoints = 10_000,
        quantity = 20,
        unit = "pages",
        actualSeconds = 1_700,
        note = null,
        occurredAt = "2026-09-01T07:30:00Z",
        updatedAt = "2026-09-01T07:31:00Z",
    )

    private fun pendingOutcome(
        operationId: String,
        expectedRevision: Long,
        status: HabitOutcomeStatusSnapshot,
        progressBasisPoints: Int,
        note: String? = null,
        occurredAt: String = "2026-09-01T07:30:00Z",
    ): PendingHabitMutation {
        val command = HabitOutcomeCommandSnapshot(
            operationId = operationId,
            expectedRevision = expectedRevision,
            outcome = HabitOutcomeInputSnapshot(
                status = status,
                progressBasisPoints = progressBasisPoints,
                quantity = if (status == HabitOutcomeStatusSnapshot.PARTIAL ||
                    status == HabitOutcomeStatusSnapshot.COMPLETED) 8 else null,
                unit = if (status == HabitOutcomeStatusSnapshot.PARTIAL ||
                    status == HabitOutcomeStatusSnapshot.COMPLETED) "pages" else null,
                actualSeconds = if (status == HabitOutcomeStatusSnapshot.PARTIAL ||
                    status == HabitOutcomeStatusSnapshot.COMPLETED) 600 else null,
                note = note,
                occurredAt = occurredAt,
            ),
        )
        return pending(
            operationId,
            PendingHabitMutationKind.OUTCOME,
            OCCURRENCE_ID,
            expectedRevision,
            command.encoded(),
        )
    }

    private fun pending(
        operationId: String,
        kind: PendingHabitMutationKind,
        targetId: String,
        expectedRevision: Long,
        requestJson: String,
    ) = PendingHabitMutation(
        schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
        kind = kind,
        habitId = HABIT_ID,
        targetId = targetId,
        expectedRevision = expectedRevision,
        idempotencyKey = operationId,
        requestJson = requestJson,
        createdAt = "2026-09-01T07:29:00Z",
        syncOrigin = ORIGIN,
        configurationId = CONFIGURATION_ID,
    )

    private fun pause(
        revision: Long = 1,
        endedAt: String? = null,
    ) = HabitPauseSnapshot(
        id = PAUSE_ID,
        habitId = HABIT_ID,
        revision = revision,
        startedAt = "2026-09-02T08:00:00Z",
        endedAt = endedAt,
        preservesStreak = true,
        createdAt = "2026-09-02T08:00:00Z",
        updatedAt = endedAt ?: "2026-09-02T08:00:00Z",
    )

    private fun retentionOccurrence(
        index: Int,
        outcome: HabitOutcomeSnapshot? = null,
    ): HabitOccurrenceSnapshot {
        val date = LocalDate.of(1990, 1, 1).plusDays(index.toLong())
        return HabitOccurrenceSnapshot(
            evidence = HabitOccurrenceEvidenceSnapshot(
                id = numberedUuid(index, version = 4),
                habitId = HABIT_ID,
                plannerOccurrenceId = numberedUuid(index, version = 5),
                sourceScheduleRevisionId = SCHEDULE_REVISION_ID,
                sourceItemRevision = 7,
                policyFingerprint = "sha256:${"a".repeat(64)}",
                identity = JsonObject(
                    mapOf(
                        "type" to JsonPrimitive("calendar_day"),
                        "date" to JsonPrimitive(date.toString()),
                        "bucket_ordinal" to JsonPrimitive(0),
                    ),
                ),
                nominalStart = "${date}T07:00:00Z",
                nominalEnd = "${date}T07:30:00Z",
                windowStart = "${date}T06:00:00Z",
                windowEnd = "${date}T09:00:00Z",
                localDate = date.toString(),
                timezoneName = "Europe/Paris",
                expectedDurationSeconds = 1_800,
                expectedQuantity = 20,
                expectedUnit = "pages",
            ),
            outcome = outcome,
        )
    }

    private fun retentionCompletedOutcome(index: Int): HabitOutcomeSnapshot {
        val date = LocalDate.of(1990, 1, 1).plusDays(index.toLong())
        return HabitOutcomeSnapshot(
            revision = 1,
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressBasisPoints = 10_000,
            quantity = 20,
            unit = "pages",
            actualSeconds = 1_700,
            note = null,
            occurredAt = "${date}T07:30:00Z",
            updatedAt = "${date}T07:31:00Z",
        )
    }

    private fun retentionReduction(
        source: HabitOccurrenceSnapshot,
        target: HabitOccurrenceSnapshot,
    ): HabitOccurrenceSnapshot = source.copy(
        missedResolution = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = source.evidence.id,
            habitId = source.evidence.habitId,
            sourcePlannerOccurrenceId = source.evidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.ReduceFrequency(
                listOf(target.evidence.plannerOccurrenceId),
            ),
            createdAt = source.evidence.windowEnd,
            updatedAt = source.evidence.windowEnd,
        ),
    )

    private fun retentionSkippedResolution(
        occurrence: HabitOccurrenceSnapshot,
    ): HabitMissedResolutionSnapshot {
        val date = occurrence.evidence.localDate
        return HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = occurrence.evidence.id,
            habitId = occurrence.evidence.habitId,
            sourcePlannerOccurrenceId = occurrence.evidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.Skip,
            createdAt = "${date}T09:01:00Z",
            updatedAt = "${date}T09:01:00Z",
        )
    }

    private fun retentionPause(index: Int, open: Boolean): HabitPauseSnapshot {
        val date = LocalDate.of(2000, 1, 1).plusDays(index.toLong())
        val startedAt = "${date}T08:00:00Z"
        val endedAt = if (open) null else "${date}T09:00:00Z"
        return HabitPauseSnapshot(
            id = numberedUuid(index, version = 4),
            habitId = numberedUuid(20_000 + index, version = 4),
            revision = if (open) 1 else 2,
            startedAt = startedAt,
            endedAt = endedAt,
            preservesStreak = true,
            createdAt = startedAt,
            updatedAt = endedAt ?: startedAt,
        )
    }

    private fun numberedUuid(index: Int, version: Int): String {
        val prefix = index.toString(16).padStart(8, '0')
        val suffix = index.toString(16).padStart(12, '0')
        return "$prefix-0000-${version}000-8000-$suffix"
    }

    private fun emptyAnalytics(habitId: String = HABIT_ID) = HabitAnalyticsSnapshot(
        habitId = habitId,
        startDate = "2026-09-01",
        endDate = "2026-09-01",
        bucket = HabitAnalyticsBucketSnapshot.DAY,
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
        supportiveFactCodes = listOf(HabitSupportiveFactCodeSnapshot.NO_DATA),
    )

    private companion object {
        const val ORIGIN = "https://api.example.test/tenant/"
        const val CONFIGURATION_ID = "habit-binding"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val REDUCED_OCCURRENCE_ID = "99999999-9999-4999-8999-999999999999"
        const val REDUCED_PLANNER_OCCURRENCE_ID = "88888888-8888-5888-8888-888888888888"
        const val CHAIN_TARGET_OCCURRENCE_ID = "12121212-1212-4212-8212-121212121212"
        const val CHAIN_TARGET_PLANNER_OCCURRENCE_ID = "13131313-1313-5313-8313-131313131313"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PAUSE_ID = "55555555-5555-4555-8555-555555555555"
        const val SECOND_PAUSE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val OTHER_HABIT_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val OPERATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SECOND_OPERATION_ID = "77777777-7777-4777-8777-777777777777"
    }
}
