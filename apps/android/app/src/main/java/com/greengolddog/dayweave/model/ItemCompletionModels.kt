package com.greengolddog.dayweave.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import java.time.Instant

@Serializable
enum class ItemCompletionMode {
    @SerialName("automatic") AUTOMATIC,
    @SerialName("keep_open") KEEP_OPEN,
    @SerialName("complete") COMPLETE,
}

@Serializable
enum class ItemCompletionProvenanceKind {
    @SerialName("automatic") AUTOMATIC,
    @SerialName("manual") MANUAL,
}

@Serializable
data class ItemCompletionReopening(
    val status: String,
    @SerialName("blocked_reason_kind") val blockedReasonKind: String?,
    @SerialName("blocked_by_item_id") val blockedByItemId: String?,
    @SerialName("blocked_reason") val blockedReason: String?,
) {
    fun requireValid(itemId: String) {
        require(status in setOf("inbox", "planned", "blocked"))
        blockedReason?.let { requireProgressText(it, 1_000) }
        when {
            status != "blocked" -> require(blockedReasonKind == null && blockedByItemId == null && blockedReason == null)
            blockedReasonKind == "dependency" -> {
                requireCanonicalUuid(requireNotNull(blockedByItemId), "completion blocker")
                require(blockedByItemId != itemId)
            }
            blockedReasonKind in setOf("manual", "external") -> require(blockedByItemId == null && blockedReason != null)
            else -> error("Invalid completion reopening custody")
        }
    }
}

@Serializable
data class ItemCompletionProvenance(val kind: ItemCompletionProvenanceKind, val reopen: ItemCompletionReopening)

@Serializable
data class ItemCompletionPolicyState(
    @SerialName("item_id") val itemId: String,
    val revision: Long,
    @SerialName("required_for_parent") val requiredForParent: Boolean,
    val mode: ItemCompletionMode,
    val provenance: ItemCompletionProvenance?,
    @SerialName("updated_at") val updatedAt: String?,
) {
    fun requireValid() {
        requireCanonicalUuid(itemId, "completion item")
        require(revision >= 0)
        if (revision == 0L) require(requiredForParent && mode == ItemCompletionMode.AUTOMATIC && provenance == null && updatedAt == null)
        else requireCompletionInstant(requireNotNull(updatedAt))
        if (mode == ItemCompletionMode.COMPLETE) require(provenance != null)
        provenance?.let {
            it.reopen.requireValid(itemId)
            require(mode == ItemCompletionMode.AUTOMATIC && it.kind == ItemCompletionProvenanceKind.AUTOMATIC ||
                mode == ItemCompletionMode.COMPLETE && it.kind == ItemCompletionProvenanceKind.MANUAL)
        }
    }
}

@Serializable
data class ItemCompletionCounts(
    @SerialName("required_descendants") val requiredDescendants: Long,
    val completed: Long,
    val incomplete: Long,
    @SerialName("occurrence_evidence_required") val occurrenceEvidenceRequired: Long,
) {
    fun requireValid() {
        require(listOf(requiredDescendants, completed, incomplete, occurrenceEvidenceRequired).all { it in 0..20_000 })
        require(Math.addExact(Math.addExact(completed, incomplete), occurrenceEvidenceRequired) == requiredDescendants)
    }
}

@Serializable
data class ItemCompletionSnapshot(
    @SerialName("schema_version") val schemaVersion: Int,
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    val state: ItemCompletionPolicyState,
    @SerialName("evidence_hash") val evidenceHash: String,
    val counts: ItemCompletionCounts,
    @SerialName("occurrence_evidence_required") val occurrenceEvidenceRequired: Boolean,
) {
    fun requireValid() {
        require(schemaVersion == 1 && itemRevision > 0)
        state.requireValid()
        require(itemId == state.itemId)
        requireCompletionHash(evidenceHash)
        counts.requireValid()
    }
}

@Serializable
data class ItemCompletionRequest(
    @SerialName("schema_version") val schemaVersion: Int = 1,
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_item_revision") val expectedItemRevision: Long,
    @SerialName("expected_completion_revision") val expectedCompletionRevision: Long,
    @SerialName("expected_evidence_hash") val expectedEvidenceHash: String,
    @SerialName("required_for_parent") val requiredForParent: Boolean,
    val mode: ItemCompletionMode,
    val reopening: ItemCompletionReopening? = null,
) {
    fun requireValid(itemId: String) {
        requireCanonicalUuid(itemId, "completion target")
        requireCanonicalUuid(operationId, "completion operation")
        require(schemaVersion == 1 && expectedItemRevision > 0 && expectedCompletionRevision >= 0)
        requireCompletionHash(expectedEvidenceHash)
        reopening?.requireValid(itemId)
    }
}

@Serializable
data class ItemCompletionMutationResult(
    @SerialName("operation_id") val operationId: String,
    val replayed: Boolean,
    val completion: ItemCompletionSnapshot,
) {
    fun requireValid() {
        requireCanonicalUuid(operationId, "completion operation")
        completion.requireValid()
        require(completion.itemRevision >= 2 && completion.state.revision > 0)
    }

    fun requireMatches(itemId: String, request: ItemCompletionRequest) {
        requireValid()
        require(operationId == request.operationId && completion.itemId == itemId)
        require(completion.itemRevision == Math.addExact(request.expectedItemRevision, 1))
        require(completion.state.revision == Math.addExact(request.expectedCompletionRevision, 1))
        require(completion.state.mode == request.mode && completion.state.requiredForParent == request.requiredForParent)
    }
}

@Serializable
enum class ItemCompletionDisposition { PENDING, REVIEW_REQUIRED, ITEM_MISSING, REJECTED }

@Serializable
data class PendingItemCompletionMutation(
    val schemaVersion: Int = 1,
    val operationId: String,
    val itemId: String,
    val syncOrigin: String,
    val configurationId: String,
    val requestJson: String,
    val createdAt: String,
    val submittedAt: String? = null,
    val disposition: ItemCompletionDisposition = ItemCompletionDisposition.PENDING,
    /** Monotonic: reviewing completion can reveal any required/sensitive descendant. */
    val wasSensitive: Boolean = true,
) {
    fun request(): ItemCompletionRequest = decodeExactItemCompletion(requestJson)
    fun requireValid() {
        require(schemaVersion == 1 && syncOrigin.isNotBlank() && configurationId.isNotBlank())
        require(syncOrigin.length <= 4_096 && configurationId.length <= 4_096)
        require(requestJson.toByteArray(Charsets.UTF_8).size <= 16_384)
        requireCompletionInstant(createdAt)
        submittedAt?.let(::requireCompletionInstant)
        request().let {
            it.requireValid(itemId)
            require(it.operationId == operationId && it.reopening == null)
        }
    }
}

@Serializable
data class ItemCompletionObservation(val snapshot: ItemCompletionSnapshot, val observedAt: String)

@Serializable
data class ItemCompletionLedger(
    val schemaVersion: Int = 1,
    val syncOrigin: String? = null,
    val configurationId: String? = null,
    val observations: Map<String, ItemCompletionObservation> = emptyMap(),
    val pending: List<PendingItemCompletionMutation> = emptyList(),
    /** A confirmed policy write changes canonical lifecycle; restart must not forget catch-up. */
    val needsCanonicalCatchUp: Boolean = false,
) {
    fun requireValid() {
        require(schemaVersion == 1 && (syncOrigin == null) == (configurationId == null))
        if (syncOrigin == null) require(observations.isEmpty() && pending.isEmpty() && !needsCanonicalCatchUp)
        else require(syncOrigin.isNotBlank() && !configurationId.isNullOrBlank())
        require(observations.size <= 256 && pending.size <= 64)
        require(pending.sumOf { it.requestJson.toByteArray(Charsets.UTF_8).size.toLong() } <= 1_048_576)
        require(pending.map { it.operationId }.distinct().size == pending.size)
        require(pending.map { it.itemId }.distinct().size == pending.size)
        observations.forEach { (id, value) ->
            value.snapshot.requireValid()
            require(id == value.snapshot.itemId)
            requireCompletionInstant(value.observedAt)
        }
        pending.forEach {
            it.requireValid()
            require(it.syncOrigin == syncOrigin && it.configurationId == configurationId)
        }
    }
}

/** Runtime-only current GET admission. It is never restored from encrypted cache or receipts. */
data class ItemCompletionReadProof(
    val snapshot: ItemCompletionSnapshot,
    val localEvidence: String,
    /** First-send parent permission only; never grants a new policy review or picker choice. */
    val authoringMutation: PendingCanonicalAuthoringMutation? = null,
)

internal inline fun <reified T> decodeExactItemCompletion(body: String): T = decodeExactItemProgress(body)

internal fun requireCompletionHash(value: String) { require(Regex("sha256:[0-9a-f]{64}").matches(value)) }

internal fun requireCompletionInstant(value: String): Instant {
    require(Regex("[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,6})?(?:Z|\\+00:00)").matches(value))
    return requireCanonicalInstant(value, "completion timestamp").also { require(it.nano % 1_000 == 0) }
}
