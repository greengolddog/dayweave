package com.greengolddog.dayweave

import android.graphics.Bitmap
import android.view.WindowManager
import android.view.inspector.WindowInspector
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.security.AppLockSettings
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.ItemProgressSyncState
import com.greengolddog.dayweave.ui.AppLockPresentationGate
import com.greengolddog.dayweave.ui.authoring.CanonicalAuthoringList
import com.greengolddog.dayweave.ui.authoring.ItemProgressReviewSheet
import com.greengolddog.dayweave.ui.screens.CanonicalHierarchyBrowserScreen
import java.util.concurrent.atomic.AtomicBoolean
import java.io.File
import kotlinx.coroutines.awaitCancellation
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Synthetic Compose host only: never MainActivity, app stores, credentials, or a transport. */
@RunWith(AndroidJUnit4::class)
class ItemProgressUiTest {
    @get:Rule val compose = createComposeRule()

    @Test fun unresolvedCanonicalCatchUpKeepsTypedReviewProtectedAndDisabled() {
        val sync = mutableStateOf(ItemProgressSyncState())
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(state(), ITEM, sync.value, true,
            onLoad = { false }, onSave = { _, _, _, _, _ -> error("Catch-up is required") },
            onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_add_percentage").performScrollTo().performClick()
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().performTextInput("Synthetic retained catchup review")
        compose.runOnIdle { sync.value = ItemProgressSyncState(canonicalCatchUpItemIds = setOf(ITEM)) }
        compose.onNodeWithTag("item_progress_name_0").assertTextContains("Synthetic retained catchup review")
        compose.onNodeWithTag("item_progress_save").performScrollTo().assertIsNotEnabled()
        compose.onNodeWithTag("item_progress_review_latest").assertDoesNotExist()
        assertSecureReviewWindow()
    }

    @Test fun refreshedObservationNeverOverwritesTypedEditorAndRequiresExplicitReview() {
        val current = mutableStateOf(state())
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(current.value, ITEM, ItemProgressSyncState(), true,
            onLoad = { false }, onSave = { _, _, _, _, _ -> error("Stale review cannot save") },
            onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_add_percentage").performScrollTo().performClick()
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().performTextInput("Synthetic retained input")
        compose.onNodeWithTag("item_progress_percentage_0").performScrollTo().performTextClearance()
        compose.onNodeWithTag("item_progress_percentage_0").performTextInput("42.50")
        compose.runOnIdle { current.value = state(listOf(ItemProgressComponent(COMPONENT, "Synthetic server observation",
            ItemProgressValue.Percentage(7500)))) }
        compose.onNodeWithTag("item_progress_percentage_0").assertTextContains("42.50")
        compose.onNodeWithTag("item_progress_name_0").assertTextContains("Synthetic retained input")
        compose.onNodeWithTag("item_progress_save").performScrollTo().assertIsNotEnabled()
    }

    @Test fun newlyQueuedIntentAtSameGetBaselineCannotBeReplacedByAnOlderOpenEditor() {
        val current = mutableStateOf(state())
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(current.value, ITEM, ItemProgressSyncState(), true,
            onLoad = { false }, onSave = { _, _, _, _, _ -> error("Old nil-operation review cannot replace new intent") },
            onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_add_percentage").performScrollTo().performClick()
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().performTextInput("Synthetic unqueued input")
        compose.runOnIdle { current.value = current.value.copy(itemProgressLedger = current.value.itemProgressLedger.copy(
            pending = listOf(pending(ItemProgressDisposition.REVIEW_REQUIRED)))) }
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().assertTextContains("Synthetic unqueued input")
        compose.onNodeWithTag("item_progress_save").performScrollTo().assertIsNotEnabled()
    }

    @Test fun reReviewedReplacementCannotBeOverwrittenByEditorForPreviousOperation() {
        val reviewed = pending(ItemProgressDisposition.REVIEW_REQUIRED)
        val current = mutableStateOf(state().let { it.copy(itemProgressLedger = it.itemProgressLedger.copy(pending = listOf(reviewed))) })
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(current.value, ITEM, ItemProgressSyncState(), true,
            onLoad = { false }, onSave = { _, _, _, _, _ -> error("Old operation review cannot replace new intent") },
            onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_review_latest").performScrollTo().performClick()
        compose.onNodeWithTag("item_progress_current_0").performScrollTo().performTextClearance()
        compose.onNodeWithTag("item_progress_current_0").performTextInput("3.25")
        val newOperation = "00000000-0000-4000-8000-000000000099"
        compose.runOnIdle { current.value = current.value.copy(itemProgressLedger = current.value.itemProgressLedger.copy(
            pending = listOf(reviewed.copy(operationId = newOperation,
                requestJson = reviewed.requestJson.replace(OPERATION, newOperation))))) }
        compose.onNodeWithTag("item_progress_current_0").assertTextContains("3.25")
        compose.onNodeWithTag("item_progress_save").performScrollTo().assertIsNotEnabled()
    }

    @Test fun completedGoalOpensIndependentReviewWithoutFullReplacementEligibility() {
        var selected: String? = null
        compose.setContent {
            MaterialTheme {
                CanonicalHierarchyBrowserScreen(state = state().copy(canonicalItems = listOf(item().copy(status = "completed"))),
                    syncState = CanonicalSyncState(CanonicalSyncPhase.OFFLINE, "Synthetic offline"), kind = ItemKind.GOAL,
                    actionsEnabled = false, onOpenEditor = {}, onBack = {}, onOpenProgress = { selected = it })
            }
        }
        compose.waitUntil(10_000) { compose.onAllNodesWithTag("hierarchy_row_$ITEM").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("hierarchy_row_$ITEM").performClick()
        compose.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        compose.onNodeWithTag("hierarchy_independent_progress").performClick()
        compose.runOnIdle { assertEquals(ITEM, selected) }
    }

    @Test fun failedInitialLoadNeverInventsEmptyProgressOrEnablesReview() {
        showSheet(state().copy(itemProgressLedger = ItemProgressLedger(syncOrigin = ORIGIN, configurationId = CONFIG)))
        compose.onNodeWithTag("item_progress_issue").assertTextEquals("Load this item's progress before editing.")
        compose.onNodeWithTag("item_progress_save").assertDoesNotExist()
        compose.onNodeWithText("Saved observation: no independent components recorded.").assertDoesNotExist()
    }

    @Test fun explicitPercentageReviewPassesExactBasisPointsAndKeepsFailedSaveInput() {
        var saved: List<ItemProgressComponent>? = null
        var revisions: Pair<Long, Long>? = null
        showSheet(state(), onSave = { _, itemRevision, progressRevision, components, replacement ->
            revisions = itemRevision to progressRevision
            assertNull(replacement)
            saved = components
            false
        })
        compose.onNodeWithTag("item_progress_add_percentage").performScrollTo().performClick()
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().performTextInput("Synthetic draft")
        compose.onNodeWithTag("item_progress_percentage_0").performScrollTo().performTextClearance()
        compose.onNodeWithTag("item_progress_percentage_0").performTextInput("42.50")
        compose.onNodeWithTag("item_progress_save").performScrollTo().performClick()
        compose.runOnIdle {
            assertEquals(7L to 0L, revisions)
            assertEquals(ItemProgressValue.Percentage(4250), saved?.single()?.value)
            assertEquals("Synthetic draft", saved?.single()?.name)
        }
        compose.onNodeWithTag("item_progress_name_0").performScrollTo().assertTextContains("Synthetic draft")
    }

    @Test fun quantityNormalizationPreservesTimeUnknownTargetDirectionOrderAndComponentIds() {
        val components = listOf(ItemProgressComponent(COMPONENT, "Synthetic time", ItemProgressValue.Time(3600, null)),
            ItemProgressComponent(OPERATION, "Synthetic pages", ItemProgressValue.Quantity("3.5", "pages", ItemProgressTarget("12", ItemProgressDirection.AT_MOST))))
        val initial = state(components)
        var saved: List<ItemProgressComponent>? = null
        showSheet(initial, onSave = { _, _, _, values, _ -> saved = values; false })
        captureSyntheticEvidence("item-progress-observation.png")
        compose.onNodeWithTag("item_progress_current_1").performScrollTo()
        captureSyntheticEvidence("item-progress-quantity-review.png")
        compose.onNodeWithTag("item_progress_current_1").performScrollTo().performTextClearance()
        compose.onNodeWithTag("item_progress_current_1").performTextInput("3.5000")
        compose.onNodeWithTag("item_progress_save").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(components, saved) }
    }

    @Test fun ambiguousSavedIntentOffersExactRetryWithoutDiscardOrFreshEditor() {
        var retries = 0
        showSheet(state().copy(itemProgressLedger = state().itemProgressLedger.copy(pending = listOf(pending()))), onRetry = { retries++ })
        compose.onNodeWithTag("item_progress_pending").assertIsDisplayed()
        compose.onNodeWithTag("item_progress_discard").assertDoesNotExist()
        compose.onNodeWithTag("item_progress_save").assertDoesNotExist()
        compose.onNodeWithTag("item_progress_retry").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(1, retries) }
    }

    @Test fun missingItemRecoveryHidesContentAndOffersDiscardOnlyAfterDefinitiveRejection() {
        val current = mutableStateOf(state().copy(canonicalItems = emptyList(),
            itemProgressLedger = state().itemProgressLedger.copy(pending = listOf(pending()))))
        var retries = 0
        var discarded: String? = null
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(current.value, ITEM, ItemProgressSyncState(), true,
            onLoad = { error("Missing items cannot authorize GET") }, onSave = { _, _, _, _, _ -> false },
            onRetry = { retries++ }, onDiscardReviewed = { discarded = it }, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_protected_recovery").assertIsDisplayed()
        assertSecureReviewWindow()
        compose.onNodeWithText("SYNTHETIC PRIVATE COMPONENT", substring = true).assertDoesNotExist()
        compose.onNodeWithText("987.654321", substring = true).assertDoesNotExist()
        compose.onNodeWithText("SECRET UNIT", substring = true).assertDoesNotExist()
        compose.onNodeWithTag("item_progress_discard").assertDoesNotExist()
        compose.onNodeWithTag("item_progress_retry").performClick()
        compose.runOnIdle {
            assertEquals(1, retries)
            current.value = current.value.copy(itemProgressLedger = current.value.itemProgressLedger.copy(
                pending = listOf(pending(ItemProgressDisposition.ITEM_MISSING))))
        }
        compose.onNodeWithTag("item_progress_retry").assertDoesNotExist()
        compose.onNodeWithTag("item_progress_discard").performClick()
        compose.runOnIdle { assertEquals(OPERATION, discarded) }
    }

    @Test fun missingItemHasContentFreeRecoveryEntryOutsideActiveCanonicalRows() {
        val initial = state().copy(canonicalItems = emptyList(), itemProgressLedger = state().itemProgressLedger.copy(pending = listOf(pending())))
        var opened: String? = null
        compose.setContent { MaterialTheme { CanonicalAuthoringList(state = initial, actionsEnabled = false, retryEnabled = false,
            onNewDetailed = {}, onOpenEditor = {}, onTrashConfirmed = { false }, onRestore = { false },
            onDiscard = { false }, onCopyConflict = { false }, onReviewLegacy = {}, onRetry = {}, googleOutboundBlocked = true,
            googlePublishingTargets = { emptyList() }, onRequestGooglePublication = { _, _ -> }, onOpenProgress = { opened = it }) } }
        compose.onNodeWithText("SYNTHETIC PRIVATE COMPONENT", substring = true).assertDoesNotExist()
        compose.onNodeWithText("987.654321", substring = true).assertDoesNotExist()
        compose.onNodeWithTag("item_progress_recovery_$ITEM").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(ITEM, opened) }
    }

    @Test fun lockingDisposesSensitiveSheetAndCancelsSelectedGet() {
        val locked = mutableStateOf(false)
        val entered = AtomicBoolean(false)
        val cancelled = AtomicBoolean(false)
        val initial = state().copy(canonicalItems = listOf(item().copy(isSensitive = true)))
        compose.setContent { MaterialTheme {
            AppLockPresentationGate(AppLockState(AppLockSettings(), isLocked = locked.value),
                lockedContent = { Text("Synthetic locked") }, unlockedContent = {
                    ItemProgressReviewSheet(initial, ITEM, ItemProgressSyncState(), true,
                        onLoad = { error("Production observer owns the selected GET") },
                        onObserve = { entered.set(true); try { awaitCancellation() } finally { cancelled.set(true) } },
                        onSave = { _, _, _, _, _ -> false }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {})
                })
        } }
        compose.waitUntil(10_000) { entered.get() }
        compose.onNodeWithTag("item_progress_sheet").assertIsDisplayed()
        compose.runOnIdle { locked.value = true }
        compose.waitUntil(10_000) { cancelled.get() }
        compose.onNodeWithTag("item_progress_sheet").assertDoesNotExist()
        compose.onNodeWithText("Synthetic progress goal").assertDoesNotExist()
        compose.onNodeWithText("Synthetic locked").assertIsDisplayed()
    }

    @Test fun sensitiveReviewWindowRemainsSecureAfterCanonicalPrivacyDowngrade() {
        val current = mutableStateOf(state().copy(canonicalItems = listOf(item().copy(isSensitive = true))))
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(current.value, ITEM, ItemProgressSyncState(), true,
            onLoad = { false }, onSave = { _, _, _, _, _ -> false }, onRetry = {}, onDiscardReviewed = {}, onDismiss = {}) } }
        compose.onNodeWithTag("item_progress_sheet").assertIsDisplayed()
        assertSecureReviewWindow()
        compose.runOnIdle { current.value = current.value.copy(canonicalItems = listOf(item())) }
        assertSecureReviewWindow()
    }

    private fun assertSecureReviewWindow() = compose.runOnIdle {
        assertTrue(WindowInspector.getGlobalWindowViews().any {
            val flags = (it.layoutParams as? WindowManager.LayoutParams)?.flags ?: 0
            flags and WindowManager.LayoutParams.FLAG_SECURE != 0
        })
    }

    private fun captureSyntheticEvidence(name: String) {
        compose.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(1_000)
        val screenshot = requireNotNull(instrumentation.uiAutomation.takeScreenshot())
        File(requireNotNull(instrumentation.targetContext.getExternalFilesDir(null)), name).outputStream().use {
            check(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it))
        }
        screenshot.recycle()
    }

    private fun showSheet(initial: DayWeaveUiState,
        onRetry: () -> Unit = {},
        onSave: suspend (String, Long, Long, List<ItemProgressComponent>, String?) -> Boolean = { _, _, _, _, _ -> false },
    ) {
        compose.setContent { MaterialTheme { ItemProgressReviewSheet(initial, ITEM, ItemProgressSyncState(), true,
            onLoad = { false }, onSave = onSave, onRetry = onRetry, onDiscardReviewed = {}, onDismiss = {}) } }
    }

    private fun item() = CanonicalItemSnapshot(id = ITEM, kind = "goal", status = "blocked", title = "Synthetic progress goal",
        timezoneName = "UTC", flexibleConstraintsJson = "{}", splitPolicyJson = "{\"mode\":\"never\"}", importance = 1,
        urgency = 1, siblingOrder = 0, isExecutable = false, revision = 7, createdAt = NOW, updatedAt = NOW)
    private fun state(components: List<ItemProgressComponent> = emptyList()) = DayWeaveUiState(canonicalItems = listOf(item()),
        canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = CONFIG, canonicalDeltaCursor = "synthetic-complete",
        itemProgressLedger = ItemProgressLedger(syncOrigin = ORIGIN, configurationId = CONFIG,
            observations = mapOf(ITEM to ItemProgressObservation(ItemProgressSnapshot(1, ITEM, 7,
                if (components.isEmpty()) 0 else 1, components, NOW.takeIf { components.isNotEmpty() }), NOW, true))))
    private fun pending(disposition: ItemProgressDisposition = ItemProgressDisposition.PENDING) = PendingItemProgressMutation(
        operationId = OPERATION, itemId = ITEM, syncOrigin = ORIGIN, configurationId = CONFIG,
        expectedItemRevision = 7, expectedProgressRevision = 0,
        requestJson = ITEM_PROGRESS_JSON.encodeToString(ItemProgressRequest(operationId = OPERATION,
            expectedItemRevision = 7, expectedProgressRevision = 0,
            components = listOf(ItemProgressComponent(COMPONENT, "SYNTHETIC PRIVATE COMPONENT", ItemProgressValue.Quantity("987.654321", "SECRET UNIT", null))))),
        createdAt = NOW, submittedAt = NOW, disposition = disposition, wasSensitive = true)

    private companion object {
        const val ITEM = "00000000-0000-4000-8000-000000000001"
        const val OPERATION = "00000000-0000-4000-8000-000000000002"
        const val COMPONENT = "00000000-0000-4000-8000-000000000003"
        const val NOW = "2026-09-08T09:00:00Z"
        const val ORIGIN = "https://api.example.test/"
        const val CONFIG = "synthetic-progress-binding"
    }
}
