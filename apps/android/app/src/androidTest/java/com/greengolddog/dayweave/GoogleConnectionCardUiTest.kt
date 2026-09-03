package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.ui.screens.GoogleConnectionCard
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class GoogleConnectionCardUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun activeAccountOffersIndependentCalendarAndTasksPublishingUpgrades() {
        val calendarRequests = mutableListOf<String>()
        val taskRequests = mutableListOf<String>()
        showCard(
            state = connectedState(
                account = account(
                    hasCalendarWriteScope = false,
                    hasTasksWriteScope = false,
                ),
            ),
            onEnableCalendarPublishing = calendarRequests::add,
            onEnableTasksPublishing = taskRequests::add,
        )

        composeRule.onNodeWithText(
            "Calendar import · Tasks import · active",
        ).assertIsDisplayed()
        composeRule.onNodeWithTag("google_enable_calendar_publishing_0")
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithTag("google_enable_tasks_publishing_0")
            .assertIsEnabled()
            .performClick()

        composeRule.runOnIdle {
            assertEquals(listOf(ACCOUNT_ID), calendarRequests)
            assertEquals(listOf(ACCOUNT_ID), taskRequests)
        }
    }

    @Test
    fun reauthorizationRequiredAccountCanRenewAnExistingTasksPublishingGrant() {
        val taskRequests = mutableListOf<String>()
        showCard(
            state = connectedState(
                account = account(
                    status = "reauthorization_required",
                    syncEnabled = false,
                    hasCalendarWriteScope = false,
                    hasTasksWriteScope = true,
                ),
            ),
            onEnableTasksPublishing = taskRequests::add,
        )

        composeRule.onNodeWithText("Reauthorize").assertIsEnabled()
        composeRule.onNodeWithTag("google_renew_tasks_publishing_0")
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithTag("google_enable_calendar_publishing_0")
            .assertDoesNotExist()

        composeRule.runOnIdle {
            assertEquals(listOf(ACCOUNT_ID), taskRequests)
        }
    }

    @Test
    fun activeAccountCanReauthorizeFromItsImportRun() {
        val requests = mutableListOf<String>()
        showCard(
            state = connectedState(
                account = account(
                    hasCalendarWriteScope = false,
                    hasTasksWriteScope = false,
                ),
            ),
            onReauthorize = requests::add,
            calendarImportReauthorizationAccountIds = setOf(ACCOUNT_ID),
        )

        composeRule.onNodeWithText("Reauthorize").assertIsEnabled().performClick()

        composeRule.runOnIdle { assertEquals(listOf(ACCOUNT_ID), requests) }
    }

    @Test
    fun operatorRecoveryDisablesEveryNewAuthorizationAction() {
        showCard(
            state = connectedState(
                account = account(
                    status = "reauthorization_required",
                    syncEnabled = false,
                    hasCalendarWriteScope = false,
                    hasTasksWriteScope = true,
                ),
            ).copy(
                phase = GoogleAccountPhase.RECOVERY_REQUIRED,
                message = "Google account recovery required",
            ),
        )

        composeRule.onNodeWithText("Reauthorize").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_renew_tasks_publishing_0").assertIsNotEnabled()
    }

    @Test
    fun orphanedAuthorizationRequiresExplicitReviewAndBlocksAccountMutations() {
        var reviewRequests = 0
        showCard(
            state = connectedState(
                account = account(
                    hasCalendarWriteScope = false,
                    hasTasksWriteScope = false,
                ),
            ).copy(
                phase = GoogleAccountPhase.AUTHORIZATION_RECOVERY,
                authorizationRecoveryDiscardRequired = true,
                message = "Google recovery required",
            ),
            onRequestAuthorizationRecoveryDiscard = { reviewRequests += 1 },
        )

        composeRule.onNodeWithText("Saved Google authorization needs review")
            .assertIsDisplayed()
        composeRule.onNodeWithTag("google_enable_calendar_publishing_0")
            .assertIsNotEnabled()
        composeRule.onNodeWithTag("google_enable_tasks_publishing_0")
            .assertIsNotEnabled()
        composeRule.onNodeWithText("Pause sync").assertIsNotEnabled()
        composeRule.onNodeWithText("Disconnect").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_review_authorization_discard")
            .assertIsEnabled()
            .performClick()

        composeRule.runOnIdle { assertEquals(1, reviewRequests) }
    }

    private fun showCard(
        state: GoogleAccountState,
        onEnableCalendarPublishing: (String) -> Unit = {},
        onEnableTasksPublishing: (String) -> Unit = {},
        onRequestAuthorizationRecoveryDiscard: () -> Unit = {},
        onReauthorize: (String) -> Unit = {},
        calendarImportReauthorizationAccountIds: Set<String> = emptySet(),
    ) {
        composeRule.setContent {
            MaterialTheme {
                GoogleConnectionCard(
                    state = state,
                    onConfigureApiConnection = {},
                    onConnect = {},
                    onRefresh = {},
                    onRestartAuthorization = {},
                    onOpenAuthorization = {},
                    onReauthorize = onReauthorize,
                    onEnableCalendarPublishing = onEnableCalendarPublishing,
                    onEnableTasksPublishing = onEnableTasksPublishing,
                    onRequestAuthorizationRecoveryReset = {},
                    onRequestAuthorizationRecoveryDiscard =
                        onRequestAuthorizationRecoveryDiscard,
                    onSetPaused = { _, _ -> },
                    onRequestDisconnect = {},
                    calendarImportBusy = false,
                    calendarImportHasRecovery = false,
                    calendarImportReauthorizationAccountIds =
                        calendarImportReauthorizationAccountIds,
                )
            }
        }
    }

    private fun connectedState(account: GoogleAccountSummary) = GoogleAccountState(
        phase = GoogleAccountPhase.CONNECTED,
        accounts = listOf(account),
        message = "Google Calendar and Tasks connected",
        configurationId = "configuration-1",
    )

    private fun account(
        status: String = "active",
        syncEnabled: Boolean = true,
        hasCalendarWriteScope: Boolean,
        hasTasksWriteScope: Boolean,
    ) = GoogleAccountSummary(
        id = ACCOUNT_ID,
        label = "Personal",
        status = status,
        syncEnabled = syncEnabled,
        isDefault = true,
        hasCalendar = true,
        hasCalendarWriteScope = hasCalendarWriteScope,
        hasTasks = true,
        hasTasksWriteScope = hasTasksWriteScope,
        revision = 7,
    )

    private companion object {
        const val ACCOUNT_ID = "11111111-1111-4111-8111-111111111111"
    }
}
