package com.greengolddog.dayweave.model

import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
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
    fun todayProjectionDoesNotExposeTheRestOfAMultiDayReplica() {
        val priorOvernight = item(
            id = "overnight",
            start = "2026-09-01T21:30:00Z",
            end = "2026-09-01T22:30:00Z",
        )
        val today = item(
            id = "today",
            start = "2026-09-02T08:00:00Z",
            end = "2026-09-02T09:00:00Z",
        )
        val tomorrow = item(
            id = "tomorrow",
            start = "2026-09-03T08:00:00Z",
            end = "2026-09-03T09:00:00Z",
        )
        val state = DayWeaveUiState(schedule = listOf(tomorrow, today, priorOvernight))

        assertEquals(
            listOf("overnight", "today"),
            state.visibleScheduleForDay(
                reference = Instant.parse("2026-09-02T12:00:00Z"),
                currentZone = ZoneId.of("Europe/Paris"),
            ).map(ScheduleItem::id),
        )
    }

    @Test
    fun calendarProjectionIncludesOnlyTheDisplayedLocalWeek() {
        val beforeWeek = item(
            id = "before-week",
            start = "2026-08-30T12:00:00Z",
            end = "2026-08-30T13:00:00Z",
        )
        val monday = item(
            id = "monday",
            start = "2026-08-31T12:00:00Z",
            end = "2026-08-31T13:00:00Z",
        )
        val sunday = item(
            id = "sunday",
            start = "2026-09-06T12:00:00Z",
            end = "2026-09-06T13:00:00Z",
        )
        val nextWeek = item(
            id = "next-week",
            start = "2026-09-07T12:00:00Z",
            end = "2026-09-07T13:00:00Z",
        )
        val state = DayWeaveUiState(
            schedule = listOf(nextWeek, sunday, monday, beforeWeek),
        )

        assertEquals(
            listOf("monday", "sunday"),
            state.visibleScheduleForWeek(
                reference = Instant.parse("2026-09-02T12:00:00Z"),
                currentZone = ZoneId.of("Europe/Paris"),
            ).map(ScheduleItem::id),
        )
    }

    @Test
    fun presentationSlicesClipLongBlocksInDeviceZoneWithoutMutatingProofIdentity() {
        val longExternal = item(
            id = "long-external",
            start = "2026-09-01T08:00:00Z",
            end = "2026-09-04T08:00:00Z",
        ).copy(
            durationMinutes = 3 * 24 * 60,
            planningZoneId = "America/Los_Angeles",
            canonicalBlockKind = "external_fixed",
        )
        val state = DayWeaveUiState(schedule = listOf(longExternal))
        val reference = Instant.parse("2026-09-02T12:00:00Z")
        val deviceZone = ZoneId.of("Europe/Paris")

        val daySlice = state.visibleScheduleSlicesForDay(reference, deviceZone).single()
        assertSame(longExternal, daySlice.item)
        assertEquals("2026-09-01T22:00:00Z", daySlice.clippedStart.toString())
        assertEquals("2026-09-02T22:00:00Z", daySlice.clippedEnd.toString())
        assertEquals("00:00", daySlice.startTimeLabel)
        assertEquals(24 * 60, daySlice.durationMinutes)
        assertEquals("All day", daySlice.durationLabel)
        assertEquals("Ongoing all day", daySlice.continuationLabel)
        assertEquals("2026-09-01T08:00:00Z", longExternal.absoluteStartAt)
        assertEquals("2026-09-04T08:00:00Z", longExternal.absoluteEndAt)

        val weekSlice = state.visibleScheduleSlicesForWeek(reference, deviceZone).single()
        assertSame(longExternal, weekSlice.item)
        assertTrue(weekSlice.weekStartLabel.endsWith("10:00"))
        assertEquals(3 * 24 * 60, weekSlice.durationMinutes)
        assertEquals("3d", weekSlice.durationLabel)
        assertEquals("Multi-day", weekSlice.continuationLabel)
        assertTrue(longExternal.timeRange().startsWith("01:00"))
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
        assertTrue(
            state.isPublishedScheduleDisplayCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("America/Los_Angeles"),
            ),
        )
        assertTrue(
            state.isScheduleDisplayCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("America/Los_Angeles"),
            ),
        )
    }

    @Test
    fun publishedReplicaRemainsCurrentAcrossEveryIntersectingHorizonDay() {
        val revision = PublishedScheduleRevisionSnapshot(
            id = "11111111-1111-4111-8111-111111111111",
            revision = "1:11111111-1111-4111-8111-111111111111",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-01T00:00:00Z",
            horizonEnd = "2026-09-10T00:00:00Z",
            timezoneName = "Europe/Madrid",
            publishedAt = "2026-09-01T07:00:00Z",
        )
        val state = DayWeaveUiState(
            canonicalSyncOrigin = "https://api.example.test/",
            canonicalConfigurationId = "connection-1",
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = revision.publishedAt,
            schedulePlanningZoneId = revision.timezoneName,
            publishedScheduleRevision = revision,
            publishedScheduleProof = PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = revision,
                asOf = revision.publishedAt,
                blocks = emptyList(),
            ),
        )

        assertTrue(
            state.isCanonicalPlanCurrent(
                Instant.parse("2026-09-07T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
            ),
        )
        assertFalse(
            state.isScheduleDisplayCurrent(
                Instant.parse("2026-09-11T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
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
