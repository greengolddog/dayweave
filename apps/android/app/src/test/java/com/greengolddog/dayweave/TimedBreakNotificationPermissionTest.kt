package com.greengolddog.dayweave

import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.notifications.TimedBreakNotificationSystemState
import com.greengolddog.dayweave.notifications.TimedBreakReminderEnableAction
import com.greengolddog.dayweave.notifications.timedBreakReminderEnableAction
import com.greengolddog.dayweave.state.TimedBreakNotificationPermissionRequestState
import com.greengolddog.dayweave.state.shouldRequestPermissionAfterDurableTimedPause
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TimedBreakNotificationPermissionTest {
    @Test
    fun androidThirteenAndNewerRequestOnlyWhilePermissionIsMissing() {
        assertTrue(shouldRequestTimedBreakNotificationPermission(sdkInt = 33, permissionGranted = false))
        assertTrue(shouldRequestTimedBreakNotificationPermission(sdkInt = 36, permissionGranted = false))
        assertFalse(shouldRequestTimedBreakNotificationPermission(sdkInt = 33, permissionGranted = true))
        assertFalse(
            shouldRequestTimedBreakNotificationPermission(
                sdkInt = 33,
                permissionGranted = false,
                permissionPreviouslyRequested = true,
            ),
        )
    }

    @Test
    fun preAndroidThirteenNeverUsesRuntimeNotificationPermissionPrompt() {
        assertFalse(shouldRequestTimedBreakNotificationPermission(sdkInt = 32, permissionGranted = false))
    }

    @Test
    fun promptRequiresNewDurableCanonicalTimedPauseAndConfirmedServerOutcome() {
        val first = canonicalTimedPause(revision = 3, deadline = DEADLINE)
        val extended = canonicalTimedPause(revision = 4, deadline = DEADLINE + 600_000L)

        assertTrue(
            shouldRequestPermissionAfterDurableTimedPause(
                before = DayWeaveUiState(),
                after = first,
                outcome = ExecutionSyncOutcome.SUCCESS,
                timedPauseRequested = true,
            ),
        )
        assertTrue(
            shouldRequestPermissionAfterDurableTimedPause(
                before = first,
                after = extended,
                outcome = ExecutionSyncOutcome.RECOVERED_COMMAND,
                timedPauseRequested = true,
            ),
        )
        assertFalse(
            shouldRequestPermissionAfterDurableTimedPause(
                before = first,
                after = first,
                outcome = ExecutionSyncOutcome.SUCCESS,
                timedPauseRequested = true,
            ),
        )
        assertFalse(
            shouldRequestPermissionAfterDurableTimedPause(
                before = DayWeaveUiState(),
                after = first.copy(canonicalExecutionSession = null),
                outcome = ExecutionSyncOutcome.SUCCESS,
                timedPauseRequested = true,
            ),
        )
        assertFalse(
            shouldRequestPermissionAfterDurableTimedPause(
                before = DayWeaveUiState(),
                after = first,
                outcome = ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
                timedPauseRequested = true,
            ),
        )
        assertFalse(
            shouldRequestPermissionAfterDurableTimedPause(
                before = DayWeaveUiState(),
                after = first,
                outcome = ExecutionSyncOutcome.SUCCESS,
                timedPauseRequested = false,
            ),
        )
    }

    @Test
    fun contextualRequestSurvivesInactiveUiAndConsumesOnceAfterExactRevalidation() {
        val first = canonicalTimedPause(revision = 3, deadline = DEADLINE)
        val firstDigest = first.authoritativeTimedBreakNotificationIdentity()!!.digest
        val replacement = canonicalTimedPause(revision = 4, deadline = DEADLINE + 600_000L)
        val replacementDigest = replacement.authoritativeTimedBreakNotificationIdentity()!!.digest
        val requests = TimedBreakNotificationPermissionRequestState()

        requests.offer(firstDigest)
        // Stop, app lock, and Activity recreation do not collect/take this ViewModel-scoped state.
        assertEquals(firstDigest, requests.requestDigest.value)
        assertEquals(firstDigest, requests.requestDigest.value)
        assertTrue(requests.takeIfCurrent(firstDigest, first))
        assertFalse(requests.takeIfCurrent(firstDigest, first))

        requests.offer(firstDigest)
        assertFalse(requests.takeIfCurrent(firstDigest, replacement))
        assertEquals(null, requests.requestDigest.value)

        requests.offer(firstDigest)
        requests.offer(replacementDigest)
        assertFalse(requests.takeIfCurrent(firstDigest, replacement))
        assertEquals(replacementDigest, requests.requestDigest.value)
        assertTrue(requests.takeIfCurrent(replacementDigest, replacement))
    }

    @Test
    fun durableFutureBreakRecoversLostPromptWithVisibleEnableOrSettingsAction() {
        val futureBreak = canonicalTimedPause(revision = 3, deadline = DEADLINE)
        val missingNeverAsked = TimedBreakNotificationSystemState(
            runtimePermissionGranted = false,
            appNotificationsEnabled = false,
            channelEnabled = true,
            runtimePermissionPreviouslyRequested = false,
        )

        // This is derived after process restart from encrypted break truth plus current Android
        // capability; no ViewModel event or launch-time auto prompt is required.
        assertEquals(
            TimedBreakReminderEnableAction.REQUEST_RUNTIME_PERMISSION,
            timedBreakReminderEnableAction(
                durableState = futureBreak,
                nowEpochMillis = DEADLINE - 1L,
                sdkInt = 35,
                systemState = missingNeverAsked,
            ),
        )

        val denied = missingNeverAsked.copy(runtimePermissionPreviouslyRequested = true)
        assertEquals(
            TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS,
            timedBreakReminderEnableAction(
                durableState = futureBreak,
                nowEpochMillis = DEADLINE - 1L,
                sdkInt = 35,
                systemState = denied,
            ),
        )
        val grantedButChannelDenied = TimedBreakNotificationSystemState(
            runtimePermissionGranted = true,
            appNotificationsEnabled = true,
            channelEnabled = false,
            runtimePermissionPreviouslyRequested = true,
        )
        assertEquals(
            TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS,
            timedBreakReminderEnableAction(
                durableState = futureBreak,
                nowEpochMillis = DEADLINE - 1L,
                sdkInt = 35,
                systemState = grantedButChannelDenied,
            ),
        )
        // Returning from settings/onResume rechecks live capability and removes the affordance.
        assertEquals(
            TimedBreakReminderEnableAction.NONE,
            timedBreakReminderEnableAction(
                durableState = futureBreak,
                nowEpochMillis = DEADLINE - 1L,
                sdkInt = 35,
                systemState = TimedBreakNotificationSystemState.ENABLED,
            ),
        )
        assertEquals(
            TimedBreakReminderEnableAction.NONE,
            timedBreakReminderEnableAction(
                durableState = futureBreak,
                nowEpochMillis = DEADLINE,
                sdkInt = 35,
                systemState = denied,
            ),
        )
        assertEquals(
            TimedBreakReminderEnableAction.NONE,
            timedBreakReminderEnableAction(
                durableState = DayWeaveUiState(),
                nowEpochMillis = DEADLINE - 1L,
                sdkInt = 35,
                systemState = denied,
            ),
        )
    }
}

private fun canonicalTimedPause(revision: Long, deadline: Long): DayWeaveUiState {
    val sessionId = "11111111-1111-4111-8111-111111111111"
    val blockId = "33333333-3333-4333-8333-333333333333"
    return DayWeaveUiState(
        canonicalExecutionRevision = revision + 4,
        canonicalExecutionSession = CanonicalExecutionSessionSnapshot(
            id = sessionId,
            itemId = "22222222-2222-4222-8222-222222222222",
            itemRevision = 2,
            sessionIndex = 0,
            plannedBlockId = blockId,
            sourceDeviceId = "44444444-4444-4444-8444-444444444444",
            status = "paused",
            revision = revision,
            accumulatedSeconds = 300,
            startedAt = "2026-09-01T06:00:00Z",
            pausedAt = "2026-09-01T06:05:00Z",
            pauseUntil = Instant.ofEpochMilli(deadline).toString(),
            createdAt = "2026-09-01T06:00:00Z",
            updatedAt = "2026-09-01T06:05:00Z",
        ),
        activeSession = ActiveSession(
            itemId = blockId,
            elapsedMinutes = 5,
            isPaused = true,
            accumulatedSeconds = 300,
            pauseUntilEpochMillis = deadline,
            canonicalExecutionSessionId = sessionId,
        ),
    )
}

private val DEADLINE = Instant.parse("2026-09-01T06:10:00Z").toEpochMilli()
