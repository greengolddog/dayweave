package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.DeviceAuthPhase
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.AccountRecoveryPhase
import com.greengolddog.dayweave.network.AccountRecoveryState
import com.greengolddog.dayweave.sync.GoogleAuthorizationCorruptArtifactIdentity
import com.greengolddog.dayweave.sync.GoogleAuthorizationRecoveryDiscardConfirmation
import com.greengolddog.dayweave.ui.components.ApiConnectionDialog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ApiConnectionDialogUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun recoveryWarningBlocksEnrollmentRetryAndSignOut() {
        val displayedAuth = mutableStateOf(deviceAuthState(DeviceAuthPhase.REAUTH))
        val discardRequired = mutableStateOf(true)
        var enrollmentCalls = 0
        var retryCalls = 0
        var signOutCalls = 0
        showDialog(
            authState = { displayedAuth.value },
            discardRequired = { discardRequired.value },
            onConsumeEnrollmentCode = { _, _ -> enrollmentCalls += 1 },
            onRetryPending = { retryCalls += 1 },
            onRevokeAndSignOut = { signOutCalls += 1 },
        )

        composeRule.onNodeWithText(
            "Review that recovery before enrollment, retry, sign-out, or account recovery.",
            substring = true,
        ).assertIsDisplayed()
        composeRule.onNodeWithText("One-time dw_en1_ code")
            .performTextInput("dw_en1_synthetic")
        composeRule.onNodeWithText("Consume code").assertIsNotEnabled()

        composeRule.runOnIdle { discardRequired.value = false }
        composeRule.onNodeWithText("Consume code").assertIsEnabled()

        composeRule.runOnIdle {
            displayedAuth.value = deviceAuthState(DeviceAuthPhase.ENROLLMENT_PENDING)
            discardRequired.value = true
        }
        composeRule.onNodeWithText("Retry exact state").assertIsNotEnabled()

        composeRule.runOnIdle {
            displayedAuth.value = deviceAuthState(DeviceAuthPhase.ACTIVE)
        }
        composeRule.onNodeWithText("Revoke & sign out").assertIsNotEnabled()
        composeRule.runOnIdle {
            assertEquals(0, enrollmentCalls)
            assertEquals(0, retryCalls)
            assertEquals(0, signOutCalls)
        }
    }

    @Test
    fun accountRecoveryIsBlockedByLocalWriteJournalAndRequiresExplicitConfirmation() {
        val blocked = mutableStateOf(true)
        val consumed = mutableListOf<Pair<String, String>>()
        showDialog(
            authState = { deviceAuthState(DeviceAuthPhase.ACTIVE) },
            discardRequired = { false },
            credentialBlocked = { blocked.value },
            accountRecoveryState = {
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.READY,
                    message = "Recovery ready.",
                )
            },
            onConsumeAccountRecoveryCode = { baseUrl, code -> consumed += baseUrl to code },
        )

        composeRule.onNodeWithTag("account_recovery_entry_mode").performClick()
        composeRule.onNodeWithText("Saved dw_rc1_ recovery code")
            .performTextInput("dw_rc1_test-value")
        composeRule.onNodeWithTag("consume_account_recovery_code").assertIsNotEnabled()
        composeRule.onNodeWithText(
            "before enrollment, sign-out, or account recovery",
            substring = true,
        ).assertIsDisplayed()

        composeRule.runOnIdle { blocked.value = false }
        composeRule.onNodeWithTag("consume_account_recovery_code")
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithText("Replace every account connection?").assertIsDisplayed()
        composeRule.runOnIdle { assertEquals(0, consumed.size) }
        composeRule.onNodeWithTag("confirm_account_recovery_consume").performClick()
        composeRule.runOnIdle {
            assertEquals(listOf(API_BASE_URL to "dw_rc1_test-value"), consumed)
        }
    }

    @Test
    fun committedRecoveryDisablesGenericCredentialDestruction() {
        showDialog(
            authState = { deviceAuthState(DeviceAuthPhase.ACTIVE) },
            discardRequired = { false },
            accountRecoveryState = {
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.PENDING,
                    message = "Recovery is committed and ready for local installation.",
                    retryAvailable = true,
                    deviceAuthorizationSuppressed = true,
                )
            },
        )

        composeRule.onNodeWithText("Revoke & sign out").assertIsNotEnabled()
        composeRule.onNodeWithText("Local-only removal").assertIsNotEnabled()
    }

    @Test
    fun keepingReviewedRecoveryNeverInvokesDiscard() {
        val exactConfirmation = discardConfirmation(presentationGeneration = 7)
        var providerCalls = 0
        var discardCalls = 0
        showDialog(
            confirmationProvider = {
                providerCalls += 1
                exactConfirmation
            },
            onDiscard = { discardCalls += 1 },
        )

        composeRule.onNodeWithTag("api_review_google_authorization_discard")
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithText("Discard this saved Google authorization?")
            .assertIsDisplayed()
        composeRule.onNodeWithText("Keep it").performClick()

        composeRule.onNodeWithTag("api_review_google_authorization_discard")
            .assertIsDisplayed()
        composeRule.runOnIdle {
            assertEquals(1, providerCalls)
            assertEquals(0, discardCalls)
        }
    }

    @Test
    fun confirmUsesTheExactReviewedTokenOnceWithoutRefetching() {
        val displayedAuth = mutableStateOf(deviceAuthState(DeviceAuthPhase.ACTIVE))
        val reviewedConfirmation = discardConfirmation(presentationGeneration = 11)
        val replacementConfirmation = discardConfirmation(presentationGeneration = 12)
        var offeredConfirmation = reviewedConfirmation
        var providerCalls = 0
        val discarded = mutableListOf<GoogleAuthorizationRecoveryDiscardConfirmation>()
        showDialog(
            authState = { displayedAuth.value },
            confirmationProvider = {
                providerCalls += 1
                offeredConfirmation
            },
            onDiscard = discarded::add,
        )

        composeRule.onNodeWithTag("api_review_google_authorization_discard").performClick()
        composeRule.runOnIdle {
            offeredConfirmation = replacementConfirmation
            displayedAuth.value = displayedAuth.value.copy(message = "Updated presentation")
            assertEquals(1, providerCalls)
        }
        composeRule.onNodeWithTag("api_confirm_google_authorization_discard")
            .performClick()

        composeRule.onAllNodesWithTag("api_confirm_google_authorization_discard")
            .assertCountEquals(0)
        composeRule.runOnIdle {
            assertEquals(1, providerCalls)
            assertEquals(1, discarded.size)
            assertSame(reviewedConfirmation, discarded.single())
        }
    }

    private fun showDialog(
        authState: () -> DeviceAuthUiState = { deviceAuthState(DeviceAuthPhase.ACTIVE) },
        discardRequired: () -> Boolean = { true },
        confirmationProvider: () -> GoogleAuthorizationRecoveryDiscardConfirmation? = {
            discardConfirmation(presentationGeneration = 1)
        },
        onConsumeEnrollmentCode: (String, String) -> Unit = { _, _ -> },
        onRetryPending: () -> Unit = {},
        onRevokeAndSignOut: () -> Unit = {},
        onDiscard: (GoogleAuthorizationRecoveryDiscardConfirmation) -> Unit = {},
        credentialBlocked: () -> Boolean = { false },
        accountRecoveryState: () -> AccountRecoveryState = {
            AccountRecoveryState(
                phase = AccountRecoveryPhase.NOT_AVAILABLE,
                message = "Account recovery is not loaded.",
            )
        },
        onConsumeAccountRecoveryCode: (String, String) -> Unit = { _, _ -> },
    ) {
        composeRule.setContent {
            MaterialTheme {
                ApiConnectionDialog(
                    authState = authState(),
                    credentialReplacementBlocked = credentialBlocked(),
                    googleAuthorizationRecoveryDiscardRequired = discardRequired(),
                    onDismiss = {},
                    onUpgradeWithBootstrap = { _, _ -> },
                    onConsumeEnrollmentCode = onConsumeEnrollmentCode,
                    onRetryPending = onRetryPending,
                    onRevokeAndSignOut = onRevokeAndSignOut,
                    onDestroyLocalOnly = {},
                    googleAuthorizationRecoveryDiscardConfirmationProvider =
                        confirmationProvider,
                    onDiscardGoogleAuthorizationRecovery = onDiscard,
                    accountRecoveryState = accountRecoveryState(),
                    onConsumeAccountRecoveryCode = onConsumeAccountRecoveryCode,
                )
            }
        }
    }

    private fun discardConfirmation(
        presentationGeneration: Long,
    ) = GoogleAuthorizationRecoveryDiscardConfirmation(
        presentationGeneration = presentationGeneration,
        binding = ApiConnectionSnapshot(
            baseUrl = API_BASE_URL,
            hasBearerToken = true,
            lastSuccessfulSyncEpochMillis = null,
            configurationId = CONFIGURATION_ID,
        ),
        expectedJournal = null,
        expectedCorruptArtifact = GoogleAuthorizationCorruptArtifactIdentity(
            "test-corrupt-artifact",
        ),
    )

    private fun deviceAuthState(phase: DeviceAuthPhase) = DeviceAuthUiState(
        phase = phase,
        baseUrl = API_BASE_URL,
        clientInstanceId = "android-client",
        sessionId = if (phase == DeviceAuthPhase.ACTIVE) "device-session" else null,
        deviceLabel = "Android test device",
        accessExpiresAt = null,
        message = "Authentication state",
    )

    private companion object {
        const val API_BASE_URL = "https://api.example.test/"
        const val CONFIGURATION_ID = "configuration-a"
    }
}
