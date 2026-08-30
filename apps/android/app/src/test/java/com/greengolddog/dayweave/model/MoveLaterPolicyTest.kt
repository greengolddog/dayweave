package com.greengolddog.dayweave.model

import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MoveLaterPolicyTest {
    @Test
    fun oneShotMoveWarnsAboutDeadlineButDoesNotClaimExactFixedOverlap() {
        val source = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        )
        val hard = block(
            id = HARD_BLOCK_ID,
            start = "2026-09-01T10:15:00Z",
            end = "2026-09-01T10:45:00Z",
        ).copy(
            title = "Doctor appointment",
            kind = ItemKind.EVENT,
            canonicalItemId = HARD_ITEM_ID,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "calendar_event",
        )
        val state = state(source, hard).copy(
            canonicalItems = listOf(
                item().copy(deadlineAt = "2026-09-01T10:30:00Z"),
            ),
        )

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertEquals(Instant.parse("2026-09-01T11:00:00Z"), assessment.targetEnd)
        assertTrue(assessment.crossesDeadline)
        assertEquals(assessment.targetEnd, assessment.canonicalDeadlineRelaxation)
        assertEquals(emptyList<ScheduleItem>(), assessment.overlappingHardBlocks)
        assertFalse(assessment.placementIsExact)
        assertTrue(assessment.requiresConfirmation)
    }

    @Test
    fun pinnedCanonicalSourceIsRepresentableOnlyThroughExplicitApprovalEnvelope() {
        val pinned = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(
            status = ItemStatus.ACTIVE,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        val lease = CanonicalExecutionSessionSnapshot(
            id = SESSION_ID,
            itemId = ITEM_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "active",
            revision = 1,
            accumulatedSeconds = 0,
            startedAt = "2026-09-01T08:00:00Z",
            runningSince = "2026-09-01T08:00:00Z",
            createdAt = "2026-09-01T08:00:00Z",
            updatedAt = "2026-09-01T08:00:00Z",
        )
        val assessment = requireNotNull(
            state(pinned).copy(canonicalExecutionSession = lease).assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T08:15:00Z"),
            ),
        )

        assertTrue(assessment.sourceRequiresOverride)
        assertFalse(assessment.isCoveredBy(null))
        assertTrue(assessment.isCoveredBy(assessment.toApprovalEnvelope()))
    }

    @Test
    fun approvalEnvelopeRejectsNewHardConflictAndAcceptsAResolvedConflict() {
        val source = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(status = ItemStatus.PAUSED)
        val lease = CanonicalExecutionSessionSnapshot(
            id = SESSION_ID,
            itemId = ITEM_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            startedAt = "2026-09-01T08:00:00Z",
            pausedAt = "2026-09-01T08:00:00Z",
            createdAt = "2026-09-01T08:00:00Z",
            updatedAt = "2026-09-01T08:00:00Z",
        )
        fun conflict(id: String, start: String, end: String) = block(id, start, end).copy(
            kind = ItemKind.EVENT,
            canonicalItemId = null,
            canonicalRevision = null,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "external_fixed",
        )
        val reviewed = conflict(
            HARD_BLOCK_ID,
            "2026-09-01T10:10:00Z",
            "2026-09-01T10:20:00Z",
        )
        val arrived = conflict(
            "66666666-6666-4666-8666-666666666666",
            "2026-09-01T10:30:00Z",
            "2026-09-01T10:40:00Z",
        )
        val moveStart = Instant.parse("2026-09-01T10:00:00Z")
        val referenceNow = Instant.parse("2026-09-01T07:00:00Z")
        val reviewedAssessment = requireNotNull(
            state(source, reviewed).copy(canonicalExecutionSession = lease)
                .assessMoveLater(BLOCK_ID, moveStart, referenceNow),
        )
        val approval = reviewedAssessment.toApprovalEnvelope()
        val newRiskAssessment = requireNotNull(
            state(source, reviewed, arrived).copy(canonicalExecutionSession = lease)
                .assessMoveLater(BLOCK_ID, moveStart, referenceNow),
        )
        val mutatedSameIdAssessment = requireNotNull(
            state(
                source,
                reviewed.copy(
                    absoluteStartAt = "2026-09-01T10:25:00Z",
                    absoluteEndAt = "2026-09-01T10:35:00Z",
                    isSensitive = true,
                ),
            ).copy(canonicalExecutionSession = lease)
                .assessMoveLater(BLOCK_ID, moveStart, referenceNow),
        )
        val resolvedRiskAssessment = requireNotNull(
            state(source).copy(canonicalExecutionSession = lease)
                .assessMoveLater(BLOCK_ID, moveStart, referenceNow),
        )

        assertFalse(newRiskAssessment.isCoveredBy(approval))
        assertFalse(mutatedSameIdAssessment.isCoveredBy(approval))
        assertTrue(resolvedRiskAssessment.isCoveredBy(approval))
    }

    @Test
    fun softSchedulerDeadlineRequiresConfirmationWithoutBlockingMove() {
        val state = state(
            block(
                id = BLOCK_ID,
                start = "2026-09-01T08:00:00Z",
                end = "2026-09-01T09:00:00Z",
            ),
        ).copy(
            canonicalItems = listOf(
                item().copy(
                    flexibleConstraintsJson =
                        """{"constraints":{"latest_finish":{"value":"2026-09-01T12:30:00+02:00","strength":{"level":"soft","weight":25}}}}""",
                ),
            ),
        )

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertEquals(Instant.parse("2026-09-01T10:30:00Z"), assessment.deadline)
        assertTrue(assessment.crossesDeadline)
        assertFalse(assessment.deadlineIsHard)
        assertFalse(assessment.crossesUnrelaxableHardDeadline)
        assertEquals(null, assessment.canonicalDeadlineRelaxation)
        assertTrue(assessment.requiresConfirmation)
    }

    @Test
    fun hardSchedulerDeadlineFailsClosedWhenOccurrenceMoveCannotRelaxIt() {
        val state = state(
            block(
                id = BLOCK_ID,
                start = "2026-09-01T08:00:00Z",
                end = "2026-09-01T09:00:00Z",
            ).copy(occurrenceId = OCCURRENCE_ID),
        ).copy(
            canonicalItems = listOf(
                item().copy(
                    flexibleConstraintsJson =
                        """{"constraints":{"latest_finish":{"value":"2026-09-01T10:30:00Z","strength":{"level":"hard"}}}}""",
                ),
            ),
        )

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertTrue(assessment.deadlineIsHard)
        assertTrue(assessment.crossesUnrelaxableHardDeadline)
        assertEquals(null, assessment.canonicalDeadlineRelaxation)
    }

    @Test
    fun eachOccurrenceLeafUsesItsOwnShiftedEndAndHardDeadlineCannotBeMasked() {
        val leafA = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(occurrenceId = OCCURRENCE_ID)
        val leafB = block(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T09:00:00Z",
            end = "2026-09-01T10:00:00Z",
        ).copy(
            canonicalItemId = SECOND_ITEM_ID,
            occurrenceId = OCCURRENCE_ID,
            sessionIndex = 1,
        )
        val softLeaf = item().copy(
            flexibleConstraintsJson =
                """{"constraints":{"latest_finish":{"value":"2026-09-01T10:30:00Z","strength":{"level":"soft","weight":25}}}}""",
        )
        val hardLeaf = item().copy(
            id = SECOND_ITEM_ID,
            title = "Hard-deadline leaf",
            flexibleConstraintsJson =
                """{"constraints":{"latest_finish":{"value":"2026-09-01T11:30:00Z","strength":{"level":"hard"}}}}""",
        )
        val state = DayWeaveUiState(
            schedule = listOf(leafA, leafB),
            canonicalItems = listOf(softLeaf, hardLeaf),
            scheduleGeneratedAt = "2026-09-01T07:00:00Z",
            schedulePlanningZoneId = "UTC",
        )

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertEquals(2, assessment.crossedDeadlines.size)
        assertEquals(
            mapOf(
                ITEM_ID to "2026-09-01T12:00:00Z",
                SECOND_ITEM_ID to "2026-09-01T12:00:00Z",
            ),
            assessment.crossedDeadlines.associate { it.itemId to it.targetEnd },
        )
        assertTrue(assessment.crossedDeadlines.any { it.itemId == SECOND_ITEM_ID && it.isHard })
        assertTrue(assessment.crossesUnrelaxableHardDeadline)
        assertEquals(MoveLaterPlacementMode.RECOMPOSED_WINDOW, assessment.placementMode)
        assertEquals(null, assessment.canonicalDeadlineRelaxation)
    }

    @Test
    fun malformedSchedulerDeadlineFailsClosed() {
        val state = state(
            block(
                id = BLOCK_ID,
                start = "2026-09-01T08:00:00Z",
                end = "2026-09-01T09:00:00Z",
            ),
        ).copy(
            canonicalItems = listOf(
                item().copy(
                    flexibleConstraintsJson =
                        """{"constraints":{"latest_finish":{"value":"tomorrow","strength":{"level":"hard"}}}}""",
                ),
            ),
        )

        assertEquals(
            null,
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )
    }

    @Test
    fun pausedExecutionWarningUsesExactRemainingSeconds() {
        val source = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(status = ItemStatus.PAUSED)
        val session = CanonicalExecutionSessionSnapshot(
            id = SESSION_ID,
            itemId = ITEM_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "paused",
            revision = 2,
            accumulatedSeconds = 900,
            startedAt = "2026-09-01T08:00:00Z",
            pausedAt = "2026-09-01T08:15:00Z",
            createdAt = "2026-09-01T08:00:00Z",
            updatedAt = "2026-09-01T08:15:00Z",
        )
        val state = state(source).copy(
            canonicalExecutionSession = session,
        )

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T08:20:00Z"),
            ),
        )

        assertEquals(Instant.parse("2026-09-01T10:45:00Z"), assessment.targetEnd)
        assertFalse(assessment.requiresConfirmation)
        assertTrue(assessment.fitsSinglePlanningDay)
    }

    @Test
    fun recurringOccurrenceMoveMustFitInsideOnePlanningDay() {
        val focused = block(
            id = BLOCK_ID,
            start = "2026-09-01T09:00:00Z",
            end = "2026-09-01T10:00:00Z",
        ).copy(occurrenceId = OCCURRENCE_ID)
        val sibling = block(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T10:00:00Z",
            end = "2026-09-01T11:00:00Z",
        ).copy(occurrenceId = OCCURRENCE_ID, sessionIndex = 1)
        val state = state(focused, sibling)

        val assessment = requireNotNull(
            state.assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T23:30:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertEquals(Instant.parse("2026-09-02T01:30:00Z"), assessment.targetEnd)
        assertFalse(assessment.fitsSinglePlanningDay)
    }

    @Test
    fun recurrenceMoveAssessesARecomposedWindowNotExactLeafConflicts() {
        val occurrence = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(occurrenceId = OCCURRENCE_ID)
        val fixed = block(
            id = HARD_BLOCK_ID,
            start = "2026-09-01T10:15:00Z",
            end = "2026-09-01T10:45:00Z",
        ).copy(
            kind = ItemKind.EVENT,
            canonicalItemId = null,
            canonicalRevision = null,
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "external_fixed",
        )

        val assessment = requireNotNull(
            state(occurrence, fixed).assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T10:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertEquals(MoveLaterPlacementMode.RECOMPOSED_WINDOW, assessment.placementMode)
        assertEquals(emptyList<ScheduleItem>(), assessment.overlappingHardBlocks)
        assertFalse(assessment.requiresConfirmation)
    }

    @Test
    fun exactExecutionMoveCannotUseAnUnloadedTargetPlanningDay() {
        val source = block(
            id = BLOCK_ID,
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-01T09:00:00Z",
        ).copy(status = ItemStatus.PAUSED)
        val lease = CanonicalExecutionSessionSnapshot(
            id = SESSION_ID,
            itemId = ITEM_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "paused",
            revision = 2,
            accumulatedSeconds = 0,
            startedAt = "2026-09-01T08:00:00Z",
            pausedAt = "2026-09-01T08:00:00Z",
            createdAt = "2026-09-01T08:00:00Z",
            updatedAt = "2026-09-01T08:00:00Z",
        )
        val assessment = requireNotNull(
            state(source).copy(canonicalExecutionSession = lease).assessMoveLater(
                BLOCK_ID,
                Instant.parse("2026-09-02T09:00:00Z"),
                Instant.parse("2026-09-01T07:00:00Z"),
            ),
        )

        assertFalse(assessment.fitsSinglePlanningDay)
        assertEquals(MoveLaterPlacementMode.EXACT, assessment.placementMode)
    }

    private fun state(vararg blocks: ScheduleItem) = DayWeaveUiState(
        schedule = blocks.toList(),
        canonicalItems = listOf(item()),
        scheduleGeneratedAt = "2026-09-01T07:00:00Z",
        schedulePlanningZoneId = "UTC",
    )

    private fun block(id: String, start: String, end: String) = ScheduleItem(
        id = id,
        title = "Focused work",
        kind = ItemKind.TASK,
        startMinute = 8 * 60,
        durationMinutes = 60,
        status = ItemStatus.SCHEDULED,
        canonicalItemId = ITEM_ID,
        canonicalRevision = 7,
        sessionIndex = 0,
        absoluteStartAt = start,
        absoluteEndAt = end,
        planningZoneId = "UTC",
        canonicalBlockKind = "planned",
    )

    private fun item() = CanonicalItemSnapshot(
        id = ITEM_ID,
        kind = "task",
        status = "planned",
        title = "Focused work",
        timezoneName = "UTC",
        durationSeconds = 3_600,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-09-01T00:00:00Z",
        updatedAt = "2026-09-01T00:00:00Z",
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val SECOND_ITEM_ID = "99999999-9999-4999-8999-999999999999"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_BLOCK_ID = "33333333-3333-4333-8333-333333333333"
        const val HARD_ITEM_ID = "44444444-4444-4444-8444-444444444444"
        const val HARD_BLOCK_ID = "55555555-5555-4555-8555-555555555555"
        const val OCCURRENCE_ID = "66666666-6666-5666-8666-666666666666"
        const val SESSION_ID = "77777777-7777-4777-8777-777777777777"
        const val DEVICE_ID = "88888888-8888-4888-8888-888888888888"
    }
}
