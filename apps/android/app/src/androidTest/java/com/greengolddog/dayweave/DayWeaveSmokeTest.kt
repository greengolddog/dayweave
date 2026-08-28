package com.greengolddog.dayweave

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DayWeaveSmokeTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<MainActivity>()

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
        composeRule.onNodeWithText("Inbox").performClick()
        composeRule.onNodeWithText("Proposal-only safety").assertIsDisplayed()

        composeRule.onNodeWithText("Suggestions (2)").performClick()

        composeRule.onNodeWithText("Protect a recovery window").assertIsDisplayed()
        composeRule.onAllNodesWithText("Accept draft")[0].assertIsDisplayed()
    }
}
