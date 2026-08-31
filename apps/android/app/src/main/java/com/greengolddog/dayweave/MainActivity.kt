package com.greengolddog.dayweave

import android.content.ActivityNotFoundException
import android.app.NotificationManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.view.Window
import android.view.WindowManager
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.core.app.NotificationManagerCompat
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import com.greengolddog.dayweave.notifications.TimedBreakNotificationRouteMailbox
import com.greengolddog.dayweave.notifications.TimedBreakNotificationSystemState
import com.greengolddog.dayweave.notifications.TimedBreakReminderEnableAction
import com.greengolddog.dayweave.notifications.TIMED_BREAK_NOTIFICATION_CHANNEL_ID
import com.greengolddog.dayweave.notifications.timedBreakReminderSystemEnableAction
import com.greengolddog.dayweave.notifications.admitTrustedTimedBreakRouteAndSanitizeMainIntent
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
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

class MainActivity : FragmentActivity() {
    private lateinit var appLockController: AppLockController
    private lateinit var appUnlockCoordinator: AppUnlockCoordinator
    private var backgroundLockJob: Job? = null
    private var autoPromptPending = false
    private val timedBreakNotificationRoutes: TimedBreakNotificationRouteMailbox
        get() = (application as DayWeaveApplication).timedBreakNotificationRoutes
    private val mutableTimedBreakNotificationSystemState =
        MutableStateFlow(TimedBreakNotificationSystemState.ENABLED)
    private val timedBreakNotificationSystemState =
        mutableTimedBreakNotificationSystemState.asStateFlow()
    private val notificationPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) {
        // Denial intentionally leaves the in-app break-ended resolution path unchanged.
        refreshTimedBreakNotificationSystemState()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        appLockController = (application as DayWeaveApplication).appLockController
        appUnlockCoordinator = AppUnlockCoordinator(
            controller = appLockController,
            authenticator = AndroidBiometricAppUnlockAuthenticator(this),
            processFence = (application as DayWeaveApplication).appAuthenticationProcessFence,
        )
        // MainActivity is non-exported. The launcher never forwards extras, so a valid route here
        // is the app-created immutable notification PendingIntent capability.
        setIntent(
            admitTrustedTimedBreakRouteAndSanitizeMainIntent(
                context = this,
                candidate = intent,
                mailbox = timedBreakNotificationRoutes,
            ),
        )
        refreshTimedBreakNotificationSystemState()
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
            val timedBreakRouteDigest =
                timedBreakNotificationRoutes.pendingDigest.collectAsStateWithLifecycle().value
            val timedBreakSystemState =
                timedBreakNotificationSystemState.collectAsStateWithLifecycle().value
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
                timedBreakNotificationRouteDigest = timedBreakRouteDigest,
                onTimedBreakNotificationRouteConsumed = { consumedDigest ->
                    timedBreakNotificationRoutes.consume(consumedDigest)
                },
                onRequestTimedBreakNotificationPermission =
                    ::requestTimedBreakNotificationPermission,
                timedBreakNotificationSystemState = timedBreakSystemState,
                onEnableTimedBreakReminders = ::enableTimedBreakReminders,
            )
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(
            admitTrustedTimedBreakRouteAndSanitizeMainIntent(
                context = this,
                candidate = intent,
                mailbox = timedBreakNotificationRoutes,
            ),
        )
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

    override fun onResume() {
        super.onResume()
        // Permission results and settings screens can both change app/channel state while paused.
        refreshTimedBreakNotificationSystemState()
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

    private fun requestTimedBreakNotificationPermission() {
        refreshTimedBreakNotificationSystemState()
        val systemState = timedBreakNotificationSystemState.value
        if (
            shouldRequestTimedBreakNotificationPermission(
                sdkInt = Build.VERSION.SDK_INT,
                permissionGranted = systemState.runtimePermissionGranted,
                permissionPreviouslyRequested =
                    systemState.runtimePermissionPreviouslyRequested,
            )
        ) {
            launchTimedBreakNotificationPermissionRequest()
        }
    }

    private fun enableTimedBreakReminders() {
        refreshTimedBreakNotificationSystemState()
        when (
            timedBreakReminderSystemEnableAction(
                sdkInt = Build.VERSION.SDK_INT,
                systemState = timedBreakNotificationSystemState.value,
            )
        ) {
            TimedBreakReminderEnableAction.REQUEST_RUNTIME_PERMISSION ->
                launchTimedBreakNotificationPermissionRequest()
            TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS ->
                openTimedBreakNotificationSettings()
            TimedBreakReminderEnableAction.NONE -> Unit
        }
    }

    private fun launchTimedBreakNotificationPermissionRequest() {
        getSharedPreferences(NOTIFICATION_PERMISSION_PREFERENCES, MODE_PRIVATE)
            .edit()
            .putBoolean(NOTIFICATION_PERMISSION_REQUESTED_KEY, true)
            .apply()
        mutableTimedBreakNotificationSystemState.value =
            timedBreakNotificationSystemState.value.copy(
                runtimePermissionPreviouslyRequested = true,
            )
        notificationPermissionLauncher.launch(POST_NOTIFICATIONS_PERMISSION)
    }

    private fun refreshTimedBreakNotificationSystemState() {
        val permissionGranted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(this, POST_NOTIFICATIONS_PERMISSION) ==
            PackageManager.PERMISSION_GRANTED
        val manager = getSystemService(NotificationManager::class.java)
        val channelEnabled = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager?.getNotificationChannel(TIMED_BREAK_NOTIFICATION_CHANNEL_ID)
                ?.importance
                ?.let { it != NotificationManager.IMPORTANCE_NONE }
                ?: true
        } else {
            true
        }
        mutableTimedBreakNotificationSystemState.value = TimedBreakNotificationSystemState(
            runtimePermissionGranted = permissionGranted,
            appNotificationsEnabled = NotificationManagerCompat.from(this)
                .areNotificationsEnabled(),
            channelEnabled = channelEnabled,
            runtimePermissionPreviouslyRequested = getSharedPreferences(
                NOTIFICATION_PERMISSION_PREFERENCES,
                MODE_PRIVATE,
            ).getBoolean(NOTIFICATION_PERMISSION_REQUESTED_KEY, false),
        )
    }

    private fun openTimedBreakNotificationSettings() {
        val channelIntent = Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
            .putExtra(Settings.EXTRA_CHANNEL_ID, TIMED_BREAK_NOTIFICATION_CHANNEL_ID)
        val appIntent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
        val detailsIntent = Intent(
            Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
            Uri.parse("package:$packageName"),
        )
        listOf(channelIntent, appIntent, detailsIntent).firstOrNull { candidate ->
            try {
                startActivity(candidate)
                true
            } catch (_: ActivityNotFoundException) {
                false
            }
        }
    }
}

internal fun shouldRequestTimedBreakNotificationPermission(
    sdkInt: Int,
    permissionGranted: Boolean,
    permissionPreviouslyRequested: Boolean = false,
): Boolean = sdkInt >= Build.VERSION_CODES.TIRAMISU && !permissionGranted &&
    !permissionPreviouslyRequested

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

private const val POST_NOTIFICATIONS_PERMISSION = "android.permission.POST_NOTIFICATIONS"
private const val NOTIFICATION_PERMISSION_PREFERENCES =
    "dayweave-timed-break-notification-permission"
private const val NOTIFICATION_PERMISSION_REQUESTED_KEY = "runtime-permission-requested"

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
