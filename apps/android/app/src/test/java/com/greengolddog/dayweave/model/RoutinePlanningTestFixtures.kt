package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import kotlinx.serialization.json.*

const val PLANNING_WORKSPACE = "00000000-0000-0000-0000-000000000301"
const val PLANNING_OWNER = "00000000-0000-0000-0000-000000000302"
const val PLANNING_PUBLICATION = "00000000-0000-0000-0000-000000000303"

fun planningTestItems() = listOf(
    planningTestItem(ROUTINE_ROOT, null).copy(kind = "routine", recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}", durationSeconds = null,
        durationKind = CanonicalDurationKind.UNKNOWN, durationMinSeconds = null, durationMaxSeconds = null, durationSource = null, isExecutable = false),
    planningTestItem(ROUTINE_CHILD, ROUTINE_ROOT),
    planningTestItem(ROUTINE_OPTIONAL, ROUTINE_ROOT).copy(status = "inbox", title = "Synthetic protected Inbox"),
)

fun planningTestItem(id: String, parent: String?) = CanonicalItemSnapshot(
    id = id, kind = "task", status = "planned", title = "Synthetic protected planning member", notes = "Synthetic private note",
    timezoneName = "UTC", durationSeconds = 600, flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
    importance = 5, urgency = 5, parentId = parent, siblingOrder = 0, isExecutable = true, revision = 7,
    createdAt = "2026-09-09T09:00:00Z", updatedAt = ROUTINE_NOW, hasExplicitStructuralMetadata = true,
)

fun planningTestRequest() = RoutinePlanningWitnessRequest(
    1, RoutinePlanningSchedule.from(SchedulePreviewRequest(
        asOf = ROUTINE_NOW, horizonStart = "2026-09-10T00:00:00Z", horizonEnd = "2026-09-11T00:00:00Z", timezoneName = "UTC",
        availability = listOf(ScheduleAvailabilityRequest("2026-09-10T00:00:00Z", "2026-09-11T00:00:00Z")),
    )), planningTestItems().associate { it.id to it.revision }, "DWR1.synthetic-terminal",
)

fun planningTestWitness() = RoutinePlanningWitness(
    PLANNING_WORKSPACE, PLANNING_OWNER,
    "routine-witness-request-sha256:" + "a".repeat(64), "routine-witness-capture-sha256:" + "b".repeat(64),
    "local-sha256:" + "c".repeat(64), "routine-witness-calendar-sha256:" + "d".repeat(64),
    planningTestRequest().expectedSourceItemRevisions, planningTestRequest().terminalCursor, planningTestRequest().schedule,
    RoutinePlanningLifecycle(2, listOf(RoutinePlanningLifecycleInstance(ROUTINE_ROOT, ROUTINE_PLANNER_ID,
        buildJsonObject { put("type", "calendar_day"); put("date", "2026-09-10"); put("bucket_ordinal", 0) },
        planningTestItems().map { RoutinePlanningLifecycleMember(it.id, it.parentId, it.revision, if (it.id == ROUTINE_CHILD) "completed" else "not_started") },
    ))), 0, 0, PLANNING_PUBLICATION,
)

fun planningTestResponse(witness: RoutinePlanningWitness = planningTestWitness()) = RoutinePlanningWitnessResponse(1, RoutinePlanningWitnessResult.Qualified(witness))

fun planningTestReadyState() = DayWeaveUiState(
    canonicalItems = planningTestItems(), canonicalSyncOrigin = "https://synthetic.example", canonicalConfigurationId = "synthetic-planning-binding",
    canonicalDeltaCursor = "synthetic-canonical-terminal",
    routineOccurrenceLedger = RoutineOccurrenceLedger(syncOrigin = "https://synthetic.example", configurationId = "synthetic-planning-binding",
        deltaCursor = planningTestRequest().terminalCursor),
)

fun planningTestHelperResponse(witness: RoutinePlanningWitness = planningTestWitness()): ByteArray {
    val schedule = witness.schedule
    return buildJsonObject {
        put("protocol", "dayweave.scheduler.helper"); put("version", 2)
        put("result", buildJsonObject {
            put("type", "composition")
            put("composition", buildJsonObject {
                put("local_input_fingerprint", witness.localInputFingerprint)
                put("source_item_count", witness.sourceItemRevisions.size)
                put("source_item_revisions", ROUTINE_PLANNING_JSON.encodeToJsonElement(witness.sourceItemRevisions))
                put("accepted_item_count", witness.sourceItemRevisions.size); put("rejected_items", JsonArray(emptyList())); put("ignored_previous_assignments", JsonArray(emptyList()))
                put("occurrence_snapshot_revision", witness.occurrenceLifecycle.snapshotRevision)
                put("plan", buildJsonObject {
                    put("as_of", schedule.asOf); put("horizon_start", schedule.horizonStart); put("horizon_end", schedule.horizonEnd)
                    for (key in listOf("blocks", "unscheduled", "decisions", "violations")) put(key, JsonArray(emptyList()))
                    put("score", buildJsonObject { for (key in listOf("scheduled_minutes", "unscheduled_minutes", "soft_penalty", "moved_minutes")) put(key, 0) })
                    put("occurrences", JsonArray(witness.occurrenceLifecycle.instances.map { instance -> buildJsonObject {
                        put("id", instance.occurrenceId); put("series_item_id", instance.rootItemId); put("identity", instance.identity)
                        put("nominal_start", schedule.horizonStart); put("nominal_end", schedule.horizonEnd)
                        put("window_start", schedule.horizonStart); put("window_end", schedule.horizonEnd)
                        put("local_date", "2026-09-10"); put("ordinal", 0); put("state", "generated")
                    } }))
                })
            })
        })
    }.toString().plus("\n").toByteArray()
}
