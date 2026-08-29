package com.greengolddog.dayweave

import android.content.ActivityNotFoundException
import android.content.Intent
import android.os.Bundle
import android.provider.Settings
import android.view.Window
import android.view.WindowManager
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import com.greengolddog.dayweave.security.AndroidBiometricAppUnlockAuthenticator
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AppLockIntents
import com.greengolddog.dayweave.security.AppLockNotice
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppUnlockCoordinator
import com.greengolddog.dayweave.ui.DayWeaveApp
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

class MainActivity : FragmentActivity() {
    private lateinit var appLockController: AppLockController
    private lateinit var appUnlockCoordinator: AppUnlockCoordinator
    private var backgroundLockJob: Job? = null
    private var autoPromptPending = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        appLockController = (application as DayWeaveApplication).appLockController
        appUnlockCoordinator = AppUnlockCoordinator(
            controller = appLockController,
            authenticator = AndroidBiometricAppUnlockAuthenticator(this),
            processFence = (application as DayWeaveApplication).appAuthenticationProcessFence,
        )
        AppLockWindowSecurity.apply(window, appLockController.state.value)
        lifecycleScope.launch {
            appLockController.state.collectLatest { state ->
                if (state.isLocked) {
                    (application as DayWeaveApplication).onAppPrivacyBoundaryLocked()
                }
                AppLockWindowSecurity.apply(window, state)
            }
        }
        enableEdgeToEdge()
        setContent {
            DayWeaveApp(
                appLockController = appLockController,
                onRequestUnlock = appUnlockCoordinator::requestUnlock,
                onSetAppLockEnabled = { enabled ->
                    if (enabled) {
                        AppLockWindowSecurity.forceSecure(window)
                        if (!appUnlockCoordinator.requestEnable()) {
                            AppLockWindowSecurity.apply(window, appLockController.state.value)
                        }
                    } else {
                        appUnlockCoordinator.requestDisable()
                    }
                },
                onSetAppLockTimeout = appLockController::updateTimeout,
                onLockNow = {
                    appUnlockCoordinator.cancelAuthentication()
                    appLockController.lockNow()
                },
                onOpenDeviceSecuritySettings = ::openDeviceSecuritySettings,
            )
        }
    }

    override fun onStart() {
        super.onStart()
        backgroundLockJob?.cancel()
        backgroundLockJob = null
        val stateBeforeForeground = appLockController.state.value
        appLockController.onForegrounded()
        appUnlockCoordinator.refreshAvailability()
        autoPromptPending = shouldAutoPromptOnStart(
            stateBeforeForeground = stateBeforeForeground,
            stateAfterForeground = appLockController.state.value,
        )
    }

    override fun onPostResume() {
        super.onPostResume()
        if (!autoPromptPending) return
        autoPromptPending = false
        window.decorView.post {
            if (
                lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) &&
                appLockController.state.value.isLocked
            ) {
                appUnlockCoordinator.requestUnlock()
            }
        }
    }

    override fun onStop() {
        if (!isChangingConfigurations) {
            autoPromptPending = false
            backgroundLockJob?.cancel()
            backgroundLockJob = null
            // BiometricPrompt owns foreground dismissal and credential fallback. In particular,
            // explicit cancellation is unreliable during device-credential auth on API 28. Keep
            // that request intact; a genuine departure produces the prompt's terminal callback.
            if (!appLockController.state.value.isAuthenticating) {
                backgroundLockJob = appLockController.onBackgrounded()?.let { request ->
                    lifecycleScope.launch {
                        delay(request.delayMillis)
                        appLockController.onBackgroundTimeout(request.generation)
                    }
                }
            } else {
                appLockController.onAuthenticationHostStopped()
            }
        }
        super.onStop()
    }

    private fun openDeviceSecuritySettings() {
        try {
            startActivity(AppLockIntents.enrollmentOrSecuritySettings())
        } catch (_: ActivityNotFoundException) {
            try {
                startActivity(Intent(Settings.ACTION_SECURITY_SETTINGS))
            } catch (_: ActivityNotFoundException) {
                // The fail-closed lock screen remains visible; no planner data is changed.
            }
        }
    }
}

/**
 * Arms the one-shot convenience prompt only for an idle cold/timeout/recovery lock.
 *
 * A device-credential surface can stop the Activity. If that attempt was active when the Activity
 * returned, or its terminal cancellation/error arrived while stopped, the lock screen must remain
 * stable instead of immediately opening a replacement prompt from the posted resume callback.
 */
internal fun shouldAutoPromptOnStart(
    stateBeforeForeground: AppLockState,
    stateAfterForeground: AppLockState,
): Boolean = stateAfterForeground.isLocked &&
    !stateAfterForeground.isAuthenticationBusy &&
    !stateBeforeForeground.isAuthenticationBusy &&
    stateBeforeForeground.notice !in AUTO_PROMPT_SUPPRESSING_NOTICES

private val AUTO_PROMPT_SUPPRESSING_NOTICES = setOf(
    AppLockNotice.AUTHENTICATION_CANCELLED,
    AppLockNotice.AUTHENTICATION_LOCKED_OUT,
    AppLockNotice.AUTHENTICATION_ERROR,
)

/** One production seam for both the state predicate and the actual Window mutation. */
internal object AppLockWindowSecurity {
    fun apply(window: Window, state: AppLockState) {
        if (state.settings.enabled || state.isAuthenticationBusy) {
            forceSecure(window)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }

    fun forceSecure(window: Window) {
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }
}
