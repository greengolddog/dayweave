package com.greengolddog.dayweave.model

internal const val COMPLETION_HASH = "sha256:1111111111111111111111111111111111111111111111111111111111111111"
internal fun completionTestSnapshot(revision: Long = 0, itemRevision: Long = 7, mode: ItemCompletionMode = ItemCompletionMode.AUTOMATIC) =
    ItemCompletionSnapshot(1, PROGRESS_ITEM, itemRevision,
        ItemCompletionPolicyState(PROGRESS_ITEM, revision, true, mode, null, PROGRESS_NOW.takeIf { revision > 0 }),
        COMPLETION_HASH, ItemCompletionCounts(0, 0, 0, 0), false)
internal fun completionTestLedger(snapshot: ItemCompletionSnapshot = completionTestSnapshot()) = ItemCompletionLedger(
    syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION,
    observations = mapOf(PROGRESS_ITEM to ItemCompletionObservation(snapshot, PROGRESS_NOW)))
internal fun completionTestState(proof: Boolean = true): DayWeaveUiState {
    val state = progressTestState().copy(itemCompletionLedger = completionTestLedger(),
        canonicalItems = listOf(progressTestItem().copy(status = "planned", splitPolicyJson = "{\"type\":\"indivisible\"}")))
    return if (proof) state.copy(itemCompletionGetProofs = mapOf(PROGRESS_ITEM to ItemCompletionReadProof(
        completionTestSnapshot(), state.completionLocalEvidence()))) else state
}
internal fun completionTestMutation(submitted: Boolean = false) = PendingItemCompletionMutation(
    operationId = PROGRESS_OPERATION, itemId = PROGRESS_ITEM, syncOrigin = PROGRESS_ORIGIN,
    configurationId = PROGRESS_CONFIGURATION, requestJson = " \n" + ITEM_PROGRESS_JSON.encodeToString(ItemCompletionRequest(
        operationId = PROGRESS_OPERATION, expectedItemRevision = 7, expectedCompletionRevision = 0,
        expectedEvidenceHash = COMPLETION_HASH, requiredForParent = true, mode = ItemCompletionMode.KEEP_OPEN)) + "\n ",
    createdAt = PROGRESS_NOW, submittedAt = PROGRESS_NOW.takeIf { submitted }, wasSensitive = true)
internal fun completionTestResult(replayed: Boolean = false) = ItemCompletionMutationResult(PROGRESS_OPERATION, replayed,
    completionTestSnapshot(1, 8, ItemCompletionMode.KEEP_OPEN))
