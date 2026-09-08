package com.greengolddog.dayweave

import android.graphics.Bitmap
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.ui.screens.CanonicalHierarchyBrowserScreen
import java.io.File
import java.util.UUID
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Inert Compose host and synthetic records only; never creates MainActivity or provider clients. */
@RunWith(AndroidJUnit4::class)
class CanonicalHierarchyRollupUiTest {
    @get:Rule val composeRule = createComposeRule()

    @Test
    fun offlineDetailSeparatesRecordedLeafLifecycleEffortEventsAndRecurrence() {
        val state = state().copy(canonicalItems = listOf(
            item(1, "goal", null).copy(status = "completed", hasOwnEffort = true),
            item(2).copy(status = "completed", durationKind = CanonicalDurationKind.RANGE,
                durationMinSeconds = 1, durationSeconds = 30, durationMaxSeconds = 59),
            item(3).copy(durationKind = CanonicalDurationKind.UNKNOWN, durationMinSeconds = null,
                durationSeconds = null, durationMaxSeconds = null, durationSource = null),
            item(4).copy(status = "skipped", durationMinSeconds = 60, durationSeconds = 60, durationMaxSeconds = 60),
            item(5, "event").copy(status = "cancelled", earliestStartAt = NOW,
                deadlineAt = "2026-09-09T11:00:00Z"),
            item(6, "routine").copy(recurrenceJson = "{\"kind\":\"daily\",\"occurrences_per_period\":1}"),
            item(7, parent = id(6)),
        ))
        show(state)
        composeRule.onNodeWithTag("hierarchy_rollup_${id(1)}", useUnmergedTree = true).assertTextEquals(SUMMARY)
        composeRule.onNodeWithTag("hierarchy_row_${id(1)}").performClick()
        composeRule.onNodeWithTag("hierarchy_rollup_detail").assertTextEquals(SUMMARY)
        composeRule.onAllNodesWithText("Goal · Completed · Synced").assertCountEquals(2)
        composeRule.onNodeWithText("Recurring leaf items · 1 · occurrence achievement is separate").assertIsDisplayed()
        composeRule.onNodeWithText("Fixed events · 1 · not flexible effort").assertIsDisplayed()
        composeRule.onNodeWithText("Recorded leaf effort estimates · 1m 30s expected · 1m 1s minimum · 1m 59s maximum")
            .performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Unknown leaf effort estimates · 1 · not counted as zero work")
            .performScrollTo().assertIsDisplayed()
        capture("hierarchy-rollup-detail.png")
    }

    @Test
    fun sensitiveDescendantConcealsAncestorNumbersInRowsAndDetailSemantics() {
        show(state().copy(canonicalItems = listOf(item(1, "goal", null), item(2).copy(isSensitive = true))))
        composeRule.onNodeWithTag("hierarchy_rollup_${id(1)}", useUnmergedTree = true)
            .assertTextEquals("Summary concealed · sensitive subtree")
        composeRule.onNodeWithTag("hierarchy_row_${id(1)}").performClick()
        composeRule.onNodeWithTag("hierarchy_rollup_detail")
            .assertTextEquals("Summary concealed · sensitive subtree")
        composeRule.onAllNodesWithText("Leaf items ·", substring = true).assertCountEquals(0)
        composeRule.onAllNodesWithText("Recorded leaf effort estimates ·", substring = true).assertCountEquals(0)
    }

    @Test
    fun queuedPrivacyDowngradeAndLostHydrationCannotRetainOldNumericalSummary() {
        val source = mutableStateOf(state())
        composeRule.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(source.value, OFFLINE, ItemKind.GOAL, false, {}, {})
        } }
        awaitRows()
        composeRule.onNodeWithTag("hierarchy_rollup_${id(1)}", useUnmergedTree = true)
            .assertTextEquals("Leaf items · 0 completed · 1 open · 0 skipped · 0 cancelled")
        val child = source.value.canonicalItems.last()
        composeRule.runOnIdle { source.value = source.value.copy(pendingCanonicalAuthoringMutations = listOf(
            PendingCanonicalAuthoringMutation(id = id(20), itemId = child.id,
                operation = CanonicalAuthoringOperation.REPLACE,
                draft = child.toCanonicalDraft().copy(isSensitive = false), baseItem = child.copy(isSensitive = true),
                expectedRevision = child.revision, createdAt = NOW),
        )) }
        awaitSummary("Summary withheld · local changes need review or sync")
        composeRule.runOnIdle { source.value = source.value.copy(canonicalDeltaCursor = null) }
        awaitSummary("Summary unavailable · complete saved hierarchy required")
        composeRule.onAllNodesWithText("Leaf items ·", substring = true).assertCountEquals(0)
    }

    private fun show(state: DayWeaveUiState) {
        composeRule.setContent { MaterialTheme {
            CanonicalHierarchyBrowserScreen(state, OFFLINE, ItemKind.GOAL, false, {}, {})
        } }
        awaitRows()
    }

    private fun awaitRows() = composeRule.waitUntil(10_000) {
        composeRule.onAllNodesWithTag("hierarchy_row_${id(1)}").fetchSemanticsNodes().isNotEmpty()
    }

    private fun awaitSummary(text: String) {
        composeRule.waitUntil(10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onNodeWithTag("hierarchy_rollup_${id(1)}", useUnmergedTree = true).assertTextEquals(text)
    }

    private fun capture(name: String) {
        composeRule.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(1_000)
        val directory = requireNotNull(instrumentation.targetContext.getExternalFilesDir(null))
        val screenshot = requireNotNull(instrumentation.uiAutomation.takeScreenshot())
        File(directory, name).outputStream().use { check(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it)) }
        screenshot.recycle()
    }

    private fun state() = DayWeaveUiState(canonicalConfigurationId = "synthetic-rollup-binding",
        canonicalSyncOrigin = "https://example.test/", canonicalDeltaCursor = "synthetic-complete-cursor",
        canonicalItems = listOf(item(1, "goal", null), item(2)))

    private fun item(value: Int, kind: String = "task", parent: String? = id(1)) = CanonicalItemSnapshot(
        id = id(value), kind = kind, parentId = parent, status = "inbox", title = "SYNTHETIC ${if (value == 1) "Learning goal" else "Leaf $value"}",
        timezoneName = "UTC", durationSeconds = 30, flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50, urgency = 50, siblingOrder = value.toLong(), isExecutable = true,
        revision = 1, createdAt = NOW, updatedAt = NOW,
    )

    private fun id(value: Int) = UUID(0, value.toLong()).toString()
    private companion object {
        const val NOW = "2026-09-09T10:00:00Z"
        const val SUMMARY = "Leaf items · 1 completed · 1 open · 1 skipped · 1 cancelled"
        val OFFLINE = CanonicalSyncState(CanonicalSyncPhase.OFFLINE, "Synthetic offline")
    }
}
