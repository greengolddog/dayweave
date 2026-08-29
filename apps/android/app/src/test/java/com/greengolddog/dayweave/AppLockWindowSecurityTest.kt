package com.greengolddog.dayweave

import android.app.Activity
import android.view.WindowManager
import com.greengolddog.dayweave.security.AppLockNotice
import com.greengolddog.dayweave.security.AppLockSettings
import com.greengolddog.dayweave.security.AppLockState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AppLockWindowSecurityTest {
    @Test
    fun activePromptAtStartCannotArmAReplacementAfterItsCancellationDrains() {
        val activeAtStart = protectedState(
            isLocked = true,
            isAuthenticating = true,
        )
        val cancellationDrainedBeforeResume = protectedState(
            isLocked = true,
            notice = AppLockNotice.AUTHENTICATION_CANCELLED,
        )

        assertFalse(
            shouldAutoPromptOnStart(
                stateBeforeForeground = activeAtStart,
                stateAfterForeground = cancellationDrainedBeforeResume,
            ),
        )
    }

    @Test
    fun terminalAuthenticationNoticeWhileStoppedSuppressesAutomaticRetry() {
        listOf(
            AppLockNotice.AUTHENTICATION_CANCELLED,
            AppLockNotice.AUTHENTICATION_LOCKED_OUT,
            AppLockNotice.AUTHENTICATION_ERROR,
        ).forEach { notice ->
            assertFalse(
                shouldAutoPromptOnStart(
                    stateBeforeForeground = protectedState(isLocked = true, notice = notice),
                    // Availability refresh may clear the notice before onPostResume.
                    stateAfterForeground = protectedState(isLocked = true),
                ),
            )
        }
    }

    @Test
    fun coldTimeoutAndCorruptIdleLocksArmExactlyOneConveniencePrompt() {
        assertTrue(
            shouldAutoPromptOnStart(
                stateBeforeForeground = protectedState(isLocked = true),
                stateAfterForeground = protectedState(isLocked = true),
            ),
        )
        assertTrue(
            shouldAutoPromptOnStart(
                stateBeforeForeground = protectedState(isLocked = false),
                stateAfterForeground = protectedState(isLocked = true),
            ),
        )
        assertTrue(
            shouldAutoPromptOnStart(
                stateBeforeForeground = protectedState(
                    isLocked = true,
                    settingsHealthy = false,
                    notice = AppLockNotice.SETTINGS_RECOVERY_REQUIRED,
                ),
                stateAfterForeground = protectedState(
                    isLocked = true,
                    settingsHealthy = false,
                    notice = AppLockNotice.SETTINGS_RECOVERY_REQUIRED,
                ),
            ),
        )
    }

    @Test
    fun postForegroundBusyOrUnlockedStateNeverArmsAutomaticPrompt() {
        assertFalse(
            shouldAutoPromptOnStart(
                stateBeforeForeground = protectedState(isLocked = true),
                stateAfterForeground = protectedState(
                    isLocked = true,
                    isAwaitingForegroundAuthenticationCompletion = true,
                ),
            ),
        )
        assertFalse(
            shouldAutoPromptOnStart(
                stateBeforeForeground = protectedState(isLocked = true),
                stateAfterForeground = protectedState(isLocked = false),
            ),
        )
    }

    @Test
    fun enabledProtectionSetsSecureWindowEvenDuringAnUnlockedSession() {
        val activity = Robolectric.buildActivity(WindowHostActivity::class.java).setup().get()

        AppLockWindowSecurity.apply(
            activity.window,
            AppLockState(
                settings = AppLockSettings(enabled = true),
                isLocked = false,
            ),
        )

        assertEquals(
            WindowManager.LayoutParams.FLAG_SECURE,
            activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE,
        )
    }

    @Test
    fun enablingAuthenticationSetsSecureWindowBeforeTheSettingIsDurable() {
        val activity = Robolectric.buildActivity(WindowHostActivity::class.java).setup().get()

        AppLockWindowSecurity.apply(
            activity.window,
            AppLockState(
                settings = AppLockSettings(enabled = false),
                isLocked = false,
                isAuthenticating = true,
            ),
        )

        assertEquals(
            WindowManager.LayoutParams.FLAG_SECURE,
            activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE,
        )
    }

    @Test
    fun disabledIdleStateClearsPreviouslyForcedSecureWindow() {
        val activity = Robolectric.buildActivity(WindowHostActivity::class.java).setup().get()
        AppLockWindowSecurity.forceSecure(activity.window)

        AppLockWindowSecurity.apply(
            activity.window,
            AppLockState(settings = AppLockSettings(), isLocked = false),
        )

        assertEquals(
            0,
            activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE,
        )
    }

    private fun protectedState(
        isLocked: Boolean,
        isAuthenticating: Boolean = false,
        isAwaitingForegroundAuthenticationCompletion: Boolean = false,
        settingsHealthy: Boolean = true,
        notice: AppLockNotice? = null,
    ) = AppLockState(
        settings = AppLockSettings(enabled = true),
        isLocked = isLocked,
        isAuthenticating = isAuthenticating,
        isAwaitingForegroundAuthenticationCompletion =
            isAwaitingForegroundAuthenticationCompletion,
        settingsHealthy = settingsHealthy,
        notice = notice,
    )
}

class WindowHostActivity : Activity()
