package com.greengolddog.dayweave.model

import java.time.Instant
import java.time.ZoneId
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class PlannerModelsTest {
    @Test
    fun firmHorizonProfileDefaultsAndBoundsArePublicAndValidated() {
        assertEquals(
            ScheduleCompositionProfileSnapshot.DEFAULT_FIRM_HORIZON_DAYS,
            ScheduleCompositionProfileSnapshot().firmHorizonDays,
        )
        assertEquals(7, ScheduleCompositionProfileSnapshot.DEFAULT_FIRM_HORIZON_DAYS)
        assertEquals(1, ScheduleCompositionProfileSnapshot.MIN_FIRM_HORIZON_DAYS)
        assertEquals(30, ScheduleCompositionProfileSnapshot.MAX_FIRM_HORIZON_DAYS)
        assertTrue(ScheduleCompositionProfileSnapshot(firmHorizonDays = 1).hasValidShape())
        assertTrue(ScheduleCompositionProfileSnapshot(firmHorizonDays = 30).hasValidShape())
        assertFalse(ScheduleCompositionProfileSnapshot(firmHorizonDays = 0).hasValidShape())
        assertFalse(ScheduleCompositionProfileSnapshot(firmHorizonDays = 31).hasValidShape())
    }

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
    fun todayProjectionStartsAtTheFirstValidInstantAfterAMidnightGap() {
        val zone = ZoneId.of("America/Havana")
        val block = item(
            id = "gap-day",
            start = "2026-03-08T14:00:00Z",
            end = "2026-03-08T15:00:00Z",
        ).copy(planningZoneId = zone.id)

        assertEquals(
            listOf(block.id),
            DayWeaveUiState(schedule = listOf(block)).visibleScheduleSlicesForDay(
                reference = Instant.parse("2026-03-08T12:00:00Z"),
                currentZone = zone,
            ).map { it.item.id },
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
            publishedScheduleRevisionHint = matchingHint(revision),
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
        val newerHead = state.copy(
            publishedScheduleRevisionHint = matchingHint(revision).copy(revisionNumber = 2uL),
        )
        assertTrue(
            newerHead.isPublishedScheduleDisplayCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
            ),
        )
        assertTrue(
            newerHead.isScheduleDisplayCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
            ),
        )
        assertFalse(
            newerHead.isCanonicalPlanCurrent(
                Instant.parse("2026-09-01T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
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
            publishedScheduleRevisionHint = matchingHint(revision),
        )

        assertTrue(
            state.isCanonicalPlanCurrent(
                Instant.parse("2026-09-07T12:00:00Z"),
                ZoneId.of("Europe/Madrid"),
            ),
        )
        assertEquals(
            ScheduleDisplayHorizon(
                start = Instant.parse(revision.horizonStart),
                end = Instant.parse(revision.horizonEnd),
                timezone = ZoneId.of(revision.timezoneName),
            ),
            state.scheduleDisplayHorizon(
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
        val crossZone = ZoneId.of("America/Los_Angeles")
        assertFalse(
            state.isPublishedScheduleDisplayCurrent(
                Instant.parse(revision.horizonStart).minusNanos(1),
                crossZone,
            ),
        )
        assertTrue(
            state.isPublishedScheduleDisplayCurrent(
                Instant.parse(revision.horizonStart),
                crossZone,
            ),
        )
        assertTrue(
            state.isPublishedScheduleDisplayCurrent(
                Instant.parse(revision.horizonEnd).minusNanos(1),
                crossZone,
            ),
        )
        assertFalse(
            state.isPublishedScheduleDisplayCurrent(
                Instant.parse(revision.horizonEnd),
                crossZone,
            ),
        )
    }

    @Test
    fun publishedAuthorityDoesNotStartAtTheEarlierRepeatedMidnight() {
        val zone = ZoneId.of("America/Havana")
        val horizonStart = Instant.parse("2026-11-01T05:00:00Z")
        val horizonEnd = Instant.parse("2026-11-08T05:00:00Z")
        val revisionId = "11111111-1111-4111-8111-111111111111"
        val revision = PublishedScheduleRevisionSnapshot(
            id = revisionId,
            revision = "1:$revisionId",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = horizonStart.toString(),
            horizonEnd = horizonEnd.toString(),
            timezoneName = zone.id,
            publishedAt = horizonStart.toString(),
        )
        val state = DayWeaveUiState(
            canonicalSyncOrigin = "https://api.example.test/",
            canonicalConfigurationId = "connection-1",
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = horizonStart.toString(),
            schedulePlanningZoneId = zone.id,
            publishedScheduleRevision = revision,
            publishedScheduleProof = PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = revision,
                asOf = horizonStart.toString(),
                blocks = emptyList(),
            ),
            publishedScheduleRevisionHint = matchingHint(revision),
        )

        assertFalse(
            state.isCanonicalPlanCurrent(Instant.parse("2026-11-01T04:30:00Z"), zone),
        )
        assertTrue(state.isCanonicalPlanCurrent(horizonStart, zone))
    }

    @Test
    fun publishedProofHonorsExactCrossPlatformSubdayHavanaInterval() {
        val zone = ZoneId.of("America/Havana")
        val horizonStart = Instant.parse("2026-11-01T04:00:00Z")
        val horizonEnd = Instant.parse("2026-11-01T05:00:00Z")
        val revisionId = "11111111-1111-4111-8111-111111111111"
        val revision = PublishedScheduleRevisionSnapshot(
            id = revisionId,
            revision = "1:$revisionId",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = horizonStart.toString(),
            horizonEnd = horizonEnd.toString(),
            timezoneName = zone.id,
            publishedAt = horizonStart.toString(),
        )
        val block = ScheduleItem(
            id = "22222222-2222-4222-8222-222222222222",
            title = "Repeated-midnight context",
            kind = ItemKind.EVENT,
            startMinute = 0,
            durationMinutes = 60,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            sessionIndex = 0,
            absoluteStartAt = horizonStart.toString(),
            absoluteEndAt = horizonEnd.toString(),
            planningZoneId = zone.id,
            canonicalBlockKind = "external_fixed",
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            revision = revision,
            asOf = horizonStart.toString(),
            blocks = listOf(PublishedScheduleBlockProofSnapshot.from(block)),
        )
        val state = DayWeaveUiState(
            schedule = listOf(block),
            canonicalSyncOrigin = proof.syncOrigin,
            canonicalConfigurationId = proof.configurationId,
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = proof.asOf,
            schedulePlanningZoneId = zone.id,
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            publishedScheduleRevisionHint = matchingHint(revision),
        )

        assertTrue(proof.hasValidShape())
        assertTrue(state.isCanonicalPlanCurrent(horizonStart, zone))
        assertTrue(state.isCanonicalPlanCurrent(horizonEnd.minusNanos(1), zone))
        assertFalse(state.isCanonicalPlanCurrent(horizonEnd, zone))
        assertEquals(
            listOf(block.id),
            state.visibleScheduleSlicesForDay(
                reference = Instant.parse("2026-11-01T04:30:00Z"),
                currentZone = zone,
            ).map { it.item.id },
        )
        assertTrue(
            state.visibleScheduleSlicesForDay(
                reference = Instant.parse("2026-11-01T03:30:00Z"),
                currentZone = zone,
            ).isEmpty(),
        )
    }

    @Test
    fun publishedProofAcceptsNinetyAbsoluteDaysButNoMore() {
        val start = Instant.parse("2026-09-01T00:00:00Z")
        val revisionId = "11111111-1111-4111-8111-111111111111"
        fun proof(end: Instant): PublishedScheduleProofSnapshot {
            val revision = PublishedScheduleRevisionSnapshot(
                id = revisionId,
                revision = "1:$revisionId",
                revisionNumber = 1uL,
                inputDigest = "sha256:${"a".repeat(64)}",
                horizonStart = start.toString(),
                horizonEnd = end.toString(),
                timezoneName = "UTC",
                publishedAt = start.toString(),
            )
            return PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = revision,
                asOf = start.toString(),
                blocks = emptyList(),
            )
        }

        assertTrue(proof(start.plusSeconds(90L * 24L * 60L * 60L)).hasValidShape())
        assertFalse(proof(start.plusSeconds(90L * 24L * 60L * 60L + 1L)).hasValidShape())
    }

    @Test
    fun maxSizePublishedPlanIsFullyValidatedOnlyOncePerStateIdentity() {
        val horizonStart = "2026-09-01T00:00:00Z"
        val horizonEnd = "2026-09-02T00:00:00Z"
        val schedule = (0 until 10_000).map { index ->
            ScheduleItem(
                id = UUID.nameUUIDFromBytes("published-block-$index".toByteArray()).toString(),
                title = "Busy context $index",
                kind = ItemKind.EVENT,
                startMinute = 9 * 60,
                durationMinutes = 1,
                status = ItemStatus.SCHEDULED,
                isFlexible = false,
                isHardConstraint = true,
                sessionIndex = 0,
                absoluteStartAt = "2026-09-01T09:00:00Z",
                absoluteEndAt = "2026-09-01T09:01:00Z",
                planningZoneId = "UTC",
                canonicalBlockKind = "external_fixed",
            )
        }
        val revisionId = "11111111-1111-4111-8111-111111111111"
        val revision = PublishedScheduleRevisionSnapshot(
            id = revisionId,
            revision = "1:$revisionId",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = horizonStart,
            horizonEnd = horizonEnd,
            timezoneName = "UTC",
            publishedAt = "2026-09-01T07:00:00Z",
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            revision = revision,
            asOf = revision.publishedAt,
            blocks = schedule.map(PublishedScheduleBlockProofSnapshot::from),
        )
        val state = DayWeaveUiState(
            schedule = schedule,
            canonicalSyncOrigin = proof.syncOrigin,
            canonicalConfigurationId = proof.configurationId,
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            publishedScheduleRevisionHint = matchingHint(revision),
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = proof.asOf,
            schedulePlanningZoneId = revision.timezoneName,
        )
        val reference = Instant.parse("2026-09-01T12:00:00Z")
        val zone = ZoneId.of("UTC")
        val before = publishedScheduleValidationComputationCount()

        repeat(5) {
            assertTrue(state.isCanonicalPlanCurrent(reference, zone))
            assertTrue(state.isScheduleDisplayCurrent(reference, zone))
            assertTrue(state.isPublishedScheduleDisplayCurrent(reference, zone))
            assertEquals(
                ScheduleDisplayHorizon(
                    Instant.parse(horizonStart),
                    Instant.parse(horizonEnd),
                    zone,
                ),
                state.scheduleDisplayHorizon(reference, zone),
            )
        }
        assertEquals(before + 1, publishedScheduleValidationComputationCount())

        assertTrue(state.copy().isCanonicalPlanCurrent(reference, zone))
        assertEquals(before + 2, publishedScheduleValidationComputationCount())
    }

    @Test
    fun publicationProofRequiresPlannedContainmentButAllowsPinnedIntersection() {
        val horizonStart = Instant.parse("2026-09-01T00:00:00Z")
        val horizonEnd = Instant.parse("2026-09-08T00:00:00Z")
        val crossing = ScheduleItem(
            id = "22222222-2222-4222-8222-222222222222",
            title = "Crossing work",
            kind = ItemKind.TASK,
            startMinute = 23 * 60 + 30,
            durationMinutes = 60,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = "11111111-1111-4111-8111-111111111111",
            canonicalRevision = 1,
            sessionIndex = 0,
            absoluteStartAt = "2026-09-07T23:30:00Z",
            absoluteEndAt = "2026-09-08T00:30:00Z",
            planningZoneId = "UTC",
            canonicalBlockKind = "planned",
        )

        assertFalse(
            PublishedScheduleBlockProofSnapshot.from(crossing).hasValidShape(
                horizonStart = horizonStart,
                horizonEnd = horizonEnd,
                requireFullSeal = true,
            ),
        )
        assertTrue(
            PublishedScheduleBlockProofSnapshot.from(
                crossing.copy(
                    canonicalBlockKind = "pinned",
                    isFlexible = false,
                    isHardConstraint = true,
                ),
            ).hasValidShape(
                horizonStart = horizonStart,
                horizonEnd = horizonEnd,
                requireFullSeal = true,
            ),
        )
    }

    private fun matchingHint(revision: PublishedScheduleRevisionSnapshot) =
        PublishedScheduleRevisionHintSnapshot(
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            revisionNumber = revision.revisionNumber,
        )

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
