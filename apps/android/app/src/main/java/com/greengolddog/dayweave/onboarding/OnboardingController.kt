package com.greengolddog.dayweave.onboarding

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

sealed interface OnboardingControllerState {
    data class Active(
        val checkpoint: OnboardingCheckpoint,
        /** Volatile session choice: it is intentionally absent from the durable schema. */
        val setupDeferredForSession: Boolean = false,
    ) : OnboardingControllerState {
        val currentStep: OnboardingStep get() = checkpoint.currentStep
        val furthestStep: OnboardingStep get() = checkpoint.furthestStep
        val privacyAcknowledged: Boolean get() = checkpoint.privacyAcknowledged
        val privacyReleaseCompleted: Boolean get() = checkpoint.privacyReleaseCompleted
        val profileReviewed: Boolean get() = checkpoint.profileReviewed
        val completed: Boolean get() = checkpoint.completed
    }

    data class RecoveryRequired(
        val artifactIdentity: OnboardingCorruptArtifactIdentity,
    ) : OnboardingControllerState {
        override fun toString(): String = "RecoveryRequired(<redacted>)"
    }
}

/**
 * UI-independent onboarding state machine.
 *
 * Every durable transition completes in the store before [state] changes. A write failure or stale
 * compare-and-set therefore leaves the observable in-memory state untouched.
 */
class OnboardingController(
    private val store: OnboardingCheckpointStore,
) {
    private val mutableStates = MutableStateFlow(store.load().toControllerState())

    /** Compose-friendly observation updated only after each durable transition succeeds. */
    val states: StateFlow<OnboardingControllerState> = mutableStates.asStateFlow()

    /** Synchronous snapshot for startup code and non-Compose runtime gates. */
    val state: OnboardingControllerState get() = mutableStates.value

    /** Re-reads durable state synchronously, clearing any process-local deferral. */
    @Synchronized
    fun refreshFromStore(): OnboardingControllerState {
        val refreshed = store.load().toControllerState()
        mutableStates.value = refreshed
        return refreshed
    }

    @Synchronized
    fun acknowledgePrivacy(): Boolean {
        val active = transitionableState() ?: return false
        if (active.privacyAcknowledged) return true
        if (active.currentStep != OnboardingStep.WELCOME) return false
        return persist(active, active.checkpoint.copy(privacyAcknowledged = true))
    }

    /**
     * Seals the separately executed pre-consent cleanup barrier. This transition is intentionally
     * available for migrated completed checkpoints and is the only durable evidence the runtime
     * may use to release private work after a process restart.
     */
    @Synchronized
    fun completePrivacyRelease(): Boolean {
        val active = state as? OnboardingControllerState.Active ?: return false
        if (!active.privacyAcknowledged) return false
        if (active.privacyReleaseCompleted) return true
        return persist(active, active.checkpoint.copy(privacyReleaseCompleted = true))
    }

    /** Records only the explicit review action; the profile values stay in encrypted planner data. */
    @Synchronized
    fun markProfileReviewed(): Boolean {
        val active = transitionableState() ?: return false
        if (active.profileReviewed) return true
        if (
            !active.privacyReleaseCompleted ||
            active.currentStep != OnboardingStep.PROFILE
        ) {
            return false
        }
        return persist(active, active.checkpoint.copy(profileReviewed = true))
    }

    @Synchronized
    fun advance(prerequisiteReady: Boolean = false): Boolean {
        val active = transitionableState() ?: return false
        if (
            !active.privacyAcknowledged ||
            !active.privacyReleaseCompleted ||
            (active.currentStep == OnboardingStep.PROFILE && !active.profileReviewed) ||
            active.completed
        ) {
            return false
        }
        if (active.currentStep != OnboardingStep.WELCOME && !prerequisiteReady) return false
        val next = active.currentStep.next() ?: return false
        return persist(
            active,
            active.checkpoint.copy(
                currentStep = next,
                furthestStep = maxOf(active.furthestStep, next),
            ),
        )
    }

    @Synchronized
    fun back(): Boolean {
        val active = transitionableState() ?: return false
        if (active.completed) return false
        val previous = active.currentStep.previous() ?: return false
        return persist(active, active.checkpoint.copy(currentStep = previous))
    }

    /**
     * Leaves the durable checkpoint exactly where it is and lets this process show the workspace.
     * Restarting the process consequently resumes setup at the same step.
     */
    @Synchronized
    fun deferSetupForSession(): Boolean {
        val active = state as? OnboardingControllerState.Active ?: return false
        if (active.completed) return false
        if (!active.setupDeferredForSession) {
            mutableStates.value = active.copy(setupDeferredForSession = true)
        }
        return true
    }

    @Synchronized
    fun resumeSetup(): Boolean {
        val active = state as? OnboardingControllerState.Active ?: return false
        if (!active.setupDeferredForSession) return true
        mutableStates.value = active.copy(setupDeferredForSession = false)
        return true
    }

    @Synchronized
    fun complete(allPrerequisitesReady: Boolean = false): Boolean {
        val active = transitionableState() ?: return false
        if (active.completed) return true
        if (
            !allPrerequisitesReady ||
            !active.privacyAcknowledged ||
            !active.privacyReleaseCompleted ||
            !active.profileReviewed ||
            active.currentStep != OnboardingStep.READY ||
            active.furthestStep != OnboardingStep.READY
        ) {
            return false
        }
        return persist(active, active.checkpoint.copy(completed = true))
    }

    /** Explicit recovery remains bound to the identity exposed by the current fail-closed state. */
    @Synchronized
    fun recoverCorruptExact(expected: OnboardingCorruptArtifactIdentity): Boolean {
        val recovery = state as? OnboardingControllerState.RecoveryRequired ?: return false
        if (recovery.artifactIdentity != expected) return false
        if (!store.resetCorruptExact(expected)) return false
        val loaded = store.load() as? OnboardingCheckpointLoadResult.Loaded ?: return false
        if (loaded.checkpoint != OnboardingCheckpoint.fresh()) return false
        mutableStates.value = OnboardingControllerState.Active(loaded.checkpoint)
        return true
    }

    private fun transitionableState(): OnboardingControllerState.Active? =
        (state as? OnboardingControllerState.Active)?.takeUnless {
            it.setupDeferredForSession
        }

    private fun persist(
        active: OnboardingControllerState.Active,
        replacement: OnboardingCheckpoint,
    ): Boolean {
        if (!replacement.isPermittedReplacementOf(active.checkpoint)) return false
        if (!store.saveIfCurrent(active.checkpoint, replacement)) return false
        mutableStates.value = OnboardingControllerState.Active(replacement)
        return true
    }
}

private fun OnboardingCheckpointLoadResult.toControllerState(): OnboardingControllerState =
    when (this) {
        is OnboardingCheckpointLoadResult.Loaded -> OnboardingControllerState.Active(checkpoint)
        is OnboardingCheckpointLoadResult.Corrupt ->
            OnboardingControllerState.RecoveryRequired(artifactIdentity)
    }
