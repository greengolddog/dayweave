package com.greengolddog.dayweave.model

import kotlinx.serialization.json.*

const val ROUTINE_ROOT = "00000000-0000-0000-0000-000000000001"
const val ROUTINE_CHILD = "00000000-0000-0000-0000-000000000002"
const val ROUTINE_OPTIONAL = "00000000-0000-0000-0000-000000000003"
const val ROUTINE_INSTANCE = "00000000-0000-0000-0000-000000000100"
const val ROUTINE_PLANNER_ID = "10000000-0000-5000-8000-000000000001"
const val ROUTINE_OPERATION = "00000000-0000-0000-0000-000000000200"
const val ROUTINE_NOW = "2026-09-10T10:00:00.123456Z"
val ROUTINE_HASH = "sha256:" + "a".repeat(64)

fun routineTestSnapshot(done: Boolean = false): RoutineOccurrenceSnapshot {
    val planned = ItemCompletionReopening("planned", null, null, null)
    val blocked = ItemCompletionReopening("blocked", "manual", null, "Synthetic waiting for input")
    val definitions = listOf(
        RoutineOccurrenceMemberDefinition(ROUTINE_ROOT, null, 7, "Synthetic routine", "routine", true, 0, true, blocked),
        RoutineOccurrenceMemberDefinition(ROUTINE_CHILD, ROUTINE_ROOT, 4, "Required leaf", "task", false, 0, true, planned),
        RoutineOccurrenceMemberDefinition(ROUTINE_OPTIONAL, ROUTINE_ROOT, 2, "Optional leaf", "task", false, 1, false, planned),
    )
    val manifest = RoutineOccurrenceManifest(1, ROUTINE_INSTANCE, ROUTINE_ROOT, ROUTINE_PLANNER_ID,
        Json.parseToJsonElement("""{"type":"calendar_day","date":"2026-09-10","bucket_ordinal":0}""").jsonObject,
        "2026-09-10T00:00:00Z", "2026-09-11T00:00:00Z", "2026-09-10T09:00:00Z", "2026-09-10T18:00:00Z",
        "UTC", ROUTINE_HASH, definitions)
    val states = definitions.map { definition ->
        val completed = done && definition.itemId != ROUTINE_OPTIONAL
        RoutineOccurrenceMemberState(definition.itemId, if (completed) 2 else 1,
            if (completed) "completed" else definition.initialOpen.status, definition.requiredForParent,
            ItemCompletionMode.AUTOMATIC, definition.initialOpen,
            if (completed && definition.itemId == ROUTINE_ROOT) ItemCompletionProvenance(ItemCompletionProvenanceKind.AUTOMATIC, blocked) else null,
            if (completed) ROUTINE_NOW else null, ROUTINE_NOW)
    }
    return RoutineOccurrenceSnapshot(1, RoutineOccurrenceAggregate(manifest, if (done) 2 else 1, states), ROUTINE_HASH, true,
        definitions.map { definition ->
            RoutineOccurrenceMemberEvaluation(definition.itemId,
                if (definition.itemId == ROUTINE_ROOT) ItemCompletionCounts(1, if (done) 1 else 0, if (done) 0 else 1, 0)
                else ItemCompletionCounts(0, 0, 0, 0), false, RoutineOccurrenceReason.UNCHANGED)
        })
}

fun routineTestRequest() = RoutineOccurrenceRequest(operationId = ROUTINE_OPERATION, expectedInstanceRevision = 1,
    expectedMemberRevision = 1, expectedEvidenceHash = ROUTINE_HASH, action = RoutineOccurrenceAction.SetOutcome("completed"))

fun routineTestMutation(replayed: Boolean = false): RoutineOccurrenceMutationResult {
    val snapshot = routineTestSnapshot(true)
    return RoutineOccurrenceMutationResult(ROUTINE_OPERATION, replayed, snapshot.copy(members = snapshot.members.map {
        it.copy(reason = when (it.itemId) {
            ROUTINE_ROOT -> RoutineOccurrenceReason.AUTOMATICALLY_COMPLETED
            ROUTINE_CHILD -> RoutineOccurrenceReason.OUTCOME_RECORDED
            else -> it.reason
        })
    }))
}

fun routineTestPage() = RoutineOccurrencePage(1, listOf(RoutineOccurrenceChange(1, routineTestSnapshot())), "DWR1.synthetic-checkpoint", false)
