package com.greengolddog.dayweave.model

import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalScheduleCompositionProvenanceTest {
    @Test
    fun `DST gap start of day is accepted as the exact zoned planning boundary`() {
        val zone = ZoneId.of("America/Santiago")
        val date = LocalDate.parse("2026-09-06")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(1).atStartOfDay(zone).toInstant()
        assertEquals(LocalTime.of(1, 0), start.atZone(zone).toLocalTime())
        val state = baseState(start.toString(), zone.id)
        val provenance = provenance(state, start.toString(), end.toString(), zone.id)

        assertTrue(provenance.hasValidShape())
        assertTrue(provenance.matchesState(state.copy(localScheduleCompositionProvenance = provenance)))
    }

    @Test
    fun `generated instant outside its horizon day fails closed`() {
        val zone = ZoneId.of("UTC")
        val date = LocalDate.parse("2026-09-01")
        val start = date.atStartOfDay(zone).toInstant()
        val end = date.plusDays(1).atStartOfDay(zone).toInstant()
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
        val end = date.plusDays(1).atStartOfDay(zone).toInstant()
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
        val end = date.plusDays(1).atStartOfDay(zone).toInstant()
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

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "connection-1"
    }
}
