package com.greengolddog.dayweave.onboarding

import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Process-local projection of the durable onboarding privacy decision.
 *
 * The checkpoint store remains the authority for consent. This type deliberately receives only
 * the resulting boolean, so credentials, planner content, provider identifiers, and readiness
 * evidence cannot accidentally cross this boundary.
 */
data class OnboardingRuntimePrivacyState(
    val privacyAcknowledged: Boolean,
    val appUnlocked: Boolean,
    val activityStarted: Boolean,
) {
    /** Background jobs may touch private state only after durable privacy acknowledgement. */
    val backgroundWorkAllowed: Boolean
        get() = privacyAcknowledged

    /** Private UI is never mounted beneath either an unacknowledged or locked presentation. */
    val privatePresentationAllowed: Boolean
        get() = privacyAcknowledged && appUnlocked && activityStarted

    /** Provider and AI work has the strictest visibility boundary. */
    val foregroundProviderWorkAllowed: Boolean
        get() = privatePresentationAllowed
}

class OnboardingRuntimeGate(
    privacyAcknowledged: Boolean,
) {
    private val mutableState = MutableStateFlow(
        OnboardingRuntimePrivacyState(
            privacyAcknowledged = privacyAcknowledged,
            appUnlocked = false,
            activityStarted = false,
        ),
    )

    val state: StateFlow<OnboardingRuntimePrivacyState> = mutableState.asStateFlow()

    fun setDurablePrivacyAcknowledgement(acknowledged: Boolean) {
        mutableState.update { current ->
            current.copy(privacyAcknowledged = acknowledged)
        }
    }

    fun setAppUnlocked(unlocked: Boolean) {
        mutableState.update { current -> current.copy(appUnlocked = unlocked) }
    }

    fun setActivityStarted(started: Boolean) {
        mutableState.update { current -> current.copy(activityStarted = started) }
    }

    fun backgroundWorkAllowed(): Boolean = state.value.backgroundWorkAllowed

    fun privatePresentationAllowed(): Boolean = state.value.privatePresentationAllowed

    fun foregroundProviderWorkAllowed(): Boolean = state.value.foregroundProviderWorkAllowed
}

/** Launches the consent-dependent application bootstrap at most once in a process. */
class OnboardingConsentBootstrap(
    private val launch: () -> Unit,
) {
    private val launched = AtomicBoolean(false)

    fun launchIfAllowed(gate: OnboardingRuntimeGate): Boolean {
        if (!gate.backgroundWorkAllowed() || !launched.compareAndSet(false, true)) return false
        return try {
            launch()
            true
        } catch (error: Throwable) {
            launched.set(false)
            throw error
        }
    }
}
