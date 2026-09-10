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
import com.greengolddog.dayweave.sync.RoutineOccurrenceSelection
import com.greengolddog.dayweave.sync.RoutineOccurrenceSyncState
import com.greengolddog.dayweave.ui.AppLockPresentationGate
import com.greengolddog.dayweave.ui.authoring.RoutineOccurrenceReviewSheet
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.awaitCancellation
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Inert Compose host only: no owner credentials, repository, network or MainActivity. */
@RunWith(AndroidJUnit4::class)
class RoutineOccurrenceUiTest {
    @get:Rule val compose = createComposeRule()
    private val selected = RoutineOccurrenceSelection(ITEM, OCCURRENCE)

    @Test fun publicTemplateStillPresentsAllOccurrenceHistoryInsideSecureWindow() {
        val initial = state()
        assertFalse(initial.canonicalItems.single().isSensitive)
        compose.setContent { MaterialTheme {
            RoutineOccurrenceReviewSheet(initial, selected, reviewed(), true,
                onObserve = {}, onSave = { _, _, _, _ -> false }, onRetry = {}, onDiscard = {}, onDismiss = {})
        } }
        compose.onNodeWithTag("routine_member_$ITEM").performScrollTo().assertExists()
        compose.onNodeWithText(TITLE).assertExists()
        compose.runOnIdle {
            assertTrue(WindowInspector.getGlobalWindowViews().any {
                val flags = (it.layoutParams as? WindowManager.LayoutParams)?.flags ?: 0
                flags and WindowManager.LayoutParams.FLAG_SECURE != 0
            })
        }
    }

    @Test fun unresolvedSubmittedRequestOffersExactRetryAndNoDiscardEvenWithoutTemplate() {
        val initial = state(submitted = true).copy(canonicalItems = emptyList())
        compose.setContent { MaterialTheme {
            RoutineOccurrenceReviewSheet(initial, selected, reviewed(), true,
                onObserve = {}, onSave = { _, _, _, _ -> false }, onRetry = {}, onDiscard = {}, onDismiss = {})
        } }
        compose.onNodeWithTag("routine_occurrence_retry").performScrollTo().assertIsEnabled()
        compose.onNodeWithTag("routine_occurrence_discard").assertDoesNotExist()
        compose.onNodeWithTag("routine_done_$ITEM").performScrollTo().assertIsNotEnabled()
    }

    @Test fun privacyRevocationRemovesPrivateRowsAndCancelsSelectedObservation() {
        val locked = mutableStateOf(false)
        val entered = AtomicBoolean(false)
        val cancelled = AtomicBoolean(false)
        val initial = state()
        compose.setContent { MaterialTheme {
            AppLockPresentationGate(AppLockState(AppLockSettings(), isLocked = locked.value),
                lockedContent = { Text("Synthetic locked occurrence host") }, unlockedContent = {
                    RoutineOccurrenceReviewSheet(initial, selected, reviewed(), true,
                        onObserve = { entered.set(true); try { awaitCancellation() } finally { cancelled.set(true) } },
                        onSave = { _, _, _, _ -> false }, onRetry = {}, onDiscard = {}, onDismiss = {})
                })
        } }
        compose.waitUntil(10_000) { entered.get() }
        compose.onNodeWithTag("routine_member_$ITEM").performScrollTo().assertExists()
        compose.runOnIdle { locked.value = true }
        compose.waitUntil(10_000) { cancelled.get() }
        compose.onNodeWithTag("routine_occurrence_sheet").assertDoesNotExist()
        compose.onNodeWithText(TITLE).assertDoesNotExist()
        compose.onNodeWithText("Synthetic locked occurrence host").assertIsDisplayed()
    }

    private fun reviewed() = RoutineOccurrenceSyncState(selection = selected, reviewed = snapshot())

    private fun snapshot(): RoutineOccurrenceSnapshot {
        val open = ItemCompletionReopening("planned", null, null, null)
        val definition = RoutineOccurrenceMemberDefinition(ITEM, null, 1, TITLE, "task", true, 0, true, open)
        val manifest = RoutineOccurrenceManifest(1, INSTANCE, ITEM, OCCURRENCE,
            Json.parseToJsonElement("{\"type\":\"calendar_day\",\"date\":\"2026-09-10\",\"bucket_ordinal\":0}").jsonObject,
            "2026-09-10T00:00:00Z", "2026-09-11T00:00:00Z", "2026-09-10T09:00:00Z", "2026-09-10T18:00:00Z",
            "UTC", HASH, listOf(definition))
        return RoutineOccurrenceSnapshot(1, RoutineOccurrenceAggregate(manifest, 1, listOf(
            RoutineOccurrenceMemberState(ITEM, 1, "planned", true, ItemCompletionMode.AUTOMATIC, open, null, null, NOW))),
            HASH, true, listOf(RoutineOccurrenceMemberEvaluation(ITEM, ItemCompletionCounts(0, 0, 0, 0), false,
                RoutineOccurrenceReason.UNCHANGED))).also { it.requireValid() }
    }

    private fun state(submitted: Boolean = false): DayWeaveUiState {
        val request = RoutineOccurrenceRequest(operationId = OPERATION, expectedInstanceRevision = 1,
            expectedMemberRevision = 1, expectedEvidenceHash = HASH, action = RoutineOccurrenceAction.SetOutcome("completed"))
        val pending = PendingRoutineOccurrenceMutation(operationId = OPERATION, instanceId = INSTANCE, memberId = ITEM,
            syncOrigin = ORIGIN, configurationId = CONFIG, requestJson = ITEM_PROGRESS_JSON.encodeToString(request),
            request = request, createdAt = NOW, submittedAt = NOW)
        val canonical = CanonicalItemSnapshot(ITEM, kind = "task", status = "planned", title = "Public synthetic template",
            timezoneName = "UTC", flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 1, urgency = 1, siblingOrder = 0, isExecutable = true, revision = 1, createdAt = NOW, updatedAt = NOW)
        return DayWeaveUiState(canonicalItems = listOf(canonical), canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = CONFIG,
            routineOccurrenceLedger = RoutineOccurrenceLedger(syncOrigin = ORIGIN, configurationId = CONFIG,
                observations = mapOf(INSTANCE to RoutineOccurrenceObservation(snapshot(), NOW)),
                pending = if (submitted) listOf(pending) else emptyList()))
    }

    private companion object {
        const val ITEM = "11111111-1111-4111-8111-111111111111"
        const val INSTANCE = "22222222-2222-4222-8222-222222222222"
        const val OCCURRENCE = "33333333-3333-5333-8333-333333333333"
        const val OPERATION = "44444444-4444-4444-8444-444444444444"
        const val ORIGIN = "https://api.example.test/"
        const val CONFIG = "synthetic-occurrence-ui"
        const val TITLE = "Synthetic protected occurrence history"
        const val NOW = "2026-09-10T10:00:00Z"
        val HASH = "sha256:" + "a".repeat(64)
    }
}
