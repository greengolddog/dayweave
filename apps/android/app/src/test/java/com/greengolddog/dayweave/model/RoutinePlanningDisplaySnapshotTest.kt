package com.greengolddog.dayweave.model

import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class RoutinePlanningDisplaySnapshotTest {
    @Test fun exactHelperEnvelopeAndCapturedInputRemainSeparateFromComputedClock(): Unit = runBlocking {
        val capsule = planningDisplayCapsule(); val composed = planningDisplayComposition(capsule)
        val saved = RoutinePlanningDisplaySnapshot.create(capsule, composed, PLANNING_DISPLAY_NOW)
        assertEquals(composed, saved.validateAndDecode(capsule))
        assertEquals(composed.helperResponseJson, saved.helperResponseJson)
        assertTrue(saved.helperResponseJson.contains(ROUTINE_NOW))
        assertEquals(ROUTINE_NOW, saved.capturedAt); assertEquals(PLANNING_DISPLAY_NOW, saved.computedAt)
        assertTrue(saved.helperRequestFingerprint.startsWith("sha256:"))
        assertEquals(capsule.originalRequestJson, planningDisplayCapsule().originalRequestJson)
        assertFalse(saved.toString().contains("Synthetic"))
    }

    @Test fun absentRawFidelityWrongHashesHeadsNumericTokensAndTimestampPrecisionFailClosed(): Unit = runBlocking {
        val capsule = planningDisplayCapsule(); val composed = planningDisplayComposition(capsule)
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) {
            RoutinePlanningDisplaySnapshot.create(capsule, composed.copy(helperResponseJson = null), PLANNING_DISPLAY_NOW)
        }
        val saved = RoutinePlanningDisplaySnapshot.create(capsule, composed, PLANNING_DISPLAY_NOW)
        for (bad in listOf(saved.copy(helperRequestFingerprint = "sha256:" + "0".repeat(64)),
            saved.copy(occurrenceSnapshotRevision = 99), saved.copy(originalRequestDigest = "routine-input-request-sha256:" + "0".repeat(64)),
            saved.copy(helperResponseJson = saved.helperResponseJson.replace("\"accepted_item_count\":3", "\"accepted_item_count\":\"3\"")),
            saved.copy(computedAt = "2026-09-10T10:00:01.1234567Z"), saved.copy(computedAt = "2026-09-10T09:00:00Z"))) {
            val error = assertThrows(RoutinePlanningWitnessProtocolException::class.java) { bad.validateAndDecode(capsule) }
            assertNull(error.cause)
        }
    }

    @Test fun runtimePresentationIsBoundToExactArtifactSourcesReadGenerationAndClock(): Unit = runBlocking {
        val base = planningDisplayState(); val capsule = requireNotNull(base.routinePlanningInputCapsule)
        val saved = planningDisplaySnapshot(capsule)
        val admission = RoutinePlanningDisplayAdmission(capsule, saved, saved.validateAndDecode(capsule), 0, 0)
        val current = base.copy(routinePlanningDisplaySnapshot = saved, routinePlanningDisplayAdmission = admission)
        assertTrue(admission.matchesState(current))
        assertTrue(admission.permitsClock(Instant.parse(PLANNING_DISPLAY_NOW), ZoneId.of("UTC")))
        assertFalse(admission.permitsClock(Instant.parse(PLANNING_DISPLAY_NOW).minusNanos(1), ZoneId.of("UTC")))
        assertFalse(admission.permitsClock(Instant.parse(PLANNING_DISPLAY_NOW), ZoneId.of("Europe/Moscow")))
        assertFalse(admission.permitsClock(Instant.parse("2026-09-11T10:00:00Z"), ZoneId.of("UTC")))
        assertFalse(admission.matchesState(current.copy(routineOccurrenceAuthorityGeneration = 1)))
        assertFalse(admission.matchesState(current.copy(itemCompletionEvidenceGeneration = 1)))
        assertFalse(admission.matchesState(current.copy(canonicalItems = current.canonicalItems.map { it.copy(revision = 8) })))
        assertFalse(admission.matchesState(current.copy(routineOccurrenceLedger = current.routineOccurrenceLedger.copy(needsRemoteScheduleCatchUp = true))))
        assertTrue(current.requiresRemoteRoutineOccurrenceComposition())
        assertNull(current.localScheduleCompositionProvenance)
        assertFalse(current.isCanonicalPlanCurrent(Instant.parse(PLANNING_DISPLAY_NOW), ZoneId.of("UTC")))
    }
}
