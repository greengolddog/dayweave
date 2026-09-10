package com.greengolddog.dayweave.model

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RoutinePlanningWitnessModelsTest {
    @Test fun qualifiedWireJoinsTheCompleteCurrentSourceTreeAndPreservesProtectedInput() {
        val original = planningTestResponse()
        val decoded = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(ROUTINE_PLANNING_JSON.encodeToString(original))
        assertEquals(original, decoded)
        decoded.requireValid()
        val witness = (decoded.result as RoutinePlanningWitnessResult.Qualified).witness
        witness.requireMatches(planningTestRequest(), PLANNING_WORKSPACE, PLANNING_OWNER, planningTestItems())
        assertEquals(3, witness.occurrenceLifecycle.instances.single().members.size)
        assertFalse(witness.toString().contains(ROUTINE_ROOT))
        assertFalse(witness.schedule.toString().contains(ROUTINE_NOW))
    }

    @Test fun allRemoteReasonsAreClosedAndCarryNoWitness() {
        RoutinePlanningRemoteReason.entries.forEach { reason ->
            val response = RoutinePlanningWitnessResponse(1, RoutinePlanningWitnessResult.RemoteRequired(reason))
            val body = ROUTINE_PLANNING_JSON.encodeToString(response)
            assertFalse(body.contains("witness"))
            assertEquals(response, decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(body))
        }
        rejects("""{"schema_version":1,"result":{"status":"remote_required","reason":"future"}}""")
        rejects("""{"schema_version":1,"result":{"status":"remote_required","reason":"source_ineligible","witness":null}}""")
    }

    @Test fun exactTokensRejectAliasesDefaultsUnknownFieldsAndDuplicateDecodedKeys() {
        val valid = ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse())
        val cases = listOf(
            valid.replace("\"schema_version\":1", "\"schema_version\":\"1\""),
            valid.replace("\"schema_version\":1", "\"schema_version\":1.0"),
            valid.replace("\"schema_version\":1", "\"schema_version\":1e0"),
            valid.replace("\"execution_snapshot_revision\":0", "\"execution_snapshot_revision\":-0"),
            valid.replace("\"habit_change_head\":0", "\"habit_change_head\":9223372036854775808"),
            valid.replace("\"parent_id\":null,", ""),
            valid.replace("\"schema_version\":1", "\"schema_version\":1,\"schema_vers\\u0069on\":1"),
            valid.replace("\"schema_version\":1", "\"schema_version\":1,\"future\":true"),
            valid.replace(PLANNING_OWNER, "00000000-0000-0000-0000-00000000030A"),
            valid.replace("\"source_revision\":7", "\"source_revision\":0"),
            valid.replace("\"status\":\"not_started\"", "\"status\":\"active\""),
            valid.replace("routine-witness-capture-sha256:", "sha256:"),
            valid.replace(ROUTINE_NOW, "2026-09-10T10:00:00.1234567Z"),
        )
        cases.forEach(::rejects)
    }

    @Test fun emptyHorizonRetainsPositiveHeadAndDoesNotPermitInventedMembersAtZero() {
        val witness = planningTestWitness().copy(occurrenceLifecycle = RoutinePlanningLifecycle(5, emptyList()))
        witness.requireValid(); witness.requireCurrentSources(planningTestItems())
        assertEquals(5L, witness.occurrenceLifecycle.snapshotRevision)
        assertThrows(IllegalArgumentException::class.java) { planningTestWitness().copy(occurrenceLifecycle = planningTestWitness().occurrenceLifecycle.copy(snapshotRevision = 0)).requireValid() }
    }

    @Test fun currentSourceJoinRejectsMissingInboxHarmlessRevisionDriftForeignScopesAndCycles() {
        val witness = planningTestWitness()
        val items = planningTestItems()
        for (changed in listOf(items.dropLast(1), items.map { it.copy(revision = it.revision + 1) }, items.map { it.copy(parentId = ROUTINE_CHILD) })) {
            assertThrows(IllegalArgumentException::class.java) { witness.requireCurrentSources(changed) }
        }
        assertThrows(IllegalArgumentException::class.java) { witness.requireMatches(planningTestRequest(), PLANNING_OWNER, PLANNING_WORKSPACE, items) }
        assertThrows(IllegalArgumentException::class.java) { witness.requireMatches(planningTestRequest().copy(terminalCursor = "other"), PLANNING_WORKSPACE, PLANNING_OWNER, items) }
        val instance = witness.occurrenceLifecycle.instances.single()
        assertThrows(IllegalArgumentException::class.java) { witness.copy(occurrenceLifecycle = witness.occurrenceLifecycle.copy(instances = listOf(instance.copy(members = instance.members.dropLast(1))))).requireCurrentSources(items) }
    }

    @Test fun immutableRequestFieldsCannotBeReplacedByAnOtherwiseValidWitness() {
        val witness = planningTestWitness(); val schedule = witness.schedule
        val changed = listOf(schedule.copy(asOf = "2026-09-10T11:00:00Z"), schedule.copy(timezoneName = "Europe/London"),
            schedule.copy(config = schedule.config.copy(stabilityWeight = 9)), schedule.copy(availability = emptyList()),
            schedule.copy(recurrenceContext = JsonObject(schedule.recurrenceContext + ("minimum_spacing" to buildJsonObject { put(ROUTINE_ROOT, 5) }))))
        changed.forEach { candidate -> assertThrows(IllegalArgumentException::class.java) {
            witness.copy(schedule = candidate).requireMatches(planningTestRequest(), PLANNING_WORKSPACE, PLANNING_OWNER, planningTestItems())
        } }
    }

    @Test fun deepFlatTreeValidationIsIterativeAndDoesNotDropUnscheduledMembers() {
        val source = (1..10_000).map { index -> planningTestItem("00000000-0000-0000-0000-${index.toString().padStart(12, '0')}",
            if (index == 1) null else "00000000-0000-0000-0000-${(index - 1).toString().padStart(12, '0')}") }
            .mapIndexed { index, item -> if (index == 0) item.copy(kind = "routine", recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}") else item }
        val base = planningTestWitness()
        val instance = base.occurrenceLifecycle.instances.single().copy(members = source.map { RoutinePlanningLifecycleMember(it.id, it.parentId, it.revision, "not_started") })
        val witness = base.copy(sourceItemRevisions = source.associate { it.id to it.revision }, occurrenceLifecycle = base.occurrenceLifecycle.copy(instances = listOf(instance)))
        witness.requireValid(); witness.requireCurrentSources(source)
        assertEquals(10_000, instance.members.size)
    }

    private fun rejects(body: String) {
        try {
            decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(body).requireValid()
            fail("Malformed protected planning evidence was admitted")
        } catch (error: Exception) {
            assertFalse(error.message.orEmpty().contains("Synthetic"))
            assertNull(error.cause)
        }
    }
}
