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
        val revision = PublishedScheduleRevisionSnapshot(
            id = "11111111-1111-4111-8111-111111111111",
            revision = "1:11111111-1111-4111-8111-111111111111",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-01T00:00:00Z",
            horizonEnd = "2026-09-02T00:00:00Z",
            timezoneName = "Europe/Madrid",
            publishedAt = "2026-09-01T07:00:00Z",
        )
        val state = DayWeaveUiState(
            canonicalSyncOrigin = "https://api.example.test/",
            canonicalConfigurationId = "connection-1",
            scheduleInputDigest = "sha256:${"a".repeat(64)}",
            scheduleGeneratedAt = "2026-09-01T07:00:00Z",
            schedulePlanningZoneId = "Europe/Madrid",
            publishedScheduleRevision = revision,
            publishedScheduleProof = PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = revision,
                asOf = "2026-09-01T07:00:00Z",
                blocks = emptyList(),
            ),
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
