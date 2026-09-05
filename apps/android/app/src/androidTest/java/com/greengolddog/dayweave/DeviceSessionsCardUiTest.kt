package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.sync.DeviceSessionRevocationConfirmation
import com.greengolddog.dayweave.sync.DeviceSessionSummary
import com.greengolddog.dayweave.sync.DeviceSessionsPhase
import com.greengolddog.dayweave.sync.DeviceSessionsState
import com.greengolddog.dayweave.ui.screens.ActiveDevicesCard
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DeviceSessionsCardUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun currentBadgeAndRemoteRevocationUseExplicitExactConfirmation() {
        val state = readyState()
        val remote = state.sessions.single { !it.isCurrent }
        val confirmation = confirmation(remote)
        val revoked = mutableListOf<DeviceSessionRevocationConfirmation>()
        showCard(
            state = state,
            confirmationProvider = { id -> confirmation.takeIf { id == remote.id } },
            onRevokeRemote = revoked::add,
        )

        composeRule.onNodeWithText("This device").assertIsDisplayed()
        composeRule.onNodeWithTag("revoke_device_${remote.id}")
            .assertIsEnabled()
            .performClick()
        composeRule.onNodeWithText("Revoke Travel Mac?").assertIsDisplayed()
        composeRule.runOnIdle { assertEquals(0, revoked.size) }

        composeRule.onNodeWithTag("confirm_remote_device_revocation").performClick()
        composeRule.runOnIdle {
            assertEquals(1, revoked.size)
            assertSame(confirmation, revoked.single())
        }
    }

    @Test
    fun currentSignOutIsConfirmedAgainstTheDisplayedSessionId() {
        val signedOut = mutableListOf<String>()
        showCard(state = readyState(), onSignOutCurrent = signedOut::add)

        composeRule.onNodeWithTag("sign_out_current_device").performClick()
        composeRule.onNodeWithText("Revoke this device session?").assertIsDisplayed()
        composeRule.runOnIdle { assertEquals(emptyList<String>(), signedOut) }

        composeRule.onNodeWithTag("confirm_current_device_sign_out").performClick()
        composeRule.runOnIdle { assertEquals(listOf(CURRENT_ID), signedOut) }
    }

    @Test
    fun staleOrOfflineRowsRemainVisibleButAllRevocationsAreDisabled() {
        showCard(
            state = readyState().copy(
                phase = DeviceSessionsPhase.OFFLINE,
                message = "Offline · this in-memory list may be outdated.",
            ),
        )

        composeRule.onNodeWithText("Travel Mac").assertIsDisplayed()
        composeRule.onNodeWithTag("sign_out_current_device").assertIsNotEnabled()
        composeRule.onNodeWithTag("revoke_device_$REMOTE_ID").assertIsNotEnabled()
        composeRule.onNodeWithTag("refresh_active_devices").assertIsEnabled()
    }

    @Test
    fun bindingQuarantineRemovesRowsAndAnOpenConfirmation() {
        var state by mutableStateOf(readyState())
        val remote = state.sessions.single { !it.isCurrent }
        val confirmation = confirmation(remote)
        composeRule.setContent {
            MaterialTheme {
                ActiveDevicesCard(
                    state = state,
                    onRefresh = {},
                    revocationConfirmationProvider = { confirmation },
                    onRevokeRemote = {},
                    onSignOutCurrent = {},
                    onConfigureApiConnection = {},
                    referenceTime = NOW,
                )
            }
        }
        composeRule.onNodeWithTag("revoke_device_$REMOTE_ID").performClick()
        composeRule.onNodeWithText("Revoke Travel Mac?").assertIsDisplayed()

        composeRule.runOnIdle {
            state = DeviceSessionsState(
                phase = DeviceSessionsPhase.NOT_CONFIGURED,
                message = "Connect this device to manage active sessions.",
            )
        }

        composeRule.onAllNodesWithText("Travel Mac").assertCountEquals(0)
        composeRule.onAllNodesWithText("Revoke Travel Mac?").assertCountEquals(0)
    }

    private fun showCard(
        state: DeviceSessionsState,
        confirmationProvider: (String) -> DeviceSessionRevocationConfirmation? = { null },
        onRevokeRemote: (DeviceSessionRevocationConfirmation) -> Unit = {},
        onSignOutCurrent: (String) -> Unit = {},
    ) {
        composeRule.setContent {
            MaterialTheme {
                ActiveDevicesCard(
                    state = state,
                    onRefresh = {},
                    revocationConfirmationProvider = confirmationProvider,
                    onRevokeRemote = onRevokeRemote,
                    onSignOutCurrent = onSignOutCurrent,
                    onConfigureApiConnection = {},
                    referenceTime = NOW,
                )
            }
        }
    }

    private fun readyState() = DeviceSessionsState(
        phase = DeviceSessionsPhase.READY,
        sessions = listOf(
            session(CURRENT_ID, "Pixel", "android", true),
            session(REMOTE_ID, "Travel Mac", "macos", false),
        ),
        lastRefreshedAt = NOW,
        message = "2 active devices",
        configurationId = CURRENT_ID,
        clientInstanceId = CURRENT_INSTANCE_ID,
        currentSessionCanRevoke = true,
    )

    private fun session(
        id: String,
        label: String,
        kind: String,
        current: Boolean,
    ) = DeviceSessionSummary(
        id = id,
        clientKind = kind,
        deviceLabel = label,
        clientVersion = "0.1.0",
        createdAt = NOW.minusSeconds(86_400),
        lastSeenAt = NOW.minusSeconds(if (current) 30 else 3_600),
        refreshIdleExpiresAt = NOW.plusSeconds(86_400),
        absoluteExpiresAt = NOW.plusSeconds(172_800),
        revision = 1,
        isCurrent = current,
    )

    private fun confirmation(session: DeviceSessionSummary) =
        DeviceSessionRevocationConfirmation(
            presentationGeneration = 7,
            binding = ApiConnectionSnapshot(
                baseUrl = "https://api.example.test/",
                hasBearerToken = true,
                lastSuccessfulSyncEpochMillis = null,
                configurationId = CURRENT_ID,
                clientInstanceId = CURRENT_INSTANCE_ID,
            ),
            sessionId = session.id,
            sessionRevision = session.revision,
        )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-05T12:00:00Z")
        const val CURRENT_ID = "11111111-1111-4111-8111-111111111111"
        const val CURRENT_INSTANCE_ID = "22222222-2222-4222-8222-222222222222"
        const val REMOTE_ID = "33333333-3333-4333-8333-333333333333"
    }
}
