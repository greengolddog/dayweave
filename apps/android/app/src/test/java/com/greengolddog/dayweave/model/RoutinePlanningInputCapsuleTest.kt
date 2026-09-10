package com.greengolddog.dayweave.model

import java.time.Instant
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RoutinePlanningInputCapsuleTest {
    private val now = Instant.parse(ROUTINE_NOW).toEpochMilli() + 1
    private fun capsule(state: DayWeaveUiState = planningTestReadyState()) = RoutinePlanningInputCapsule.create(
        state, " \n" + ROUTINE_PLANNING_JSON.encodeToString(planningTestRequest()) + "\n", planningTestWitness(), ROUTINE_NOW)

    @Test fun fixedInputRetainsExactOriginalBytesCompletePrivateSourcesAndPositiveLifecycleHead() {
        val saved = capsule()
        saved.requireValid()
        assertTrue(saved.originalRequestJson.startsWith(" \n"))
        assertEquals(planningTestRequest(), saved.originalRequest())
        assertEquals(planningTestItems().sortedBy { it.id }, saved.canonicalItems)
        assertTrue(saved.canonicalItems.any { it.status == "inbox" && it.notes == "Synthetic private note" })
        assertEquals(2L, saved.witness.occurrenceLifecycle.snapshotRevision)
        assertTrue(saved.isReusableInput(planningTestReadyState().copy(routinePlanningInputCapsule = saved), now, true))
        assertFalse(saved.toString().contains("Synthetic"))
    }

    @Test fun timestampsDigestScopeAndUnknownFieldsHaveNoPermissiveRestorePath() {
        val saved = capsule()
        for (bad in listOf(saved.copy(capturedAt = "2026-09-10T10:00:00.1234567Z"),
            saved.copy(capturedAt = "2026-09-10T10:00:00.123456+00:00"),
            saved.copy(originalRequestJson = saved.originalRequestJson.trim()),
            saved.copy(workspaceId = PLANNING_OWNER), saved.copy(canonicalItems = saved.canonicalItems.dropLast(1)))) {
            val error = assertThrows(RoutinePlanningWitnessProtocolException::class.java) { bad.requireValid() }
            assertNull(error.cause)
        }
        val raw = ROUTINE_CAPSULE_JSON.encodeToJsonElement(saved).jsonObject
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) {
            decodeExactRoutinePlanningWitness<RoutinePlanningInputCapsule>(JsonObject(raw + ("liveAdmission" to JsonPrimitive(true))).toString())
        }
    }

    @Test fun rollbackDayHorizonProfileSourceCursorAndPrivateAccessRevokeDataEligibilityWithoutErasingCustody() {
        val saved = capsule(); val state = planningTestReadyState().copy(routinePlanningInputCapsule = saved)
        assertFalse(saved.isReusableInput(state, now, false))
        // The microsecond tail must not get rounded down into apparent clock continuity.
        assertFalse(saved.isReusableInput(state, Instant.parse(ROUTINE_NOW).toEpochMilli(), true))
        assertFalse(saved.isReusableInput(state, Instant.parse("2026-09-11T00:00:00Z").toEpochMilli(), true))
        val changed = listOf(state.copy(canonicalItems = state.canonicalItems.map { it.copy(revision = 8) }),
            state.copy(scheduleCompositionProfile = state.scheduleCompositionProfile.copy(dayStartMinute = 60)),
            state.copy(scheduleCompositionProfile = state.scheduleCompositionProfile.copy(timezoneName = "Europe/Moscow")),
            state.copy(canonicalConfigurationId = "replacement"),
            state.copy(routineOccurrenceLedger = state.routineOccurrenceLedger.copy(deltaCursor = "DWR1.changed")),
            state.copy(routineOccurrenceLedger = state.routineOccurrenceLedger.copy(needsRemoteScheduleCatchUp = true)),
            state.copy(canonicalExecutionRevision = 1))
        for (current in changed) {
            assertFalse(saved.isReusableInput(current, now, true))
            assertEquals(saved, current.routinePlanningInputCapsule)
        }
    }

    @Test fun runtimeGenerationIsNotPersistedDataAuthorityAndFixedCapturedDayIsRequiredWithinLongHorizon() {
        val original = planningTestReadyState()
        assertEquals(original.routinePlanningStableInputFingerprint(), original.copy(routineOccurrenceAuthorityGeneration = 9).routinePlanningStableInputFingerprint())
        val request = planningTestRequest().let { it.copy(schedule = it.schedule.copy(horizonEnd = "2026-09-12T00:00:00Z")) }
        val saved = RoutinePlanningInputCapsule.create(original, ROUTINE_PLANNING_JSON.encodeToString(request),
            planningTestWitness().copy(schedule = request.schedule), ROUTINE_NOW)
        assertFalse(saved.isReusableInput(original.copy(routinePlanningInputCapsule = saved), Instant.parse("2026-09-11T10:00:00Z").toEpochMilli(), true))
    }
}
