package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.sync.AssistantDisclosureSummary
import com.greengolddog.dayweave.sync.AssistantPhase
import com.greengolddog.dayweave.sync.AssistantState
import com.greengolddog.dayweave.ui.screens.AssistantScreen
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AssistantUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun disconnectedAssistantOffersConfigurationWithoutPretendingToBeAi() {
        val configured = AtomicBoolean(false)
        composeRule.setContent {
            MaterialTheme {
                AssistantScreen(
                    state = DayWeaveUiState(),
                    assistantState = AssistantState(
                        phase = AssistantPhase.NOT_CONFIGURED,
                        message = "Connect the DayWeave API to use the Android assistant.",
                    ),
                    onSend = { false },
                    onStop = {},
                    onConfigureConnection = { configured.set(true) },
                )
            }
        }

        composeRule.onNodeWithText("Configure connection").assertIsDisplayed().performClick()
        composeRule.onNodeWithText(
            "The assistant can discuss your plan. It cannot directly change it.",
        ).assertIsDisplayed()
        composeRule.onNodeWithContentDescription("Send").assertIsNotEnabled()
        assertTrue(configured.get())
    }

    @Test
    fun activeTurnShowsDisclosureManifestAndManualStop() {
        val stopped = AtomicBoolean(false)
        composeRule.setContent {
            MaterialTheme {
                AssistantScreen(
                    state = DayWeaveUiState(),
                    assistantState = AssistantState(
                        phase = AssistantPhase.SENDING,
                        message = "Sharing redacted planning context.",
                        disclosure = AssistantDisclosureSummary(
                            publicScheduledBlocks = 3,
                            privateBusySpans = 2,
                            plannerItems = 5,
                            omittedFields = 6,
                        ),
                    ),
                    onSend = { false },
                    onStop = { stopped.set(true) },
                    onConfigureConnection = {},
                )
            }
        }

        composeRule.onNodeWithText(
            "Context · 3 public blocks · 2 private busy spans · 5 planner items",
        ).assertIsDisplayed()
        composeRule.onNodeWithText(
            "Sensitive titles, all notes, stable IDs, and raw constraints are omitted.",
        ).assertIsDisplayed()
        composeRule.onNodeWithText("Stop").assertIsDisplayed().performClick()
        assertTrue(stopped.get())
    }

    @Test
    fun storedTranscriptExplainsWhenProviderContextStartsOver() {
        composeRule.setContent {
            MaterialTheme {
                AssistantScreen(
                    state = DayWeaveUiState(
                        messages = listOf(
                            ChatMessage("saved-user", ChatRole.USER, "Plan today"),
                        ),
                    ),
                    assistantState = AssistantState(
                        phase = AssistantPhase.READY,
                        message = "Ready.",
                    ),
                    onSend = { false },
                    onStop = {},
                    onConfigureConnection = {},
                )
            }
        }

        composeRule.onNodeWithText("Conversation context").assertIsDisplayed()
        composeRule.onNodeWithText(
            "Earlier messages stay visible on this device for reference. " +
                "Assistant context starts over after app lock, background, restart, or API " +
                "connection changes.",
        ).assertIsDisplayed()
    }
}
