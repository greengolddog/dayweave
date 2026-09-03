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
import com.greengolddog.dayweave.network.ConfigureGoogleCollectionRequest
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleEventDisposition
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState

/**
 * Google Calendar and Tasks source controls plus an explicit generated-schedule review entry.
 *
 * Collection settings mirror the server contract. Marking a source Publish only selects a
 * destination; every external mutation still goes through a separate preview/approval workflow.
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
        request: ConfigureGoogleCollectionRequest,
    ) -> Unit,
    onPublishGeneratedSchedule: () -> Unit = {},
    schedulePublicationHasRecovery: Boolean = false,
    schedulePublicationHasCurrentSchedule: Boolean = true,
    modifier: Modifier = Modifier,
    actionsEnabled: Boolean = true,
    configurationActionsEnabled: Boolean = true,
) {
    val accounts = googleAccountState.accounts.filter(GoogleAccountSummary::isActiveGoogleSourceAccount)
    val sameCredentialBinding = googleAccountState.configurationId != null &&
        googleAccountState.configurationId == importState.configurationId
    val stableGoogleAuthorization = googleAccountState.phase == GoogleAccountPhase.CONNECTED &&
        googleAccountState.authorization == null &&
        googleAccountState.authorizationRecovery == null &&
        !googleAccountState.authorizationRecoveryResetRequired &&
        !googleAccountState.authorizationRecoveryDiscardRequired
    val controlsEnabled = actionsEnabled && configurationActionsEnabled &&
        sameCredentialBinding && stableGoogleAuthorization &&
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

        HorizontalDivider()
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.End,
        ) {
            TextButton(
                onClick = onPublishGeneratedSchedule,
                enabled = actionsEnabled &&
                    (schedulePublicationHasRecovery || schedulePublicationHasCurrentSchedule),
                modifier = Modifier.testTag("google_publish_generated_schedule"),
            ) {
                Text(
                    if (schedulePublicationHasRecovery) {
                        "Review saved publication"
                    } else {
                        "Publish generated schedule"
                    },
                )
            }
        }
        if (!schedulePublicationHasRecovery && !schedulePublicationHasCurrentSchedule) {
            Text(
                "Compose and publish a current schedule before sending it to Google Calendar.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp)
                    .testTag("google_schedule_publication_unavailable"),
            )
        }

        if (accounts.isEmpty()) {
            HorizontalDivider()
            Text(
                "Activate a Google account with Calendar or Tasks access to configure sources.",
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
                                account = account,
                                collection = collection,
                                controlsEnabled = configurationEnabled,
                                tagKey = "${accountIndex}_$sourceIndex",
                                tagPrefix = "google_calendar",
                                onConfigure = { request ->
                                    onConfigure(account.id, collection.id, request)
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
                                account = account,
                                collection = collection,
                                controlsEnabled = configurationEnabled,
                                tagKey = "${accountIndex}_$sourceIndex",
                                tagPrefix = "google_task",
                                onConfigure = { request ->
                                    onConfigure(account.id, collection.id, request)
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
            "Selected sources are imported independently of visibility. Publish requires the full Google write grant; every outbound change still needs a separate exact review.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp)
                .testTag("google_collection_contract_notice"),
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun GoogleSourceControls(
    account: GoogleAccountSummary,
    collection: GoogleImportCollectionState,
    controlsEnabled: Boolean,
    tagKey: String,
    tagPrefix: String,
    onConfigure: (ConfigureGoogleCollectionRequest) -> Unit,
) {
    val selectedMode = collection.selectedMode()
    val sourceControlsEnabled = controlsEnabled && !collection.providerDeleted
    val canPublish = collection.canPublish(account)
    val sourceSettingsEnabled = sourceControlsEnabled &&
        (collection.syncRole != RemoteGoogleSyncRole.WRITABLE || canPublish)
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
            CollectionModeChip(
                label = "Off",
                mode = GoogleCollectionMode.OFF,
                selectedMode = selectedMode,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                tagPrefix = tagPrefix,
                onConfigure = { mode ->
                    onConfigure(collection.requestForMode(mode))
                },
            )
            CollectionModeChip(
                label = if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
                    "Reference"
                } else {
                    "Import"
                },
                mode = GoogleCollectionMode.REFERENCE,
                selectedMode = selectedMode,
                enabled = sourceControlsEnabled,
                collectionTag = tagKey,
                tagPrefix = tagPrefix,
                onConfigure = { mode ->
                    onConfigure(collection.requestForMode(mode))
                },
            )
            if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
                CollectionModeChip(
                    label = "Blocking",
                    mode = GoogleCollectionMode.BLOCKING,
                    selectedMode = selectedMode,
                    enabled = sourceControlsEnabled,
                    collectionTag = tagKey,
                    tagPrefix = tagPrefix,
                    onConfigure = { mode ->
                        onConfigure(collection.requestForMode(mode))
                    },
                )
            }
            CollectionModeChip(
                label = "Publish",
                mode = GoogleCollectionMode.PUBLISH,
                selectedMode = selectedMode,
                enabled = sourceControlsEnabled && canPublish,
                collectionTag = tagKey,
                tagPrefix = tagPrefix,
                onConfigure = { mode ->
                    onConfigure(collection.requestForMode(mode))
                },
            )
            BooleanConfigurationChip(
                label = "Visible",
                selected = collection.visible,
                enabled = sourceSettingsEnabled,
                tag = "${tagPrefix}_visible_$tagKey",
                onToggle = { visible ->
                    onConfigure(collection.requestWith(visible = visible))
                },
            )
        }
        if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
            Text("Calendar event policy", style = MaterialTheme.typography.labelMedium)
            CalendarDispositionControl(
                label = "Confirmed busy",
                current = collection.calendarPolicy.confirmedBusy,
                enabled = sourceSettingsEnabled,
                tagPrefix = "${tagPrefix}_policy_${tagKey}_confirmed_busy",
                onChange = { disposition ->
                    onConfigure(
                        collection.requestWith(
                            calendarPolicy = collection.calendarPolicy.copy(
                                confirmedBusy = disposition,
                            ),
                        ),
                    )
                },
            )
            CalendarDispositionControl(
                label = "Tentative",
                current = collection.calendarPolicy.tentative,
                enabled = sourceSettingsEnabled,
                tagPrefix = "${tagPrefix}_policy_${tagKey}_tentative",
                onChange = { disposition ->
                    onConfigure(
                        collection.requestWith(
                            calendarPolicy = collection.calendarPolicy.copy(
                                tentative = disposition,
                            ),
                        ),
                    )
                },
            )
            CalendarDispositionControl(
                label = "Free",
                current = collection.calendarPolicy.free,
                enabled = sourceSettingsEnabled,
                tagPrefix = "${tagPrefix}_policy_${tagKey}_free",
                onChange = { disposition ->
                    onConfigure(
                        collection.requestWith(
                            calendarPolicy = collection.calendarPolicy.copy(free = disposition),
                        ),
                    )
                },
            )
            CalendarDispositionControl(
                label = "All-day",
                current = collection.calendarPolicy.allDay,
                enabled = sourceSettingsEnabled,
                tagPrefix = "${tagPrefix}_policy_${tagKey}_all_day",
                onChange = { disposition ->
                    onConfigure(
                        collection.requestWith(
                            calendarPolicy = collection.calendarPolicy.copy(allDay = disposition),
                        ),
                    )
                },
            )
            if (collection.syncRole == RemoteGoogleSyncRole.WRITABLE && collection.selected) {
                Text("Publish event types", style = MaterialTheme.typography.labelMedium)
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    BooleanConfigurationChip(
                        label = "All-day",
                        selected = collection.calendarPolicy.publishAllDay,
                        enabled = sourceControlsEnabled && canPublish,
                        tag = "${tagPrefix}_publish_${tagKey}_all_day",
                        onToggle = { value ->
                            onConfigure(
                                collection.requestWith(
                                    calendarPolicy = collection.calendarPolicy.copy(
                                        publishAllDay = value,
                                    ),
                                ),
                            )
                        },
                    )
                    BooleanConfigurationChip(
                        label = "Tentative",
                        selected = collection.calendarPolicy.publishTentative,
                        enabled = sourceControlsEnabled && canPublish,
                        tag = "${tagPrefix}_publish_${tagKey}_tentative",
                        onToggle = { value ->
                            onConfigure(
                                collection.requestWith(
                                    calendarPolicy = collection.calendarPolicy.copy(
                                        publishTentative = value,
                                    ),
                                ),
                            )
                        },
                    )
                    BooleanConfigurationChip(
                        label = "Free",
                        selected = collection.calendarPolicy.publishFree,
                        enabled = sourceControlsEnabled && canPublish,
                        tag = "${tagPrefix}_publish_${tagKey}_free",
                        onToggle = { value ->
                            onConfigure(
                                collection.requestWith(
                                    calendarPolicy = collection.calendarPolicy.copy(
                                        publishFree = value,
                                    ),
                                ),
                            )
                        },
                    )
                }
            }
        }
        if (collection.syncRole == RemoteGoogleSyncRole.WRITABLE && !canPublish) {
            Text(
                "This source no longer has the write grant required for Publish. Choose a non-Publish mode to change its other settings.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
            )
        } else if (
            collection.kind == RemoteGoogleCollectionKind.CALENDAR &&
            collection.providerAccessRole !in setOf("owner", "writer")
        ) {
            Text(
                "Google reports read-only access. Publish needs owner or writer access to this calendar.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else if (
            collection.kind == RemoteGoogleCollectionKind.CALENDAR &&
            !account.hasCalendarWriteScope
        ) {
            Text(
                "Enable the full Google Calendar grant before choosing Publish.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else if (
            collection.kind == RemoteGoogleCollectionKind.TASK_LIST &&
            !account.hasTasksWriteScope
        ) {
            Text(
                "Enable the full Google Tasks grant before choosing Publish.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun CollectionModeChip(
    label: String,
    mode: GoogleCollectionMode,
    selectedMode: GoogleCollectionMode?,
    enabled: Boolean,
    collectionTag: String,
    tagPrefix: String,
    onConfigure: (GoogleCollectionMode) -> Unit,
) {
    val selected = selectedMode == mode
    FilterChip(
        selected = selected,
        onClick = { if (!selected) onConfigure(mode) },
        enabled = enabled,
        label = { Text(label) },
        modifier = Modifier
            .testTag("${tagPrefix}_role_${collectionTag}_${mode.tagValue}")
            .semantics {
                stateDescription = if (selected) "$label selected" else "$label not selected"
            },
    )
}

@Composable
private fun BooleanConfigurationChip(
    label: String,
    selected: Boolean,
    enabled: Boolean,
    tag: String,
    onToggle: (Boolean) -> Unit,
) {
    FilterChip(
        selected = selected,
        onClick = { onToggle(!selected) },
        enabled = enabled,
        label = { Text(label) },
        modifier = Modifier.testTag(tag).semantics {
            stateDescription = if (selected) "$label enabled" else "$label disabled"
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CalendarDispositionControl(
    label: String,
    current: RemoteGoogleEventDisposition,
    enabled: Boolean,
    tagPrefix: String,
    onChange: (RemoteGoogleEventDisposition) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(label, style = MaterialTheme.typography.labelSmall)
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            RemoteGoogleEventDisposition.entries.forEach { disposition ->
                val optionLabel = when (disposition) {
                    RemoteGoogleEventDisposition.IGNORE -> "Ignore"
                    RemoteGoogleEventDisposition.VISIBLE_NONBLOCKING -> "Reference"
                    RemoteGoogleEventDisposition.BLOCKING -> "Blocking"
                }
                val selected = disposition == current
                FilterChip(
                    selected = selected,
                    onClick = { if (!selected) onChange(disposition) },
                    enabled = enabled,
                    label = { Text(optionLabel) },
                    modifier = Modifier
                        .testTag("${tagPrefix}_${disposition.tagValue}")
                        .semantics {
                            stateDescription = if (selected) {
                                "$label $optionLabel selected"
                            } else {
                                "$label $optionLabel not selected"
                            }
                        },
                )
            }
        }
    }
}

private fun GoogleAccountSummary.isActiveGoogleSourceAccount(): Boolean =
    status == "active" && syncEnabled && (hasCalendar || hasTasks)

private enum class GoogleCollectionMode(val tagValue: String) {
    OFF("off"),
    REFERENCE("read_only"),
    BLOCKING("blocking"),
    PUBLISH("writable"),
}

private val RemoteGoogleEventDisposition.tagValue: String
    get() = when (this) {
        RemoteGoogleEventDisposition.IGNORE -> "ignore"
        RemoteGoogleEventDisposition.VISIBLE_NONBLOCKING -> "reference"
        RemoteGoogleEventDisposition.BLOCKING -> "blocking"
    }

private fun GoogleImportCollectionState.selectedMode(): GoogleCollectionMode? = when {
    !selected -> GoogleCollectionMode.OFF
    syncRole == RemoteGoogleSyncRole.READ_ONLY -> GoogleCollectionMode.REFERENCE
    syncRole == RemoteGoogleSyncRole.BLOCKING && kind == RemoteGoogleCollectionKind.CALENDAR ->
        GoogleCollectionMode.BLOCKING
    syncRole == RemoteGoogleSyncRole.WRITABLE -> GoogleCollectionMode.PUBLISH
    else -> null
}

private fun GoogleImportCollectionState.canPublish(account: GoogleAccountSummary): Boolean =
    when (kind) {
        RemoteGoogleCollectionKind.CALENDAR ->
            account.hasCalendarWriteScope && providerAccessRole in setOf("owner", "writer")
        RemoteGoogleCollectionKind.TASK_LIST -> account.hasTasksWriteScope
    }

private fun GoogleImportCollectionState.requestForMode(
    mode: GoogleCollectionMode,
): ConfigureGoogleCollectionRequest = when (mode) {
    GoogleCollectionMode.OFF -> requestWith(
        selected = false,
        syncRole = RemoteGoogleSyncRole.READ_ONLY,
    )
    GoogleCollectionMode.REFERENCE -> requestWith(
        selected = true,
        syncRole = RemoteGoogleSyncRole.READ_ONLY,
    )
    GoogleCollectionMode.BLOCKING -> requestWith(
        selected = true,
        syncRole = RemoteGoogleSyncRole.BLOCKING,
    )
    GoogleCollectionMode.PUBLISH -> requestWith(
        selected = true,
        syncRole = RemoteGoogleSyncRole.WRITABLE,
    )
}

private fun GoogleImportCollectionState.requestWith(
    selected: Boolean = this.selected,
    visible: Boolean = this.visible,
    syncRole: RemoteGoogleSyncRole = this.syncRole,
    calendarPolicy: RemoteGoogleCalendarPolicy = this.calendarPolicy,
): ConfigureGoogleCollectionRequest {
    val outboundSafePolicy = if (
        kind == RemoteGoogleCollectionKind.TASK_LIST ||
        syncRole != RemoteGoogleSyncRole.WRITABLE
    ) {
        calendarPolicy.withoutPublication()
    } else {
        calendarPolicy
    }
    return ConfigureGoogleCollectionRequest(
        expectedRevision = revision,
        kind = kind,
        selected = selected,
        visible = visible,
        syncRole = syncRole,
        calendarPolicy = outboundSafePolicy,
    )
}

private fun collectionModeLabel(collection: GoogleImportCollectionState): String = when {
    collection.providerDeleted -> when (collection.kind) {
        RemoteGoogleCollectionKind.CALENDAR -> "Unavailable · removed from Google Calendar"
        RemoteGoogleCollectionKind.TASK_LIST -> "Unavailable · removed from Google Tasks"
    }
    !collection.selected -> "Off · not imported"
    collection.syncRole == RemoteGoogleSyncRole.WRITABLE -> when (collection.kind) {
        RemoteGoogleCollectionKind.CALENDAR -> "Publish · writable Calendar destination"
        RemoteGoogleCollectionKind.TASK_LIST -> "Publish · writable Tasks destination"
    }
    collection.syncRole == RemoteGoogleSyncRole.BLOCKING -> "Blocks planning time"
    collection.kind == RemoteGoogleCollectionKind.TASK_LIST -> "Imported to Inbox"
    else -> "Reference · does not block planning time"
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
    "$calendarCount ${if (calendarCount == 1) "calendar" else "calendars"} · " +
        "$taskListCount ${if (taskListCount == 1) "task list" else "task lists"}"

private const val CALENDAR_PAGE_SIZE = 50
private const val TASK_LIST_PAGE_SIZE = 50
