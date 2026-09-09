package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test

class ItemProgressStateTest {
    @Test fun allKnownKindsAndLiveStatusesCanReviewOwnProgressWithoutReplacementEligibility() {
        for (kind in listOf("task", "goal", "project", "routine", "habit", "break", "event")) {
            for (status in listOf("inbox", "planned", "scheduled", "in_progress", "paused", "blocked", "completed", "skipped", "cancelled")) {
                val item = progressTestItem().copy(kind = kind, status = status)
                val state = progressTestState().copy(canonicalItems = listOf(item))
                assertEquals("$kind/$status", item, state.progressItem(PROGRESS_ITEM))
                assertNull("$kind/$status", state.progressReviewIssue(PROGRESS_ITEM))
            }
        }
    }

    @Test fun missingDuplicateCyclicUnknownAndUnadmittedIdentityCannotAuthorizeReview() {
        val item = progressTestItem()
        val invalid = listOf(progressTestState().copy(canonicalItems = emptyList()),
            progressTestState().copy(canonicalItems = listOf(item, item)),
            progressTestState().copy(canonicalItems = listOf(item.copy(parentId = PROGRESS_COMPONENT))),
            progressTestState().copy(canonicalItems = listOf(item.copy(parentId = PROGRESS_ITEM))),
            progressTestState().copy(canonicalItems = listOf(item.copy(kind = "future"))),
            progressTestState().copy(canonicalItems = listOf(item.copy(status = "future"))),
            progressTestState().copy(canonicalItems = listOf(item.copy(deletedAt = PROGRESS_NOW))),
            progressTestState().copy(canonicalDeltaCursor = null),
            progressTestState().copy(canonicalConfigurationId = null))
        invalid.forEach { assertNull(it.progressItem(PROGRESS_ITEM)); assertNotNull(it.progressReviewIssue(PROGRESS_ITEM)) }
    }

    @Test fun initialEmptyAndHistoricalWriteObservationsHaveDistinctAdmissionAuthority() {
        val missing = progressTestState().copy(itemProgressLedger = progressTestLedger().copy(observations = emptyMap()))
        assertNotNull(missing.progressReviewIssue(PROGRESS_ITEM))
        val historical = ItemProgressObservation(progressTestSnapshot(1), PROGRESS_NOW, false)
        val state = progressTestState().copy(itemProgressLedger = progressTestLedger().withObservation(historical))
        assertNotNull(state.progressReviewIssue(PROGRESS_ITEM))
        assertThrows(IllegalArgumentException::class.java) { state.stageItemProgress(progressTestMutation(), null) }
        val refreshed = state.copy(itemProgressLedger = state.itemProgressLedger.withObservation(historical.copy(isGetProof = true)))
        assertNull(refreshed.progressReviewIssue(PROGRESS_ITEM))
    }

    @Test fun explicitNewReviewKeepsStickyPrivacyAndRequiresNewOperationIdentity() {
        val rejected = progressTestMutation(submitted = true, sensitive = true).copy(disposition = ItemProgressDisposition.REVIEW_REQUIRED)
        val state = progressTestState().copy(itemProgressLedger = progressTestLedger().copy(pending = listOf(rejected)))
        val operation = "00000000-0000-4000-8000-000000000099"
        val fresh = progressTestMutation(sensitive = true).let { it.copy(operationId = operation,
            requestJson = it.requestJson.replace(PROGRESS_OPERATION, operation)) }
        assertTrue(state.progressReviewSensitive(PROGRESS_ITEM))
        assertThrows(IllegalArgumentException::class.java) { state.stageItemProgress(fresh.copy(wasSensitive = false), rejected.operationId) }
        assertThrows(IllegalArgumentException::class.java) { state.stageItemProgress(fresh, null) }
        assertThrows(IllegalArgumentException::class.java) { state.stageItemProgress(progressTestMutation(sensitive = true), rejected.operationId) }
        assertEquals(listOf(fresh), state.stageItemProgress(fresh, rejected.operationId).pending)
    }

    @Test fun inheritedSensitivityHardensQueuedIntentAndSurvivesLaterDowngrade() {
        val parent = progressTestItem(PROGRESS_COMPONENT).copy(isSensitive = true)
        val child = progressTestItem().copy(parentId = parent.id)
        val state = progressTestState().copy(canonicalItems = listOf(parent, child),
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation())))
        val hardened = state.withPendingSensitivityHardened()
        assertTrue(hardened.itemProgressLedger.pending.single().wasSensitive)
        val downgraded = hardened.copy(canonicalItems = listOf(child.copy(parentId = null))).withPendingSensitivityHardened()
        assertTrue(downgraded.progressReviewSensitive(PROGRESS_ITEM))
        assertTrue(downgraded.itemProgressLedger.pending.single().wasSensitive)
    }

    @Test fun boundedObservationCacheNeverEvictsPendingCustodyAndRejectsEqualRevisionForks() {
        val pending = progressTestMutation(submitted = true)
        val observations = (1..256).associate { index ->
            val id = if (index == 1) PROGRESS_ITEM else UUID.nameUUIDFromBytes("synthetic-progress-$index".toByteArray()).toString()
            id to ItemProgressObservation(progressTestSnapshot().copy(itemId = id), PROGRESS_NOW, true)
        }
        val ledger = progressTestLedger().copy(observations = observations, pending = listOf(pending))
        val newId = UUID.nameUUIDFromBytes("synthetic-new-progress".toByteArray()).toString()
        val bounded = ledger.withObservation(ItemProgressObservation(progressTestSnapshot().copy(itemId = newId), PROGRESS_NOW, true))
        assertEquals(256, bounded.observations.size)
        assertTrue(bounded.observations.containsKey(PROGRESS_ITEM))
        assertTrue(bounded.observations.containsKey(newId))
        assertEquals(listOf(pending), bounded.pending)
        val confirmed = progressTestLedger(progressTestSnapshot(1))
        assertThrows(IllegalArgumentException::class.java) {
            confirmed.withObservation(ItemProgressObservation(progressTestSnapshot(1).copy(components = emptyList()), PROGRESS_NOW, true))
        }
        assertEquals(confirmed, confirmed.withObservation(ItemProgressObservation(progressTestSnapshot(), PROGRESS_NOW, true)))
    }

    @Test fun allSavedDispositionsBlockCredentialReplacementAndDirectAbandonment() {
        for (disposition in ItemProgressDisposition.entries) {
            val store = PlannerStore(progressTestState().copy(itemProgressLedger = progressTestLedger().copy(
                pending = listOf(progressTestMutation().copy(disposition = disposition)))))
            assertTrue(store.hasCredentialReplacementBlocker())
            assertThrows(IllegalArgumentException::class.java) { store.abandonCanonicalConnection() }
        }
        val empty = PlannerStore(progressTestState())
        assertFalse(empty.hasCredentialReplacementBlocker())
        assertNotNull(empty.abandonCanonicalConnection())
        assertEquals(ItemProgressLedger(), empty.state.value.itemProgressLedger)
    }

    @Test fun unresolvedProgressPinsExpiredTombstoneForRecovery() {
        val deletedAt = "2026-01-01T00:00:00Z"
        val tombstone = CanonicalRecentlyDeletedRecord(id = PROGRESS_ITEM, revision = 8, deletedAt = deletedAt,
            lastKnownItem = progressTestItem(), effectiveIsSensitive = true, retentionAnchorAt = deletedAt)
        val state = progressTestState().copy(canonicalItems = emptyList(), canonicalRecentlyDeleted = listOf(tombstone),
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation(submitted = true))))
        val now = Instant.parse(PROGRESS_NOW).toEpochMilli()
        assertEquals(listOf(tombstone.copy(lastKnownItem = null)), state.withCanonicalTrashRetention(now).canonicalRecentlyDeleted)
        assertTrue(state.copy(itemProgressLedger = progressTestLedger()).withCanonicalTrashRetention(now).canonicalRecentlyDeleted.isEmpty())
    }
}
