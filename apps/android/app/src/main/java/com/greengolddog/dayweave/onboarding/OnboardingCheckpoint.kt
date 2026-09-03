package com.greengolddog.dayweave.onboarding

/** The stable order of the guided Android setup. Wire values must never be reordered. */
enum class OnboardingStep(internal val wireValue: Int) {
    WELCOME(0),
    API(1),
    GOOGLE(2),
    PROFILE(3),
    NOTIFICATIONS(4),
    FIRST_ITEM(5),
    FIRST_PLAN(6),
    READY(7),
    ;

    internal fun next(): OnboardingStep? = entries.getOrNull(ordinal + 1)

    internal fun previous(): OnboardingStep? = entries.getOrNull(ordinal - 1)

    internal companion object {
        fun fromWireValue(value: Int): OnboardingStep? = entries.firstOrNull {
            it.wireValue == value
        }
    }
}

/**
 * The complete, deliberately content-free durable onboarding schema.
 *
 * Item IDs, account IDs, URLs, secrets, user content, and integration readiness must never be
 * added here. Integration state belongs to each integration's own guarded store.
 */
data class OnboardingCheckpoint(
    val version: Int = CURRENT_VERSION,
    val currentStep: OnboardingStep = OnboardingStep.WELCOME,
    val furthestStep: OnboardingStep = OnboardingStep.WELCOME,
    val privacyAcknowledged: Boolean = false,
    /** True only after stale pre-consent OS work and notification routes were durably fenced. */
    val privacyReleaseCompleted: Boolean = false,
    /** Content-free proof that the user explicitly saved the week profile during setup. */
    val profileReviewed: Boolean = false,
    val completed: Boolean = false,
) {
    init {
        require(version == CURRENT_VERSION) { "Unsupported onboarding checkpoint version" }
        require(currentStep.ordinal <= furthestStep.ordinal) {
            "The current onboarding step cannot be beyond the furthest step"
        }
        if (!privacyAcknowledged) {
            require(currentStep == OnboardingStep.WELCOME)
            require(furthestStep == OnboardingStep.WELCOME)
            require(!privacyReleaseCompleted)
            require(!profileReviewed)
            require(!completed)
        }
        require(!privacyReleaseCompleted || privacyAcknowledged)
        if (profileReviewed) {
            require(privacyReleaseCompleted)
            require(furthestStep.ordinal >= OnboardingStep.PROFILE.ordinal)
        }
        if (completed) {
            require(privacyAcknowledged)
            require(currentStep == OnboardingStep.READY)
            require(furthestStep == OnboardingStep.READY)
        }
    }

    companion object {
        const val CURRENT_VERSION = 2

        fun fresh(): OnboardingCheckpoint = OnboardingCheckpoint()
    }
}

/** Content-free identity binding a destructive recovery decision to one corrupt artifact set. */
class OnboardingCorruptArtifactIdentity internal constructor(
    private val fingerprint: String,
) {
    override fun equals(other: Any?): Boolean =
        other is OnboardingCorruptArtifactIdentity && fingerprint == other.fingerprint

    override fun hashCode(): Int = fingerprint.hashCode()

    override fun toString(): String = "OnboardingCorruptArtifactIdentity(<redacted>)"
}

sealed interface OnboardingCheckpointLoadResult {
    /** A missing record is represented as a healthy canonical [OnboardingCheckpoint.fresh]. */
    data class Loaded(val checkpoint: OnboardingCheckpoint) : OnboardingCheckpointLoadResult

    data class Corrupt(
        val artifactIdentity: OnboardingCorruptArtifactIdentity,
    ) : OnboardingCheckpointLoadResult {
        override fun toString(): String = "Corrupt(<redacted>)"
    }
}

/** Synchronous by design so Application startup and worker gates can fail closed before work. */
interface OnboardingCheckpointStore {
    fun load(): OnboardingCheckpointLoadResult

    /** Replaces only [expected], preventing a stale process from rolling progress backward. */
    fun saveIfCurrent(
        expected: OnboardingCheckpoint,
        replacement: OnboardingCheckpoint,
    ): Boolean

    /** Atomically resets only the exact unreadable artifact set the user approved replacing. */
    fun resetCorruptExact(expected: OnboardingCorruptArtifactIdentity): Boolean
}

internal fun OnboardingCheckpoint.isPermittedReplacementOf(
    expected: OnboardingCheckpoint,
): Boolean {
    if (this == expected) return true
    if (version != expected.version) return false

    // The cleanup barrier is deliberately its own crash-durable transition. It remains legal for
    // a migrated, already-completed V1 checkpoint so startup can close that historical gap before
    // mounting private state.
    val completedPrivacyRelease =
        expected.privacyAcknowledged &&
            !expected.privacyReleaseCompleted &&
            this == expected.copy(privacyReleaseCompleted = true)
    if (completedPrivacyRelease) return true

    val reviewedProfile =
        expected.privacyReleaseCompleted &&
            !expected.profileReviewed &&
            expected.currentStep == OnboardingStep.PROFILE &&
            this == expected.copy(profileReviewed = true)
    if (reviewedProfile) return true
    if (expected.completed) return false

    val acknowledgedPrivacy =
        expected.currentStep == OnboardingStep.WELCOME &&
            expected.furthestStep == OnboardingStep.WELCOME &&
            !expected.privacyAcknowledged &&
            !expected.privacyReleaseCompleted &&
            !expected.completed &&
            this == expected.copy(privacyAcknowledged = true)
    if (acknowledgedPrivacy) return true

    val next = expected.currentStep.next()
    val advanced =
        expected.privacyAcknowledged &&
            expected.privacyReleaseCompleted &&
            (expected.currentStep != OnboardingStep.PROFILE || expected.profileReviewed) &&
            next != null &&
            this == expected.copy(
                currentStep = next,
                furthestStep = maxOf(expected.furthestStep, next),
            )
    if (advanced) return true

    val previous = expected.currentStep.previous()
    val movedBack = expected.privacyReleaseCompleted &&
        previous != null && this == expected.copy(currentStep = previous)
    if (movedBack) return true

    return expected.privacyAcknowledged &&
        expected.privacyReleaseCompleted &&
        expected.profileReviewed &&
        expected.currentStep == OnboardingStep.READY &&
        expected.furthestStep == OnboardingStep.READY &&
        this == expected.copy(completed = true)
}
