package com.greengolddog.dayweave

import android.view.inspector.WindowInspector
import android.view.WindowManager
import android.graphics.Bitmap
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.espresso.Espresso.closeSoftKeyboard
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.ui.authoring.*
import com.greengolddog.dayweave.ui.screens.CanonicalHierarchyBrowserScreen
import java.util.UUID
import java.io.File
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Synthetic component host only: never MainActivity, account state, or a provider client. */
@RunWith(AndroidJUnit4::class)
class CanonicalStructuralAuthoringUiTest {
    @get:Rule val composeRule = createComposeRule()

    @Test
    fun bothRootButtonsOpenBlankOrdinaryInboxPresets() {
        val kind = mutableStateOf(ItemKind.GOAL)
        var opened: CanonicalItemEditorRoute? = null
        composeRule.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(source(), offline(), kind.value, true, { opened = it }, {})
        } }
        composeRule.onNodeWithTag("hierarchy_create_root").performClick()
        composeRule.runOnIdle {
            assertEquals(ItemKind.GOAL, opened?.initialDraft?.kind)
            assertEquals("", opened?.initialDraft?.title)
            assertEquals("Europe/Istanbul", opened?.initialDraft?.timezoneName)
            assertTrue(opened?.isHierarchyCreation == true)
            assertNull(opened?.sourceInboxId)
            kind.value = ItemKind.PROJECT
        }
        composeRule.onNodeWithTag("hierarchy_create_root").performClick()
        composeRule.runOnIdle {
            assertEquals(ItemKind.PROJECT, opened?.initialDraft?.kind)
            assertEquals(CanonicalDraftPlacement.INBOX, opened?.initialDraft?.placement)
            assertNull(opened?.initialDraft?.parentId)
            assertNull(opened?.initialDraft?.durationSeconds)
            assertNull(opened?.initialDraft?.recurrence)
        }
    }

    @Test
    fun blockedParentOffersSeparateSubtaskCaptureWithoutBodyEdit() {
        val blocked = parent().copy(status = "blocked", blockedReasonKind = CanonicalBlockedReasonKind.MANUAL,
            blockedReason = "Synthetic wait")
        var opened: CanonicalItemEditorRoute? = null
        composeRule.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(source(blocked), offline(), ItemKind.PROJECT, true, { opened = it }, {})
        } }
        awaitTag("hierarchy_row_$PARENT")
        composeRule.onNodeWithTag("hierarchy_row_$PARENT").performClick()
        composeRule.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_add_child").performClick()
        composeRule.runOnIdle {
            assertEquals(ItemKind.TASK, opened?.initialDraft?.kind)
            assertEquals(PARENT, opened?.initialDraft?.parentId)
            assertEquals("", opened?.initialDraft?.title)
            assertNull(opened?.initialDraft?.durationSeconds)
            assertTrue(opened?.isHierarchyCreation == true)
        }
    }

    @Test
    fun projectEditorSavesTypedDateSoftWeightOwnEffortIntoOrdinaryJournal() {
        val state = source()
        val route = CanonicalItemEditorRoute.hierarchy(state, ItemKind.PROJECT)
        val store = PlannerStore(state)
        composeRule.setContent { MaterialTheme {
            CanonicalItemEditorSheet(route, emptyList(), canonicalDependencyEditorContext(state, route.itemId), {}, { draft ->
                store.enqueueCanonicalCreate(draft, route.itemId, UUID.randomUUID().toString()) != null
            })
        } }
        composeRule.onNodeWithTag("canonical_editor_title").performTextInput("SYNTHETIC reviewed project")
        closeSoftKeyboard()
        composeRule.onNodeWithText("Date deadline").performScrollTo().performClick()
        composeRule.onNodeWithText("Date deadline").assertIsSelected()
        composeRule.onNodeWithTag("canonical_editor_deadline_date").performScrollTo().performTextInput("2026-10-01")
        closeSoftKeyboard()
        composeRule.onNodeWithText("Soft preference").performScrollTo().performClick()
        composeRule.onNodeWithText("Soft weight (0–1000000)").performScrollTo().performTextClearance()
        composeRule.onNodeWithText("Soft weight (0–1000000)").performTextInput("0")
        closeSoftKeyboard()
        captureSyntheticEvidence("structural-project-deadline.png")
        composeRule.onNodeWithTag("canonical_editor_project_own_effort").performScrollTo().performClick()
        captureSyntheticEvidence("structural-project-effort.png")
        composeRule.onNodeWithTag("canonical_editor_save").performScrollTo().performClick()
        composeRule.runOnIdle {
            val journal = store.state.value.pendingCanonicalAuthoringMutations.single()
            val draft = requireNotNull(journal.draft)
            assertEquals(route.itemId, journal.itemId)
            assertEquals(2, journal.structuralRequestShapeVersion)
            assertEquals(CanonicalDeadlineKind.DATE, draft.deadlineKind)
            assertEquals("2026-10-01", draft.deadlineDate)
            assertEquals(CanonicalDeadlineStrength.SOFT, draft.deadlineStrength)
            assertEquals(0L, draft.deadlineSoftWeight)
            assertTrue(draft.hasOwnEffort)
            assertNull(draft.constraints.hasOwnEffort)
            assertNull(draft.deadlineAt)
            assertNull(store.state.value.onboardingFirstItemAnchor)
            assertFalse(journal.isSubmitted)
        }
    }

    @Test
    fun sensitiveOriginStaysSecureAfterDetachingWithoutChangingOwnPrivacyFlag() {
        val state = source(parent().copy(isSensitive = true))
        val route = CanonicalItemEditorRoute.hierarchy(state, ItemKind.TASK, PARENT)
        var saved: CanonicalItemDraft? = null
        composeRule.setContent { MaterialTheme {
            CanonicalItemEditorSheet(route, canonicalParentOptions(state, route.itemId),
                canonicalDependencyEditorContext(state, route.itemId), {}, { saved = it; true })
        } }
        composeRule.onNodeWithTag("canonical_editor_title").performTextInput("SYNTHETIC private-context review")
        composeRule.onAllNodesWithText(parent().title).assertCountEquals(0)
        assertSecureReviewWindow()
        composeRule.onNodeWithTag("canonical_editor_parent").performScrollTo().performClick()
        composeRule.onNodeWithText("No parent").performClick()
        assertSecureReviewWindow()
        composeRule.onNodeWithTag("canonical_editor_save").performScrollTo().performClick()
        composeRule.runOnIdle {
            assertNotNull(saved)
            assertNull(saved?.parentId)
            assertFalse(requireNotNull(saved).isSensitive)
            assertTrue(route.minimumSensitive)
        }
    }

    @Test
    fun changedSourceCannotKeepOldAddChildAuthority() {
        val state = mutableStateOf(source(parent()))
        var opened: CanonicalItemEditorRoute? = null
        composeRule.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(state.value, offline(), ItemKind.PROJECT, true, { opened = it }, {})
        } }
        awaitTag("hierarchy_row_$PARENT")
        composeRule.onNodeWithTag("hierarchy_row_$PARENT").performClick()
        composeRule.onNodeWithTag("hierarchy_add_child").assertIsEnabled()
        composeRule.runOnIdle {
            val parent = parent()
            state.value = state.value.copy(pendingCanonicalAuthoringMutations = listOf(
                PendingCanonicalAuthoringMutation(id = UUID.randomUUID().toString(), itemId = PARENT,
                    operation = CanonicalAuthoringOperation.REPLACE, draft = parent.toCanonicalDraft(),
                    expectedRevision = parent.revision, baseItem = parent, createdAt = NOW,
                    disposition = CanonicalAuthoringDisposition.CONFLICTED, diagnostic = "Synthetic conflict"),
            ))
        }
        awaitTag("hierarchy_item_details")
        composeRule.onNodeWithTag("hierarchy_add_child").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        composeRule.runOnIdle { assertNull(opened) }
    }

    private fun assertSecureReviewWindow() = composeRule.runOnIdle {
        assertTrue(WindowInspector.getGlobalWindowViews().any {
            val flags = (it.layoutParams as? WindowManager.LayoutParams)?.flags ?: 0
            flags and WindowManager.LayoutParams.FLAG_SECURE != 0
        })
    }
    private fun captureSyntheticEvidence(name: String) {
        composeRule.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.waitForIdleSync()
        // ATD's renderer may publish a frame after Compose has become semantically idle.
        android.os.SystemClock.sleep(1_000)
        val screenshot = requireNotNull(instrumentation.uiAutomation.takeScreenshot())
        File(requireNotNull(instrumentation.targetContext.getExternalFilesDir(null)), name)
            .outputStream().use { check(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it)) }
        screenshot.recycle()
    }
    private fun awaitTag(tag: String) = composeRule.waitUntil(10_000) {
        composeRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
    }
    private fun source(vararg items: CanonicalItemSnapshot) = DayWeaveUiState(
        canonicalConfigurationId = "synthetic-binding", canonicalDeltaCursor = "synthetic-cursor",
        canonicalItems = items.toList(), scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(timezoneName = "Europe/Istanbul"))
    private fun parent() = CanonicalItemSnapshot(id = PARENT, kind = "project", status = "inbox", title = "SYNTHETIC parent",
        timezoneName = "UTC", flexibleConstraintsJson = "{}", splitPolicyJson = """{"type":"indivisible"}""",
        importance = 50, urgency = 50, siblingOrder = 0, isExecutable = false, revision = 1, createdAt = NOW, updatedAt = NOW)
    private fun offline() = CanonicalSyncState(CanonicalSyncPhase.OFFLINE, "Synthetic offline")
    private companion object {
        const val PARENT = "10000000-0000-4000-8000-000000000101"
        const val NOW = "2026-09-01T00:00:00Z"
    }
}
