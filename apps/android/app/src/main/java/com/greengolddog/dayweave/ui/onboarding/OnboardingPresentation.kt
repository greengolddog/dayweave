package com.greengolddog.dayweave.ui.onboarding

import com.greengolddog.dayweave.onboarding.OnboardingStep

/**
 * A content-free projection of authoritative setup evidence for the onboarding UI.
 *
 * The shell deliberately accepts no free-form status detail. Account names, resource identifiers,
 * planner content, credentials, and provider errors therefore cannot be retained by this model or
 * copied into accessibility semantics. The owning coordinator must derive these values live.
 */
enum class OnboardingCheckState {
    PENDING,
    WORKING,
    READY,
    NEEDS_ATTENTION,
}

data class OnboardingReadiness(
    val api: OnboardingCheckState = OnboardingCheckState.PENDING,
    val google: OnboardingCheckState = OnboardingCheckState.PENDING,
    val profile: OnboardingCheckState = OnboardingCheckState.PENDING,
    val notifications: OnboardingCheckState = OnboardingCheckState.READY,
    val firstItem: OnboardingCheckState = OnboardingCheckState.PENDING,
    val firstPlan: OnboardingCheckState = OnboardingCheckState.PENDING,
) {
    fun checkFor(step: OnboardingStep): OnboardingCheckState? = when (step) {
        OnboardingStep.WELCOME,
        OnboardingStep.READY,
        -> null
        OnboardingStep.API -> api
        OnboardingStep.GOOGLE -> google
        OnboardingStep.PROFILE -> profile
        OnboardingStep.NOTIFICATIONS -> notifications
        OnboardingStep.FIRST_ITEM -> firstItem
        OnboardingStep.FIRST_PLAN -> firstPlan
    }
    val allReady: Boolean
        get() = api == OnboardingCheckState.READY &&
            google == OnboardingCheckState.READY &&
            profile == OnboardingCheckState.READY &&
            notifications == OnboardingCheckState.READY &&
            firstItem == OnboardingCheckState.READY &&
            firstPlan == OnboardingCheckState.READY

    companion object {
        val Ready = OnboardingReadiness(
            api = OnboardingCheckState.READY,
            google = OnboardingCheckState.READY,
            profile = OnboardingCheckState.READY,
            notifications = OnboardingCheckState.READY,
            firstItem = OnboardingCheckState.READY,
            firstPlan = OnboardingCheckState.READY,
        )
    }
}

/** A content-free presentation of a fail-closed onboarding checkpoint load. */
enum class OnboardingRecoveryUiState {
    NONE,
    CORRUPT,
    UNSUPPORTED_FUTURE_VERSION,
}

data class OnboardingUiState(
    val step: OnboardingStep = OnboardingStep.WELCOME,
    val privacyAcknowledged: Boolean = false,
    val readiness: OnboardingReadiness = OnboardingReadiness(),
    val recovery: OnboardingRecoveryUiState = OnboardingRecoveryUiState.NONE,
) {
    /** Recovery and unacknowledged states always replace the requested page with opaque privacy UI. */
    val presentedStep: OnboardingStep
        get() = if (privacyBoundaryOpen) step else OnboardingStep.WELCOME

    val privacyBoundaryOpen: Boolean
        get() = privacyAcknowledged && recovery == OnboardingRecoveryUiState.NONE

    val canContinue: Boolean
        get() {
            if (recovery != OnboardingRecoveryUiState.NONE) return false
            if (!privacyAcknowledged) return false
            return when (step) {
                OnboardingStep.WELCOME -> true
                OnboardingStep.READY -> readiness.allReady
                else -> readiness.checkFor(step) == OnboardingCheckState.READY
            }
        }

    val canGoBack: Boolean
        get() = privacyBoundaryOpen && step != OnboardingStep.WELCOME
}

data class OnboardingCallbacks(
    val onPrivacyAcknowledgementChanged: (Boolean) -> Unit = {},
    val onConnectThisPhone: () -> Unit = {},
    val onChooseGoogleResources: () -> Unit = {},
    val onReviewWeekProfile: () -> Unit = {},
    val onOpenNotificationSettings: () -> Unit = {},
    val onCreateFirstItem: () -> Unit = {},
    val onComposeFirstPlan: () -> Unit = {},
    val onBack: () -> Unit = {},
    val onSetUpLater: () -> Unit = {},
    val onContinue: () -> Unit = {},
    val onFinish: () -> Unit = {},
    val onResetProgressAfterWarning: () -> Unit = {},
)

object OnboardingTestTags {
    const val ROOT = "onboarding_root"
    const val OPAQUE_PRIVACY = "onboarding_opaque_privacy"
    const val PRIVACY_CHECKBOX = "onboarding_privacy_checkbox"
    const val PRIMARY_ACTION = "onboarding_primary_action"
    const val BACK = "onboarding_back"
    const val SET_UP_LATER = "onboarding_set_up_later"
    const val CONTINUE = "onboarding_continue"
    const val FINISH = "onboarding_finish"
    const val RECOVERY_WARNING = "onboarding_recovery_warning"
    const val RECOVERY_RESET = "onboarding_recovery_reset"
    const val RECOVERY_CONFIRM = "onboarding_recovery_confirm"
    const val READINESS = "onboarding_readiness"
    const val READY_CHECKLIST = "onboarding_ready_checklist"

    fun page(step: OnboardingStep): String = "onboarding_page_${step.tagValue}"
}

internal val onboardingSteps: List<OnboardingStep> = listOf(
    OnboardingStep.WELCOME,
    OnboardingStep.API,
    OnboardingStep.GOOGLE,
    OnboardingStep.PROFILE,
    OnboardingStep.NOTIFICATIONS,
    OnboardingStep.FIRST_ITEM,
    OnboardingStep.FIRST_PLAN,
    OnboardingStep.READY,
)

internal val OnboardingStep.ordinalInFlow: Int
    get() = onboardingSteps.indexOf(this).coerceAtLeast(0)

internal val OnboardingStep.tagValue: String
    get() = when (this) {
        OnboardingStep.WELCOME -> "welcome"
        OnboardingStep.API -> "api"
        OnboardingStep.GOOGLE -> "google"
        OnboardingStep.PROFILE -> "profile"
        OnboardingStep.NOTIFICATIONS -> "notifications"
        OnboardingStep.FIRST_ITEM -> "first_item"
        OnboardingStep.FIRST_PLAN -> "first_plan"
        OnboardingStep.READY -> "ready"
    }

internal val OnboardingStep.title: String
    get() = when (this) {
        OnboardingStep.WELCOME -> "Welcome & privacy"
        OnboardingStep.API -> "Connect this phone"
        OnboardingStep.GOOGLE -> "Google resources"
        OnboardingStep.PROFILE -> "Your week"
        OnboardingStep.NOTIFICATIONS -> "Notifications"
        OnboardingStep.FIRST_ITEM -> "First real item"
        OnboardingStep.FIRST_PLAN -> "First exact plan"
        OnboardingStep.READY -> "Ready"
    }
