package com.greengolddog.dayweave

import android.view.WindowManager
import android.view.inspector.WindowInspector
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.compose.runtime.CompositionLocalProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.ui.authoring.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Inert Compose host: no MainActivity, credentials, network, helper or action callbacks. */
@RunWith(AndroidJUnit4::class)
class RoutinePlanningPreviewUiTest {
    @get:Rule val compose = createComposeRule()
    private fun preview() = RoutinePlanningPreviewPresentation("2026-09-10T10:00:00.123456Z", "2026-09-10T10:00:01.123456Z", "UTC",
        "Synthetic fixed horizon", listOf(RoutinePlanningPreviewRow("Synthetic private planned work", "Synthetic exact interval")), emptyList(),
        listOf(RoutinePlanningPreviewRow("Synthetic violation", "Synthetic plain explanation")),
        listOf(RoutinePlanningPreviewRow("Synthetic Inbox member", "not started")))

    @Test fun privateReadOnlyPreviewSecuresWindowAndExposesNoCanonicalActions() {
        compose.setContent { MaterialTheme { RoutinePlanningPreviewSheet(preview(), {}) } }
        compose.onNodeWithTag("routine_preview_block_0").performScrollTo().assertExists()
        compose.runOnIdle { assertTrue(WindowInspector.getGlobalWindowViews().any {
            ((it.layoutParams as? WindowManager.LayoutParams)?.flags ?: 0) and WindowManager.LayoutParams.FLAG_SECURE != 0
        }) }
        for (action in listOf("Start", "Done", "Skipped", "Move", "Defer", "Publish")) compose.onNodeWithText(action).assertDoesNotExist()
        compose.onNodeWithTag("routine_preview_diagnostic_0").performScrollTo().assertExists()
        compose.onNodeWithText("Synthetic plain explanation").assertExists()
        compose.onNodeWithTag("routine_preview_member_0").performScrollTo().assertExists()
    }

    @Test fun privacyRevocationDropsRememberedPreviewRows() {
        val value = mutableStateOf<RoutinePlanningPreviewPresentation?>(preview())
        compose.setContent { MaterialTheme { RoutinePlanningPreviewSheet(value.value, {}) } }
        compose.onNodeWithTag("routine_preview_block_0").performScrollTo().assertExists()
        compose.runOnIdle { value.value = null }
        compose.onNodeWithTag("routine_planning_preview").assertDoesNotExist()
        compose.onNodeWithText("Synthetic private planned work").assertDoesNotExist()
    }

    @Test fun backgroundHidesRowsAndWithdrawsPresentationAdmission() {
        val owner = object : LifecycleOwner {
            val registry = LifecycleRegistry.createUnsafe(this)
            override val lifecycle: Lifecycle = registry
        }
        var dismissed = false
        compose.runOnIdle { owner.registry.currentState = Lifecycle.State.RESUMED }
        compose.setContent { CompositionLocalProvider(LocalLifecycleOwner provides owner) { MaterialTheme {
            RoutinePlanningPreviewSheet(preview(), { dismissed = true })
        } } }
        compose.onNodeWithTag("routine_preview_block_0").performScrollTo().assertExists()
        compose.runOnIdle { owner.registry.currentState = Lifecycle.State.CREATED }
        compose.onNodeWithTag("routine_planning_preview").assertDoesNotExist()
        compose.runOnIdle { assertTrue(dismissed) }
    }
}
