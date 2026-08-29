package com.greengolddog.dayweave.security

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

fun interface MonotonicClock {
    fun nowMillis(): Long
}

enum class AppUnlockAvailability {
    UNKNOWN,
    AVAILABLE,
    NOT_ENROLLED,
    TEMPORARILY_UNAVAILABLE,
    UNAVAILABLE,
}

enum class AppLockAuthenticationPurpose {
    UNLOCK,
    ENABLE,
    DISABLE,
}

enum class AppLockAuthenticationOutcome {
    SUCCESS,
    CANCELLED,
    LOCKED_OUT,
    ERROR,
}

enum class AppLockNotice {
    SETTINGS_RECOVERY_REQUIRED,
    SETTINGS_SAVE_FAILED,
    AUTHENTICATION_CANCELLED,
    AUTHENTICATION_LOCKED_OUT,
    AUTHENTICATION_ERROR,
}

data class AppLockState(
    val settings: AppLockSettings,
    val isLocked: Boolean,
    val isAuthenticating: Boolean = false,
    val isAwaitingForegroundAuthenticationCompletion: Boolean = false,
    val availability: AppUnlockAvailability = AppUnlockAvailability.UNKNOWN,
    val settingsHealthy: Boolean = true,
    val notice: AppLockNotice? = null,
    val pendingAuthenticationPurpose: AppLockAuthenticationPurpose? = null,
    val pendingAuthenticationRequestId: Long? = null,
) {
    val isAuthenticationBusy: Boolean
        get() = isAuthenticating || isAwaitingForegroundAuthenticationCompletion
}

data class BackgroundLockRequest(
    val generation: Long,
    val delayMillis: Long,
)

/**
 * Process-wide presentation lock state machine.
 *
 * Every authentication completion and background timer is generation-bound, so a late biometric
 * callback or cancelled timeout cannot reopen or relock a newer session. Corrupt settings recover
 * only after device authentication and a successful durable rewrite; planner data is never reset.
 */
class AppLockController(
    private val settingsStore: AppLockSettingsStore,
    private val clock: MonotonicClock,
) {
    private val stateLock = Any()
    private var authenticationGeneration = 0L
    private var backgroundGeneration = 0L
    private var backgroundedAtMillis: Long? = null
    private var hostInForeground = false
    private var deferredAuthenticationSuccess: DeferredAuthenticationSuccess? = null

    private val mutableState = MutableStateFlow(loadInitialState())
    val state: StateFlow<AppLockState> = mutableState.asStateFlow()

    fun updateAvailability(availability: AppUnlockAvailability) = synchronized(stateLock) {
        val current = mutableState.value
        mutableState.value = current.copy(
            availability = availability,
            notice = if (
                availability == AppUnlockAvailability.AVAILABLE &&
                current.notice in AUTHENTICATION_NOTICES
            ) {
                null
            } else {
                current.notice
            },
        )
    }

    fun beginAuthentication(purpose: AppLockAuthenticationPurpose): Long? =
        synchronized(stateLock) {
            val current = mutableState.value
            val validPurpose = when (purpose) {
                AppLockAuthenticationPurpose.UNLOCK ->
                    current.settings.enabled && current.isLocked
                AppLockAuthenticationPurpose.ENABLE ->
                    !current.settings.enabled && !current.isLocked && current.settingsHealthy
                AppLockAuthenticationPurpose.DISABLE ->
                    current.settings.enabled && !current.isLocked && current.settingsHealthy
            }
            if (
                !validPurpose || current.isAuthenticationBusy ||
                current.availability != AppUnlockAvailability.AVAILABLE
            ) {
                return@synchronized null
            }

            authenticationGeneration = authenticationGeneration.nextGeneration()
            mutableState.value = current.copy(
                isAuthenticating = true,
                notice = null,
                pendingAuthenticationPurpose = purpose,
                pendingAuthenticationRequestId = authenticationGeneration,
            )
            authenticationGeneration
        }

    fun completeAuthentication(
        requestId: Long,
        outcome: AppLockAuthenticationOutcome,
    ): Boolean = synchronized(stateLock) {
        val current = mutableState.value
        if (
            !current.isAuthenticating ||
            current.pendingAuthenticationRequestId != requestId ||
            current.pendingAuthenticationPurpose == null
        ) {
            return@synchronized false
        }

        val settled = current.copy(
            isAuthenticating = false,
            pendingAuthenticationPurpose = null,
            pendingAuthenticationRequestId = null,
        )
        if (outcome != AppLockAuthenticationOutcome.SUCCESS) {
            mutableState.value = settled.copy(
                isLocked = settled.isLocked || (!hostInForeground && settled.settings.enabled),
                notice = when (outcome) {
                    AppLockAuthenticationOutcome.CANCELLED ->
                        AppLockNotice.AUTHENTICATION_CANCELLED
                    AppLockAuthenticationOutcome.LOCKED_OUT ->
                        AppLockNotice.AUTHENTICATION_LOCKED_OUT
                    AppLockAuthenticationOutcome.ERROR ->
                        AppLockNotice.AUTHENTICATION_ERROR
                    AppLockAuthenticationOutcome.SUCCESS -> null
                },
            )
            return@synchronized true
        }

        if (!hostInForeground) {
            deferredAuthenticationSuccess = DeferredAuthenticationSuccess(
                purpose = current.pendingAuthenticationPurpose,
                completedAtMillis = clock.nowMillis(),
            )
            mutableState.value = settled.copy(
                isLocked = if (
                    current.pendingAuthenticationPurpose == AppLockAuthenticationPurpose.UNLOCK
                ) {
                    true
                } else {
                    settled.isLocked
                },
                isAwaitingForegroundAuthenticationCompletion = true,
                notice = null,
            )
            return@synchronized true
        }
        applyAuthenticationSuccess(current.pendingAuthenticationPurpose, settled)
        true
    }

    fun updateTimeout(timeout: AppLockTimeout): Boolean = synchronized(stateLock) {
        val current = mutableState.value
        if (current.isLocked || current.isAuthenticationBusy || !current.settingsHealthy) {
            return@synchronized false
        }
        if (current.settings.timeout == timeout) return@synchronized true
        val updated = current.settings.copy(timeout = timeout)
        if (!persist(updated)) {
            mutableState.value = current.copy(notice = AppLockNotice.SETTINGS_SAVE_FAILED)
            return@synchronized false
        }
        mutableState.value = current.copy(settings = updated, notice = null)
        true
    }

    fun lockNow() = synchronized(stateLock) {
        val current = mutableState.value
        if (!current.settings.enabled) return@synchronized
        authenticationGeneration = authenticationGeneration.nextGeneration()
        backgroundGeneration = backgroundGeneration.nextGeneration()
        backgroundedAtMillis = null
        deferredAuthenticationSuccess = null
        mutableState.value = current.copy(
            isLocked = true,
            isAuthenticating = false,
            isAwaitingForegroundAuthenticationCompletion = false,
            notice = null,
            pendingAuthenticationPurpose = null,
            pendingAuthenticationRequestId = null,
        )
    }

    /** Records a real app background transition and returns a generation-bound timeout. */
    fun onBackgrounded(): BackgroundLockRequest? = synchronized(stateLock) {
        val current = mutableState.value
        hostInForeground = false
        authenticationGeneration = authenticationGeneration.nextGeneration()
        deferredAuthenticationSuccess = null
        if (current.isAuthenticationBusy) {
            mutableState.value = current.copy(
                isAuthenticating = false,
                isAwaitingForegroundAuthenticationCompletion = false,
                pendingAuthenticationPurpose = null,
                pendingAuthenticationRequestId = null,
            )
        }
        if (!current.settings.enabled || current.isLocked) {
            backgroundedAtMillis = null
            return@synchronized null
        }

        backgroundGeneration = backgroundGeneration.nextGeneration()
        backgroundedAtMillis = clock.nowMillis()
        val timeout = current.settings.timeout.durationMillis
        if (timeout == 0L) {
            mutableState.value = mutableState.value.copy(isLocked = true, notice = null)
            return@synchronized null
        }
        BackgroundLockRequest(backgroundGeneration, timeout)
    }

    /**
     * Records an Activity stop owned by the system authentication surface.
     *
     * The prompt remains authoritative across device-credential fallback. A success received while
     * stopped is never applied in the background; it gets a short, one-shot foreground handoff.
     */
    fun onAuthenticationHostStopped() = synchronized(stateLock) {
        hostInForeground = false
        val current = mutableState.value
        if (current.settings.enabled && !current.isLocked) {
            // The system credential surface can stop the host Activity without immediately
            // delivering a terminal callback. Hide the unlocked composition at once, but retain
            // the exact pending attempt so only its eventual success may unlock or disable.
            mutableState.value = current.copy(isLocked = true, notice = null)
        }
    }

    /** Invalidates an outstanding timer and applies elapsed background time before content draws. */
    fun onForegrounded() = synchronized(stateLock) {
        hostInForeground = true
        backgroundGeneration = backgroundGeneration.nextGeneration()
        deferredAuthenticationSuccess?.let { deferred ->
            deferredAuthenticationSuccess = null
            backgroundedAtMillis = null
            val current = mutableState.value.copy(
                isAwaitingForegroundAuthenticationCompletion = false,
            )
            if (
                withinAuthenticationReturnGrace(
                    completedAt = deferred.completedAtMillis,
                    now = clock.nowMillis(),
                )
            ) {
                applyAuthenticationSuccess(deferred.purpose, current)
            } else {
                mutableState.value = current.copy(
                    isLocked = current.settings.enabled,
                    notice = AppLockNotice.AUTHENTICATION_ERROR,
                )
            }
            return@synchronized
        }

        val current = mutableState.value
        val backgroundedAt = backgroundedAtMillis
        backgroundedAtMillis = null
        if (
            current.settings.enabled && !current.isLocked && backgroundedAt != null &&
            timeoutElapsed(
                startedAt = backgroundedAt,
                now = clock.nowMillis(),
                timeout = current.settings.timeout.durationMillis,
            )
        ) {
            mutableState.value = current.copy(isLocked = true, notice = null)
        }
    }

    fun onBackgroundTimeout(generation: Long): Boolean = synchronized(stateLock) {
        val current = mutableState.value
        val backgroundedAt = backgroundedAtMillis
        if (
            generation != backgroundGeneration || backgroundedAt == null ||
            !current.settings.enabled || current.isLocked ||
            !timeoutElapsed(
                startedAt = backgroundedAt,
                now = clock.nowMillis(),
                timeout = current.settings.timeout.durationMillis,
            )
        ) {
            return@synchronized false
        }
        mutableState.value = current.copy(isLocked = true, notice = null)
        true
    }

    private fun completeUnlock(settled: AppLockState) {
        if (settled.settingsHealthy) {
            mutableState.value = settled.copy(isLocked = false, notice = null)
            return
        }

        val recoveredSettings = settled.settings.copy(enabled = true)
        if (persist(recoveredSettings)) {
            mutableState.value = settled.copy(
                settings = recoveredSettings,
                settingsHealthy = true,
                isLocked = false,
                notice = null,
            )
        } else {
            mutableState.value = settled.copy(
                isLocked = true,
                notice = AppLockNotice.SETTINGS_SAVE_FAILED,
            )
        }
    }

    private fun completeEnable(settled: AppLockState) {
        val enabledSettings = settled.settings.copy(enabled = true)
        if (persist(enabledSettings)) {
            mutableState.value = settled.copy(
                settings = enabledSettings,
                isLocked = false,
                notice = null,
            )
        } else {
            mutableState.value = settled.copy(
                isLocked = false,
                notice = AppLockNotice.SETTINGS_SAVE_FAILED,
            )
        }
    }

    private fun completeDisable(settled: AppLockState) {
        val disabledSettings = settled.settings.copy(enabled = false)
        if (persist(disabledSettings)) {
            backgroundedAtMillis = null
            backgroundGeneration = backgroundGeneration.nextGeneration()
            mutableState.value = settled.copy(
                settings = disabledSettings,
                isLocked = false,
                notice = null,
            )
        } else {
            mutableState.value = settled.copy(
                isLocked = false,
                notice = AppLockNotice.SETTINGS_SAVE_FAILED,
            )
        }
    }

    private fun applyAuthenticationSuccess(
        purpose: AppLockAuthenticationPurpose,
        settled: AppLockState,
    ) {
        when (purpose) {
            AppLockAuthenticationPurpose.UNLOCK -> completeUnlock(settled)
            AppLockAuthenticationPurpose.ENABLE -> completeEnable(settled)
            AppLockAuthenticationPurpose.DISABLE -> completeDisable(settled)
        }
    }

    private fun persist(settings: AppLockSettings): Boolean =
        runCatching { settingsStore.save(settings) }.getOrDefault(false)

    private fun loadInitialState(): AppLockState = when (
        val result = runCatching { settingsStore.load() }
            .getOrDefault(AppLockSettingsLoadResult.Corrupt)
    ) {
        is AppLockSettingsLoadResult.Loaded -> AppLockState(
            settings = result.settings,
            isLocked = result.settings.enabled,
        )
        AppLockSettingsLoadResult.Corrupt -> AppLockState(
            settings = AppLockSettings(enabled = true),
            isLocked = true,
            settingsHealthy = false,
            notice = AppLockNotice.SETTINGS_RECOVERY_REQUIRED,
        )
    }

    private companion object {
        const val AUTHENTICATION_RETURN_GRACE_MILLIS = 10_000L

        val AUTHENTICATION_NOTICES = setOf(
            AppLockNotice.AUTHENTICATION_CANCELLED,
            AppLockNotice.AUTHENTICATION_LOCKED_OUT,
            AppLockNotice.AUTHENTICATION_ERROR,
        )

        fun Long.nextGeneration(): Long = if (this == Long.MAX_VALUE) 1L else this + 1L

        fun timeoutElapsed(startedAt: Long, now: Long, timeout: Long): Boolean =
            now < startedAt || now - startedAt >= timeout

        fun withinAuthenticationReturnGrace(completedAt: Long, now: Long): Boolean =
            now >= completedAt && now - completedAt <= AUTHENTICATION_RETURN_GRACE_MILLIS
    }

    private data class DeferredAuthenticationSuccess(
        val purpose: AppLockAuthenticationPurpose,
        val completedAtMillis: Long,
    )
}
