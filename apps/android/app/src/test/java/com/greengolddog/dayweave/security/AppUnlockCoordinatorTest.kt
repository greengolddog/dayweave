package com.greengolddog.dayweave.security

import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AppUnlockCoordinatorTest {
    @Test
    fun unavailableAuthenticatorNeverLaunchesPrompt() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator(AppUnlockAvailability.NOT_ENROLLED)
        val coordinator = AppUnlockCoordinator(controller, authenticator)

        assertFalse(coordinator.requestUnlock())
        assertEquals(0, authenticator.authenticateCalls)
        assertTrue(controller.state.value.isLocked)
        assertEquals(AppUnlockAvailability.NOT_ENROLLED, controller.state.value.availability)
    }

    @Test
    fun duplicateUnlockTapCannotLaunchCompetingPrompt() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator()
        val coordinator = AppUnlockCoordinator(controller, authenticator)

        assertTrue(coordinator.requestUnlock())
        assertFalse(coordinator.requestUnlock())
        assertEquals(1, authenticator.authenticateCalls)

        authenticator.complete(AppLockAuthenticationOutcome.SUCCESS)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun cancelledAttemptKeepsProcessSlotUntilItsTerminalCallbackDrains() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator()
        val fence = AppAuthenticationProcessFence()
        val coordinator = AppUnlockCoordinator(controller, authenticator, fence)
        assertTrue(coordinator.requestUnlock())
        val attemptA = requireNotNull(authenticator.attempt)

        assertTrue(coordinator.cancelAuthentication())
        assertTrue(controller.state.value.isAuthenticating)
        assertTrue(fence.hasActiveAttempt())
        assertFalse(coordinator.requestUnlock())
        assertEquals(1, authenticator.authenticateCalls)

        authenticator.complete(AppLockAuthenticationOutcome.CANCELLED)
        assertFalse(fence.hasActiveAttempt())
        assertFalse(controller.state.value.isAuthenticating)
        assertTrue(coordinator.requestUnlock())
        val attemptB = requireNotNull(authenticator.attempt)
        assertNotEquals(attemptA.processAttemptId, attemptB.processAttemptId)
    }

    @Test
    fun lateSuccessAndErrorFromDrainedAttemptCannotCompleteReplacementAttempt() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator()
        val coordinator = AppUnlockCoordinator(
            controller,
            authenticator,
            AppAuthenticationProcessFence(),
        )
        assertTrue(coordinator.requestUnlock())
        val callbackA = requireNotNull(authenticator.callback)
        val attemptA = requireNotNull(authenticator.attempt)
        coordinator.cancelAuthentication()
        callbackA(attemptA, AppLockAuthenticationOutcome.CANCELLED)

        assertTrue(coordinator.requestUnlock())
        val attemptB = requireNotNull(authenticator.attempt)
        callbackA(attemptA, AppLockAuthenticationOutcome.SUCCESS)
        callbackA(attemptA, AppLockAuthenticationOutcome.ERROR)

        assertTrue(controller.state.value.isLocked)
        assertTrue(controller.state.value.isAuthenticating)
        assertEquals(attemptB.controllerRequestId, controller.state.value.pendingAuthenticationRequestId)
        authenticator.complete(AppLockAuthenticationOutcome.SUCCESS)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun activityReplacementReconnectsExactAttemptWithoutSecondPrompt() {
        val controller = lockedController()
        val fence = AppAuthenticationProcessFence()
        val firstAuthenticator = FakeAuthenticator()
        val firstCoordinator = AppUnlockCoordinator(controller, firstAuthenticator, fence)
        assertTrue(firstCoordinator.requestUnlock())
        val originalAttempt = requireNotNull(firstAuthenticator.attempt)

        val replacementAuthenticator = FakeAuthenticator()
        AppUnlockCoordinator(controller, replacementAuthenticator, fence)

        assertEquals(1, firstAuthenticator.authenticateCalls)
        assertEquals(1, replacementAuthenticator.reconnectCalls)
        assertEquals(originalAttempt, replacementAuthenticator.attempt)
        replacementAuthenticator.complete(AppLockAuthenticationOutcome.SUCCESS)
        assertFalse(controller.state.value.isLocked)
    }

    @Test
    fun staleActivityOwnerCannotCancelReconnectedPrompt() {
        val controller = lockedController()
        val fence = AppAuthenticationProcessFence()
        val firstAuthenticator = FakeAuthenticator()
        val firstCoordinator = AppUnlockCoordinator(controller, firstAuthenticator, fence)
        firstCoordinator.requestUnlock()

        val replacementAuthenticator = FakeAuthenticator()
        val replacementCoordinator = AppUnlockCoordinator(controller, replacementAuthenticator, fence)

        assertFalse(firstCoordinator.cancelAuthentication())
        assertEquals(0, firstAuthenticator.cancelCalls)
        assertEquals(0, replacementAuthenticator.cancelCalls)
        assertTrue(replacementCoordinator.cancelAuthentication())
        assertEquals(1, replacementAuthenticator.cancelCalls)
        assertTrue(fence.hasActiveAttempt())
    }

    @Test
    fun lockNowStillCannotBypassCancellationDrainWithANewPrompt() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator()
        val fence = AppAuthenticationProcessFence()
        val coordinator = AppUnlockCoordinator(controller, authenticator, fence)
        coordinator.requestUnlock()
        val callbackA = requireNotNull(authenticator.callback)
        val attemptA = requireNotNull(authenticator.attempt)

        coordinator.cancelAuthentication()
        controller.lockNow()
        assertFalse(coordinator.requestUnlock())

        callbackA(attemptA, AppLockAuthenticationOutcome.CANCELLED)
        assertTrue(coordinator.requestUnlock())
        assertEquals(2, authenticator.authenticateCalls)
    }

    @Test
    fun recreatedActivityRebindsCancelledAttemptEvenAfterControllerInvalidatesIt() {
        val controller = lockedController()
        val fence = AppAuthenticationProcessFence()
        val firstAuthenticator = FakeAuthenticator()
        val firstCoordinator = AppUnlockCoordinator(controller, firstAuthenticator, fence)
        firstCoordinator.requestUnlock()
        firstCoordinator.cancelAuthentication()
        controller.lockNow()

        val replacementAuthenticator = FakeAuthenticator()
        val replacementCoordinator = AppUnlockCoordinator(
            controller,
            replacementAuthenticator,
            fence,
        )

        assertEquals(1, replacementAuthenticator.reconnectCalls)
        assertEquals(1, replacementAuthenticator.cancelCalls)
        assertFalse(replacementCoordinator.requestUnlock())
        replacementAuthenticator.complete(AppLockAuthenticationOutcome.CANCELLED)
        assertTrue(replacementCoordinator.requestUnlock())
    }

    @Test
    fun cancellationExceptionDoesNotReleaseThePlatformAttempt() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator(throwOnCancel = true)
        val fence = AppAuthenticationProcessFence()
        val coordinator = AppUnlockCoordinator(controller, authenticator, fence)
        coordinator.requestUnlock()

        assertFalse(coordinator.cancelAuthentication())
        assertTrue(fence.hasActiveAttempt())
        assertFalse(coordinator.requestUnlock())

        authenticator.complete(AppLockAuthenticationOutcome.ERROR)
        assertFalse(fence.hasActiveAttempt())
        assertTrue(coordinator.requestUnlock())
    }

    @Test
    fun reconnectExceptionKeepsExactAttemptFencedForAnOldTerminalCallback() {
        val controller = lockedController()
        val fence = AppAuthenticationProcessFence()
        val firstAuthenticator = FakeAuthenticator()
        AppUnlockCoordinator(controller, firstAuthenticator, fence).requestUnlock()
        val oldCallback = requireNotNull(firstAuthenticator.callback)
        val attempt = requireNotNull(firstAuthenticator.attempt)

        val brokenReplacement = FakeAuthenticator(throwOnReconnect = true)
        val replacementCoordinator = AppUnlockCoordinator(controller, brokenReplacement, fence)

        assertTrue(controller.state.value.isAuthenticating)
        assertTrue(fence.hasActiveAttempt())
        assertFalse(replacementCoordinator.requestUnlock())
        oldCallback(attempt, AppLockAuthenticationOutcome.SUCCESS)
        assertFalse(controller.state.value.isLocked)
        assertFalse(fence.hasActiveAttempt())
    }

    @Test
    fun promptExceptionFailsClosedAndReleasesProcessFence() {
        val controller = lockedController()
        val authenticator = FakeAuthenticator(throwOnAuthenticate = true)
        val fence = AppAuthenticationProcessFence()
        val coordinator = AppUnlockCoordinator(controller, authenticator, fence)

        assertFalse(coordinator.requestUnlock())
        assertTrue(controller.state.value.isLocked)
        assertEquals(AppLockNotice.AUTHENTICATION_ERROR, controller.state.value.notice)
        assertFalse(fence.hasActiveAttempt())
    }

    @Test
    fun disablingUsesItsOwnSystemAuthenticationAttempt() {
        val controller = lockedController()
        val fence = AppAuthenticationProcessFence()
        val authenticator = FakeAuthenticator()
        val coordinator = AppUnlockCoordinator(controller, authenticator, fence)
        coordinator.requestUnlock()
        authenticator.complete(AppLockAuthenticationOutcome.SUCCESS)

        assertTrue(coordinator.requestDisable())
        assertEquals(2, authenticator.authenticateCalls)
        assertEquals(AppLockAuthenticationPurpose.DISABLE, authenticator.attempt?.purpose)
        assertTrue(controller.state.value.settings.enabled)

        authenticator.complete(AppLockAuthenticationOutcome.SUCCESS)
        assertFalse(controller.state.value.settings.enabled)
    }

    @Test
    fun androidResultMappingUsesCryptoCompatibleAuthenticatorCombinations() {
        assertEquals(
            BiometricManager.Authenticators.BIOMETRIC_STRONG,
            AndroidBiometricAppUnlockAuthenticator.allowedAuthenticatorsForSdk(29),
        )
        assertEquals(
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL,
            AndroidBiometricAppUnlockAuthenticator.allowedAuthenticatorsForSdk(30),
        )
        assertEquals(
            AppUnlockAvailability.AVAILABLE,
            AndroidBiometricAppUnlockAuthenticator.availabilityForResult(
                BiometricManager.BIOMETRIC_SUCCESS,
            ),
        )
        assertEquals(
            AppUnlockAvailability.NOT_ENROLLED,
            AndroidBiometricAppUnlockAuthenticator.availabilityForResult(
                BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED,
            ),
        )
        assertEquals(
            AppLockAuthenticationOutcome.CANCELLED,
            AndroidBiometricAppUnlockAuthenticator.authenticationOutcomeForError(
                BiometricPrompt.ERROR_USER_CANCELED,
            ),
        )
        assertEquals(
            AppLockAuthenticationOutcome.LOCKED_OUT,
            AndroidBiometricAppUnlockAuthenticator.authenticationOutcomeForError(
                BiometricPrompt.ERROR_LOCKOUT_PERMANENT,
            ),
        )
    }

    private fun lockedController(): AppLockController = AppLockController(
        settingsStore = object : AppLockSettingsStore {
            override fun load(): AppLockSettingsLoadResult = AppLockSettingsLoadResult.Loaded(
                AppLockSettings(enabled = true),
            )

            override fun save(settings: AppLockSettings): Boolean = true
        },
        clock = MonotonicClock { 0L },
    ).also { it.onForegrounded() }
}

private class FakeAuthenticator(
    var currentAvailability: AppUnlockAvailability = AppUnlockAvailability.AVAILABLE,
    private val throwOnAuthenticate: Boolean = false,
    private val throwOnReconnect: Boolean = false,
    private val throwOnCancel: Boolean = false,
) : AppUnlockAuthenticator {
    var authenticateCalls = 0
    var reconnectCalls = 0
    var cancelCalls = 0
    var attempt: AppLockAuthenticationAttempt? = null
    var callback: (
        (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit
    )? = null

    override fun availability(): AppUnlockAvailability = currentAvailability

    override fun authenticate(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ) {
        authenticateCalls += 1
        if (throwOnAuthenticate) error("synthetic prompt failure")
        this.attempt = attempt
        callback = onResult
    }

    override fun reconnect(
        attempt: AppLockAuthenticationAttempt,
        onResult: (AppLockAuthenticationAttempt, AppLockAuthenticationOutcome) -> Unit,
    ) {
        reconnectCalls += 1
        if (throwOnReconnect) error("synthetic reconnect failure")
        this.attempt = attempt
        callback = onResult
    }

    override fun cancel(attempt: AppLockAuthenticationAttempt) {
        if (throwOnCancel) error("synthetic cancellation failure")
        if (this.attempt == attempt) cancelCalls += 1
    }

    fun complete(outcome: AppLockAuthenticationOutcome) {
        requireNotNull(callback)(requireNotNull(attempt), outcome)
    }
}
