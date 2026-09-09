package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.ItemProgressLedger
import com.greengolddog.dayweave.model.ITEM_PROGRESS_JSON
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

/**
 * Older feature tests construct historic snapshots by relabelling today's empty-progress state.
 * Remove only that provably empty, not-yet-invented field so those fixtures remain historical.
 * Never use this in progress-injection tests, which exercise the unmodified stored bytes.
 */
internal fun PlannerSnapshotEntity.asPreProgressFixtureWhenRelabelled(): PlannerSnapshotEntity {
    if (payloadFormat == PlannerSnapshotFormats.JSON_V22) return this
    val root = Json.parseToJsonElement(payload).jsonObject
    val ledger = root["itemProgressLedger"] ?: return this
    require(ledger == ITEM_PROGRESS_JSON.encodeToJsonElement(ItemProgressLedger())) {
        "A historical fixture cannot discard nonempty progress authority"
    }
    return copy(payload = JsonObject(root - "itemProgressLedger").toString())
}
