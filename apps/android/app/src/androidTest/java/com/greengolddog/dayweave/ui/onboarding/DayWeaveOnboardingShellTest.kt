package com.greengolddog.dayweave.ui.onboarding

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.onboarding.OnboardingStep
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DayWeaveOnboardingShellTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun rendersEveryStepWithItsDistinctGuidance() {
        var renderedState by mutableStateOf(
            OnboardingUiState(
                step = OnboardingStep.WELCOME,
                privacyAcknowledged = true,
                readiness = OnboardingReadiness.Ready,
            ),
        )
        showShell(state = { renderedState })

        val cases = listOf(
            OnboardingStep.WELCOME to "A calmer, executable day",
            OnboardingStep.API to "Connect this phone",
            OnboardingStep.GOOGLE to "Choose what helps shape your day",
            OnboardingStep.PROFILE to "Give planning real boundaries",
            OnboardingStep.NOTIFICATIONS to "Ask only when it becomes useful",
            OnboardingStep.FIRST_ITEM to "Add something real to plan",
            OnboardingStep.FIRST_PLAN to "Publish one exact plan",
            OnboardingStep.READY to "Your planning workspace is ready",
        )

        cases.forEach { (step, title) ->
            composeRule.runOnIdle { renderedState = renderedState.copy(step = step) }
            composeRule.onNodeWithTag(OnboardingTestTags.page(step)).assertIsDisplayed()
            composeRule.onNodeWithText(title).assertExists()
        }
    }

    @Test
    fun privacyBoundaryReplacesRequestedPrivateStepWithOpaqueWelcome() {
        showShell(
            state = {
                OnboardingUiState(
                    step = OnboardingStep.GOOGLE,
                    privacyAcknowledged = false,
                    readiness = OnboardingReadiness.Ready,
                )
            },
        )

        composeRule.onNodeWithTag(OnboardingTestTags.OPAQUE_PRIVACY).assertIsDisplayed()
        composeRule.onNodeWithTag(OnboardingTestTags.page(OnboardingStep.WELCOME))
            .assertIsDisplayed()
        composeRule.onAllNodesWithText("Choose what helps shape your day").assertCountEquals(0)
        composeRule.onAllNodesWithText("Google Calendar · read for planning").assertCountEquals(0)
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsNotEnabled()
    }

    @Test
    fun primaryNavigationAndDeferActionsReachOnlyTheirCallbacks() {
        val primaryCalls = AtomicInteger()
        val backCalls = AtomicInteger()
        val deferCalls = AtomicInteger()
        val continueCalls = AtomicInteger()
        showShell(
            state = {
                OnboardingUiState(
                    step = OnboardingStep.API,
                    privacyAcknowledged = true,
                    readiness = OnboardingReadiness.Ready,
                )
            },
            callbacks = OnboardingCallbacks(
                onConnectThisPhone = { primaryCalls.incrementAndGet() },
                onBack = { backCalls.incrementAndGet() },
                onSetUpLater = { deferCalls.incrementAndGet() },
                onContinue = { continueCalls.incrementAndGet() },
            ),
        )

        composeRule.onNodeWithTag(OnboardingTestTags.PRIMARY_ACTION)
            .performScrollTo()
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithTag(OnboardingTestTags.BACK).performClick()
        composeRule.onNodeWithTag(OnboardingTestTags.SET_UP_LATER).performClick()
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).performClick()
        composeRule.onNodeWithText(
            "Set up later keeps setup incomplete and returns you to this step next time.",
        ).assertExists()

        composeRule.runOnIdle {
            assertEquals(1, primaryCalls.get())
            assertEquals(1, backCalls.get())
            assertEquals(1, deferCalls.get())
            assertEquals(1, continueCalls.get())
        }
    }

    @Test
    fun continuationStaysDisabledUntilPrivacyAndLiveReadinessAreReady() {
        var renderedState by mutableStateOf(OnboardingUiState())
        showShell(
            state = { renderedState },
            callbacks = OnboardingCallbacks(
                onPrivacyAcknowledgementChanged = { acknowledged ->
                    renderedState = renderedState.copy(privacyAcknowledged = acknowledged)
                },
            ),
        )

        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsNotEnabled()
        composeRule.onNodeWithTag(OnboardingTestTags.PRIVACY_CHECKBOX).performClick()
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsEnabled()

        composeRule.runOnIdle {
            renderedState = OnboardingUiState(
                step = OnboardingStep.API,
                privacyAcknowledged = true,
                readiness = OnboardingReadiness(api = OnboardingCheckState.PENDING),
            )
        }
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsNotEnabled()

        composeRule.runOnIdle {
            renderedState = renderedState.copy(
                readiness = renderedState.readiness.copy(api = OnboardingCheckState.READY),
            )
        }
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsEnabled()
    }

    @Test
    fun contextualNotificationDefaultDoesNotOfferASetupPermissionPrompt() {
        showShell(
            state = {
                OnboardingUiState(
                    step = OnboardingStep.NOTIFICATIONS,
                    privacyAcknowledged = true,
                )
            },
        )

        composeRule.onNodeWithText("Default · Ask when first needed")
            .performScrollTo()
            .assertIsDisplayed()
        composeRule.onNodeWithText("No permission prompt is launched by onboarding.")
            .assertExists()
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsEnabled()
    }

    @Test
    fun corruptOrFutureCheckpointRequiresExactResetWarning() {
        val resets = AtomicInteger()
        showShell(
            state = {
                OnboardingUiState(
                    step = OnboardingStep.FIRST_PLAN,
                    privacyAcknowledged = true,
                    readiness = OnboardingReadiness.Ready,
                    recovery = OnboardingRecoveryUiState.UNSUPPORTED_FUTURE_VERSION,
                )
            },
            callbacks = OnboardingCallbacks(
                onResetProgressAfterWarning = { resets.incrementAndGet() },
            ),
        )

        composeRule.onNodeWithTag(OnboardingTestTags.RECOVERY_WARNING)
            .performScrollTo()
            .assertIsDisplayed()
        composeRule.onNodeWithTag(OnboardingTestTags.CONTINUE).assertIsNotEnabled()
        composeRule.onNodeWithTag(OnboardingTestTags.RECOVERY_RESET).performClick()
        composeRule.onNodeWithText("Reset only guided-setup progress?").assertIsDisplayed()
        composeRule.onNodeWithText(
            "This replaces the newer-version setup checkpoint. It does not remove planner data, accounts, credentials, Google recovery, permissions, or schedules.",
        ).assertIsDisplayed()
        composeRule.onNodeWithTag(OnboardingTestTags.RECOVERY_CONFIRM).performClick()

        composeRule.runOnIdle { assertEquals(1, resets.get()) }
    }

    @Test
    fun readyStepRequiresEveryCheckAndCallsFinishOnly() {
        val finishes = AtomicInteger()
        val continues = AtomicInteger()
        var renderedState by mutableStateOf(
            OnboardingUiState(
                step = OnboardingStep.READY,
                privacyAcknowledged = true,
                readiness = OnboardingReadiness.Ready.copy(
                    firstPlan = OnboardingCheckState.NEEDS_ATTENTION,
                ),
            ),
        )
        showShell(
            state = { renderedState },
            callbacks = OnboardingCallbacks(
                onFinish = { finishes.incrementAndGet() },
                onContinue = { continues.incrementAndGet() },
            ),
        )

        composeRule.onNodeWithTag(OnboardingTestTags.READY_CHECKLIST).assertIsDisplayed()
        composeRule.onNodeWithTag(OnboardingTestTags.FINISH).assertIsNotEnabled()
        composeRule.runOnIdle {
            renderedState = renderedState.copy(readiness = OnboardingReadiness.Ready)
        }
        composeRule.onNodeWithTag(OnboardingTestTags.FINISH)
            .assertIsEnabled()
            .performClick()

        composeRule.runOnIdle {
            assertEquals(1, finishes.get())
            assertEquals(0, continues.get())
        }
    }

    private fun showShell(
        state: () -> OnboardingUiState,
        callbacks: OnboardingCallbacks = OnboardingCallbacks(),
    ) {
        composeRule.setContent {
            DayWeaveTheme(useDynamicColor = false) {
                DayWeaveOnboardingShell(
                    state = state(),
                    callbacks = callbacks,
                )
            }
        }
    }
}
