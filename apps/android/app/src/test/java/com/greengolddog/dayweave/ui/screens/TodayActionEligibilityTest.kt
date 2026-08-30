package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
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
        val state = DayWeaveUiState(schedule = listOf(task))

        assertTrue(state.canSafelySkipScheduled(task))
        assertFalse(state.canSafelySkipScheduled(task.copy(isSplittable = true)))
        assertFalse(state.canSafelySkipScheduled(task.copy(occurrenceId = "occurrence")))
        assertFalse(
            state.copy(schedule = listOf(task, task.copy(id = "second")))
                .canSafelySkipScheduled(task),
        )
    }

    @Test
    fun scheduledLaterRejectsUnsafeOneShotSplitsButAllowsAnOccurrenceMove() {
        val task = item(ItemKind.TASK)
        val state = DayWeaveUiState(schedule = listOf(task))

        assertTrue(state.canMoveScheduledLater(task))
        assertFalse(state.canMoveScheduledLater(task.copy(isSplittable = true)))
        assertFalse(
            state.copy(schedule = listOf(task, task.copy(id = "second")))
                .canMoveScheduledLater(task),
        )
        val occurrence = task.copy(occurrenceId = OCCURRENCE_ID, isSplittable = true)
        val dailySource = occurrenceSource(
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
        )
        assertTrue(
            DayWeaveUiState(
                schedule = listOf(occurrence),
                recurrenceOccurrenceSources = mapOf(OCCURRENCE_ID to dailySource),
            )
                .canMoveScheduledLater(occurrence),
        )
        assertFalse(
            DayWeaveUiState(
                schedule = listOf(occurrence),
                recurrenceOccurrenceSources = mapOf(
                    OCCURRENCE_ID to occurrenceSource("""{"type":"custom"}"""),
                ),
            ).canMoveScheduledLater(occurrence),
        )
        val pinnedSibling = occurrence.copy(
            id = "pinned-sibling",
            sessionIndex = 1,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        assertFalse(
            DayWeaveUiState(
                schedule = listOf(occurrence, pinnedSibling),
                recurrenceOccurrenceSources = mapOf(OCCURRENCE_ID to dailySource),
            ).canMoveScheduledLater(occurrence),
        )
        assertFalse(
            DayWeaveUiState(
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
            id = "active-block",
            sessionIndex = 1,
        )
        val state = DayWeaveUiState(
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
        id = "block",
        title = "Work",
        kind = kind,
        startMinute = 9 * 60,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        canonicalItemId = "item",
        canonicalRevision = 1,
        sessionIndex = 0,
        canonicalBlockKind = "planned",
    )

    private fun occurrenceSource(identityJson: String) = RecurrenceOccurrenceSourceSnapshot(
        itemId = "item",
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

    private companion object {
        const val OCCURRENCE_ID = "66666666-6666-5666-8666-666666666666"
    }
}
