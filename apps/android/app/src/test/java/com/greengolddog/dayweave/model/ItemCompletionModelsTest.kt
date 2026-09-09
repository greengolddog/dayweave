package com.greengolddog.dayweave.model

import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ItemCompletionModelsTest {
    @Test fun currentGetBesideEntirelyPublicLocalForestStillRequiresProtectedCompletionEvidence() {
        val state = completionTestState()
        assertNotNull(state.currentCompletionProof(PROGRESS_ITEM))
        assertTrue(state.canonicalItems.none { it.isSensitive })
        assertTrue(state.completionReviewSensitive(PROGRESS_ITEM))
        val retained = state.copy(itemCompletionLedger = state.itemCompletionLedger.copy(
            pending = listOf(completionTestMutation(true).copy(wasSensitive = false))))
            .withPendingSensitivityHardened()
        assertTrue(retained.itemCompletionLedger.pending.single().wasSensitive)
    }

    @Test fun sharedClosedWireFixturesAreDynamicallyAdmittedOrRejected() {
        val fixture = Json.parseToJsonElement(generateSequence(File(requireNotNull(System.getProperty("user.dir")))) {
            it.parentFile }.map { File(it, "fixtures/item-completion/wire-v1.json") }.first(File::isFile).readText()).jsonObject
        for (group in listOf("valid", "invalid")) fixture.getValue(group).jsonArray.forEach { raw ->
            val row = raw.jsonObject
            val body = row.getValue("value").toString()
            val result = runCatching { when (row.getValue("kind").jsonPrimitive.content) {
                "state" -> decodeExactItemCompletion<ItemCompletionPolicyState>(body).requireValid()
                "snapshot" -> decodeExactItemCompletion<ItemCompletionSnapshot>(body).requireValid()
                "command" -> decodeExactItemCompletion<ItemCompletionRequest>(body).requireValid("00000000-0000-0000-0000-000000000001")
                "receipt" -> decodeExactItemCompletion<ItemCompletionMutationResult>(body).requireValid()
                else -> error("Unknown fixture")
            } }
            assertEquals(row.getValue("name").jsonPrimitive.content, group == "valid", result.isSuccess)
        }
    }

    @Test fun rawDuplicateAliasesFractionExponentAndMissingNullCannotBecomeProof() {
        val original = ITEM_PROGRESS_JSON.encodeToString(completionTestSnapshot())
        val invalid = listOf(original.replace("\"revision\":0", "\"revision\":0,\"rev\\u0069sion\":0"),
            original.replace(",\"updated_at\":null", "")) + listOf("0.0", "0e0", "-0", "\"0\"").map {
            original.replace("\"revision\":0", "\"revision\":$it") }
        invalid.forEach { assertTrue(runCatching { decodeExactItemCompletion<ItemCompletionSnapshot>(it) }.isFailure) }
    }

    @Test fun receiptChecksBothRevisionIncrementsAndReviewedPolicy() {
        val request = completionTestMutation().request()
        completionTestResult().requireMatches(PROGRESS_ITEM, request)
        listOf(completionTestResult().copy(operationId = PROGRESS_COMPONENT),
            completionTestResult().copy(completion = completionTestSnapshot(1, 7, ItemCompletionMode.KEEP_OPEN)),
            completionTestResult().copy(completion = completionTestSnapshot(2, 8, ItemCompletionMode.KEEP_OPEN)),
            completionTestResult().copy(completion = completionTestSnapshot(1, 8)))
            .forEach { assertTrue(runCatching { it.requireMatches(PROGRESS_ITEM, request) }.isFailure) }
        assertTrue(runCatching { completionTestResult().requireMatches(PROGRESS_ITEM, request.copy(expectedItemRevision = Long.MAX_VALUE)) }.isFailure)
    }

    @Test fun completedParentRequiresCurrentGlobalProofOrExactChildOnlyProof() {
        val snapshot = completionTestSnapshot(1).let { it.copy(state = it.state.copy(provenance = ItemCompletionProvenance(
            ItemCompletionProvenanceKind.AUTOMATIC, ItemCompletionReopening("planned", null, null, null)))) }
        val base = completionTestState(false).copy(canonicalItems = listOf(progressTestItem().copy(status = "completed")),
            itemCompletionLedger = completionTestLedger(snapshot))
        fun authority(state: DayWeaveUiState) = CanonicalHierarchyParentAuthority.build(state)
        assertNotNull(authority(base).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
        val admitted = base.copy(itemCompletionGetProofs = mapOf(PROGRESS_ITEM to ItemCompletionReadProof(snapshot, base.completionLocalEvidence())))
        assertNull(authority(admitted).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
        val recurring = snapshot.copy(occurrenceEvidenceRequired = true)
        val recurringState = admitted.copy(itemCompletionLedger = completionTestLedger(recurring),
            itemCompletionGetProofs = mapOf(PROGRESS_ITEM to ItemCompletionReadProof(recurring, admitted.completionLocalEvidence())))
        assertNotNull(authority(recurringState).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
        val child = PendingCanonicalAuthoringMutation(id = PROGRESS_OPERATION, itemId = PROGRESS_COMPONENT,
            operation = CanonicalAuthoringOperation.CREATE, createdAt = PROGRESS_NOW,
            draft = CanonicalItemDraft(title = "Synthetic child", timezoneName = "UTC", parentId = PROGRESS_ITEM))
        val queued = admitted.copy(pendingCanonicalAuthoringMutations = listOf(child))
        assertNull(authority(queued).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
        assertNull(queued.currentCompletionProof(PROGRESS_ITEM))
        val scoped = queued.copy(itemCompletionGetProofs = mapOf(PROGRESS_ITEM to ItemCompletionReadProof(snapshot,
            queued.completionLocalEvidence(), child)))
        assertNull(authority(scoped).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
        assertNotNull(authority(scoped).issue(PROGRESS_ITEM, PROGRESS_OPERATION))
        assertNotNull(authority(scoped.copy(pendingCanonicalAuthoringMutations = listOf(child.copy(draft = child.draft?.copy(title = "Other"))))).issue(PROGRESS_ITEM, PROGRESS_COMPONENT))
    }

    @Test fun unrelatedCanonicalExecutionAndPrivacyChangesInvalidateReadPermission() {
        val state = completionTestState()
        assertNotNull(state.currentCompletionProof(PROGRESS_ITEM))
        assertNull(state.copy(canonicalItems = state.canonicalItems + progressTestItem(PROGRESS_COMPONENT)).currentCompletionProof(PROGRESS_ITEM))
        assertNull(state.copy(canonicalExecutionRevision = state.canonicalExecutionRevision + 1).currentCompletionProof(PROGRESS_ITEM))
        val child = progressTestItem(PROGRESS_COMPONENT).copy(parentId = PROGRESS_ITEM, isSensitive = true)
        assertTrue(state.copy(canonicalItems = state.canonicalItems + child).completionReviewSensitive(PROGRESS_ITEM))
        assertTrue(state.copy(canonicalItems = emptyList()).completionReviewSensitive(PROGRESS_ITEM))
    }

    @Test fun samePolicyRevisionAndSameCanonicalRevisionContradictionsAreRejectedBeforeHistoricalIgnore() {
        val current = completionTestSnapshot(2, 9, ItemCompletionMode.KEEP_OPEN)
        val ledger = completionTestLedger(current)
        val contradictions = listOf(completionTestSnapshot(3, 9, ItemCompletionMode.KEEP_OPEN),
            completionTestSnapshot(2, 8, ItemCompletionMode.AUTOMATIC))
        contradictions.forEach { snapshot -> assertTrue(runCatching {
            ledger.withCompletionObservation(ItemCompletionObservation(snapshot, PROGRESS_NOW))
        }.isFailure) }
        assertEquals(ledger, ledger.withCompletionObservation(ItemCompletionObservation(completionTestSnapshot(1, 8, ItemCompletionMode.KEEP_OPEN), PROGRESS_NOW)))
    }

    @Test fun pendingAuthorityAbaAdvancesGenerationAndCannotReviveOldProof() {
        val initial = completionTestState()
        val queued = initial.copy(itemCompletionLedger = initial.itemCompletionLedger.copy(pending = listOf(completionTestMutation())))
            .fenceCompletionEvidence(initial)
        val cleared = queued.copy(itemCompletionLedger = queued.itemCompletionLedger.copy(pending = emptyList()))
            .fenceCompletionEvidence(queued)
        assertEquals(2L, cleared.itemCompletionEvidenceGeneration)
        assertTrue(cleared.itemCompletionGetProofs.isEmpty())
        assertNotEquals(initial.completionLocalEvidence(), cleared.completionLocalEvidence())
    }
}
