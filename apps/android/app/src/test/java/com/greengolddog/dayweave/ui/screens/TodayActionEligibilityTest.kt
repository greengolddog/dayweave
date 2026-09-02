package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TodayActionEligibilityTest {
    @Test
    fun laterIsAvailableOnlyForFlexibleActionableWork() {
        val task = item(ItemKind.TASK)

        assertTrue(task.isMoveLaterEligible())
        assertTrue(item(ItemKind.HABIT).isMoveLaterEligible())
        assertTrue(item(ItemKind.ROUTINE).isMoveLaterEligible())
        assertTrue(item(ItemKind.GOAL).isMoveLaterEligible())
        assertFalse(item(ItemKind.EVENT).isMoveLaterEligible())
        assertFalse(task.copy(isFlexible = false).isMoveLaterEligible())
        assertFalse(task.copy(isHardConstraint = true).isMoveLaterEligible())
        assertTrue(
            task.copy(
                status = ItemStatus.ACTIVE,
                isFlexible = false,
                isHardConstraint = true,
                canonicalBlockKind = "pinned",
            ).isMoveLaterEligible(),
        )
        assertFalse(
            task.copy(
                isFlexible = false,
                isHardConstraint = true,
                canonicalBlockKind = "planned",
            ).isMoveLaterEligible(),
        )
        assertFalse(
            task.copy(
                isFlexible = false,
                isHardConstraint = true,
                canonicalBlockKind = "pinned",
            ).isMoveLaterEligible(),
        )
        assertFalse(task.copy(canonicalBlockKind = "external_fixed").isMoveLaterEligible())
        assertFalse(task.copy(canonicalItemId = null).isMoveLaterEligible())
    }

    @Test
    fun localActiveLaterIsHiddenUntilSelectedTimeCanBePersistedDurably() {
        val localActive = item(ItemKind.TASK).copy(
            status = ItemStatus.ACTIVE,
            canonicalItemId = null,
            canonicalRevision = null,
        )

        assertFalse(localActive.isMoveLaterEligible())
    }

    @Test
    fun scheduledSkipIsLimitedToOneShotIndivisibleWork() {
        val task = item(ItemKind.TASK)
        val state = publishedState(schedule = listOf(task))

        assertTrue(state.canSafelySkipScheduled(task))
        assertFalse(DayWeaveUiState(schedule = listOf(task)).canSafelySkipScheduled(task))
        assertFalse(state.canSafelySkipScheduled(task.copy(isSplittable = true)))
        assertFalse(state.canSafelySkipScheduled(task.copy(occurrenceId = "occurrence")))
        assertFalse(
            state.copy(schedule = listOf(task, task.copy(id = SECOND_BLOCK_ID)))
                .canSafelySkipScheduled(task),
        )
    }

    @Test
    fun scheduledLaterRejectsUnsafeOneShotSplitsButAllowsAnOccurrenceMove() {
        val task = item(ItemKind.TASK)
        val state = publishedState(schedule = listOf(task))

        assertTrue(state.canMoveScheduledLater(task))
        assertFalse(DayWeaveUiState(schedule = listOf(task)).canMoveScheduledLater(task))
        assertFalse(state.canMoveScheduledLater(task.copy(isSplittable = true)))
        assertFalse(
            state.copy(schedule = listOf(task, task.copy(id = SECOND_BLOCK_ID)))
                .canMoveScheduledLater(task),
        )
        val occurrence = task.copy(occurrenceId = OCCURRENCE_ID, isSplittable = true)
        val dailySource = occurrenceSource(
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
        )
        assertTrue(
            publishedState(
                schedule = listOf(occurrence),
                recurrenceOccurrenceSources = mapOf(OCCURRENCE_ID to dailySource),
            )
                .canMoveScheduledLater(occurrence),
        )
        assertFalse(
            publishedState(
                schedule = listOf(occurrence),
                recurrenceOccurrenceSources = mapOf(
                    OCCURRENCE_ID to occurrenceSource("""{"type":"custom"}"""),
                ),
            ).canMoveScheduledLater(occurrence),
        )
        val pinnedSibling = occurrence.copy(
            id = SECOND_BLOCK_ID,
            sessionIndex = 1,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        assertFalse(
            publishedState(
                schedule = listOf(occurrence, pinnedSibling),
                recurrenceOccurrenceSources = mapOf(OCCURRENCE_ID to dailySource),
            ).canMoveScheduledLater(occurrence),
        )
        assertFalse(
            publishedState(
                schedule = listOf(occurrence),
                recurrenceOccurrenceSources = mapOf(OCCURRENCE_ID to dailySource),
                unscheduledWork = listOf(
                    UnscheduledWorkSnapshot(
                        itemId = "descendant",
                        occurrenceId = OCCURRENCE_ID,
                        remainingMinutes = 15,
                        reason = "no_capacity",
                    ),
                ),
            ).canMoveScheduledLater(occurrence),
        )
    }

    @Test
    fun scheduledSiblingCannotMoveWhileCanonicalLeaseOwnsSplitOccurrence() {
        val focused = item(ItemKind.HABIT).copy(
            occurrenceId = OCCURRENCE_ID,
            isSplittable = true,
        )
        val activeSibling = focused.copy(
            id = SECOND_BLOCK_ID,
            sessionIndex = 1,
        )
        val state = publishedState(
            schedule = listOf(focused, activeSibling),
            recurrenceOccurrenceSources = mapOf(
                OCCURRENCE_ID to occurrenceSource(
                    """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
                ),
            ),
            canonicalExecutionSession = executionLease(activeSibling),
        )

        assertFalse(state.canMoveScheduledLater(focused))
    }

    private fun item(kind: ItemKind) = ScheduleItem(
        id = BLOCK_ID,
        title = "Work",
        kind = kind,
        startMinute = 9 * 60,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        canonicalItemId = ITEM_ID,
        canonicalRevision = 1,
        sessionIndex = 0,
        canonicalBlockKind = "planned",
        absoluteStartAt = "2026-09-01T09:00:00Z",
        absoluteEndAt = "2026-09-01T09:30:00Z",
        planningZoneId = "UTC",
    )

    private fun occurrenceSource(identityJson: String) = RecurrenceOccurrenceSourceSnapshot(
        itemId = ITEM_ID,
        itemRevision = 1,
        identityJson = identityJson,
        nominalStart = "2026-09-01T09:00:00Z",
        nominalEnd = "2026-09-01T09:30:00Z",
        localDate = if ("custom" in identityJson) null else "2026-09-01",
        ordinal = 0,
    )

    private fun executionLease(block: ScheduleItem) = CanonicalExecutionSessionSnapshot(
        id = "77777777-7777-4777-8777-777777777777",
        itemId = requireNotNull(block.canonicalItemId),
        itemRevision = requireNotNull(block.canonicalRevision),
        occurrenceId = block.occurrenceId,
        sessionIndex = requireNotNull(block.sessionIndex),
        plannedBlockId = block.id,
        sourceDeviceId = "88888888-8888-4888-8888-888888888888",
        status = "active",
        revision = 1,
        accumulatedSeconds = 0,
        startedAt = "2026-09-01T07:00:00Z",
        runningSince = "2026-09-01T07:00:00Z",
        createdAt = "2026-09-01T07:00:00Z",
        updatedAt = "2026-09-01T07:00:00Z",
    )

    private fun publishedState(
        schedule: List<ScheduleItem>,
        recurrenceOccurrenceSources: Map<String, RecurrenceOccurrenceSourceSnapshot> = emptyMap(),
        unscheduledWork: List<UnscheduledWorkSnapshot> = emptyList(),
        canonicalExecutionSession: CanonicalExecutionSessionSnapshot? = null,
    ): DayWeaveUiState {
        val revision = PublishedScheduleRevisionSnapshot(
            id = PUBLICATION_ID,
            revision = "1:$PUBLICATION_ID",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-01T00:00:00Z",
            horizonEnd = "2026-09-02T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-09-01T07:00:00Z",
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            revision = revision,
            asOf = "2026-09-01T07:00:00Z",
            blocks = schedule.map { block ->
                PublishedScheduleBlockProofSnapshot.from(block)
            },
        )
        return DayWeaveUiState(
            canonicalItems = listOf(
                CanonicalItemSnapshot(
                    id = ITEM_ID,
                    kind = "task",
                    status = "planned",
                    title = "Work",
                    timezoneName = "UTC",
                    durationSeconds = 1_800,
                    flexibleConstraintsJson = "{}",
                    splitPolicyJson = "{\"type\":\"indivisible\"}",
                    importance = 50,
                    urgency = 50,
                    siblingOrder = 0,
                    isExecutable = true,
                    revision = 1,
                    createdAt = "2026-09-01T07:00:00Z",
                    updatedAt = "2026-09-01T07:00:00Z",
                ),
            ),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-1",
            schedule = schedule,
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = "2026-09-01T07:00:00Z",
            schedulePlanningZoneId = "UTC",
            recurrenceOccurrenceSources = recurrenceOccurrenceSources,
            unscheduledWork = unscheduledWork,
            canonicalExecutionSession = canonicalExecutionSession,
        )
    }

    private companion object {
        const val CANONICAL_ORIGIN = "https://api.example.test/"
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_BLOCK_ID = "33333333-3333-4333-8333-333333333333"
        const val PUBLICATION_ID = "44444444-4444-4444-8444-444444444444"
        const val OCCURRENCE_ID = "66666666-6666-5666-8666-666666666666"
    }
}
