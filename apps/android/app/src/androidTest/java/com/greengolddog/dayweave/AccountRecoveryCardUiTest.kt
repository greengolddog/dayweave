package com.greengolddog.dayweave

import android.content.ClipboardManager
import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.network.AccountRecoveryDisclosure
import com.greengolddog.dayweave.network.AccountRecoveryIssuanceConfirmation
import com.greengolddog.dayweave.network.AccountRecoveryJournalDiscardConfirmation
import com.greengolddog.dayweave.network.AccountRecoveryPhase
import com.greengolddog.dayweave.network.AccountRecoveryState
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.StoredDeviceAuthEnvelope
import com.greengolddog.dayweave.network.StoredDeviceAuthState
import com.greengolddog.dayweave.ui.screens.AccountRecoveryCard
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AccountRecoveryCardUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun issuanceRequiresAndUsesTheExactDisplayedConfirmation() {
        val confirmation = issuanceConfirmation()
        val issued = mutableListOf<AccountRecoveryIssuanceConfirmation>()
        showCard(
            state = readyState(),
            issuanceConfirmationProvider = { confirmation },
            onIssueOrRotate = issued::add,
        )

        composeRule.onNodeWithTag("issue_or_rotate_account_recovery").performClick()
        composeRule.onNodeWithText("Create recovery code?").assertIsDisplayed()
        composeRule.runOnIdle { assertEquals(0, issued.size) }
        composeRule.onNodeWithTag("confirm_account_recovery_issue").performClick()

        composeRule.runOnIdle {
            assertEquals(1, issued.size)
            assertSame(confirmation, issued.single())
        }
    }

    @Test
    fun disclosureCopyIsClearedWhenOwnerHidesWithoutAcknowledging() {
        val disclosure = AccountRecoveryDisclosure(
            generation = 9,
            journalId = RECOVERY_ID,
            journalCode = RECOVERY_CODE,
            source = "successor",
        )
        val acknowledged = mutableListOf<AccountRecoveryDisclosure>()
        showCard(
            state = readyState().copy(
                phase = AccountRecoveryPhase.DISCLOSURE_READY,
                message = "Save successor.",
                disclosureReady = true,
                canIssueOrRotate = false,
            ),
            disclosureProvider = { disclosure },
            onAcknowledge = acknowledged::add,
        )
        composeRule.onNodeWithTag("reveal_account_recovery_code").performClick()
        composeRule.onNodeWithTag("account_recovery_code_value")
            .assertIsDisplayed()
            .assert(SemanticsMatcher.keyNotDefined(SemanticsActions.CopyText))
            .assert(SemanticsMatcher.keyNotDefined(SemanticsActions.SetSelection))
        composeRule.onNodeWithTag("copy_account_recovery_code")
            .assertIsEnabled()
            .performClick()

        composeRule.runOnIdle { assertEquals(RECOVERY_CODE, clipboardText()) }
        composeRule.onNodeWithText("Hide for now").performClick()
        composeRule.runOnIdle {
            assertNotEquals(RECOVERY_CODE, clipboardText())
            assertEquals(0, acknowledged.size)
        }
    }

    @Test
    fun repairRequiresExplicitExactConfirmationAndPreservesCancelPath() {
        val confirmation = discardConfirmation(repair = true)
        val discarded = mutableListOf<AccountRecoveryJournalDiscardConfirmation>()
        showCard(
            state = AccountRecoveryState(
                phase = AccountRecoveryPhase.REPAIR_REQUIRED,
                message = "Saved recovery state requires owner-confirmed repair.",
                discardAvailable = true,
                repairRequired = true,
            ),
            discardConfirmationProvider = { confirmation },
            onDiscard = discarded::add,
        )

        composeRule.onNodeWithText("Repair state").performClick()
        composeRule.onNodeWithText("Remove unreadable recovery state?").assertIsDisplayed()
        composeRule.onNodeWithText("Keep for retry").performClick()
        composeRule.runOnIdle { assertEquals(0, discarded.size) }

        composeRule.onNodeWithText("Repair state").performClick()
        composeRule.onNodeWithTag("confirm_discard_account_recovery_journal").performClick()
        composeRule.runOnIdle { assertSame(confirmation, discarded.single()) }
    }

    @Test
    fun unresolvedWriteJournalDisablesRecoveryEntryWithVisibleGuidance() {
        showCard(state = readyState(), recoveryStartBlocked = true)

        composeRule.onNodeWithTag("open_account_recovery").assertIsNotEnabled()
        composeRule.onNodeWithText(
            "Finish the saved Planner or Google operation before using a recovery code.",
        ).assertIsDisplayed()
        composeRule.onNodeWithTag("refresh_account_recovery").assertIsEnabled()
    }

    private fun showCard(
        state: AccountRecoveryState,
        recoveryStartBlocked: Boolean = false,
        issuanceConfirmationProvider: () -> AccountRecoveryIssuanceConfirmation? = { null },
        onIssueOrRotate: (AccountRecoveryIssuanceConfirmation) -> Unit = {},
        disclosureProvider: () -> AccountRecoveryDisclosure? = { null },
        onAcknowledge: (AccountRecoveryDisclosure) -> Unit = {},
        discardConfirmationProvider: () -> AccountRecoveryJournalDiscardConfirmation? = { null },
        onDiscard: (AccountRecoveryJournalDiscardConfirmation) -> Unit = {},
    ) {
        composeRule.setContent {
            MaterialTheme {
                AccountRecoveryCard(
                    state = state,
                    recoveryStartBlocked = recoveryStartBlocked,
                    onRefresh = {},
                    issuanceConfirmationProvider = issuanceConfirmationProvider,
                    onIssueOrRotate = onIssueOrRotate,
                    onRetry = {},
                    disclosureProvider = disclosureProvider,
                    onAcknowledgeDisclosure = onAcknowledge,
                    journalDiscardConfirmationProvider = discardConfirmationProvider,
                    onDiscardJournal = onDiscard,
                    onRecoverAccount = {},
                )
            }
        }
    }

    private fun readyState() = AccountRecoveryState(
        phase = AccountRecoveryPhase.READY,
        message = "No account recovery code is active.",
        canIssueOrRotate = true,
    )

    private fun issuanceConfirmation() = AccountRecoveryIssuanceConfirmation(
        generation = 7,
        binding = binding(),
        currentCodeId = null,
        currentCodeRevision = null,
    )

    private fun discardConfirmation(repair: Boolean) =
        AccountRecoveryJournalDiscardConfirmation(
            generation = 8,
            expected = StoredDeviceAuthEnvelope(
                revision = 3,
                state = StoredDeviceAuthState.Unconfigured(
                    clientInstanceId = CLIENT_INSTANCE_ID,
                ),
            ),
            repairsUnreadableState = repair,
        )

    private fun binding() = ApiConnectionSnapshot(
        baseUrl = API_BASE_URL,
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = SESSION_ID,
        clientInstanceId = CLIENT_INSTANCE_ID,
    )

    private fun clipboardText(): String? {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        return clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString()
    }

    private companion object {
        const val API_BASE_URL = "https://api.example.test/"
        const val SESSION_ID = "11111111-1111-4111-8111-111111111111"
        const val CLIENT_INSTANCE_ID = "22222222-2222-4222-8222-222222222222"
        const val RECOVERY_ID = "55555555-5555-4555-8555-555555555555"
        const val RECOVERY_CODE =
            "dw_rc1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    }
}
