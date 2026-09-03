package com.greengolddog.dayweave.ui

import com.greengolddog.dayweave.onboarding.OnboardingCheckpoint
import com.greengolddog.dayweave.onboarding.OnboardingControllerState
import com.greengolddog.dayweave.onboarding.OnboardingCorruptArtifactIdentity
import com.greengolddog.dayweave.onboarding.OnboardingRuntimePrivacyState
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DayWeavePrivatePresentationAdmissionTest {
    private val acknowledged = OnboardingControllerState.Active(
        OnboardingCheckpoint.fresh().copy(
            privacyAcknowledged = true,
            privacyReleaseCompleted = true,
        ),
    )
    private val openRuntime = OnboardingRuntimePrivacyState(
        privacyAcknowledged = true,
        appUnlocked = true,
        activityStarted = true,
    )

    @Test
    fun privatePlannerRequiresExactAcknowledgedUnlockedStartedState() {
        assertTrue(shouldMountPrivatePlannerSubtree(acknowledged, openRuntime))

        assertFalse(
            shouldMountPrivatePlannerSubtree(
                OnboardingControllerState.Active(OnboardingCheckpoint.fresh()),
                openRuntime,
            ),
        )
        assertFalse(
            shouldMountPrivatePlannerSubtree(
                OnboardingControllerState.Active(
                    OnboardingCheckpoint.fresh().copy(privacyAcknowledged = true),
                ),
                openRuntime,
            ),
        )
        assertFalse(
            shouldMountPrivatePlannerSubtree(
                OnboardingControllerState.RecoveryRequired(
                    OnboardingCorruptArtifactIdentity("test-corrupt-artifact"),
                ),
                openRuntime,
            ),
        )
        assertFalse(
            shouldMountPrivatePlannerSubtree(
                acknowledged,
                openRuntime.copy(appUnlocked = false),
            ),
        )
        assertFalse(
            shouldMountPrivatePlannerSubtree(
                acknowledged,
                openRuntime.copy(activityStarted = false),
            ),
        )
        assertFalse(shouldMountPrivatePlannerSubtree(null, openRuntime))
        assertFalse(shouldMountPrivatePlannerSubtree(acknowledged, null))
    }
}
