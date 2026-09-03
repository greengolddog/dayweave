package com.greengolddog.dayweave.ui.onboarding

import com.greengolddog.dayweave.onboarding.OnboardingStep
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingPresentationTest {
    @Test
    fun flowContainsEveryStepInStableOrder() {
        assertEquals(
            listOf(
                OnboardingStep.WELCOME,
                OnboardingStep.API,
                OnboardingStep.GOOGLE,
                OnboardingStep.PROFILE,
                OnboardingStep.NOTIFICATIONS,
                OnboardingStep.FIRST_ITEM,
                OnboardingStep.FIRST_PLAN,
                OnboardingStep.READY,
            ),
            onboardingSteps,
        )
        assertEquals(onboardingSteps.size, onboardingSteps.map { it.tagValue }.toSet().size)
        onboardingSteps.forEach { step ->
            assertTrue(step.title.isNotBlank())
            assertTrue(OnboardingTestTags.page(step).startsWith("onboarding_page_"))
        }
    }

    @Test
    fun unacknowledgedAndRecoveryStatesAlwaysPresentOpaqueWelcome() {
        assertEquals(
            OnboardingStep.WELCOME,
            OnboardingUiState(
                step = OnboardingStep.GOOGLE,
                privacyAcknowledged = false,
                readiness = OnboardingReadiness.Ready,
            ).presentedStep,
        )

        OnboardingRecoveryUiState.entries
            .filter { it != OnboardingRecoveryUiState.NONE }
            .forEach { recovery ->
                val state = OnboardingUiState(
                    step = OnboardingStep.FIRST_PLAN,
                    privacyAcknowledged = true,
                    readiness = OnboardingReadiness.Ready,
                    recovery = recovery,
                )
                assertEquals(OnboardingStep.WELCOME, state.presentedStep)
                assertFalse(state.privacyBoundaryOpen)
                assertFalse(state.canContinue)
                assertFalse(state.canGoBack)
            }
    }

    @Test
    fun continuationRequiresPrivacyAndTheCurrentLiveCheck() {
        assertFalse(OnboardingUiState().canContinue)
        assertTrue(
            OnboardingUiState(
                step = OnboardingStep.WELCOME,
                privacyAcknowledged = true,
            ).canContinue,
        )

        val pending = OnboardingReadiness()
        onboardingSteps
            .filter { it != OnboardingStep.WELCOME && it != OnboardingStep.READY }
            .forEach { step ->
                val state = OnboardingUiState(
                    step = step,
                    privacyAcknowledged = true,
                    readiness = pending,
                )
                assertEquals(
                    "Only notifications have a safe ready-by-default choice",
                    step == OnboardingStep.NOTIFICATIONS,
                    state.canContinue,
                )
            }

        onboardingSteps
            .filter { it != OnboardingStep.WELCOME }
            .forEach { step ->
                assertTrue(
                    "Ready evidence should admit ${step.name}",
                    OnboardingUiState(
                        step = step,
                        privacyAcknowledged = true,
                        readiness = OnboardingReadiness.Ready,
                    ).canContinue,
                )
            }
    }

    @Test
    fun workingAndNeedsAttentionNeverCountAsReady() {
        OnboardingCheckState.entries
            .filter { it != OnboardingCheckState.READY }
            .forEach { check ->
                val readiness = OnboardingReadiness.Ready.copy(api = check)
                assertFalse(
                    OnboardingUiState(
                        step = OnboardingStep.API,
                        privacyAcknowledged = true,
                        readiness = readiness,
                    ).canContinue,
                )
                assertFalse(
                    OnboardingUiState(
                        step = OnboardingStep.READY,
                        privacyAcknowledged = true,
                        readiness = readiness,
                    ).canContinue,
                )
            }
    }

    @Test
    fun readinessProjectionHasNoWelcomeOrCompletionEvidence() {
        val readiness = OnboardingReadiness.Ready
        assertNull(readiness.checkFor(OnboardingStep.WELCOME))
        assertNull(readiness.checkFor(OnboardingStep.READY))
        onboardingSteps
            .filter { it != OnboardingStep.WELCOME && it != OnboardingStep.READY }
            .forEach { assertEquals(OnboardingCheckState.READY, readiness.checkFor(it)) }
    }

    @Test
    fun notificationDefaultIsContextualAndReadyWithoutSystemPromptEvidence() {
        val readiness = OnboardingReadiness()

        assertEquals(OnboardingCheckState.READY, readiness.notifications)
        assertTrue(
            OnboardingUiState(
                step = OnboardingStep.NOTIFICATIONS,
                privacyAcknowledged = true,
                readiness = readiness,
            ).canContinue,
        )
    }
}
