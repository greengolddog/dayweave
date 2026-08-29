package com.greengolddog.dayweave.security

import android.content.Intent
import android.os.Build
import android.provider.Settings
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

data class AppLockAuthenticationAttempt(
    val processAttemptId: Long,
    val controllerRequestId: Long,
    val purpose: AppLockAuthenticationPurpose,
)

@JvmInline
value class AppAuthenticationOwnerToken internal constructor(internal val value: Long)

interface AppUnlockAuthenticator {
    fun availability(): AppUnlockAvailability

    /** Starts a new system-owned authentication surface for this immutable attempt. */
    fun authenticate(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    )

    /** Rebinds the exact retained attempt after Activity recreation without launching a prompt. */
    fun reconnect(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    )

    /** Requests cancellation but keeps the attempt bound until its terminal callback drains. */
    fun cancel(attempt: AppLockAuthenticationAttempt)
}

/**
 * AndroidX BiometricPrompt adapter. No biometric, PIN, pattern, or password value enters DayWeave.
 *
 * Each callback closes over one immutable attempt. Reusing an Activity for a later prompt therefore
 * cannot retag a delayed callback from an earlier platform operation.
 */
class AndroidBiometricAppUnlockAuthenticator(
    private val activity: FragmentActivity,
) : AppUnlockAuthenticator {
    private val biometricManager = BiometricManager.from(activity)
    private var activeBinding: PromptBinding? = null

    override fun availability(): AppUnlockAvailability = availabilityForResult(
        biometricManager.canAuthenticate(ALLOWED_AUTHENTICATORS),
    )

    override fun authenticate(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ) {
        val prompt = createPrompt(attempt, onResult)
        activeBinding = PromptBinding(attempt, prompt)
        prompt.authenticate(promptInfo(attempt.purpose))
    }

    override fun reconnect(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ) {
        // Constructing BiometricPrompt with the recreated FragmentActivity reconnects to the
        // retained AndroidX fragment. Calling authenticate here would launch a competing prompt.
        val prompt = createPrompt(attempt, onResult)
        activeBinding = PromptBinding(attempt, prompt)
    }

    override fun cancel(attempt: AppLockAuthenticationAttempt) {
        activeBinding
            ?.takeIf { it.attempt == attempt }
            ?.prompt
            ?.cancelAuthentication()
    }

    private fun createPrompt(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ): BiometricPrompt = BiometricPrompt(
        activity,
        ContextCompat.getMainExecutor(activity),
        object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                dispatch(attempt, AppLockAuthenticationOutcome.SUCCESS, onResult)
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                dispatch(attempt, authenticationOutcomeForError(errorCode), onResult)
            }

            override fun onAuthenticationFailed() {
                // The system prompt remains open and announces the failed attempt itself.
            }
        },
    )

    private fun dispatch(
        attempt: AppLockAuthenticationAttempt,
        outcome: AppLockAuthenticationOutcome,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ) {
        if (activeBinding?.attempt == attempt) activeBinding = null
        onResult(attempt, outcome)
    }

    private data class PromptBinding(
        val attempt: AppLockAuthenticationAttempt,
        val prompt: BiometricPrompt,
    )

    internal companion object {
        val ALLOWED_AUTHENTICATORS: Int =
            BiometricManager.Authenticators.BIOMETRIC_WEAK or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL

        private fun promptInfo(purpose: AppLockAuthenticationPurpose) =
            BiometricPrompt.PromptInfo.Builder()
                .setTitle(
                    when (purpose) {
                        AppLockAuthenticationPurpose.UNLOCK -> "Unlock DayWeave"
                        AppLockAuthenticationPurpose.ENABLE -> "Turn on app lock"
                        AppLockAuthenticationPurpose.DISABLE -> "Turn off app lock"
                    },
                )
                .setSubtitle("Use your device screen lock or biometrics")
                .setAllowedAuthenticators(ALLOWED_AUTHENTICATORS)
                .build()

        fun availabilityForResult(result: Int): AppUnlockAvailability = when (result) {
            BiometricManager.BIOMETRIC_SUCCESS -> AppUnlockAvailability.AVAILABLE
            BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED -> AppUnlockAvailability.NOT_ENROLLED
            BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE,
            BiometricManager.BIOMETRIC_STATUS_UNKNOWN,
            -> AppUnlockAvailability.TEMPORARILY_UNAVAILABLE
            BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE,
            BiometricManager.BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED,
            BiometricManager.BIOMETRIC_ERROR_UNSUPPORTED,
            -> AppUnlockAvailability.UNAVAILABLE
            else -> AppUnlockAvailability.UNAVAILABLE
        }

        fun authenticationOutcomeForError(errorCode: Int): AppLockAuthenticationOutcome =
            when (errorCode) {
                BiometricPrompt.ERROR_USER_CANCELED,
                BiometricPrompt.ERROR_NEGATIVE_BUTTON,
                BiometricPrompt.ERROR_CANCELED,
                -> AppLockAuthenticationOutcome.CANCELLED
                BiometricPrompt.ERROR_LOCKOUT,
                BiometricPrompt.ERROR_LOCKOUT_PERMANENT,
                -> AppLockAuthenticationOutcome.LOCKED_OUT
                else -> AppLockAuthenticationOutcome.ERROR
            }
    }
}

/**
 * Process-wide single-flight fence for the platform authentication surface.
 *
 * Cancellation does not release the slot: the exact platform attempt must deliver a terminal
 * callback first. Activity recreation transfers ownership of that same attempt. A stale owner may
 * neither cancel the transferred prompt nor relabel an old callback as a newer attempt.
 */
class AppAuthenticationProcessFence {
    private val fenceLock = Any()
    private var ownerGeneration = 0L
    private var attemptGeneration = 0L
    private var activeAttempt: ActiveAttempt? = null

    fun newOwnerToken(): AppAuthenticationOwnerToken = synchronized(fenceLock) {
        ownerGeneration = ownerGeneration.nextGeneration()
        AppAuthenticationOwnerToken(ownerGeneration)
    }

    fun begin(
        owner: AppAuthenticationOwnerToken,
        purpose: AppLockAuthenticationPurpose,
        authenticator: AppUnlockAuthenticator,
        beginControllerAttempt: () -> Long?,
        onTerminal: (Long, AppLockAuthenticationOutcome) -> Unit,
    ): Boolean {
        val attempt = synchronized(fenceLock) {
            if (activeAttempt != null) return false
            val controllerRequestId = beginControllerAttempt() ?: return false
            attemptGeneration = attemptGeneration.nextGeneration()
            AppLockAuthenticationAttempt(
                processAttemptId = attemptGeneration,
                controllerRequestId = controllerRequestId,
                purpose = purpose,
            ).also { created ->
                activeAttempt = ActiveAttempt(
                    attempt = created,
                    owner = owner,
                    authenticator = authenticator,
                    onTerminal = onTerminal,
                )
            }
        }

        return runCatching {
            authenticator.authenticate(attempt, ::receiveTerminal)
            cancelAfterStartIfRequested(attempt)
            true
        }.getOrElse {
            receiveTerminal(attempt, AppLockAuthenticationOutcome.ERROR)
            false
        }
    }

    fun reconnect(
        owner: AppAuthenticationOwnerToken,
        expectedControllerRequestId: Long?,
        expectedPurpose: AppLockAuthenticationPurpose?,
        authenticator: AppUnlockAuthenticator,
        onTerminal: (Long, AppLockAuthenticationOutcome) -> Unit,
    ): Boolean {
        val binding = synchronized(fenceLock) {
            val active = activeAttempt ?: return false
            if (
                expectedControllerRequestId != null &&
                (
                    active.attempt.controllerRequestId != expectedControllerRequestId ||
                        active.attempt.purpose != expectedPurpose
                )
            ) {
                return false
            }
            active.copy(
                owner = owner,
                authenticator = authenticator,
                onTerminal = onTerminal,
            ).also { activeAttempt = it }
        }

        return runCatching {
            authenticator.reconnect(binding.attempt, ::receiveTerminal)
            cancelAfterStartIfRequested(binding.attempt)
            true
        }.getOrElse {
            false
        }
    }

    fun cancel(owner: AppAuthenticationOwnerToken): Boolean {
        val binding = synchronized(fenceLock) {
            val active = activeAttempt ?: return false
            if (active.owner != owner) return false
            active.copy(cancellationRequested = true).also { activeAttempt = it }
        }
        return runCatching {
            binding.authenticator.cancel(binding.attempt)
            true
        }.getOrDefault(false)
    }

    internal fun hasActiveAttempt(): Boolean = synchronized(fenceLock) {
        activeAttempt != null
    }

    private fun cancelAfterStartIfRequested(attempt: AppLockAuthenticationAttempt) {
        val binding = synchronized(fenceLock) {
            activeAttempt?.takeIf {
                it.attempt == attempt && it.cancellationRequested
            }
        } ?: return
        // A failed cancellation request is not a terminal platform result. Keep the process slot
        // occupied until the immutable attempt's real callback drains (or the process restarts).
        runCatching { binding.authenticator.cancel(binding.attempt) }
    }

    private fun receiveTerminal(
        attempt: AppLockAuthenticationAttempt,
        outcome: AppLockAuthenticationOutcome,
    ) {
        val terminal = synchronized(fenceLock) {
            val active = activeAttempt ?: return
            if (active.attempt != attempt) return
            activeAttempt = null
            active
        }
        terminal.onTerminal(attempt.controllerRequestId, outcome)
    }

    private data class ActiveAttempt(
        val attempt: AppLockAuthenticationAttempt,
        val owner: AppAuthenticationOwnerToken,
        val authenticator: AppUnlockAuthenticator,
        val onTerminal: (Long, AppLockAuthenticationOutcome) -> Unit,
        val cancellationRequested: Boolean = false,
    )

    private companion object {
        fun Long.nextGeneration(): Long = if (this == Long.MAX_VALUE) 1L else this + 1L
    }
}

/** Coordinates the controller with the application-scoped platform prompt fence. */
class AppUnlockCoordinator(
    private val controller: AppLockController,
    private val authenticator: AppUnlockAuthenticator,
    private val processFence: AppAuthenticationProcessFence = AppAuthenticationProcessFence(),
) {
    private val owner = processFence.newOwnerToken()

    init {
        val pending = controller.state.value
        val requestId = pending.pendingAuthenticationRequestId
        val purpose = pending.pendingAuthenticationPurpose
        val reconnected = processFence.reconnect(
            owner = owner,
            expectedControllerRequestId = requestId,
            expectedPurpose = purpose,
            authenticator = authenticator,
            onTerminal = { terminalRequestId, outcome ->
                controller.completeAuthentication(terminalRequestId, outcome)
            },
        )
        if (
            requestId != null && purpose != null && !reconnected &&
            !processFence.hasActiveAttempt()
        ) {
            controller.completeAuthentication(requestId, AppLockAuthenticationOutcome.ERROR)
        }
    }

    fun refreshAvailability(): AppUnlockAvailability {
        val availability = runCatching { authenticator.availability() }
            .getOrDefault(AppUnlockAvailability.TEMPORARILY_UNAVAILABLE)
        controller.updateAvailability(availability)
        return availability
    }

    fun requestUnlock(): Boolean = request(AppLockAuthenticationPurpose.UNLOCK)

    fun requestEnable(): Boolean = request(AppLockAuthenticationPurpose.ENABLE)

    fun requestDisable(): Boolean = request(AppLockAuthenticationPurpose.DISABLE)

    fun cancelAuthentication(): Boolean = processFence.cancel(owner)

    private fun request(purpose: AppLockAuthenticationPurpose): Boolean {
        refreshAvailability()
        return processFence.begin(
            owner = owner,
            purpose = purpose,
            authenticator = authenticator,
            beginControllerAttempt = { controller.beginAuthentication(purpose) },
            onTerminal = { requestId, outcome ->
                controller.completeAuthentication(requestId, outcome)
            },
        )
    }
}

object AppLockIntents {
    fun enrollmentOrSecuritySettings(): Intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        Intent(Settings.ACTION_BIOMETRIC_ENROLL).putExtra(
            Settings.EXTRA_BIOMETRIC_AUTHENTICATORS_ALLOWED,
            AndroidBiometricAppUnlockAuthenticator.ALLOWED_AUTHENTICATORS,
        )
    } else {
        Intent(Settings.ACTION_SECURITY_SETTINGS)
    }
}
