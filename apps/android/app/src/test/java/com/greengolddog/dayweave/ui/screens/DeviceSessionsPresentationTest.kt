package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.sync.DeviceSessionSummary
import com.greengolddog.dayweave.sync.DeviceSessionsPhase
import com.greengolddog.dayweave.sync.DeviceSessionsState
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceSessionsPresentationTest {
    private val reference = Instant.parse("2026-09-05T12:00:00Z")

    @Test
    fun supportingTextNamesAndroidClientAndRecentActivity() {
        assertEquals(
            "Android · DayWeave 0.1.0 · Active 1 minute ago",
            deviceSessionSupportingText(
                session(clientKind = "android", lastSeenAt = reference.minusSeconds(60)),
                reference,
            ),
        )
    }

    @Test
    fun supportingTextNamesMacAndClampsServerClockSkew() {
        assertEquals(
            "macOS · DayWeave 4.2 · Active just now",
            deviceSessionSupportingText(
                session(
                    clientKind = "macos",
                    clientVersion = "4.2",
                    lastSeenAt = reference.plusSeconds(30),
                ),
                reference,
            ),
        )
    }

    @Test
    fun supportingTextUsesReadablePluralHoursAndDays() {
        assertEquals(
            "Android · DayWeave 0.1.0 · Active 3 hours ago",
            deviceSessionSupportingText(
                session(lastSeenAt = reference.minusSeconds(3 * 3_600L)),
                reference,
            ),
        )
        assertEquals(
            "Android · DayWeave 0.1.0 · Active 2 days ago",
            deviceSessionSupportingText(
                session(lastSeenAt = reference.minusSeconds(2 * 86_400L)),
                reference,
            ),
        )
    }

    @Test
    fun readOnlyCurrentSessionDisablesActionsAndExplainsWhy() {
        val state = DeviceSessionsState(
            phase = DeviceSessionsPhase.READY,
            sessions = listOf(session(lastSeenAt = reference)),
            message = "1 active device · Read-only access",
            configurationId = "11111111-1111-4111-8111-111111111111",
            clientInstanceId = "22222222-2222-4222-8222-222222222222",
            currentSessionCanRevoke = false,
        )

        assertFalse(state.canRevokeRemoteSessions)
        assertTrue(deviceSessionInventoryPrivacyMessage(state).startsWith("Read-only access:"))
        assertTrue(deviceSessionInventoryPrivacyMessage(state).contains("cannot revoke or sign out"))
    }

    private fun session(
        clientKind: String = "android",
        clientVersion: String = "0.1.0",
        lastSeenAt: Instant,
    ) = DeviceSessionSummary(
        id = "11111111-1111-4111-8111-111111111111",
        clientKind = clientKind,
        deviceLabel = "Personal device",
        clientVersion = clientVersion,
        createdAt = reference.minusSeconds(86_400),
        lastSeenAt = lastSeenAt,
        refreshIdleExpiresAt = reference.plusSeconds(86_400),
        absoluteExpiresAt = reference.plusSeconds(172_800),
        revision = 1,
        isCurrent = true,
    )
}
