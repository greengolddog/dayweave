package com.greengolddog.dayweave

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.security.AppLockAuthenticationOutcome
import com.greengolddog.dayweave.security.AppLockAuthenticationPurpose
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AppLockSettings
import com.greengolddog.dayweave.security.AppLockSettingsLoadResult
import com.greengolddog.dayweave.security.AppLockSettingsStore
import com.greengolddog.dayweave.security.AppUnlockAvailability
import com.greengolddog.dayweave.security.MonotonicClock
import com.greengolddog.dayweave.ui.AppLockPresentationGate
import com.greengolddog.dayweave.ui.DayWeaveApp
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AppLockUiPrivacyTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun coldStartLockReplacesEverySensitivePlannerSurface() {
        val controller = AppLockController(
            settingsStore = LockedSettingsStore,
            clock = MonotonicClock { 0L },
        )

        composeRule.setContent {
            DayWeaveApp(
                appLockController = controller,
                onRequestUnlock = {},
                onSetAppLockEnabled = {},
                onSetAppLockTimeout = {},
                onLockNow = {},
                onOpenDeviceSecuritySettings = {},
            )
        }

        composeRule.onNodeWithTag("app_lock_screen").assertIsDisplayed()
        composeRule.onNodeWithText("DayWeave is locked").assertIsDisplayed()
        listOf(
            "Architecture deep work",
            "Weekly planning call",
            "Renew travel insurance",
            "Protect a recovery window",
            "Today",
            "Calendar",
            "Inbox",
            "Assistant",
        ).forEach { forbiddenText ->
            composeRule.onAllNodesWithText(forbiddenText).assertCountEquals(0)
        }
    }

    @Test
    fun lockingDisposesAnAlreadyOpenDialogAndItsPrivateCanary() {
        val controller = AppLockController(
            settingsStore = LockedSettingsStore,
            clock = MonotonicClock { 0L },
        ).also { appLock ->
            appLock.onForegrounded()
            appLock.updateAvailability(AppUnlockAvailability.AVAILABLE)
            val request = requireNotNull(
                appLock.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
            )
            appLock.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS)
        }

        composeRule.setContent {
            val state by controller.state.collectAsState()
            AppLockPresentationGate(
                appLockState = state,
                lockedContent = { Text("Locked replacement") },
                unlockedContent = {
                    var showDialog by remember { mutableStateOf(false) }
                    Button(onClick = { showDialog = true }) { Text("Open private dialog") }
                    if (showDialog) {
                        AlertDialog(
                            onDismissRequest = {},
                            confirmButton = { Text("Keep open") },
                            title = { Text("Quick capture private canary") },
                        )
                    }
                },
            )
        }

        composeRule.onNodeWithText("Open private dialog").performClick()
        composeRule.onNodeWithText("Quick capture private canary").assertIsDisplayed()
        composeRule.runOnIdle(controller::lockNow)

        composeRule.onNodeWithText("Locked replacement").assertIsDisplayed()
        composeRule.onAllNodesWithText("Quick capture private canary").assertCountEquals(0)
        composeRule.onAllNodesWithText("Open private dialog").assertCountEquals(0)
    }

    private object LockedSettingsStore : AppLockSettingsStore {
        override fun load(): AppLockSettingsLoadResult = AppLockSettingsLoadResult.Loaded(
            AppLockSettings(enabled = true),
        )

        override fun save(settings: AppLockSettings): Boolean = true
    }
}
