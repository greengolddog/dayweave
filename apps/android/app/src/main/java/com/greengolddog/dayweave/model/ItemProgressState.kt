package com.greengolddog.dayweave.model

/** Safe canonical identity is independent of full-item replacement and timer eligibility. */
internal fun DayWeaveUiState.progressItem(itemId: String): CanonicalItemSnapshot? {
    if (canonicalConfigurationId.isNullOrBlank() || canonicalSyncOrigin.isNullOrBlank() || canonicalDeltaCursor.isNullOrBlank()) return null
    val active = canonicalItems.filter { it.deletedAt == null }
    val byId = active.associateBy { it.id }
    if (byId.size != active.size) return null
    val item = byId[itemId] ?: return null
    val visited = mutableSetOf<String>()
    var cursor: CanonicalItemSnapshot? = item
    while (cursor != null) {
        if (!visited.add(cursor.id) || cursor.kind !in PROGRESS_ITEM_KINDS || cursor.status !in PROGRESS_ITEM_STATUSES ||
            runCatching(cursor::requireCanonicalAuthoringShape).isFailure
        ) return null
        val parent = cursor.parentId
        cursor = if (parent == null) null else byId[parent] ?: return null
    }
    return item
}

internal fun DayWeaveUiState.progressReviewIssue(itemId: String): String? {
    val item = progressItem(itemId) ?: return "A complete admitted item and safe hierarchy are required."
    if (itemCompletionLedger.needsCanonicalCatchUp || itemCompletionLedger.pending.any { it.disposition == ItemCompletionDisposition.PENDING }) {
        return "Resolve completion authority before reviewing independent progress."
    }
    if (pendingCanonicalAuthoringMutations.isNotEmpty() || pendingCanonicalMutation != null || pendingProposalApplicationMutation != null) {
        return "Resolve the saved item changes before reviewing new progress."
    }
    val ledger = itemProgressLedger
    if (ledger.syncOrigin != canonicalSyncOrigin || ledger.configurationId != canonicalConfigurationId) return "Progress is not loaded for this account."
    val baseline = ledger.observations[itemId] ?: return "Load this item's progress before editing."
    if (!baseline.isGetProof) return "The saved write is confirmed; refresh the current progress before editing."
    if (baseline.snapshot.itemRevision != item.revision) return "The item changed. Refresh the canonical item before editing progress."
    if (ledger.pending.any { it.itemId == itemId && it.disposition == ItemProgressDisposition.PENDING }) return "This exact progress change is waiting for confirmation."
    return null
}

internal fun ItemProgressLedger.withObservation(observation: ItemProgressObservation): ItemProgressLedger {
    observation.snapshot.requireValid()
    val snapshot = observation.snapshot
    val previous = observations[snapshot.itemId]
    if (previous != null) {
        if (snapshot.revision < previous.snapshot.revision) return this
        if (snapshot.revision == previous.snapshot.revision) {
            require(snapshot.sameProgress(previous.snapshot))
            if (snapshot.itemRevision < previous.snapshot.itemRevision) return this
            if (!observation.isGetProof && previous.isGetProof) return this
        }
    }
    val updated = (observations + (snapshot.itemId to observation)).toMutableMap()
    val protected = pending.mapTo(mutableSetOf()) { it.itemId } + snapshot.itemId
    if (updated.size > 256) {
        updated.entries.filter { it.key !in protected }.sortedBy { it.value.observedAt }
            .take(updated.size - 256).forEach { updated.remove(it.key) }
    }
    return copy(observations = updated).also(ItemProgressLedger::requireValid)
}

internal fun DayWeaveUiState.stageItemProgress(mutation: PendingItemProgressMutation, replacingOperationId: String?): ItemProgressLedger {
    mutation.requireValid()
    require(mutation.disposition == ItemProgressDisposition.PENDING && mutation.submittedAt == null)
    require(progressReviewIssue(mutation.itemId) == null)
    val ledger = itemProgressLedger
    require(ledger.syncOrigin == mutation.syncOrigin && ledger.configurationId == mutation.configurationId)
    require(progressItem(mutation.itemId)?.revision == mutation.expectedItemRevision)
    val baseline = requireNotNull(ledger.observations[mutation.itemId])
    require(baseline.isGetProof && baseline.snapshot.itemRevision == mutation.expectedItemRevision && baseline.snapshot.revision == mutation.expectedProgressRevision)
    val existing = ledger.pending.singleOrNull { it.itemId == mutation.itemId }
    require(existing?.operationId == replacingOperationId)
    require(existing == null || existing.disposition != ItemProgressDisposition.PENDING)
    require(mutation.wasSensitive || !progressReviewSensitive(mutation.itemId))
    require(ledger.pending.none { it.operationId == mutation.operationId })
    return ledger.copy(pending = ledger.pending.filterNot { it.operationId == replacingOperationId } + mutation).also(ItemProgressLedger::requireValid)
}

internal fun DayWeaveUiState.progressReviewSensitive(itemId: String): Boolean =
    CanonicalSensitivityIndex.build(canonicalItems, pendingCanonicalMutation, pendingCanonicalAuthoringMutations)[itemId] ||
        itemProgressLedger.pending.any { it.itemId == itemId && it.wasSensitive }

internal fun DayWeaveUiState.canFirstSendItemProgress(mutation: PendingItemProgressMutation): Boolean {
    val item = progressItem(mutation.itemId) ?: return false
    val baseline = itemProgressLedger.observations[mutation.itemId] ?: return false
    return pendingCanonicalAuthoringMutations.isEmpty() && pendingCanonicalMutation == null && pendingProposalApplicationMutation == null &&
        !itemCompletionLedger.needsCanonicalCatchUp && itemCompletionLedger.pending.none { it.disposition == ItemCompletionDisposition.PENDING } &&
        item.revision == mutation.expectedItemRevision && baseline.isGetProof &&
        baseline.snapshot.itemRevision == mutation.expectedItemRevision && baseline.snapshot.revision == mutation.expectedProgressRevision
}

internal fun ItemProgressLedger.settleItemProgress(mutation: PendingItemProgressMutation, result: ItemProgressMutationResult, now: String): ItemProgressLedger {
    val exact = pending.singleOrNull { it.operationId == mutation.operationId }
    require(exact != null && exact.sameProgressCustody(mutation) && exact.disposition == ItemProgressDisposition.PENDING && exact.submittedAt != null)
    result.progress.requireValid()
    val body = exact.request()
    require(result.operationId == exact.operationId && result.progress.itemId == exact.itemId &&
        result.progress.itemRevision == exact.expectedItemRevision &&
        result.progress.revision == Math.addExact(exact.expectedProgressRevision, 1) && result.progress.components == body.components)
    return copy(pending = pending.filterNot { it.operationId == exact.operationId })
        .withObservation(ItemProgressObservation(result.progress, now, isGetProof = false))
}

/** Only monotonic local privacy hardening may change while the exact wire operation is in flight. */
internal fun PendingItemProgressMutation.sameProgressCustody(expected: PendingItemProgressMutation): Boolean =
    copy(wasSensitive = expected.wasSensitive) == expected && (wasSensitive || !expected.wasSensitive)

private val PROGRESS_ITEM_KINDS = setOf("task", "goal", "project", "routine", "habit", "break", "event")
private val PROGRESS_ITEM_STATUSES = setOf("inbox", "planned", "scheduled", "in_progress", "paused", "blocked", "completed", "skipped", "cancelled")
