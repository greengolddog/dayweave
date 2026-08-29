package com.greengolddog.dayweave.security

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockControllerTest {
    @Test
    fun firstLaunchIsOptInAndDoesNotLock() {
        val controller = controller()

        assertFalse(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
        assertTrue(controller.state.value.settingsHealthy)
    }

    @Test
    fun enabledColdStartIsLockedBeforeAuthentication() {
        val controller = controller(settings = enabledSettings())

        assertTrue(controller.state.value.settings.enabled)
        assertTrue(controller.state.value.isLocked)
        assertFalse(controller.state.value.isAuthenticating)
    }

    @Test
    fun enablingRequiresSuccessfulAuthenticationAndDurableSave() {
        val store = FakeSettingsStore()
        val controller = controller(store = store)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)

        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.ENABLE),
        )
        assertFalse(controller.state.value.settings.enabled)
        assertTrue(controller.state.value.isAuthenticating)

        assertTrue(
            controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS),
        )
        assertEquals(enabledSettings(), store.savedSettings.single())
        assertTrue(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun cancelledEnableLeavesLockDisabled() {
        val store = FakeSettingsStore()
        val controller = controller(store = store)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.ENABLE),
        )

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.CANCELLED)

        assertFalse(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
        assertEquals(AppLockNotice.AUTHENTICATION_CANCELLED, controller.state.value.notice)
        assertTrue(store.savedSettings.isEmpty())
    }

    @Test
    fun successfulUnlockOpensEnabledSession() {
        val controller = controller(settings = enabledSettings())
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertFalse(controller.state.value.isLocked)
        assertTrue(controller.state.value.settings.enabled)
    }

    @Test
    fun corruptSettingsFailClosedAndOnlyHealAfterAuthenticationAndSave() {
        val store = FakeSettingsStore(loadResult = AppLockSettingsLoadResult.Corrupt)
        val controller = controller(store = store)
        assertTrue(controller.state.value.isLocked)
        assertFalse(controller.state.value.settingsHealthy)
        assertEquals(AppLockNotice.SETTINGS_RECOVERY_REQUIRED, controller.state.value.notice)

        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertTrue(controller.state.value.settingsHealthy)
        assertFalse(controller.state.value.isLocked)
        assertEquals(enabledSettings(), store.savedSettings.single())
    }

    @Test
    fun corruptSettingsRemainLockedWhenRecoverySaveFails() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Corrupt,
            saveSucceeds = false,
        )
        val controller = controller(store = store)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertTrue(controller.state.value.isLocked)
        assertFalse(controller.state.value.settingsHealthy)
        assertEquals(AppLockNotice.SETTINGS_SAVE_FAILED, controller.state.value.notice)
    }

    @Test
    fun immediatePolicyLocksOnBackground() {
        val controller = controller(
            settings = enabledSettings(AppLockTimeout.IMMEDIATELY),
        ).also(::unlock)

        assertNull(controller.onBackgrounded())
        assertTrue(controller.state.value.isLocked)
    }

    @Test
    fun delayedPolicyLocksOnlyWhenItsDeadlineIsReached() {
        val clock = FakeClock(1_000L)
        val controller = controller(
            settings = enabledSettings(AppLockTimeout.ONE_MINUTE),
            clock = clock,
        ).also(::unlock)
        val request = requireNotNull(controller.onBackgrounded())

        clock.now = 60_999L
        assertFalse(controller.onBackgroundTimeout(request.generation))
        assertFalse(controller.state.value.isLocked)

        clock.now = 61_000L
        assertTrue(controller.onBackgroundTimeout(request.generation))
        assertTrue(controller.state.value.isLocked)
    }

    @Test
    fun foregroundBeforeDeadlineInvalidatesOldTimer() {
        val clock = FakeClock(5_000L)
        val controller = controller(settings = enabledSettings(), clock = clock).also(::unlock)
        val request = requireNotNull(controller.onBackgrounded())

        clock.now += 10_000L
        controller.onForegrounded()
        clock.now += 60_000L

        assertFalse(controller.onBackgroundTimeout(request.generation))
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun foregroundAtDeadlineLocksBeforeSensitiveUiCanReturn() {
        val clock = FakeClock(10_000L)
        val controller = controller(settings = enabledSettings(), clock = clock).also(::unlock)
        controller.onBackgrounded()
        clock.now += AppLockTimeout.ONE_MINUTE.durationMillis

        controller.onForegrounded()

        assertTrue(controller.state.value.isLocked)
    }

    @Test
    fun monotonicClockDiscontinuityFailsClosed() {
        val clock = FakeClock(10_000L)
        val controller = controller(settings = enabledSettings(), clock = clock).also(::unlock)
        controller.onBackgrounded()
        clock.now = 1L

        controller.onForegrounded()

        assertTrue(controller.state.value.isLocked)
    }

    @Test
    fun lateAuthenticationCallbackCannotUnlockAfterBackgroundCancellation() {
        val controller = controller(settings = enabledSettings())
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        controller.onBackgrounded()

        assertFalse(
            controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS),
        )
        assertTrue(controller.state.value.isLocked)
    }

    @Test
    fun lockedStateCannotBeDisabledWithoutUnlocking() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Loaded(enabledSettings()),
        )
        val controller = controller(store = store)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)

        assertNull(controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE))
        assertTrue(controller.state.value.settings.enabled)
        assertTrue(store.savedSettings.isEmpty())
    }

    @Test
    fun disablingRequiresFreshAuthenticationAndDurableSave() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Loaded(enabledSettings()),
        )
        val controller = controller(store = store).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )

        assertTrue(controller.state.value.settings.enabled)
        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertEquals(AppLockSettings(enabled = false), store.savedSettings.single())
        assertFalse(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun failedAuthenticatedDisableSaveKeepsProtectionEnabled() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Loaded(enabledSettings()),
            saveSucceeds = false,
        )
        val controller = controller(store = store).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )

        assertTrue(
            controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS),
        )
        assertTrue(controller.state.value.settings.enabled)
        assertEquals(AppLockNotice.SETTINGS_SAVE_FAILED, controller.state.value.notice)
    }

    @Test
    fun cancelledDisableLeavesProtectionEnabledAndUnchanged() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Loaded(enabledSettings()),
        )
        val controller = controller(store = store).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.CANCELLED)

        assertTrue(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
        assertTrue(store.savedSettings.isEmpty())
        assertEquals(AppLockNotice.AUTHENTICATION_CANCELLED, controller.state.value.notice)
    }

    @Test
    fun credentialSuccessWhileHostStoppedIsAppliedOnlyOnPromptReturn() {
        val clock = FakeClock(1_000L)
        val controller = controller(settings = enabledSettings(), clock = clock)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        controller.onAuthenticationHostStopped()

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertTrue(controller.state.value.isLocked)
        assertTrue(controller.state.value.isAwaitingForegroundAuthenticationCompletion)
        clock.now += 1_000L
        controller.onForegrounded()
        assertFalse(controller.state.value.isLocked)
        assertFalse(controller.state.value.isAuthenticationBusy)
    }

    @Test
    fun stoppedPromptSuccessExpiresWithoutCreatingUntimedUnlockedSession() {
        val clock = FakeClock(1_000L)
        val controller = controller(settings = enabledSettings(), clock = clock)
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        controller.onAuthenticationHostStopped()
        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        clock.now += 10_001L
        controller.onForegrounded()

        assertTrue(controller.state.value.isLocked)
        assertFalse(controller.state.value.isAuthenticationBusy)
        assertEquals(AppLockNotice.AUTHENTICATION_ERROR, controller.state.value.notice)
    }

    @Test
    fun terminalCancellationWhileStoppedRelocksAuthenticatedDisableSession() {
        val controller = controller(settings = enabledSettings()).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )
        controller.onAuthenticationHostStopped()

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.CANCELLED)

        assertTrue(controller.state.value.settings.enabled)
        assertTrue(controller.state.value.isLocked)
        assertEquals(AppLockNotice.AUTHENTICATION_CANCELLED, controller.state.value.notice)
    }

    @Test
    fun authenticationHostStopImmediatelyReplacesUnlockedPresentationAndRetainsExactAttempt() {
        val controller = controller(settings = enabledSettings()).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )

        controller.onAuthenticationHostStopped()

        val stopped = controller.state.value
        assertTrue(stopped.isLocked)
        assertTrue(stopped.isAuthenticating)
        assertEquals(request, stopped.pendingAuthenticationRequestId)
        assertEquals(AppLockAuthenticationPurpose.DISABLE, stopped.pendingAuthenticationPurpose)
        assertTrue(stopped.settings.enabled)
    }

    @Test
    fun homeAndReentryRemainLockedUntilExactDelayedDisableSuccess() {
        val controller = controller(settings = enabledSettings()).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )
        controller.onAuthenticationHostStopped()

        controller.onForegrounded()

        assertTrue(controller.state.value.isLocked)
        assertTrue(controller.state.value.isAuthenticating)
        assertFalse(
            controller.completeAuthentication(
                request + 1,
                AppLockAuthenticationOutcome.SUCCESS,
            ),
        )
        assertTrue(controller.state.value.settings.enabled)
        assertTrue(controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS))
        assertFalse(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun lostPlatformTerminalOnHomeAndReentryNeverRestoresUnlockedPresentation() {
        val controller = controller(settings = enabledSettings()).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )
        controller.onAuthenticationHostStopped()

        repeat(3) { controller.onForegrounded() }

        assertTrue(controller.state.value.isLocked)
        assertTrue(controller.state.value.isAuthenticationBusy)
        assertEquals(request, controller.state.value.pendingAuthenticationRequestId)
        assertNull(controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK))
    }

    @Test
    fun delayedDisableSuccessWhileStoppedIsAppliedOnlyOnPromptReturn() {
        val clock = FakeClock(5_000L)
        val controller = controller(settings = enabledSettings(), clock = clock).also(::unlock)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.DISABLE),
        )
        controller.onAuthenticationHostStopped()

        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)

        assertTrue(controller.state.value.settings.enabled)
        assertTrue(controller.state.value.isLocked)
        assertTrue(controller.state.value.isAwaitingForegroundAuthenticationCompletion)
        clock.now += 500L
        controller.onForegrounded()
        assertFalse(controller.state.value.settings.enabled)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun timeoutChangeIsDurableAndUsedByNextBackgroundTransition() {
        val store = FakeSettingsStore(
            loadResult = AppLockSettingsLoadResult.Loaded(enabledSettings()),
        )
        val controller = controller(store = store).also(::unlock)

        assertTrue(controller.updateTimeout(AppLockTimeout.FIVE_MINUTES))
        assertEquals(
            enabledSettings(AppLockTimeout.FIVE_MINUTES),
            store.savedSettings.single(),
        )
        assertEquals(
            AppLockTimeout.FIVE_MINUTES.durationMillis,
            requireNotNull(controller.onBackgrounded()).delayMillis,
        )
    }

    private fun unlock(controller: AppLockController) {
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)
        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)
    }

    private fun controller(
        settings: AppLockSettings = AppLockSettings(),
        store: FakeSettingsStore = FakeSettingsStore(
            AppLockSettingsLoadResult.Loaded(settings),
        ),
        clock: FakeClock = FakeClock(),
    ): AppLockController = AppLockController(store, clock).also { it.onForegrounded() }

    private fun enabledSettings(
        timeout: AppLockTimeout = AppLockTimeout.ONE_MINUTE,
    ) = AppLockSettings(enabled = true, timeout = timeout)
}

private class FakeClock(var now: Long = 0L) : MonotonicClock {
    override fun nowMillis(): Long = now
}

private class FakeSettingsStore(
    var loadResult: AppLockSettingsLoadResult =
        AppLockSettingsLoadResult.Loaded(AppLockSettings()),
    var saveSucceeds: Boolean = true,
) : AppLockSettingsStore {
    val savedSettings = mutableListOf<AppLockSettings>()

    override fun load(): AppLockSettingsLoadResult = loadResult

    override fun save(settings: AppLockSettings): Boolean {
        if (saveSucceeds) savedSettings += settings
        return saveSucceeds
    }
}
