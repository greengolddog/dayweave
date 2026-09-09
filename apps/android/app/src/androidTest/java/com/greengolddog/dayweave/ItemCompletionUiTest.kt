package com.greengolddog.dayweave

import android.view.WindowManager
import android.view.inspector.WindowInspector
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.security.AppLockSettings
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.ItemCompletionSyncState
import com.greengolddog.dayweave.ui.AppLockPresentationGate
import com.greengolddog.dayweave.ui.authoring.ItemCompletionReviewSheet
import com.greengolddog.dayweave.ui.screens.CanonicalHierarchyBrowserScreen
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.awaitCancellation
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Inert Compose host: no MainActivity, repository, credentials, network, or live account. */
@RunWith(AndroidJUnit4::class)
class ItemCompletionUiTest {
    @get:Rule val compose = createComposeRule()

    @Test fun freshCompletionEvidenceBesidePublicLocalTreeStillUsesSecureWindow() {
        val publicState = state()
        assertTrue(publicState.canonicalItems.none { it.isSensitive })
        assertNotNull(publicState.currentCompletionProof(ITEM))
        showSheet(publicState)
        compose.onNodeWithText("Protected evidence · descendant privacy can change remotely.").assertExists()
        assertSecureReviewWindow()
    }

    @Test fun explicitPolicyReviewPassesExactBaselineAndPreservesFailedSaveFields() {
        val initial = state()
        var saved: SavedReview? = null
        showSheet(initial, onSave = { id, snapshot, required, mode, replacement ->
            saved = SavedReview(id, snapshot, required, mode, replacement)
            false
        })
        compose.onNodeWithText("Current policy · Keep open · revision 2").assertExists()
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOn().performClick()
        compose.onNodeWithTag("item_completion_mode_complete").performScrollTo().performClick()
        compose.onNodeWithTag("item_completion_save").performScrollTo().performClick()
        compose.runOnIdle {
            assertEquals(SavedReview(ITEM, snapshot(), false, ItemCompletionMode.COMPLETE, null), saved)
        }
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOff()
        compose.onNodeWithTag("item_completion_mode_complete").performScrollTo().assertIsSelected()
        compose.onNodeWithText("The change was not saved. Keep this review and retry after refreshing evidence.").assertExists()
    }

    @Test fun updatedGetPreservesFieldsUntilExplicitEvidenceReviewWithoutResettingInput() {
        val current = mutableStateOf(state())
        var saved: SavedReview? = null
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(current.value, ITEM, ItemCompletionSyncState(), true,
            onObserve = {}, onSave = { id, snapshot, required, mode, replacement ->
                saved = SavedReview(id, snapshot, required, mode, replacement); false
            }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_completion_required").performScrollTo().performClick()
        compose.onNodeWithTag("item_completion_mode_automatic").performScrollTo().performClick()
        val refreshed = snapshot().copy(evidenceHash = OTHER_HASH, counts = ItemCompletionCounts(1, 1, 0, 0))
        compose.runOnIdle {
            val changed = current.value.copy(canonicalItems = current.value.canonicalItems.map {
                if (it.id == CHILD) it.copy(status = "completed", revision = 2, completedAt = NOW) else it
            })
            current.value = withProof(changed, refreshed)
        }
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOff()
        compose.onNodeWithTag("item_completion_mode_automatic").performScrollTo().assertIsSelected()
        compose.onNodeWithTag("item_completion_save").performScrollTo().assertIsNotEnabled()
        compose.onNodeWithTag("item_completion_review_latest").performScrollTo().performClick()
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOff()
        compose.onNodeWithTag("item_completion_mode_automatic").performScrollTo().assertIsSelected()
        compose.onNodeWithTag("item_completion_save").performScrollTo().assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(SavedReview(ITEM, refreshed, false, ItemCompletionMode.AUTOMATIC, null), saved) }
    }

    @Test fun newSavedOperationAtSameGetCannotBeOverwrittenByAnOlderNilOperationReview() {
        val current = mutableStateOf(state())
        var saved: SavedReview? = null
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(current.value, ITEM, ItemCompletionSyncState(), true,
            onObserve = {}, onSave = { id, snapshot, required, mode, replacement ->
                saved = SavedReview(id, snapshot, required, mode, replacement); false
            }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_completion_required").performScrollTo().performClick()
        compose.runOnIdle {
            // Keep the same valid GET deliberately: operation identity alone must fence the old form.
            current.value = current.value.copy(itemCompletionLedger = current.value.itemCompletionLedger.copy(
                pending = listOf(pending(ItemCompletionDisposition.REVIEW_REQUIRED))))
        }
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOff()
        compose.onNodeWithTag("item_completion_save").performScrollTo().assertIsNotEnabled()
        compose.runOnIdle { assertNull(saved) }
        compose.onNodeWithTag("item_completion_review_latest").performScrollTo().performClick()
        compose.onNodeWithTag("item_completion_save").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(OPERATION, saved?.replacement); assertFalse(requireNotNull(saved).required) }
    }

    @Test fun missingItemRecoveryIsContentFreeAndOnlyDefinitiveOutcomeAllowsDiscard() {
        val privateObservation = snapshot().copy(state = snapshot().state.copy(mode = ItemCompletionMode.COMPLETE,
            provenance = ItemCompletionProvenance(ItemCompletionProvenanceKind.MANUAL,
                ItemCompletionReopening("blocked", "manual", null, PRIVATE_REASON))))
        val current = mutableStateOf(state().let { it.copy(canonicalItems = emptyList(),
            itemCompletionLedger = it.itemCompletionLedger.copy(
                observations = mapOf(ITEM to ItemCompletionObservation(privateObservation, NOW)), pending = listOf(pending()))) })
        var retries = 0
        var discarded: String? = null
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(current.value, ITEM, ItemCompletionSyncState(), true,
            onObserve = { error("Missing item cannot start selected GET") },
            onSave = { _, _, _, _, _ -> error("Missing item cannot authorize a new command") },
            onRetry = { retries++ }, onDiscardReviewed = { discarded = it }, onDismiss = {}) } }
        compose.onNodeWithTag("item_completion_protected_recovery").assertIsDisplayed()
        assertSecureReviewWindow()
        compose.onNodeWithText(PRIVATE_REASON, substring = true).assertDoesNotExist()
        compose.onNodeWithText(TITLE, substring = true).assertDoesNotExist()
        compose.onNodeWithText("Current required descendants", substring = true).assertDoesNotExist()
        compose.onNodeWithTag("item_completion_save").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_discard").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_retry").performScrollTo().performClick()
        compose.runOnIdle {
            assertEquals(1, retries)
            current.value = current.value.copy(itemCompletionLedger = current.value.itemCompletionLedger.copy(
                pending = listOf(pending(ItemCompletionDisposition.ITEM_MISSING))))
        }
        compose.onNodeWithTag("item_completion_retry").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_discard").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(OPERATION, discarded) }
    }

    @Test fun savedObservationWithoutRuntimeGetProofNeverAuthorizesAnEditor() {
        showSheet(state().copy(itemCompletionGetProofs = emptyMap()))
        compose.onNodeWithTag("item_completion_issue").assertTextEquals("Load current completion evidence before reviewing a change.")
        compose.onNodeWithTag("item_completion_review_latest").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_required").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_save").assertDoesNotExist()
        compose.onNodeWithText("Current policy", substring = true).assertDoesNotExist()
        assertSecureReviewWindow()
    }

    @Test fun durableCatchUpFencePreservesFieldsButCannotReuseAnOldGet() {
        val current = mutableStateOf(state())
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(current.value, ITEM, ItemCompletionSyncState(), true,
            onObserve = {}, onSave = { _, _, _, _, _ -> error("Catch-up cannot authorize save") },
            onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_completion_required").performScrollTo().performClick()
        compose.runOnIdle {
            current.value = current.value.copy(itemCompletionLedger = current.value.itemCompletionLedger.copy(needsCanonicalCatchUp = true))
        }
        compose.onNodeWithTag("item_completion_required").performScrollTo().assertIsOff()
        compose.onNodeWithTag("item_completion_save").performScrollTo().assertIsNotEnabled()
        compose.onNodeWithTag("item_completion_review_latest").assertDoesNotExist()
        compose.onNodeWithTag("item_completion_issue").assertTextEquals("Canonical catch-up is required before completion review.")
        assertSecureReviewWindow()
    }

    @Test fun sensitiveDescendantProtectsPublicParentReviewAndProtectionStaysAfterDowngrade() {
        val current = mutableStateOf(state(sensitiveChild = true))
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(current.value, ITEM, ItemCompletionSyncState(), true,
            onObserve = {}, onSave = { _, _, _, _, _ -> false }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_completion_sheet").assertIsDisplayed()
        assertSecureReviewWindow()
        compose.runOnIdle { current.value = state(sensitiveChild = false) }
        assertSecureReviewWindow()
        compose.onNodeWithTag("item_completion_required").assertExists()
    }

    @Test fun lockingDisposesCompletionContentAndCancelsTheSelectedObserver() {
        val locked = mutableStateOf(false)
        val entered = AtomicBoolean(false)
        val cancelled = AtomicBoolean(false)
        val initial = state(sensitiveChild = true)
        compose.setContent { MaterialTheme {
            AppLockPresentationGate(AppLockState(AppLockSettings(), isLocked = locked.value),
                lockedContent = { Text("Synthetic locked completion host") }, unlockedContent = {
                    ItemCompletionReviewSheet(initial, ITEM, ItemCompletionSyncState(), true,
                        onObserve = { entered.set(true); try { awaitCancellation() } finally { cancelled.set(true) } },
                        onSave = { _, _, _, _, _ -> false }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {})
                })
        } }
        compose.waitUntil(10_000) { entered.get() }
        compose.onNodeWithTag("item_completion_sheet").assertIsDisplayed()
        compose.runOnIdle { locked.value = true }
        compose.waitUntil(10_000) { cancelled.get() }
        compose.onNodeWithTag("item_completion_sheet").assertDoesNotExist()
        compose.onNodeWithText(TITLE).assertDoesNotExist()
        compose.onNodeWithText("Synthetic locked completion host").assertIsDisplayed()
    }

    @Test fun completedParentHasIndependentCompletionEntryWithoutFullReplacementActions() {
        val completed = state().let { it.copy(canonicalItems = it.canonicalItems.map { row ->
            if (row.id == ITEM) row.copy(status = "completed", completedAt = NOW) else row
        }) }
        val initial = withProof(completed, snapshot().copy(state = snapshot().state.copy(mode = ItemCompletionMode.COMPLETE,
            provenance = ItemCompletionProvenance(ItemCompletionProvenanceKind.MANUAL,
                ItemCompletionReopening("planned", null, null, null)))))
        var selected: String? = null
        compose.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(state = initial,
                syncState = CanonicalSyncState(CanonicalSyncPhase.OFFLINE, "Synthetic offline"), kind = ItemKind.GOAL,
                actionsEnabled = false, onOpenEditor = {}, onBack = {}, onOpenCompletion = { selected = it })
        } }
        compose.waitUntil(10_000) { compose.onAllNodesWithTag("hierarchy_row_$ITEM").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("hierarchy_row_$ITEM").performClick()
        compose.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        compose.onNodeWithTag("hierarchy_completion_policy").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(ITEM, selected) }
    }

    private data class SavedReview(val itemId: String, val snapshot: ItemCompletionSnapshot,
        val required: Boolean, val mode: ItemCompletionMode, val replacement: String?)

    private fun showSheet(initial: DayWeaveUiState,
        onSave: suspend (String, ItemCompletionSnapshot, Boolean, ItemCompletionMode, String?) -> Boolean = { _, _, _, _, _ -> false },
    ) {
        compose.setContent { MaterialTheme { ItemCompletionReviewSheet(initial, ITEM, ItemCompletionSyncState(), true,
            onObserve = {}, onSave = onSave, onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
    }

    private fun assertSecureReviewWindow() = compose.runOnIdle {
        assertTrue(WindowInspector.getGlobalWindowViews().any {
            val flags = (it.layoutParams as? WindowManager.LayoutParams)?.flags ?: 0
            flags and WindowManager.LayoutParams.FLAG_SECURE != 0
        })
    }

    private fun snapshot() = ItemCompletionSnapshot(1, ITEM, 7,
        ItemCompletionPolicyState(ITEM, 2, true, ItemCompletionMode.KEEP_OPEN, null, NOW),
        HASH, ItemCompletionCounts(1, 0, 1, 0), false)

    private fun state(sensitiveChild: Boolean = false): DayWeaveUiState {
        val parent = CanonicalItemSnapshot(id = ITEM, kind = "goal", status = "planned", title = TITLE,
            timezoneName = "UTC", flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 1, urgency = 1, siblingOrder = 0, isExecutable = false, revision = 7, createdAt = NOW, updatedAt = NOW)
        val child = parent.copy(id = CHILD, parentId = ITEM, kind = "task", title = "Synthetic required leaf",
            isSensitive = sensitiveChild, isExecutable = true, revision = 1, durationSeconds = 1_800,
            durationKind = CanonicalDurationKind.EXACT, durationMinSeconds = 1_800, durationMaxSeconds = 1_800,
            durationSource = CanonicalDurationSource.USER)
        return withProof(DayWeaveUiState(canonicalItems = listOf(parent, child), canonicalSyncOrigin = ORIGIN,
            canonicalConfigurationId = CONFIG, canonicalDeltaCursor = "synthetic-complete",
            itemCompletionLedger = ItemCompletionLedger(syncOrigin = ORIGIN, configurationId = CONFIG)), snapshot())
    }

    private fun withProof(state: DayWeaveUiState, snapshot: ItemCompletionSnapshot): DayWeaveUiState {
        snapshot.requireValid()
        val observed = state.copy(itemCompletionLedger = state.itemCompletionLedger.copy(
            observations = mapOf(ITEM to ItemCompletionObservation(snapshot, NOW))))
        return observed.copy(itemCompletionGetProofs = mapOf(ITEM to ItemCompletionReadProof(snapshot, observed.completionLocalEvidence())))
    }

    private fun pending(disposition: ItemCompletionDisposition = ItemCompletionDisposition.PENDING) =
        PendingItemCompletionMutation(operationId = OPERATION, itemId = ITEM, syncOrigin = ORIGIN, configurationId = CONFIG,
            requestJson = " \n" + ITEM_PROGRESS_JSON.encodeToString(ItemCompletionRequest(operationId = OPERATION,
                expectedItemRevision = 7, expectedCompletionRevision = 2, expectedEvidenceHash = HASH,
                requiredForParent = true, mode = ItemCompletionMode.KEEP_OPEN)) + "\n ",
            createdAt = NOW, submittedAt = NOW, disposition = disposition, wasSensitive = true)
            .also(PendingItemCompletionMutation::requireValid)

    private companion object {
        const val ITEM = "00000000-0000-4000-8000-000000000001"
        const val CHILD = "00000000-0000-4000-8000-000000000002"
        const val OPERATION = "00000000-0000-4000-8000-000000000100"
        const val NOW = "2026-09-09T10:00:00.123456Z"
        const val ORIGIN = "https://api.example.test/"
        const val CONFIG = "synthetic-completion-binding"
        const val TITLE = "Synthetic completion goal"
        const val PRIVATE_REASON = "SYNTHETIC PRIVATE REOPENING REASON"
        const val HASH = "sha256:1111111111111111111111111111111111111111111111111111111111111111"
        const val OTHER_HASH = "sha256:2222222222222222222222222222222222222222222222222222222222222222"
    }
}
