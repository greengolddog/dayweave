package com.greengolddog.dayweave.ui.components

import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferConflictSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferViolationSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.MoveLaterPlacementMode
import com.greengolddog.dayweave.model.ScheduleItem
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MoveLaterChooserTest {
    @Test
    fun presetsRoundForwardAndKeepTomorrowMorningInTheLocalZone() {
        val now = Instant.parse("2026-08-30T18:30:20Z")
        val presets = moveLaterPresets(now, ZoneId.of("Europe/Madrid"), true)

        assertEquals(listOf("In 1 hour", "In 3 hours", "Tomorrow morning"), presets.map { it.label })
        assertEquals(Instant.parse("2026-08-30T19:31:00Z"), presets[0].moveStart)
        assertEquals(Instant.parse("2026-08-30T21:31:00Z"), presets[1].moveStart)
        assertEquals(Instant.parse("2026-08-31T07:00:00Z"), presets[2].moveStart)
    }

    @Test
    fun presetDetailsFollowTheSelectedClockConvention() {
        val now = Instant.parse("2026-08-30T18:30:20Z")
        val zone = ZoneId.of("Europe/Madrid")

        val twentyFourHour = moveLaterPresets(now, zone, true).first().detail
        val twelveHour = moveLaterPresets(now, zone, false).first().detail

        org.junit.Assert.assertTrue(twentyFourHour.contains("21:31"))
        org.junit.Assert.assertTrue(twelveHour.contains("9:31"))
        org.junit.Assert.assertFalse(twelveHour.contains("21:31"))
    }

    @Test
    fun loadedPlanningDayFiltersTomorrowAndAnyCrossDayPreset() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-08-30T18:30:20Z")

        val presets = moveLaterPresets(
            now = now,
            zoneId = zone,
            use24HourFormat = true,
            loadedPlanningDate = LocalDate.of(2026, 8, 30),
        )

        assertEquals(listOf("In 1 hour", "In 3 hours"), presets.map { it.label })
    }

    @Test
    fun customSelectionRejectsNearPastAndReturnsAnExactMinute() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-08-30T18:30:20Z")

        assertNull(
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(20, 31),
                zone,
                now,
            ),
        )
        assertEquals(
            Instant.parse("2026-08-30T19:45:00Z"),
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(21, 45, 59),
                zone,
                now,
            ),
        )
    }

    @Test
    fun customSelectionRejectsDstGapAndOverlapInsteadOfSilentlyNormalizing() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-01-01T00:00:00Z")

        assertNull(
            customMoveStart(
                LocalDate.of(2026, 3, 29),
                LocalTime.of(2, 30),
                zone,
                now,
            ),
        )
        assertNull(
            customMoveStart(
                LocalDate.of(2026, 10, 25),
                LocalTime.of(2, 30),
                zone,
                now,
            ),
        )
    }

    @Test
    fun executionTargetsRequireFiveMinuteSlotsAndTtlPlusOneSlotLead() {
        val now = Instant.parse("2026-08-30T18:30:00Z")
        val zone = ZoneId.of("Europe/Madrid")

        assertNull(
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(20, 39),
                zone,
                now,
                serverAuthoritativeExecution = true,
            ),
        )
        assertNull(
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(20, 41),
                zone,
                now,
                serverAuthoritativeExecution = true,
            ),
        )
        assertEquals(
            Instant.parse("2026-08-30T18:40:00Z"),
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(20, 40),
                zone,
                now,
                serverAuthoritativeExecution = true,
            ),
        )
        assertEquals(
            Instant.parse("2026-08-30T19:35:00Z"),
            moveLaterPresets(
                Instant.parse("2026-08-30T18:30:20Z"),
                zone,
                true,
                serverAuthoritativeExecution = true,
            ).first().moveStart,
        )
    }

    @Test
    fun sensitiveConflictIsRedactedAndForcesASecureWarningWindow() {
        val public = conflict("Team meeting", isSensitive = false)
        val sensitive = conflict("Private diagnosis", isSensitive = true)

        val presentation = moveLaterConflictPresentation(listOf(public, sensitive))

        assertEquals(listOf("Team meeting", "Sensitive busy time"), presentation.labels)
        org.junit.Assert.assertFalse(presentation.labels.any { "diagnosis" in it })
        org.junit.Assert.assertTrue(presentation.requiresSecureWindow)
    }

    @Test
    fun sensitiveMovedItemForcesSecureChooserWithoutAnyConflict() {
        val noConflicts = moveLaterConflictPresentation(emptyList())

        org.junit.Assert.assertTrue(
            moveLaterRequiresSecureWindow(itemIsSensitive = true, conflicts = noConflicts),
        )
        org.junit.Assert.assertFalse(
            moveLaterRequiresSecureWindow(itemIsSensitive = false, conflicts = noConflicts),
        )
    }

    @Test
    fun oneShotCopyPromisesOnlyAnEarliestSchedulingTime() {
        assertEquals(
            "DayWeave will allow scheduling this work from the selected time, then recompose your day.",
            moveLaterChooserExplanation(MoveLaterPlacementMode.EARLIEST_START),
        )
        assertEquals(
            "DayWeave will preserve the earliest start and deadline change you approve.",
            moveLaterConfirmationPromise(MoveLaterPlacementMode.EARLIEST_START),
        )
        org.junit.Assert.assertFalse(
            moveLaterChooserExplanation(MoveLaterPlacementMode.EARLIEST_START).contains("exact"),
        )
    }

    @Test
    fun recurrenceCopyPromisesRecompositionInsideAWindowNotExactLeaves() {
        val explanation = moveLaterChooserExplanation(
            MoveLaterPlacementMode.RECOMPOSED_WINDOW,
        )

        org.junit.Assert.assertTrue(explanation.contains("recompose"))
        org.junit.Assert.assertTrue(explanation.contains("window"))
        org.junit.Assert.assertFalse(explanation.contains("exact"))
    }

    @Test
    fun authoritativeWarningPresentationUsesOnlyContentFreeMessagesAndSecuresSensitiveConflicts() {
        val sensitiveConflictId = "22222222-2222-4222-8222-222222222222"
        val assessment = ExecutionDeferAssessmentSnapshot(
            sessionId = "11111111-1111-4111-8111-111111111111",
            executionRevision = 4,
            sessionRevision = 2,
            itemId = "33333333-3333-4333-8333-333333333333",
            itemRevision = 7,
            sourceSessionIndex = 0,
            replacementSessionIndex = 1,
            sourceScheduleRevisionId = "44444444-4444-4444-8444-444444444444",
            sourceBlockId = "55555555-5555-4555-8555-555555555555",
            actualSeconds = 120,
            creditedSourceSeconds = 120,
            plannedDurationSeconds = 1_800,
            remainingDurationSeconds = 1_680,
            moveStart = "2026-09-01T08:00:00Z",
            moveEnd = "2026-09-01T08:28:00Z",
            environmentDigest = "sha256:${"a".repeat(64)}",
            assessmentDigest = "sha256:${"b".repeat(64)}",
            approvalRequired = true,
            violations = listOf(
                ExecutionDeferViolationSnapshot(
                    code = "immutable_overlap",
                    itemIds = emptyList(),
                    occurrenceIds = emptyList(),
                    conflictingBlockIds = listOf(sensitiveConflictId),
                    conflictingBlocks = listOf(
                        ExecutionDeferConflictSnapshot(
                            blockId = sensitiveConflictId,
                            kind = "calendar_event",
                            start = "2026-09-01T08:05:00Z",
                            end = "2026-09-01T08:20:00Z",
                        ),
                    ),
                    start = "2026-09-01T08:00:00Z",
                    end = "2026-09-01T08:28:00Z",
                    message = "The placement overlaps immutable scheduled time.",
                ),
            ),
            expiresAt = "2026-09-01T07:05:00Z",
        )

        val presentation = executionDeferWarningPresentation(
            assessment = assessment,
            sourceIsSensitive = false,
            sensitiveBlockIds = setOf(sensitiveConflictId),
        )

        assertEquals(
            listOf("immutable_overlap: The placement overlaps immutable scheduled time."),
            presentation.messages,
        )
        assertEquals(1, presentation.conflictingBlockCount)
        org.junit.Assert.assertTrue(presentation.requiresSecureWindow)
        org.junit.Assert.assertFalse(presentation.messages.any { "Private diagnosis" in it })
    }

    @Test
    fun authoritativeWarningPresentationNeverSilentlyTruncatesSevenDistinctRestrictions() {
        val violations = (1..7).map { index ->
            ExecutionDeferViolationSnapshot(
                code = if (index % 2 == 0) "dependency" else "outside_availability",
                itemIds = emptyList(),
                occurrenceIds = emptyList(),
                conflictingBlockIds = emptyList(),
                conflictingBlocks = emptyList(),
                start = "2026-09-01T08:00:00Z",
                end = "2026-09-01T08:28:00Z",
                message = "Restriction $index",
            )
        }
        val assessment = ExecutionDeferAssessmentSnapshot(
            sessionId = "11111111-1111-4111-8111-111111111111",
            executionRevision = 4,
            sessionRevision = 2,
            itemId = "33333333-3333-4333-8333-333333333333",
            itemRevision = 7,
            sourceSessionIndex = 0,
            replacementSessionIndex = 1,
            sourceScheduleRevisionId = "44444444-4444-4444-8444-444444444444",
            sourceBlockId = "55555555-5555-4555-8555-555555555555",
            actualSeconds = 120,
            creditedSourceSeconds = 120,
            plannedDurationSeconds = 1_800,
            remainingDurationSeconds = 1_680,
            moveStart = "2026-09-01T08:00:00Z",
            moveEnd = "2026-09-01T08:28:00Z",
            environmentDigest = "sha256:${"a".repeat(64)}",
            assessmentDigest = "sha256:${"b".repeat(64)}",
            approvalRequired = true,
            violations = violations,
            expiresAt = "2026-09-01T07:05:00Z",
        )

        val presentation = executionDeferWarningPresentation(
            assessment,
            sourceIsSensitive = false,
            sensitiveBlockIds = emptySet(),
        )

        assertEquals(7, presentation.messages.size)
        assertEquals(
            violations.map { "${it.code}: ${it.message}" },
            presentation.messages,
        )
    }

    private fun conflict(title: String, isSensitive: Boolean) = ScheduleItem(
        id = title,
        title = title,
        kind = ItemKind.EVENT,
        startMinute = 9 * 60,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        isSensitive = isSensitive,
        isFlexible = false,
        isHardConstraint = true,
    )
}
