package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.ItemProgressLedger
import com.greengolddog.dayweave.model.ItemCompletionLedger
import com.greengolddog.dayweave.model.RoutineOccurrenceLedger
import com.greengolddog.dayweave.model.ITEM_PROGRESS_JSON
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

/**
 * Older feature tests construct historic snapshots by relabelling today's empty-progress state.
 * Remove only that provably empty, not-yet-invented field so those fixtures remain historical.
 * Never use this in progress-injection tests, which exercise the unmodified stored bytes.
 */
internal fun PlannerSnapshotEntity.asPreProgressFixtureWhenRelabelled(): PlannerSnapshotEntity {
    if (payloadFormat == PlannerSnapshotFormats.JSON_V25) return this
    var root = Json.parseToJsonElement(payload).jsonObject
    root["routinePlanningInputCapsule"]?.let {
        require(it == JsonNull) { "A historical fixture cannot discard protected planning input" }
        root = JsonObject(root - "routinePlanningInputCapsule")
    }
    if (payloadFormat == PlannerSnapshotFormats.JSON_V24) return copy(payload = root.toString())
    root["routineOccurrenceLedger"]?.let {
        require(it == ITEM_PROGRESS_JSON.encodeToJsonElement(RoutineOccurrenceLedger())) {
            "A historical fixture cannot discard occurrence authority"
        }
        root = JsonObject(root - "routineOccurrenceLedger")
    }
    if (payloadFormat == PlannerSnapshotFormats.JSON_V23) return copy(payload = root.toString())
    root["itemCompletionLedger"]?.let {
        require(it == ITEM_PROGRESS_JSON.encodeToJsonElement(ItemCompletionLedger())) {
            "A historical fixture cannot discard completion authority"
        }
        root = JsonObject(root - "itemCompletionLedger")
    }
    if (payloadFormat == PlannerSnapshotFormats.JSON_V22) return copy(payload = root.toString())
    val ledger = root["itemProgressLedger"] ?: return copy(payload = root.toString())
    require(ledger == ITEM_PROGRESS_JSON.encodeToJsonElement(ItemProgressLedger())) {
        "A historical fixture cannot discard nonempty progress authority"
    }
    return copy(payload = JsonObject(root - "itemProgressLedger").toString())
}
