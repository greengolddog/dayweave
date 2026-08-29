package com.greengolddog.dayweave.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Fingerprint
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.LockOpen
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.security.AppLockNotice
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppLockTimeout
import com.greengolddog.dayweave.security.AppUnlockAvailability

@Composable
fun AppLockedScreen(
    state: AppLockState,
    onUnlock: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
) {
    Surface(
        modifier = Modifier
            .fillMaxSize()
            .testTag("app_lock_screen")
            .semantics { paneTitle = "DayWeave locked" },
        color = MaterialTheme.colorScheme.background,
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 32.dp, vertical = 48.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Icon(
                imageVector = Icons.Outlined.Lock,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
                modifier = Modifier.size(56.dp),
            )
            Text(
                text = "DayWeave is locked",
                style = MaterialTheme.typography.headlineSmall,
                modifier = Modifier.padding(top = 20.dp),
            )
            Text(
                text = "Your schedule, tasks, calendar, assistant, and item titles are hidden.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 10.dp),
            )
            Text(
                text = lockedStatusText(state),
                style = MaterialTheme.typography.bodySmall,
                color = if (state.notice == null) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.error
                },
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 16.dp),
            )
            Button(
                onClick = onUnlock,
                enabled = !state.isAuthenticationBusy &&
                    state.availability == AppUnlockAvailability.AVAILABLE,
                modifier = Modifier
                    .padding(top = 24.dp)
                    .testTag("app_lock_unlock_button"),
            ) {
                if (state.isAuthenticationBusy) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        strokeWidth = 2.dp,
                    )
                } else {
                    Icon(Icons.Outlined.Fingerprint, contentDescription = null)
                }
                Text("Unlock", modifier = Modifier.padding(start = 8.dp))
            }
            if (
                state.availability == AppUnlockAvailability.NOT_ENROLLED ||
                state.availability == AppUnlockAvailability.UNAVAILABLE ||
                state.availability == AppUnlockAvailability.TEMPORARILY_UNAVAILABLE
            ) {
                TextButton(
                    onClick = onOpenDeviceSecuritySettings,
                    modifier = Modifier.testTag("app_lock_device_settings_button"),
                ) {
                    Text("Open device security settings")
                }
            }
        }
    }
}

@Composable
fun AppLockSettingsCard(
    state: AppLockState,
    onSetEnabled: (Boolean) -> Unit,
    onSetTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
) {
    var timeoutMenuExpanded by remember { mutableStateOf(false) }
    val canEnable = state.availability == AppUnlockAvailability.AVAILABLE

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("app_lock_settings_card")
            .semantics {
                stateDescription = if (state.settings.enabled) "App lock on" else "App lock off"
            },
    ) {
        ListItem(
            headlineContent = { Text("App lock") },
            supportingContent = {
                Text(
                    if (state.settings.enabled) {
                        "Locks ${state.settings.timeout.backgroundDescription}"
                    } else {
                        "Protect DayWeave with your device screen lock or biometrics"
                    },
                )
            },
            leadingContent = {
                Icon(
                    if (state.settings.enabled) Icons.Outlined.Lock else Icons.Outlined.LockOpen,
                    contentDescription = null,
                )
            },
            trailingContent = {
                if (state.isAuthenticationBusy) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
                } else {
                    Switch(
                        checked = state.settings.enabled,
                        onCheckedChange = onSetEnabled,
                        enabled = state.settings.enabled || canEnable,
                        modifier = Modifier.testTag("app_lock_toggle"),
                    )
                }
            },
        )

        if (state.settings.enabled) {
            Text(
                "Turning app lock off requires device verification again.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }

        if (!canEnable) {
            Text(
                text = availabilitySettingsText(state.availability),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
            )
            TextButton(onClick = onOpenDeviceSecuritySettings) {
                Text(
                    if (state.settings.enabled) {
                        "Open device security settings"
                    } else {
                        "Set up device unlock"
                    },
                )
            }
        }

        state.notice?.let { notice ->
            Text(
                text = noticeText(notice),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
            )
        }

        if (state.settings.enabled) {
            HorizontalDivider()
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text("Lock after leaving", style = MaterialTheme.typography.bodyLarge)
                    Text(
                        "The planner is also locked on every cold start.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Box {
                    OutlinedButton(
                        onClick = { timeoutMenuExpanded = true },
                        enabled = !state.isAuthenticationBusy,
                        modifier = Modifier.testTag("app_lock_timeout_button"),
                    ) {
                        Text(state.settings.timeout.label)
                    }
                    DropdownMenu(
                        expanded = timeoutMenuExpanded,
                        onDismissRequest = { timeoutMenuExpanded = false },
                    ) {
                        AppLockTimeout.entries.forEach { timeout ->
                            DropdownMenuItem(
                                text = { Text(timeout.label) },
                                onClick = {
                                    timeoutMenuExpanded = false
                                    onSetTimeout(timeout)
                                },
                            )
                        }
                    }
                }
            }
            TextButton(
                onClick = onLockNow,
                enabled = !state.isAuthenticationBusy,
                modifier = Modifier.testTag("app_lock_now_button"),
            ) {
                Text("Lock now")
            }
        }
    }
}

private fun lockedStatusText(state: AppLockState): String = when (state.notice) {
    AppLockNotice.SETTINGS_RECOVERY_REQUIRED ->
        "App lock settings need secure recovery. Authenticate to repair them without changing your planner."
    AppLockNotice.SETTINGS_SAVE_FAILED ->
        "App lock could not save its secure state. Your planner remains locked and unchanged."
    AppLockNotice.AUTHENTICATION_CANCELLED -> "Unlock was cancelled."
    AppLockNotice.AUTHENTICATION_LOCKED_OUT ->
        "Device authentication is temporarily locked. Use your device credential or try again later."
    AppLockNotice.AUTHENTICATION_ERROR -> "Device authentication could not be completed."
    null -> when (state.availability) {
        AppUnlockAvailability.UNKNOWN -> "Checking device authentication…"
        AppUnlockAvailability.AVAILABLE -> if (state.isAuthenticationBusy) {
            "Waiting for device verification…"
        } else {
            "Unlock with your device screen lock or biometrics."
        }
        AppUnlockAvailability.NOT_ENROLLED ->
            "Set up a PIN, pattern, password, face, or fingerprint before unlocking."
        AppUnlockAvailability.TEMPORARILY_UNAVAILABLE ->
            "Device authentication is temporarily unavailable."
        AppUnlockAvailability.UNAVAILABLE ->
            "Compatible device authentication is unavailable."
    }
}

private fun availabilitySettingsText(availability: AppUnlockAvailability): String =
    when (availability) {
        AppUnlockAvailability.UNKNOWN -> "Checking device authentication availability."
        AppUnlockAvailability.NOT_ENROLLED ->
            "Set up a PIN, pattern, password, face, or fingerprint first."
        AppUnlockAvailability.TEMPORARILY_UNAVAILABLE ->
            "Device authentication is temporarily unavailable."
        AppUnlockAvailability.UNAVAILABLE ->
            "This device cannot currently provide a compatible unlock method."
        AppUnlockAvailability.AVAILABLE -> "Device authentication is ready."
    }

private fun noticeText(notice: AppLockNotice): String = when (notice) {
    AppLockNotice.SETTINGS_RECOVERY_REQUIRED -> "App lock settings require secure recovery."
    AppLockNotice.SETTINGS_SAVE_FAILED -> "The app lock setting could not be saved."
    AppLockNotice.AUTHENTICATION_CANCELLED -> "Authentication was cancelled."
    AppLockNotice.AUTHENTICATION_LOCKED_OUT -> "Device authentication is temporarily locked."
    AppLockNotice.AUTHENTICATION_ERROR -> "Device authentication could not be completed."
}
