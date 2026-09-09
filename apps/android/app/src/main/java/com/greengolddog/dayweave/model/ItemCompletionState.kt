package com.greengolddog.dayweave.model

import java.security.MessageDigest

/** Local invalidation only. The server's opaque complete-forest hash is never recreated here. */
internal fun DayWeaveUiState.completionLocalEvidence(): String {
    val digest = MessageDigest.getInstance("SHA-256")
    fun field(value: String?) {
        val bytes = value.orEmpty().toByteArray(Charsets.UTF_8)
        digest.update(bytes.size.toString().toByteArray(Charsets.UTF_8))
        digest.update(0.toByte())
        digest.update(bytes)
    }
    field("dayweave.android.completion-local.v1")
    field(itemCompletionEvidenceGeneration.toString())
    field(canonicalSyncOrigin); field(canonicalConfigurationId); field(canonicalDeltaCursor)
    canonicalItems.sortedBy { it.id }.forEach { field(ITEM_PROGRESS_JSON.encodeToString(it)) }
    field(canonicalExecutionSyncOrigin); field(canonicalExecutionConfigurationId)
    field(canonicalExecutionRevision.toString())
    field(canonicalExecutionSession?.let { ITEM_PROGRESS_JSON.encodeToString(it) })
    return "sha256:" + digest.digest().joinToString("") { "%02x".format(it) }
}

internal fun DayWeaveUiState.completionItem(itemId: String): CanonicalItemSnapshot? = progressItem(itemId)

/** Irreversible runtime fencing for pending-intent ABA and cancellation-insensitive reads. */
internal fun DayWeaveUiState.fenceCompletionEvidence(previous: DayWeaveUiState): DayWeaveUiState {
    val changed = canonicalSyncOrigin != previous.canonicalSyncOrigin || canonicalConfigurationId != previous.canonicalConfigurationId ||
        canonicalDeltaCursor != previous.canonicalDeltaCursor || canonicalItems != previous.canonicalItems ||
        canonicalExecutionSyncOrigin != previous.canonicalExecutionSyncOrigin ||
        canonicalExecutionConfigurationId != previous.canonicalExecutionConfigurationId ||
        canonicalExecutionRevision != previous.canonicalExecutionRevision || canonicalExecutionSession != previous.canonicalExecutionSession ||
        pendingCanonicalMutation != previous.pendingCanonicalMutation || pendingCanonicalAuthoringMutations != previous.pendingCanonicalAuthoringMutations ||
        pendingExecutionCommand != previous.pendingExecutionCommand || pendingExecutionDeferIntent != previous.pendingExecutionDeferIntent ||
        pendingProposalApplicationMutation != previous.pendingProposalApplicationMutation || pendingSchedulePublication != previous.pendingSchedulePublication ||
        itemCompletionLedger.pending != previous.itemCompletionLedger.pending
    return if (changed) copy(itemCompletionEvidenceGeneration = Math.addExact(previous.itemCompletionEvidenceGeneration, 1),
        itemCompletionGetProofs = emptyMap()) else this
}

internal fun DayWeaveUiState.hasCompletionMutationBlocker(): Boolean =
    pendingCanonicalAuthoringMutations.isNotEmpty() || pendingCanonicalMutation != null ||
        pendingProposalApplicationMutation != null || pendingSchedulePublication != null ||
        pendingExecutionCommand != null || pendingExecutionDeferIntent != null

internal fun DayWeaveUiState.currentCompletionProof(
    itemId: String,
    localEvidence: String = completionLocalEvidence(),
): ItemCompletionReadProof? {
    val ledger = itemCompletionLedger
    if (ledger.syncOrigin != canonicalSyncOrigin || ledger.configurationId != canonicalConfigurationId ||
        ledger.needsCanonicalCatchUp || hasCompletionMutationBlocker()) return null
    val item = completionItem(itemId) ?: return null
    val proof = itemCompletionGetProofs[itemId] ?: return null
    if (proof.authoringMutation != null || proof.localEvidence != localEvidence || proof.snapshot.itemRevision != item.revision ||
        ledger.observations[itemId]?.snapshot != proof.snapshot) return null
    return proof
}

internal fun DayWeaveUiState.completionReviewIssue(itemId: String): String? {
    if (completionItem(itemId) == null) return "A complete admitted item and hierarchy are required."
    if (hasCompletionMutationBlocker()) return "Resolve saved canonical or execution changes before reviewing completion."
    if (itemCompletionLedger.needsCanonicalCatchUp) return "Canonical catch-up is required before completion review."
    if (currentCompletionProof(itemId) == null) return "Load current completion evidence before reviewing a change."
    if (itemCompletionLedger.pending.any { it.disposition == ItemCompletionDisposition.PENDING }) {
        return "An exact completion change is awaiting confirmation."
    }
    return null
}

/** The lifecycle exemption is revision-bound sidecar authority, not a new executable status. */
internal fun DayWeaveUiState.hasQualifiedCompletedParent(itemId: String, childId: String, localEvidence: String): Boolean {
    if (itemCompletionLedger.pending.any { it.disposition == ItemCompletionDisposition.PENDING }) return false
    val item = completionItem(itemId) ?: return false
    val proof = itemCompletionGetProofs[itemId] ?: return false
    val candidate = pendingCanonicalAuthoringMutations.singleOrNull { it.itemId == childId }
    if (proof.authoringMutation != null && (proof.authoringMutation != candidate || candidate.isSubmitted)) return false
    // Saving injects the exact new candidate into hierarchy validation. Only that candidate
    // may be excluded; unrelated queued authority still invalidates the global GET admission.
    if (candidate != null && (candidate.isSubmitted || candidate.disposition != CanonicalAuthoringDisposition.PENDING)) return false
    val reviewState = copy(pendingCanonicalAuthoringMutations = pendingCanonicalAuthoringMutations.filterNot { it == candidate })
    if (reviewState.hasCompletionMutationBlocker() || itemCompletionLedger.needsCanonicalCatchUp ||
        itemCompletionLedger.syncOrigin != canonicalSyncOrigin || itemCompletionLedger.configurationId != canonicalConfigurationId ||
        proof.localEvidence != localEvidence || proof.snapshot.itemRevision != item.revision ||
        itemCompletionLedger.observations[itemId]?.snapshot != proof.snapshot) return false
    return item.status == "completed" && proof.snapshot.state.provenance != null && !proof.snapshot.occurrenceEvidenceRequired &&
        runCatching(proof.snapshot::requireValid).isSuccess
}

/**
 * V1 supplies no privacy/cursor witness: a newly sensitive remote descendant can change
 * aggregates without changing the selected item revision. Even a fresh GET beside a locally
 * public tree therefore remains protected. This also monotonically hardens retained intent.
 */
internal fun DayWeaveUiState.completionReviewSensitive(@Suppress("UNUSED_PARAMETER") itemId: String): Boolean = true

internal fun ItemCompletionLedger.withCompletionObservation(observation: ItemCompletionObservation): ItemCompletionLedger {
    observation.snapshot.requireValid()
    val incoming = observation.snapshot
    val previous = observations[incoming.itemId]?.snapshot
    if (previous != null) {
        if (incoming.state.revision == previous.state.revision) require(incoming.state == previous.state)
        if (incoming.itemRevision == previous.itemRevision) require(incoming.state == previous.state)
        if (incoming.state.revision < previous.state.revision || incoming.itemRevision < previous.itemRevision) return this
    }
    val updated = (observations + (incoming.itemId to observation)).toMutableMap()
    val pinned = pending.mapTo(hashSetOf()) { it.itemId } + incoming.itemId
    updated.entries.filter { it.key !in pinned }.sortedBy { it.value.observedAt }
        .take((updated.size - 256).coerceAtLeast(0)).forEach { updated.remove(it.key) }
    return copy(observations = updated).also(ItemCompletionLedger::requireValid)
}

internal fun DayWeaveUiState.stageItemCompletion(
    mutation: PendingItemCompletionMutation,
    replacingOperationId: String?,
): ItemCompletionLedger {
    mutation.requireValid()
    require(mutation.submittedAt == null && mutation.disposition == ItemCompletionDisposition.PENDING)
    require(completionReviewIssue(mutation.itemId) == null)
    val proof = requireNotNull(currentCompletionProof(mutation.itemId))
    val command = mutation.request()
    require(command.expectedItemRevision == proof.snapshot.itemRevision &&
        command.expectedCompletionRevision == proof.snapshot.state.revision &&
        command.expectedEvidenceHash == proof.snapshot.evidenceHash)
    require(itemCompletionLedger.syncOrigin == mutation.syncOrigin && itemCompletionLedger.configurationId == mutation.configurationId)
    val previous = itemCompletionLedger.pending.singleOrNull { it.itemId == mutation.itemId }
    require(previous?.operationId == replacingOperationId)
    require(previous == null || previous.disposition != ItemCompletionDisposition.PENDING)
    require(mutation.wasSensitive || !completionReviewSensitive(mutation.itemId))
    require(itemCompletionLedger.pending.none { it.operationId == mutation.operationId })
    return itemCompletionLedger.copy(pending = itemCompletionLedger.pending.filterNot { it.operationId == replacingOperationId } + mutation)
        .also(ItemCompletionLedger::requireValid)
}

internal fun PendingItemCompletionMutation.sameCompletionCustody(expected: PendingItemCompletionMutation): Boolean =
    copy(wasSensitive = expected.wasSensitive) == expected && (wasSensitive || !expected.wasSensitive)

internal fun ItemCompletionLedger.settleItemCompletion(
    expected: PendingItemCompletionMutation,
    result: ItemCompletionMutationResult,
    now: String,
): ItemCompletionLedger {
    val saved = requireNotNull(pending.singleOrNull { it.operationId == expected.operationId })
    require(saved.sameCompletionCustody(expected) && saved.submittedAt != null && saved.disposition == ItemCompletionDisposition.PENDING)
    result.requireMatches(saved.itemId, saved.request())
    return copy(pending = pending.filterNot { it.operationId == saved.operationId }, needsCanonicalCatchUp = true)
        .withCompletionObservation(ItemCompletionObservation(result.completion, now))
}
