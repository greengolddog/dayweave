package com.greengolddog.dayweave.model

import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PlannerModelsTest {
    @Test
    fun fallBackRangeShowsBothOffsetsAndSortsByInstant() {
        val earlier = item(
            id = "earlier",
            start = "2026-10-25T00:50:00Z",
            end = "2026-10-25T01:05:00Z",
        )
        val later = item(
            id = "later",
            start = "2026-10-25T01:10:00Z",
            end = "2026-10-25T01:20:00Z",
        )
        val spanning = item(
            id = "spanning",
            start = "2026-10-25T00:10:00Z",
            end = "2026-10-25T01:20:00Z",
        )

        assertTrue(spanning.timeRange().contains("+02:00"))
        assertTrue(spanning.timeRange().contains("+01:00"))
        // Local labels are 02:50 then 02:10; the timeline must still follow real instants.
        assertEquals(
            listOf("earlier", "later"),
            DayWeaveUiState(schedule = listOf(later, earlier)).visibleSchedule.map { it.id },
        )
    }

    @Test
    fun springForwardRangeShowsBothOffsets() {
        val spanning = item(
            id = "spring",
            start = "2026-03-29T00:50:00Z",
            end = "2026-03-29T01:10:00Z",
        )

        assertTrue(spanning.timeRange().contains("+01:00"))
        assertTrue(spanning.timeRange().contains("+02:00"))
    }

    @Test
    fun currentPlanRequiresTheCurrentDeviceZone() {
        val state = DayWeaveUiState(
            canonicalSyncOrigin = "https://api.example.test/",
            scheduleGeneratedAt = "2026-09-01T07:00:00Z",
            schedulePlanningZoneId = "Europe/Madrid",
        )

        assertTrue(
            state.isCanonicalPlanCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
            ),
        )
        assertFalse(
            state.isCanonicalPlanCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("America/Los_Angeles"),
            ),
        )
    }

    private fun item(id: String, start: String, end: String) = ScheduleItem(
        id = id,
        title = id,
        kind = ItemKind.TASK,
        startMinute = 0,
        durationMinutes = 10,
        status = ItemStatus.SCHEDULED,
        absoluteStartAt = start,
        absoluteEndAt = end,
        planningZoneId = "Europe/Madrid",
    )
}
