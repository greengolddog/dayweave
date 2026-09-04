package com.greengolddog.dayweave.model

import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalScheduleCompositionProvenanceTest {
    @Test
    fun `multi-day horizon keeps exact midnight boundaries across DST`() {
        val zone = ZoneId.of("Europe/Paris")
        val date = LocalDate.parse("2026-03-27")
        val start = requireNotNull(strictLocalDayStartInstant(date, zone))
        val end = requireNotNull(strictLocalDayEndInstant(date.plusDays(7), zone))
        assertEquals(LocalTime.MIDNIGHT, start.atZone(zone).toLocalTime())
        assertEquals(Duration.ofHours(167), Duration.between(start, end))
        val state = baseState(start.toString(), zone.id)
        val provenance = provenance(state, start.toString(), end.toString(), zone.id)

        assertTrue(provenance.hasValidShape())
        assertTrue(
            provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)),
        )
    }

    @Test
    fun `nonexistent local midnight cannot become a provenance boundary`() {
        val zone = ZoneId.of("America/Santiago")
        val date = LocalDate.parse("2026-09-06")
        assertEquals(null, strictLocalDayStartInstant(date, zone))
        val adjustedStart = date.atStartOfDay(zone).toInstant()
        val end = requireNotNull(strictLocalDayEndInstant(date.plusDays(7), zone))
        assertEquals(LocalTime.of(1, 0), adjustedStart.atZone(zone).toLocalTime())
        val state = baseState(adjustedStart.toString(), zone.id)
        val provenance = provenance(
            state,
            adjustedStart.toString(),
            end.toString(),
            zone.id,
        )

        assertFalse(provenance.hasValidShape())
        assertFalse(
            provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)),
        )
    }

    @Test
    fun `ambiguous Havana midnight selects the later start offset`() {
        val zone = ZoneId.of("America/Havana")
        val date = LocalDate.parse("2026-11-01")
        val start = requireNotNull(strictLocalDayStartInstant(date, zone))
        val end = requireNotNull(strictLocalDayEndInstant(date.plusDays(7), zone))
        assertEquals("2026-11-01T05:00:00Z", start.toString())
        assertEquals("-05:00", start.atZone(zone).offset.toString())
        val state = baseState(start.toString(), zone.id)
        val provenance = provenance(state, start.toString(), end.toString(), zone.id)

        assertTrue(provenance.hasValidShape())
        assertTrue(
            provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)),
        )
    }

    @Test
    fun `local horizon does not become current at the earlier repeated midnight`() {
        val zone = ZoneId.of("America/Havana")
        val date = LocalDate.parse("2026-11-01")
        val start = requireNotNull(strictLocalDayStartInstant(date, zone))
        val end = requireNotNull(strictLocalDayEndInstant(date.plusDays(7), zone))
        val base = baseState(start.toString(), zone.id)
        val provenance = provenance(base, start.toString(), end.toString(), zone.id)
        val state = base.copy(localScheduleCompositionProvenance = provenance)

        assertFalse(
            state.isScheduleDisplayCurrent(Instant.parse("2026-11-01T04:30:00Z"), zone),
        )
        assertEquals(
            null,
            state.scheduleDisplayHorizon(Instant.parse("2026-11-01T04:30:00Z"), zone),
        )
        assertTrue(state.isScheduleDisplayCurrent(start, zone))
        assertEquals(ScheduleDisplayHorizon(start, end, zone), state.scheduleDisplayHorizon(start, zone))
        assertTrue(state.isScheduleDisplayCurrent(end.minusNanos(1), zone))
        assertFalse(state.isScheduleDisplayCurrent(end, zone))
    }

    @Test
    fun `ambiguous Havana midnight uses the earlier end offset`() {
        val zone = ZoneId.of("America/Havana")
        val startDate = LocalDate.parse("2026-10-25")
        val endDate = LocalDate.parse("2026-11-01")
        val start = requireNotNull(strictLocalDayStartInstant(startDate, zone))
        val end = requireNotNull(strictLocalDayEndInstant(endDate, zone))
        assertEquals("2026-11-01T04:00:00Z", end.toString())
        assertEquals("-04:00", end.atZone(zone).offset.toString())
        val base = baseState(start.plusSeconds(12 * 60 * 60).toString(), zone.id)
        val provenance = provenance(base, start.toString(), end.toString(), zone.id)
        val state = base.copy(localScheduleCompositionProvenance = provenance)

        assertTrue(provenance.hasValidShape())
        assertTrue(provenance.matchesState(state))
        assertTrue(state.isScheduleDisplayCurrent(end.minusNanos(1), zone))
        assertFalse(state.isScheduleDisplayCurrent(end, zone))
    }

    @Test
    fun `generated instant outside its horizon day fails closed`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(7).atStartOfDay(zone).toInstant()
        val state = baseState(end.toString(), zone.id)
        val provenance = provenance(state, start.toString(), end.toString(), zone.id)
            .copy(generatedAt = end.toString(), asOf = end.toString())

        assertFalse(provenance.hasValidShape())
        assertFalse(provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)))
    }

    @Test
    fun `canonical block with unknown item and null revision never matches provenance`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(7).atStartOfDay(zone).toInstant()
        val state = baseState(start.toString(), zone.id).copy(
            schedule = listOf(
                ScheduleItem(
                    id = "local-block",
                    title = "Unknown canonical block",
                    kind = ItemKind.TASK,
                    startMinute = 480,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                    canonicalItemId = "11111111-1111-4111-8111-111111111111",
                    canonicalRevision = null,
                ),
            ),
        )
        val provenance = provenance(state, start.toString(), end.toString(), zone.id)

        assertTrue(provenance.hasValidShape())
        assertFalse(provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)))
    }

    @Test
    fun `arbitrary equal data class copy cannot inherit a trusted fingerprint memo`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(7).atStartOfDay(zone).toInstant()
        val source = baseState(start.toString(), zone.id)
        val provenance = provenance(source, start.toString(), end.toString(), zone.id)
        val installed = source.copy(localScheduleCompositionProvenance = provenance)
        assertTrue(provenance.matchesState(installed))

        val copiedOutsideStore = installed.copy()
        val before = localScheduleCompositionFingerprintComputationCount()
        assertTrue(provenance.matchesState(copiedOutsideStore))
        assertEquals(before + 1, localScheduleCompositionFingerprintComputationCount())
        repeat(3) {
            assertTrue(provenance.matchesState(copiedOutsideStore))
        }
        assertEquals(before + 1, localScheduleCompositionFingerprintComputationCount())
    }

    @Test
    fun `habit delta completeness and mutation readiness cannot inherit a composition memo`() {
        val generatedAt = "2026-09-01T07:00:00Z"
        val readyLedger = HabitLedgerSnapshot(
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            deltaCursor = "habit-cursor",
            deltaCaughtUp = true,
        ).also(HabitLedgerSnapshot::requireValid)
        val ready = baseState(generatedAt, "UTC").copy(habitLedger = readyLedger)
        val before = localScheduleCompositionFingerprintComputationCount()
        val readyFingerprint = ready.localScheduleCompositionStateFingerprint()
        assertEquals(before + 1, localScheduleCompositionFingerprintComputationCount())

        val intermediate = ready.copy(
            habitLedger = readyLedger.copy(deltaCaughtUp = false),
        )
        intermediate.inheritLocalScheduleCompositionMemo(ready)
        assertFalse(readyFingerprint == intermediate.localScheduleCompositionStateFingerprint())
        assertEquals(before + 2, localScheduleCompositionFingerprintComputationCount())

        val command = HabitOutcomeCommandSnapshot(
            operationId = "11111111-1111-4111-8111-111111111111",
            expectedRevision = 0,
            outcome = HabitOutcomeInputSnapshot(
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
                quantity = null,
                unit = null,
                actualSeconds = null,
                note = null,
                occurredAt = generatedAt,
            ),
        )
        val reviewedMutation = PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.OUTCOME,
            habitId = "22222222-2222-4222-8222-222222222222",
            targetId = "33333333-3333-4333-8333-333333333333",
            expectedRevision = 0,
            idempotencyKey = "11111111-1111-4111-8111-111111111111",
            requestJson = command.encoded(),
            createdAt = generatedAt,
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            disposition = PendingHabitMutationDisposition.CONFLICT,
        )
        val mutationBlocked = ready.copy(
            habitLedger = readyLedger.copy(pendingMutations = listOf(reviewedMutation))
                .also(HabitLedgerSnapshot::requireValid),
        )
        mutationBlocked.inheritLocalScheduleCompositionMemo(ready)
        assertFalse(readyFingerprint == mutationBlocked.localScheduleCompositionStateFingerprint())
        assertEquals(before + 3, localScheduleCompositionFingerprintComputationCount())
    }

    @Test
    fun `configured horizon crossing Sunday stays current on each intersecting day`() {
        val zone = ZoneId.of("Europe/Paris")
        val date = LocalDate.parse("2026-09-04")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(3).atStartOfDay(zone).toInstant()
        val base = baseState(start.plusSeconds(8 * 60 * 60).toString(), zone.id).copy(
            scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(
                firmHorizonDays = 3,
            ),
        )
        val provenance = provenance(base, start.toString(), end.toString(), zone.id)
        val state = base.copy(localScheduleCompositionProvenance = provenance)

        assertTrue(provenance.matchesState(state))
        listOf("2026-09-04", "2026-09-05", "2026-09-06").forEach { currentDate ->
            val reference = LocalDate.parse(currentDate).atTime(12, 0).atZone(zone).toInstant()
            assertTrue(state.isScheduleDisplayCurrent(reference, zone))
            assertEquals(
                ScheduleDisplayHorizon(start, end, zone),
                state.scheduleDisplayHorizon(reference, zone),
            )
        }
        val expired = date.plusDays(3).atTime(12, 0).atZone(zone).toInstant()
        assertFalse(state.isScheduleDisplayCurrent(expired, zone))
        assertEquals(null, state.scheduleDisplayHorizon(expired, zone))
    }

    @Test
    fun `old one-day provenance cannot match the default seven-day profile`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(1).atStartOfDay(zone).toInstant()
        val base = baseState(start.toString(), zone.id)
        val provenance = provenance(base, start.toString(), end.toString(), zone.id)
        val state = base.copy(localScheduleCompositionProvenance = provenance)

        assertTrue(provenance.hasValidShape())
        assertFalse(provenance.matchesState(state))
        assertFalse(state.isScheduleDisplayCurrent(start.plusSeconds(1), zone))
        assertEquals(null, state.scheduleDisplayHorizon(start.plusSeconds(1), zone))
    }

    @Test
    fun `firm horizon projection clips exact edges and excludes later blocks`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-04")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(3).atStartOfDay(zone).toInstant()
        val blocks = listOf(
            scheduleItem("before", start.minusSeconds(3_600), start.plusSeconds(3_600)),
            scheduleItem("sunday", end.minusSeconds(7_200), end.plusSeconds(3_600)),
            scheduleItem("after", end, end.plusSeconds(3_600)),
        )
        val base = baseState(start.plusSeconds(1).toString(), zone.id).copy(
            schedule = blocks,
            scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(firmHorizonDays = 3),
        )
        val provenance = provenance(base, start.toString(), end.toString(), zone.id)
        val state = base.copy(localScheduleCompositionProvenance = provenance)

        val slices = state.visibleScheduleSlicesForFirmHorizon(
            reference = Instant.parse("2026-09-06T12:00:00Z"),
            currentZone = zone,
        )

        assertEquals(listOf("before", "sunday"), slices.map { it.item.id })
        assertEquals(start, slices.first().clippedStart)
        assertEquals(end, slices.last().clippedEnd)
        assertEquals("Started earlier", slices.first().continuationLabel)
    }

    @Test
    fun `provenance rejects horizons beyond the public bounds`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val state = baseState(start.toString(), zone.id)

        assertFalse(
            provenance(
                state,
                start.toString(),
                date.plusDays(31).atStartOfDay(zone).toInstant().toString(),
                zone.id,
            ).hasValidShape(),
        )
    }

    private fun baseState(generatedAt: String, zoneId: String) = DayWeaveUiState(
        canonicalSyncOrigin = ORIGIN,
        canonicalConfigurationId = CONFIGURATION_ID,
        canonicalDeltaCursor = "cursor-1",
        canonicalExecutionSyncOrigin = ORIGIN,
        canonicalExecutionConfigurationId = CONFIGURATION_ID,
        canonicalExecutionHistoryWindowRevision = 0,
        canonicalExecutionHistoryContinuityEstablished = true,
        canonicalExecutionHistoryVerified = true,
        scheduleGeneratedAt = generatedAt,
        schedulePlanningZoneId = zoneId,
    )

    private fun provenance(
        state: DayWeaveUiState,
        horizonStart: String,
        horizonEnd: String,
        zoneId: String,
    ) = LocalScheduleCompositionProvenanceSnapshot(
        syncOrigin = ORIGIN,
        configurationId = CONFIGURATION_ID,
        deltaCursor = "cursor-1",
        localInputFingerprint = "local-sha256:${"a".repeat(64)}",
        scheduleRequestFingerprint = "sha256:${"b".repeat(64)}",
        stateInputFingerprint = state.localScheduleCompositionStateFingerprint(),
        generatedAt = state.scheduleGeneratedAt ?: error("generatedAt"),
        asOf = state.scheduleGeneratedAt,
        horizonStart = horizonStart,
        horizonEnd = horizonEnd,
        timezoneName = zoneId,
        sourceItemRevisions = emptyMap(),
    )

    private fun scheduleItem(id: String, start: Instant, end: Instant) = ScheduleItem(
        id = id,
        title = id,
        kind = ItemKind.TASK,
        startMinute = 0,
        durationMinutes = Duration.between(start, end).toMinutes().toInt(),
        status = ItemStatus.SCHEDULED,
        absoluteStartAt = start.toString(),
        absoluteEndAt = end.toString(),
        planningZoneId = "UTC",
    )

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "connection-1"
    }
}
