package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.ui.screens.PlanningProfileEditorDialog
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PlanningProfileEditorUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun rendersEveryProfileFieldWithUnambiguousTimeLabels() {
        showEditor(
            currentProfile = ScheduleCompositionProfileSnapshot(
                dayStartMinute = 8 * 60 + 15,
                dayEndMinute = 23 * 60 + 45,
                slotGranularityMinutes = 20,
                stabilityWeight = 123,
                defaultSoftWeight = 456,
            ),
        )

        composeRule.onNodeWithTag("planning_profile_editor").assertIsDisplayed()
        composeRule.onNodeWithText("Start hour").assertExists()
        composeRule.onNodeWithText("Start minute").assertExists()
        composeRule.onNodeWithText("End hour").assertExists()
        composeRule.onNodeWithText("End minute").assertExists()
        composeRule.onNodeWithTag("planning_start_hour").assertTextContains("08")
        composeRule.onNodeWithTag("planning_start_minute").assertTextContains("15")
        composeRule.onNodeWithTag("planning_end_hour").assertTextContains("23")
        composeRule.onNodeWithTag("planning_end_minute").assertTextContains("45")
        composeRule.onNodeWithContentDescription("Slot size").assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.StateDescription,
                "20 minutes",
            ),
        )
        composeRule.onNodeWithTag("planning_stability_weight").assertTextContains("123")
        composeRule.onNodeWithTag("planning_soft_weight").assertTextContains("456")
        composeRule.onNodeWithTag("save_planning_profile").assertIsNotEnabled()
    }

    @Test
    fun invalidDraftGatesSaveAndValidDraftReturnsExactProfile() {
        val savedProfile = AtomicReference<ScheduleCompositionProfileSnapshot?>()
        showEditor(onSave = savedProfile::set)

        replaceText("planning_end_hour", "06")
        composeRule.onNodeWithText("End must be later than start.")
            .performScrollTo()
            .assertIsDisplayed()
        composeRule.onNodeWithTag("save_planning_profile").assertIsNotEnabled()
        assertNull(savedProfile.get())

        replaceText("planning_start_hour", "08")
        replaceText("planning_start_minute", "15")
        replaceText("planning_end_hour", "23")
        replaceText("planning_end_minute", "45")
        replaceText("planning_stability_weight", "123")
        replaceText("planning_soft_weight", "456")

        composeRule.onNodeWithTag("save_planning_profile")
            .assertIsEnabled()
            .performClick()
        composeRule.runOnIdle {
            assertEquals(
                ScheduleCompositionProfileSnapshot(
                    dayStartMinute = 8 * 60 + 15,
                    dayEndMinute = 23 * 60 + 45,
                    slotGranularityMinutes = 5,
                    stabilityWeight = 123,
                    defaultSoftWeight = 456,
                ),
                savedProfile.get(),
            )
        }
    }

    @Test
    fun resetDefaultsReturnsTheExactDefaultProfile() {
        val savedProfile = AtomicReference<ScheduleCompositionProfileSnapshot?>()
        showEditor(
            currentProfile = ScheduleCompositionProfileSnapshot(
                dayStartMinute = 9 * 60 + 30,
                dayEndMinute = 18 * 60,
                slotGranularityMinutes = 20,
                stabilityWeight = 50,
                defaultSoftWeight = 900,
            ),
            onSave = savedProfile::set,
        )

        composeRule.onNodeWithTag("reset_planning_profile")
            .performScrollTo()
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithTag("planning_start_hour").assertTextContains("07")
        composeRule.onNodeWithTag("planning_start_minute").assertTextContains("00")
        composeRule.onNodeWithTag("planning_end_hour").assertTextContains("22")
        composeRule.onNodeWithTag("planning_end_minute").assertTextContains("00")
        composeRule.onNodeWithTag("planning_stability_weight").assertTextContains("4")
        composeRule.onNodeWithTag("planning_soft_weight").assertTextContains("100")
        composeRule.onNodeWithTag("save_planning_profile")
            .assertIsEnabled()
            .performClick()
        composeRule.runOnIdle {
            assertEquals(ScheduleCompositionProfileSnapshot(), savedProfile.get())
        }
    }

    @Test
    fun canonicalBlockDisablesEveryMutableControlAndExplainsWhy() {
        showEditor(editBlockedMessage = BLOCKED_MESSAGE)

        assertEveryMutableControlIsDisabled()
        composeRule.onNodeWithTag("planning_profile_status")
            .performScrollTo()
            .assertTextContains(BLOCKED_MESSAGE)
            .assertIsDisplayed()
        composeRule.onNodeWithText("Cancel").assertIsEnabled()
    }

    @Test
    fun saveInProgressDisablesEveryControlIncludingDismiss() {
        showEditor(
            updateState = ScheduleCompositionProfileUpdateState(
                phase = ScheduleCompositionProfileUpdatePhase.SAVING,
                requestedProfile = ScheduleCompositionProfileSnapshot(),
                message = SAVING_MESSAGE,
            ),
        )

        assertEveryMutableControlIsDisabled()
        composeRule.onNodeWithTag("planning_profile_status")
            .performScrollTo()
            .assertTextContains(SAVING_MESSAGE)
            .assertIsDisplayed()
        composeRule.onNodeWithText("Cancel").assertIsNotEnabled()
        composeRule.onNodeWithText("Saving").assertIsDisplayed()
    }

    private fun showEditor(
        currentProfile: ScheduleCompositionProfileSnapshot =
            ScheduleCompositionProfileSnapshot(),
        editBlockedMessage: String? = null,
        updateState: ScheduleCompositionProfileUpdateState =
            ScheduleCompositionProfileUpdateState(),
        onSave: (ScheduleCompositionProfileSnapshot) -> Unit = {},
    ) {
        composeRule.setContent {
            MaterialTheme {
                PlanningProfileEditorDialog(
                    currentProfile = currentProfile,
                    editBlockedMessage = editBlockedMessage,
                    updateState = updateState,
                    onSave = onSave,
                    onDismiss = {},
                )
            }
        }
    }

    private fun replaceText(tag: String, value: String) {
        composeRule.onNodeWithTag(tag)
            .performScrollTo()
            .performTextReplacement(value)
    }

    private fun assertEveryMutableControlIsDisabled() {
        listOf(
            "planning_start_hour",
            "planning_start_minute",
            "planning_end_hour",
            "planning_end_minute",
            "planning_slot_granularity",
            "planning_stability_weight",
            "planning_soft_weight",
            "reset_planning_profile",
            "save_planning_profile",
        ).forEach { tag ->
            composeRule.onNodeWithTag(tag).assertIsNotEnabled()
        }
    }

    private companion object {
        const val BLOCKED_MESSAGE = "Wait for the current planner action to finish."
        const val SAVING_MESSAGE = "Saving the encrypted planning profile…"
    }
}
