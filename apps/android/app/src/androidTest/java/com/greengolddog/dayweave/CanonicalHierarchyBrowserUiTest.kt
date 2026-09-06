package com.greengolddog.dayweave

import android.graphics.Bitmap
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.security.AppLockSettings
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.ui.AppLockPresentationGate
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorMode
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorRoute
import com.greengolddog.dayweave.ui.navigation.DayWeaveNavigationBar
import com.greengolddog.dayweave.ui.screens.CanonicalHierarchyBrowserScreen
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** Synthetic component tests: no MainActivity, app store, credentials, or network clients. */
@RunWith(AndroidJUnit4::class)
class CanonicalHierarchyBrowserUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun unscheduledHierarchySearchTemporarilyExpandsAndRestoresDisclosure() {
        showBrowser()
        captureSyntheticEvidence("hierarchy-goals.png")
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_disclosure_$GOAL").performClick()
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_search").performTextInput("ReCoRd")
        composeRule.onNodeWithTag("hierarchy_row_$PROJECT").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_row_$GOAL").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_disclosure_$GOAL").assertIsNotEnabled()
        composeRule.onNodeWithTag("hierarchy_search").performTextClearance()
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_disclosure_$GOAL").performClick()
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertIsDisplayed()
    }

    @Test
    fun supportedRowReviewUsesExactCanonicalEditorRoute() {
        var opened: CanonicalItemEditorRoute? = null
        showBrowser(onOpenEditor = { opened = it })
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").performClick()
        composeRule.onNodeWithTag("hierarchy_item_details").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_edit").performClick()
        composeRule.runOnIdle {
            assertEquals(CHILD, opened?.itemId)
            assertEquals(CanonicalItemEditorMode.REPLACE, opened?.mode)
            assertEquals(state().canonicalItems.last().toCanonicalDraft(), opened?.initialDraft)
            assertNull(opened?.mutationId)
        }
    }

    @Test
    fun projectsAndTerminalRowsAreInspectableWithoutNewEditingAuthority() {
        var opened: CanonicalItemEditorRoute? = null
        showBrowser(
            kind = ItemKind.PROJECT,
            state = state().copy(canonicalItems = state().canonicalItems.map {
                if (it.id == CHILD) it.copy(status = "completed", isExecutable = false) else it
            }),
            onOpenEditor = { opened = it },
        )
        composeRule.onNodeWithTag("hierarchy_row_$PROJECT").performClick()
        composeRule.onNodeWithTag("hierarchy_item_details").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        captureSyntheticEvidence("hierarchy-project-read-only.png")
        composeRule.onNodeWithText("Close").performClick()
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").performClick()
        composeRule.onNodeWithTag("hierarchy_item_details").assertIsDisplayed()
        composeRule.onNodeWithTag("hierarchy_edit").assertDoesNotExist()
        composeRule.runOnIdle { assertNull(opened) }
    }

    @Test
    fun busyCanonicalActionsDoNotDisableBrowsingButPreventEditorMutationRoute() {
        showBrowser(actionsEnabled = false)
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").performClick()
        composeRule.onNodeWithTag("hierarchy_edit").assertIsNotEnabled()
    }

    @Test
    fun appLockDisposesPrivateSearchAndOpenDetailsAndUnlockStartsFresh() {
        val locked = mutableStateOf(false)
        composeRule.setContent {
            MaterialTheme {
                AppLockPresentationGate(
                    appLockState = AppLockState(AppLockSettings(), isLocked = locked.value),
                    lockedContent = { Text("Locked synthetic boundary") },
                    unlockedContent = {
                        CanonicalHierarchyBrowserScreen(
                            state(), offline(), ItemKind.GOAL, true, {}, {},
                        )
                    },
                )
            }
        }
        awaitBrowserRows()
        composeRule.onNodeWithTag("hierarchy_search").performTextInput("record")
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").performClick()
        composeRule.onNodeWithTag("hierarchy_item_details").assertIsDisplayed()
        composeRule.runOnIdle { locked.value = true }
        composeRule.onNodeWithTag("hierarchy_search").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_item_details").assertDoesNotExist()
        composeRule.onAllNodesWithText("SYNTHETIC Record sample", substring = true).assertCountEquals(0)
        composeRule.runOnIdle { locked.value = false }
        awaitBrowserRows()
        composeRule.onNodeWithTag("hierarchy_item_details").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_search")
            .assertTextEquals("Search goals and descendants", "")
    }

    @Test
    fun changedAuthenticationBindingDisposesSearchDisclosureAndSelection() {
        val binding = mutableStateOf("first-synthetic-binding")
        composeRule.setContent {
            MaterialTheme {
                key(binding.value) {
                    CanonicalHierarchyBrowserScreen(
                        state(), offline(), ItemKind.GOAL, true, {}, {},
                    )
                }
            }
        }
        awaitBrowserRows()
        composeRule.onNodeWithTag("hierarchy_disclosure_$GOAL").performClick()
        composeRule.onNodeWithTag("hierarchy_search").performTextInput("record")
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").performClick()
        composeRule.runOnIdle { binding.value = "second-synthetic-binding" }
        awaitBrowserRows()
        composeRule.onNodeWithTag("hierarchy_item_details").assertDoesNotExist()
        composeRule.onNodeWithTag("hierarchy_search")
            .assertTextEquals("Search goals and descendants", "")
        composeRule.onNodeWithTag("hierarchy_row_$CHILD").assertIsDisplayed()
    }

    @Test
    fun nestedPersistedDestinationKeepsExactlyFiveBottomTabsAndMoreSelected() {
        composeRule.setContent {
            MaterialTheme {
                DayWeaveNavigationBar(AppDestination.GOALS, 0, {})
            }
        }
        listOf("Today", "Calendar", "Inbox", "Assistant", "More").forEach {
            composeRule.onNodeWithText(it).assertIsDisplayed()
        }
        composeRule.onNodeWithText("More").assertIsSelected()
        composeRule.onAllNodesWithText("Goals").assertCountEquals(0)
        composeRule.onAllNodesWithText("Projects").assertCountEquals(0)
    }

    private fun showBrowser(
        state: DayWeaveUiState = state(),
        kind: ItemKind = ItemKind.GOAL,
        actionsEnabled: Boolean = true,
        onOpenEditor: (CanonicalItemEditorRoute) -> Unit = {},
    ) {
        composeRule.setContent {
            MaterialTheme {
                CanonicalHierarchyBrowserScreen(state, offline(), kind, actionsEnabled, onOpenEditor, {})
            }
        }
        awaitBrowserRows()
    }

    private fun awaitBrowserRows() {
        composeRule.waitUntil(10_000) {
            composeRule.onAllNodesWithTag("hierarchy_row_$PROJECT")
                .fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun captureSyntheticEvidence(name: String) {
        composeRule.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        // ATD's host renderer can publish a frame after Compose's semantic tree is already idle.
        // This affects evidence capture only, never the UI behavior assertions above/below it.
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(1_000)
        val directory = requireNotNull(instrumentation.targetContext.getExternalFilesDir(null))
        val screenshot = requireNotNull(instrumentation.uiAutomation.takeScreenshot())
        File(directory, name).outputStream().use { output ->
            check(screenshot.compress(Bitmap.CompressFormat.PNG, 100, output))
        }
        screenshot.recycle()
    }

    private fun state() = DayWeaveUiState(
        canonicalConfigurationId = "synthetic-binding",
        canonicalDeltaCursor = "synthetic-hydrated-cursor",
        canonicalItems = listOf(
            item(PROJECT, "SYNTHETIC Launch", "project", null),
            item(GOAL, "SYNTHETIC Learn", "goal", PROJECT),
            item(CHILD, "SYNTHETIC Record sample", "task", GOAL),
        ),
    )

    private fun item(id: String, title: String, kind: String, parent: String?) = CanonicalItemSnapshot(
        id = id, kind = kind, status = "inbox", title = title, timezoneName = "UTC",
        flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50, urgency = 50, parentId = parent, siblingOrder = 0,
        isExecutable = false, revision = 1, createdAt = NOW, updatedAt = NOW,
    )

    private fun offline() = CanonicalSyncState(CanonicalSyncPhase.OFFLINE, "Synthetic offline")

    private companion object {
        const val PROJECT = "10000000-0000-4000-8000-000000000001"
        const val GOAL = "10000000-0000-4000-8000-000000000002"
        const val CHILD = "10000000-0000-4000-8000-000000000003"
        const val NOW = "2026-09-06T12:00:00Z"
    }
}
