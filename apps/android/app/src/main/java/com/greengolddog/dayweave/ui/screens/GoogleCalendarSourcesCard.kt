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
import com.greengolddog.dayweave.network.GoogleInboundCollectionRole
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState

/**
 * Inbound-only Google Calendar and Tasks controls for the Android settings surface.
 *
 * The writable server role is intentionally display-only here. Every configuration callback is
 * constrained to [GoogleInboundCollectionRole], so this component cannot request outbound access.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun GoogleSourcesCard(
    googleAccountState: GoogleAccountState,
    importState: GoogleCalendarImportState,
    onDiscover: (accountId: String) -> Unit,
    onRefreshOrCheck: (accountId: String) -> Unit,
    onConfigure: (
        accountId: String,
        collectionId: String,
        currentRevision: Long,
        kind: RemoteGoogleCollectionKind,
        role: GoogleInboundCollectionRole,
    ) -> Unit,
    modifier: Modifier = Modifier,
    actionsEnabled: Boolean = true,
) {
    val accounts = googleAccountState.accounts.filter(GoogleAccountSummary::isActiveGoogleSourceAccount)
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
            headlineContent = { Text("Google sources") },
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
                "Activate a Google account with Calendar or Tasks access to choose inbound sources.",
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
                val taskLists = accountState?.collections.orEmpty().filter { collection ->
                    collection.accountId == account.id &&
                        collection.kind == RemoteGoogleCollectionKind.TASK_LIST
                }
                val checkImport = shouldCheckImport(
                    accountId = account.id,
                    importState = importState,
                    runState = accountState?.run?.state,
                )
                var visibleCalendarCount by rememberSaveable(
                    account.id,
                    "calendar",
                    calendars.size,
                ) {
                    mutableIntStateOf(minOf(CALENDAR_PAGE_SIZE, calendars.size))
                }
                val visibleCalendars = calendars.take(visibleCalendarCount)
                var visibleTaskListCount by rememberSaveable(
                    account.id,
                    "task_list",
                    taskLists.size,
                ) {
                    mutableIntStateOf(minOf(TASK_LIST_PAGE_SIZE, taskLists.size))
                }
                val visibleTaskLists = taskLists.take(visibleTaskListCount)

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
                                sourceCountLabel(calendars.size, taskLists.size),
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
                            Text(
                                if (calendars.isEmpty() && taskLists.isEmpty()) {
                                    "Discover sources"
                                } else {
                                    "Discover"
                                },
                            )
                        }
                        TextButton(
                            onClick = { onRefreshOrCheck(account.id) },
                            enabled = controlsEnabled,
                            modifier = Modifier.testTag("google_calendar_refresh_$accountIndex"),
                        ) {
                            Text(if (checkImport) "Check import" else "Refresh import")
                        }
                    }

                    if (calendars.isEmpty() && taskLists.isEmpty()) {
                        Text(
                            "Discover calendars and task lists, then choose which sources DayWeave imports.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag("google_calendar_empty_$accountIndex"),
                        )
                    } else {
                        if (calendars.isNotEmpty()) {
                            Text("Calendars", style = MaterialTheme.typography.labelLarge)
                        }
                        visibleCalendars.forEachIndexed { sourceIndex, collection ->
                            if (sourceIndex > 0) HorizontalDivider()
                            GoogleSourceControls(
                                collection = collection,
                                controlsEnabled = configurationEnabled,
                                tagKey = "${accountIndex}_$sourceIndex",
                                tagPrefix = "google_calendar",
                                onConfigure = { role ->
                                    onConfigure(
                                        account.id,
                                        collection.id,
                                        collection.revision,
                                        collection.kind,
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
                        if (taskLists.isNotEmpty()) {
                            if (calendars.isNotEmpty()) HorizontalDivider()
                            Text("Task lists", style = MaterialTheme.typography.labelLarge)
                        }
                        visibleTaskLists.forEachIndexed { sourceIndex, collection ->
                            if (sourceIndex > 0) HorizontalDivider()
                            GoogleSourceControls(
                                collection = collection,
                                controlsEnabled = configurationEnabled,
                                tagKey = "${accountIndex}_$sourceIndex",
                                tagPrefix = "google_task",
                                onConfigure = { role ->
                                    onConfigure(
                                        account.id,
                                        collection.id,
                                        collection.revision,
                                        collection.kind,
                                        role,
                                    )
                                },
                            )
                        }
                        if (visibleTaskListCount < taskLists.size) {
                            TextButton(
                                onClick = {
                                    visibleTaskListCount = minOf(
                                        visibleTaskListCount + TASK_LIST_PAGE_SIZE,
                                        taskLists.size,
                                    )
                                },
                                modifier = Modifier.testTag(
                                    "google_task_load_more_$accountIndex",
                                ),
                            ) {
                                Text("Load more task lists")
                            }
                        }
                    }
                }
            }
        }

        Text(
            "Android imports calendars and task lists only. Publishing changes back to Google is never enabled from these controls.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp)
                .testTag("google_calendar_inbound_only_notice"),
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun GoogleSourceControls(
    collection: GoogleImportCollectionState,
    controlsEnabled: Boolean,
    tagKey: String,
    tagPrefix: String,
    onConfigure: (GoogleInboundCollectionRole) -> Unit,
) {
    val selectedRole = collection.selectedRole()
    val sourceControlsEnabled = controlsEnabled && !collection.providerDeleted &&
        collection.syncRole != RemoteGoogleSyncRole.WRITABLE
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag("${tagPrefix}_collection_$tagKey"),
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
            modifier = Modifier.testTag("${tagPrefix}_collection_status_$tagKey"),
        )
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            InboundRoleChip(
                label = "Off",
                role = GoogleInboundCollectionRole.OFF,
                selectedRole = selectedRole,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                tagPrefix = tagPrefix,
                onConfigure = onConfigure,
            )
            InboundRoleChip(
                label = if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
                    "Show only"
                } else {
                    "Import"
                },
                role = GoogleInboundCollectionRole.READ_ONLY,
                selectedRole = selectedRole,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                tagPrefix = tagPrefix,
                onConfigure = onConfigure,
            )
            if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
                InboundRoleChip(
                    label = "Block time",
                    role = GoogleInboundCollectionRole.BLOCKING,
                    selectedRole = selectedRole,
                    enabled = sourceControlsEnabled,
                    collectionTag = tagKey,
                    tagPrefix = tagPrefix,
                    onConfigure = onConfigure,
                )
            }
        }
    }
}

@Composable
private fun InboundRoleChip(
    label: String,
    role: GoogleInboundCollectionRole,
    selectedRole: GoogleInboundCollectionRole?,
    enabled: Boolean,
    collectionTag: String,
    tagPrefix: String,
    onConfigure: (GoogleInboundCollectionRole) -> Unit,
) {
    val selected = selectedRole == role
    FilterChip(
        selected = selected,
        onClick = { if (!selected) onConfigure(role) },
        enabled = enabled,
        label = { Text(label) },
        modifier = Modifier
            .testTag("${tagPrefix}_role_${collectionTag}_${role.name.lowercase()}")
            .semantics {
                stateDescription = if (selected) "$label selected" else "$label not selected"
            },
    )
}

private fun GoogleAccountSummary.isActiveGoogleSourceAccount(): Boolean =
    status == "active" && syncEnabled && (hasCalendar || hasTasks)

private fun GoogleImportCollectionState.selectedRole(): GoogleInboundCollectionRole? = when {
    !selected -> GoogleInboundCollectionRole.OFF
    // Writable is a valid cross-device server state but is never an Android action.
    syncRole == RemoteGoogleSyncRole.WRITABLE -> null
    syncRole == RemoteGoogleSyncRole.READ_ONLY -> GoogleInboundCollectionRole.READ_ONLY
    syncRole == RemoteGoogleSyncRole.BLOCKING && kind == RemoteGoogleCollectionKind.CALENDAR ->
        GoogleInboundCollectionRole.BLOCKING
    else -> null
}

private fun collectionModeLabel(collection: GoogleImportCollectionState): String = when {
    collection.providerDeleted -> when (collection.kind) {
        RemoteGoogleCollectionKind.CALENDAR -> "Unavailable · removed from Google Calendar"
        RemoteGoogleCollectionKind.TASK_LIST -> "Unavailable · removed from Google Tasks"
    }
    !collection.selected -> "Off · not imported"
    collection.syncRole == RemoteGoogleSyncRole.WRITABLE ->
        "Writable · managed on another device"
    collection.syncRole == RemoteGoogleSyncRole.BLOCKING -> "Blocks planning time"
    collection.kind == RemoteGoogleCollectionKind.TASK_LIST -> "Imported to Inbox"
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
    GoogleCalendarImportPhase.LOADING_COLLECTIONS -> "Loading Google sources"
    GoogleCalendarImportPhase.DISCOVERING_COLLECTIONS -> "Discovering Google sources"
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

private fun sourceCountLabel(calendarCount: Int, taskListCount: Int): String =
    "Inbound only · $calendarCount ${if (calendarCount == 1) "calendar" else "calendars"} · " +
        "$taskListCount ${if (taskListCount == 1) "task list" else "task lists"}"

private const val CALENDAR_PAGE_SIZE = 50
private const val TASK_LIST_PAGE_SIZE = 50
