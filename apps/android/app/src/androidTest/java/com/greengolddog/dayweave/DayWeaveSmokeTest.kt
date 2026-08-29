package com.greengolddog.dayweave

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.state.PlannerLoadState
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Rule
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DayWeaveSmokeTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<MainActivity>()

    @Before
    fun returnToStableDestination() {
        composeRule.onAllNodesWithText("Today")[0].performClick()
        composeRule.waitForIdle()
    }

    @Test
    fun todayAndPrimaryNavigationAreVisible() {
        composeRule.onNodeWithText("Today").performClick()
        composeRule.onAllNodesWithText("DayWeave")[0].assertIsDisplayed()
        composeRule.onNodeWithText("Your timeline").assertIsDisplayed()
        composeRule.onNodeWithText("Calendar").assertIsDisplayed()
        composeRule.onNodeWithText("Inbox").assertIsDisplayed()
        composeRule.onNodeWithText("Assistant").assertIsDisplayed()
    }

    @Test
    fun externalSuggestionsArePresentedAsReviewableDrafts() {
        val suggestionTitle = "SYNTHETIC-REVIEWABLE-SUGGESTION-${System.nanoTime()}"
        val application = composeRule.activity.application as DayWeaveApplication
        runBlocking {
            application.plannerStore.loadState.first { it != PlannerLoadState.LOADING }
            requireNotNull(
                application.plannerStore.replaceRemoteSuggestions(
                    listOf(
                        PlanningSuggestion(
                            id = "synthetic-reviewable-suggestion-${System.nanoTime()}",
                            title = suggestionTitle,
                            summary = "Keep a synthetic recovery window open.",
                            source = "Synthetic test",
                            kind = SuggestionKind.SCHEDULE_CHANGE,
                            expiresInDays = 7,
                            remoteRevision = 1,
                            remotePayloadJson = "{}",
                        ),
                    ),
                ),
            ).awaitDurable()
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Inbox").performClick()
        composeRule.onNodeWithText("Proposal-only safety").assertIsDisplayed()

        composeRule.onNodeWithText("Suggestions", substring = true).performClick()

        composeRule.onNodeWithText(suggestionTitle).assertIsDisplayed()
        composeRule.onAllNodesWithText("Accept draft")[0].assertIsDisplayed()
    }

    @Test
    fun manualEnergyCheckInHasAccessibleSelectableControls() {
        composeRule.onNodeWithText("Today").performClick()
        composeRule.onNodeWithTag("energy_signal_card").assertIsDisplayed()
        composeRule.onNodeWithTag("energy_check_in_low").performClick().assertIsSelected()
    }

    @Test
    fun quickCaptureCanCreateAnExplicitSensitiveDraft() {
        val title = "SYNTHETIC-SENSITIVE-QUICK-CAPTURE-${System.nanoTime()}"
        composeRule.onNodeWithContentDescription("Quick capture").performClick()
        composeRule.onNodeWithText("What do you need to do?").performTextInput(title)
        composeRule.onNodeWithTag("quick_capture_sensitive_toggle").performClick().assertIsOn()
        composeRule.onNodeWithText("Add to Inbox").performClick()
        composeRule.onNodeWithText("Inbox").performClick()

        composeRule.onNodeWithText(title).assertIsDisplayed()
        composeRule.onNodeWithText("SENSITIVE").assertIsDisplayed()
    }
}
