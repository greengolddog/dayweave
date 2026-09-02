package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.network.GoogleCalendarInboundRole
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState

/**
 * Inbound-only Google Calendar controls for the Android settings surface.
 *
 * The writable server role is intentionally display-only here. Every configuration callback is
 * constrained to [GoogleCalendarInboundRole], so this component cannot request outbound access.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun GoogleCalendarSourcesCard(
    googleAccountState: GoogleAccountState,
    importState: GoogleCalendarImportState,
    onDiscover: (accountId: String) -> Unit,
    onRefreshOrCheck: (accountId: String) -> Unit,
    onConfigure: (
        accountId: String,
        collectionId: String,
        currentRevision: Long,
        role: GoogleCalendarInboundRole,
    ) -> Unit,
    modifier: Modifier = Modifier,
    actionsEnabled: Boolean = true,
) {
    val accounts = googleAccountState.accounts.filter(GoogleAccountSummary::isActiveCalendarAccount)
    val sameCredentialBinding = googleAccountState.configurationId != null &&
        googleAccountState.configurationId == importState.configurationId
    val controlsEnabled = actionsEnabled && sameCredentialBinding &&
        !googleAccountState.isBusy && !importState.isBusy
    val configurationEnabled = controlsEnabled && importState.pendingRecoveryCount == 0

    Card(
        modifier = modifier
            .fillMaxWidth()
            .testTag("google_calendar_sources_card")
            .semantics { stateDescription = importState.message },
    ) {
        ListItem(
            headlineContent = { Text("Calendar sources") },
            supportingContent = {
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(importPhaseLabel(importState.phase))
                    Text(
                        importState.message,
                        style = MaterialTheme.typography.bodySmall,
                        color = importPhaseColor(importState.phase),
                        modifier = Modifier.testTag("google_calendar_import_status"),
                    )
                    if (importState.pendingRecoveryCount > 0) {
                        Text(
                            savedImportLabel(importState.pendingRecoveryCount),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.tertiary,
                            modifier = Modifier.testTag("google_calendar_import_recovery_count"),
                        )
                    }
                }
            },
            leadingContent = {
                Icon(Icons.Outlined.CalendarMonth, contentDescription = null)
            },
            trailingContent = {
                if (googleAccountState.isBusy || importState.isBusy) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(24.dp).testTag("google_calendar_import_progress"),
                    )
                } else {
                    Icon(
                        Icons.Outlined.Sync,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
        )

        if (accounts.isEmpty()) {
            HorizontalDivider()
            Text(
                "Activate a Google account with Calendar access to choose inbound sources.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 14.dp)
                    .testTag("google_calendar_no_active_accounts"),
            )
        } else {
            accounts.forEachIndexed { accountIndex, account ->
                val accountState = importState.accounts[account.id]
                val calendars = accountState?.collections.orEmpty().filter { collection ->
                    collection.accountId == account.id &&
                        collection.kind == RemoteGoogleCollectionKind.CALENDAR
                }
                val checkImport = shouldCheckImport(
                    accountId = account.id,
                    importState = importState,
                    runState = accountState?.run?.state,
                )
                var visibleCalendarCount by rememberSaveable(account.id, calendars.size) {
                    mutableIntStateOf(minOf(CALENDAR_PAGE_SIZE, calendars.size))
                }
                val visibleCalendars = calendars.take(visibleCalendarCount)

                HorizontalDivider()
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 12.dp)
                        .testTag("google_calendar_account_$accountIndex"),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                account.label + if (account.isDefault) " · default" else "",
                                style = MaterialTheme.typography.titleSmall,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                "Inbound only · ${calendars.size} ${calendarCountNoun(calendars.size)}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }

                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                        verticalArrangement = Arrangement.spacedBy(2.dp),
                    ) {
                        TextButton(
                            onClick = { onDiscover(account.id) },
                            enabled = controlsEnabled,
                            modifier = Modifier.testTag("google_calendar_discover_$accountIndex"),
                        ) {
                            Text(if (calendars.isEmpty()) "Discover calendars" else "Discover")
                        }
                        TextButton(
                            onClick = { onRefreshOrCheck(account.id) },
                            enabled = controlsEnabled,
                            modifier = Modifier.testTag("google_calendar_refresh_$accountIndex"),
                        ) {
                            Text(if (checkImport) "Check import" else "Refresh import")
                        }
                    }

                    if (calendars.isEmpty()) {
                        Text(
                            "Discover calendars, then choose whether each one is shown or blocks planning time.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag("google_calendar_empty_$accountIndex"),
                        )
                    } else {
                        visibleCalendars.forEachIndexed { sourceIndex, collection ->
                            if (sourceIndex > 0) HorizontalDivider()
                            CalendarSourceControls(
                                collection = collection,
                                controlsEnabled = configurationEnabled,
                                tagKey = "${accountIndex}_$sourceIndex",
                                onConfigure = { role ->
                                    onConfigure(
                                        account.id,
                                        collection.id,
                                        collection.revision,
                                        role,
                                    )
                                },
                            )
                        }
                        if (visibleCalendarCount < calendars.size) {
                            TextButton(
                                onClick = {
                                    visibleCalendarCount = minOf(
                                        visibleCalendarCount + CALENDAR_PAGE_SIZE,
                                        calendars.size,
                                    )
                                },
                                modifier = Modifier.testTag(
                                    "google_calendar_load_more_$accountIndex",
                                ),
                            ) {
                                Text("Load more calendars")
                            }
                        }
                    }
                }
            }
        }

        Text(
            "Android imports calendars only. Publishing changes back to Google is never enabled from these controls.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp)
                .testTag("google_calendar_inbound_only_notice"),
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CalendarSourceControls(
    collection: GoogleImportCollectionState,
    controlsEnabled: Boolean,
    tagKey: String,
    onConfigure: (GoogleCalendarInboundRole) -> Unit,
) {
    val selectedRole = collection.selectedRole()
    val sourceControlsEnabled = controlsEnabled && !collection.providerDeleted
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag("google_calendar_collection_$tagKey"),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            collection.displayName,
            style = MaterialTheme.typography.bodyLarge,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            collectionModeLabel(collection),
            style = MaterialTheme.typography.bodySmall,
            color = when {
                collection.providerDeleted -> MaterialTheme.colorScheme.error
                collection.syncRole == RemoteGoogleSyncRole.WRITABLE && collection.selected ->
                    MaterialTheme.colorScheme.tertiary
                else -> MaterialTheme.colorScheme.onSurfaceVariant
            },
            modifier = Modifier.testTag("google_calendar_collection_status_$tagKey"),
        )
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            InboundRoleChip(
                label = "Off",
                role = GoogleCalendarInboundRole.OFF,
                selectedRole = selectedRole,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                onConfigure = onConfigure,
            )
            InboundRoleChip(
                label = "Show only",
                role = GoogleCalendarInboundRole.READ_ONLY,
                selectedRole = selectedRole,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                onConfigure = onConfigure,
            )
            InboundRoleChip(
                label = "Block time",
                role = GoogleCalendarInboundRole.BLOCKING,
                selectedRole = selectedRole,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                onConfigure = onConfigure,
            )
        }
    }
}

@Composable
private fun InboundRoleChip(
    label: String,
    role: GoogleCalendarInboundRole,
    selectedRole: GoogleCalendarInboundRole?,
    enabled: Boolean,
    collectionTag: String,
    onConfigure: (GoogleCalendarInboundRole) -> Unit,
) {
    val selected = selectedRole == role
    FilterChip(
        selected = selected,
        onClick = { if (!selected) onConfigure(role) },
        enabled = enabled,
        label = { Text(label) },
        modifier = Modifier
            .testTag("google_calendar_role_${collectionTag}_${role.name.lowercase()}")
            .semantics {
                stateDescription = if (selected) "$label selected" else "$label not selected"
            },
    )
}

private fun GoogleAccountSummary.isActiveCalendarAccount(): Boolean =
    status == "active" && syncEnabled && hasCalendar

private fun GoogleImportCollectionState.selectedRole(): GoogleCalendarInboundRole? = when {
    !selected -> GoogleCalendarInboundRole.OFF
    // Writable is a valid cross-device server state but is never an Android action.
    syncRole == RemoteGoogleSyncRole.WRITABLE -> null
    syncRole == RemoteGoogleSyncRole.READ_ONLY -> GoogleCalendarInboundRole.READ_ONLY
    syncRole == RemoteGoogleSyncRole.BLOCKING -> GoogleCalendarInboundRole.BLOCKING
    else -> null
}

private fun collectionModeLabel(collection: GoogleImportCollectionState): String = when {
    collection.providerDeleted -> "Unavailable · removed from Google Calendar"
    !collection.selected -> "Off · not imported"
    collection.syncRole == RemoteGoogleSyncRole.WRITABLE ->
        "Writable · managed on another device"
    collection.syncRole == RemoteGoogleSyncRole.BLOCKING -> "Blocks planning time"
    else -> "Show only · does not block planning time"
}

private fun shouldCheckImport(
    accountId: String,
    importState: GoogleCalendarImportState,
    runState: RemoteGoogleSyncRunState?,
): Boolean {
    if (runState != null && runState != RemoteGoogleSyncRunState.IDLE) return true
    if (accountId in importState.pendingRecoveryAccountIds) return true
    if (importState.activeAccountId != accountId) return false
    return importState.acceptedRefreshGeneration != null || importState.phase in setOf(
        GoogleCalendarImportPhase.RESPONSE_UNKNOWN,
        GoogleCalendarImportPhase.CHECKING_COMPLETION,
        GoogleCalendarImportPhase.SERVER_BACKOFF,
        GoogleCalendarImportPhase.PERSISTING_CANONICAL_RESULT,
        GoogleCalendarImportPhase.RECOVERY_REQUIRED,
    )
}

private fun importPhaseLabel(phase: GoogleCalendarImportPhase): String = when (phase) {
    GoogleCalendarImportPhase.NOT_CONFIGURED -> "Import not configured"
    GoogleCalendarImportPhase.READY -> "Ready to import"
    GoogleCalendarImportPhase.LOADING_COLLECTIONS -> "Loading calendars"
    GoogleCalendarImportPhase.DISCOVERING_COLLECTIONS -> "Discovering calendars"
    GoogleCalendarImportPhase.CONFIGURING_COLLECTION -> "Saving source settings"
    GoogleCalendarImportPhase.PREPARING_REFRESH,
    GoogleCalendarImportPhase.REQUESTING_REFRESH,
    -> "Starting import"
    GoogleCalendarImportPhase.RESPONSE_UNKNOWN,
    GoogleCalendarImportPhase.RECOVERY_REQUIRED,
    -> "Import needs a safe status check"
    GoogleCalendarImportPhase.CHECKING_COMPLETION -> "Checking import"
    GoogleCalendarImportPhase.SERVER_BACKOFF -> "Waiting for server retry"
    GoogleCalendarImportPhase.PERSISTING_CANONICAL_RESULT -> "Updating your schedule"
    GoogleCalendarImportPhase.COMPLETED -> "Import complete"
    GoogleCalendarImportPhase.AUTH_REQUIRED -> "Authorization required"
    GoogleCalendarImportPhase.OFFLINE -> "Offline · import saved"
    GoogleCalendarImportPhase.ERROR -> "Import needs attention"
}

@Composable
private fun importPhaseColor(phase: GoogleCalendarImportPhase) = when (phase) {
    GoogleCalendarImportPhase.AUTH_REQUIRED,
    GoogleCalendarImportPhase.RECOVERY_REQUIRED,
    GoogleCalendarImportPhase.ERROR,
    -> MaterialTheme.colorScheme.error
    GoogleCalendarImportPhase.RESPONSE_UNKNOWN,
    GoogleCalendarImportPhase.SERVER_BACKOFF,
    GoogleCalendarImportPhase.OFFLINE,
    -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

private fun savedImportLabel(count: Int): String =
    "$count saved ${if (count == 1) "import needs" else "imports need"} checking"

private fun calendarCountNoun(count: Int): String = if (count == 1) "calendar" else "calendars"

private const val CALENDAR_PAGE_SIZE = 50
