package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.AddTask
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.hasOpenOrPendingExecutionForOccurrence
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.model.isRepresentableMoveLaterSource
import com.greengolddog.dayweave.model.recurrenceIdentityType
import com.greengolddog.dayweave.ui.components.ActiveItemActions
import com.greengolddog.dayweave.ui.components.EnergySignalCard
import com.greengolddog.dayweave.ui.components.MetricCard
import com.greengolddog.dayweave.ui.components.ScheduleItemCard
import com.greengolddog.dayweave.sync.CanonicalSyncState
import java.time.LocalDate
import java.time.format.DateTimeFormatter

@Composable
fun TodayScreen(
    state: DayWeaveUiState,
    syncState: CanonicalSyncState,
    canonicalExecutionActionsEnabled: Boolean,
    onStart: (String) -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onComplete: () -> Unit,
    onSkip: () -> Unit,
    onLater: () -> Unit,
    onSkipScheduled: (String) -> Unit,
    onLaterScheduled: (String) -> Unit,
    onRetryTerminalProjection: (String) -> Unit,
    onKeepLatestItem: (String) -> Unit,
    onEnergyCheckIn: (EnergyLevel) -> Unit,
    onClearManualEnergyCheckIn: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val isCurrentPlan = state.isCanonicalPlanCurrent()
    val isDisplayCurrent = state.isScheduleDisplayCurrent()
    val isLocalPlan = isDisplayCurrent && !isCurrentPlan
    val visibleTimeline = if (isDisplayCurrent) state.visibleSchedule else emptyList()
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 18.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Column(verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(
                    LocalDate.now().format(DateTimeFormatter.ofPattern("EEEE, MMMM d")),
                    style = MaterialTheme.typography.headlineSmall,
                )
                Text(
                    if (
                        !isDisplayCurrent || !isLocalPlan && syncState.phase in setOf(
                            com.greengolddog.dayweave.sync.CanonicalSyncPhase.AUTH_REQUIRED,
                            com.greengolddog.dayweave.sync.CanonicalSyncPhase.OFFLINE,
                            com.greengolddog.dayweave.sync.CanonicalSyncPhase.ERROR,
                        )
                    ) {
                        syncState.message
                    } else {
                        state.scheduleMessage
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        if (!isDisplayCurrent) {
            item {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer,
                    ),
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text("Today’s plan is not available", style = MaterialTheme.typography.titleMedium)
                        Text(
                            state.canonicalPlanningDate()?.let { cachedDate ->
                                "The encrypted plan is from $cachedDate and is hidden until today is recomposed."
                            } ?: "No canonical plan has been composed for today yet.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onErrorContainer,
                        )
                    }
                }
            }
        }

        if (isLocalPlan) {
            item {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.secondaryContainer,
                    ),
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text("Composed on this device", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "This encrypted plan is visible offline. Sync and publish before starting, skipping, or moving canonical work.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }

        val terminalConflict = state.terminalExecutionOutcomes.values.firstOrNull {
            it.session.status in setOf("completed", "skipped") &&
                state.isNewestExecutionForProjection(it.session) &&
                it.canonicalProjectionConflict != null
        }
        if (terminalConflict != null) {
            val canonicalWritePending = state.pendingCanonicalMutation != null
            item {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer,
                    ),
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        val terminalLabel = if (terminalConflict.session.status == "completed") {
                            "completed"
                        } else {
                            "skipped"
                        }
                        val itemTitle = state.canonicalItems.firstOrNull {
                            it.id == terminalConflict.session.itemId
                        }?.title ?: "Deleted or unavailable item"
                        Text(
                            "Execution outcome needs review",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            "$itemTitle was $terminalLabel, but that exact outcome cannot be " +
                                "safely applied to the latest item. " +
                                requireNotNull(terminalConflict.canonicalProjectionConflict),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onErrorContainer,
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(
                                onClick = {
                                    onRetryTerminalProjection(terminalConflict.session.id)
                                },
                                enabled =
                                    terminalConflict.canonicalProjectionRetryAuthorizedAt == null &&
                                        !canonicalWritePending,
                            ) {
                                Text(
                                    if (
                                        terminalConflict.canonicalProjectionRetryAuthorizedAt == null
                                    ) {
                                        "Approve retry"
                                    } else {
                                        "Retry approved"
                                    },
                                )
                            }
                            OutlinedButton(
                                onClick = {
                                    onKeepLatestItem(terminalConflict.session.id)
                                },
                                enabled = !canonicalWritePending,
                            ) {
                                Text(
                                    if (canonicalWritePending) {
                                        "Canonical write pending"
                                    } else {
                                        "Keep latest as new work"
                                    },
                                )
                            }
                        }
                    }
                }
            }
        }

        item {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                MetricCard(
                    value = "${visibleTimeline.count { it.status == com.greengolddog.dayweave.model.ItemStatus.COMPLETED }}/${visibleTimeline.size}",
                    label = "done",
                    modifier = Modifier.weight(1f),
                )
                MetricCard(
                    value = if (isCurrentPlan) "${state.protectedFreeMinutes}m" else "—",
                    label = "protected",
                    modifier = Modifier.weight(1f),
                )
                MetricCard(
                    value = if (isCurrentPlan) state.dayScore.toString() else "—",
                    label = "day score",
                    modifier = Modifier.weight(1f),
                )
            }
        }

        item {
            EnergySignalCard(
                state = state,
                onCheckIn = onEnergyCheckIn,
                onClearManualCheckIn = onClearManualEnergyCheckIn,
            )
        }

        // An already-running cross-midnight session remains resolvable, but stale scheduled work
        // is quarantined and cannot be started.
        val activeItem = state.activeItem
        val activeSession = state.activeSession
        if (activeItem != null && activeSession != null) {
            item {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.62f),
                    ),
                ) {
                    Column(
                        modifier = Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Icon(
                                Icons.Outlined.AutoAwesome,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.primary,
                            )
                            Column {
                                Text(
                                    if (activeSession.isPaused) "Session paused" else "Current focus",
                                    style = MaterialTheme.typography.labelLarge,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                                Text(activeItem.title, style = MaterialTheme.typography.titleLarge)
                                Text(
                                    "${activeSession.elapsedMinutes} minutes recorded · ${activeItem.durationMinutes} planned",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                        }
                        ActiveItemActions(
                            isPaused = activeSession.isPaused,
                            canDefer = activeSession.canonicalExecutionSessionId != null ||
                                activeItem.isMoveLaterEligible(),
                            actionsEnabled = activeItem.canonicalItemId == null ||
                                canonicalExecutionActionsEnabled,
                            onPause = onPause,
                            onResume = onResume,
                            onComplete = onComplete,
                            onSkip = onSkip,
                            onLater = onLater,
                        )
                    }
                }
            }
        }

        item {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text("Your timeline", style = MaterialTheme.typography.titleLarge)
                Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    Icon(
                        Icons.Outlined.Shield,
                        contentDescription = null,
                        tint = if (state.scheduleErrorViolationCount > 0) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.secondary
                        },
                    )
                    Text(
                        when {
                            state.scheduleErrorViolationCount > 0 ->
                                "${state.scheduleErrorViolationCount} hard conflict(s) need review"
                            state.scheduleViolationCount > 0 ->
                                "${state.scheduleViolationCount} planning warning(s)"
                            else -> "No planner conflicts reported"
                        },
                        style = MaterialTheme.typography.labelMedium,
                        color = if (state.scheduleErrorViolationCount > 0) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            }
        }

        if (state.scheduleViolationMessages.isNotEmpty()) {
            item {
                Card {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text("Planner review", style = MaterialTheme.typography.titleSmall)
                        state.scheduleViolationMessages.take(3).forEach { warning ->
                            Text(
                                "• $warning",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }

        items(visibleTimeline, key = { it.id }) { item ->
            val terminalStartBlocked = state.terminalExecutionOutcomes.values.any { outcome ->
                val sameOrigin = outcome.syncOrigin == (
                    state.canonicalSyncOrigin ?: state.canonicalExecutionSyncOrigin
                )
                val exactIdentity = outcome.session.itemId == item.canonicalItemId &&
                    outcome.session.itemRevision == item.canonicalRevision &&
                    outcome.session.occurrenceId == item.occurrenceId &&
                    outcome.session.sessionIndex == item.sessionIndex
                val exactClosedApplies = exactIdentity && (
                    outcome.session.status == "deferred" ||
                        outcome.session.status in setOf("completed", "skipped") &&
                        outcome.canonicalProjectionResolution != "user_kept_latest_item"
                    )
                val unresolvedParentProjection =
                    outcome.session.status in setOf("completed", "skipped") &&
                    state.isNewestExecutionForProjection(outcome.session) &&
                    outcome.requiresCanonicalItemProjection &&
                        outcome.canonicalProjectionRevision == null &&
                        outcome.canonicalProjectionResolution == null &&
                        outcome.session.itemId == item.canonicalItemId
                sameOrigin && (exactClosedApplies || unresolvedParentProjection)
            } || item.canonicalItemId != null && (
                !state.canonicalExecutionHistoryVerified ||
                    state.canonicalExecutionSyncOrigin != state.canonicalSyncOrigin ||
                    state.canonicalExecutionConfigurationId != state.canonicalConfigurationId
                )
            ScheduleItemCard(
                item = item,
                onStart = { onStart(item.id) },
                onLater = if (
                    state.canMoveScheduledLater(item)
                ) {
                    { onLaterScheduled(item.id) }
                } else {
                    null
                },
                onSkip = if (
                    item.canonicalItemId != null && state.canSafelySkipScheduled(item)
                ) {
                    { onSkipScheduled(item.id) }
                } else {
                    null
                },
                canStart = !terminalStartBlocked && (
                    item.canonicalItemId == null ||
                        canonicalExecutionActionsEnabled && state.hasPublishedExecutionAuthority(item)
                ),
                unavailableLabel = when {
                    terminalStartBlocked -> "Needs review"
                    item.canonicalItemId != null && !state.hasPublishedExecutionAuthority(item) ->
                        "Sync to start"
                    else -> "Syncing…"
                },
            )
        }

        if (visibleTimeline.isEmpty() && isDisplayCurrent) {
            item {
                Card {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(18.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Icon(Icons.Outlined.AddTask, contentDescription = null)
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("Your timeline is empty", style = MaterialTheme.typography.titleMedium)
                            Text(
                                "Use Quick capture to add a task, event, habit, routine, goal, or break. Nothing is scheduled until you review it.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }
    }
}

internal fun ScheduleItem.isMoveLaterEligible(): Boolean {
    return isRepresentableMoveLaterSource()
}

internal fun DayWeaveUiState.canSafelySkipScheduled(item: ScheduleItem): Boolean {
    val itemId = item.canonicalItemId ?: return false
    return hasPublishedExecutionAuthority(item) && item.status == ItemStatus.SCHEDULED &&
        item.isMoveLaterEligible() && item.occurrenceId == null && !item.isSplittable &&
        schedule.count { block ->
            block.canonicalItemId == itemId && block.occurrenceId == null
        } == 1 && unscheduledWork.none { work ->
            work.itemId == itemId && work.occurrenceId == null && work.remainingMinutes > 0
        }
}

internal fun DayWeaveUiState.canMoveScheduledLater(item: ScheduleItem): Boolean {
    val itemId = item.canonicalItemId ?: return false
    if (
        !hasPublishedExecutionAuthority(item) ||
        item.status != ItemStatus.SCHEDULED || !item.isMoveLaterEligible()
    ) return false
    item.occurrenceId?.let { occurrenceId ->
        if (hasOpenOrPendingExecutionForOccurrence(occurrenceId)) return false
        val occurrenceBlocks = schedule.filter { block -> block.occurrenceId == occurrenceId }
        if (
            occurrenceBlocks.isEmpty() || occurrenceBlocks.any { block ->
                block.status != ItemStatus.SCHEDULED || !block.isMoveLaterEligible()
            } || unscheduledWork.any { work ->
                work.occurrenceId == occurrenceId && work.remainingMinutes > 0
            }
        ) {
            return false
        }
        val identityType = recurrenceIdentityType(
            recurrenceOccurrenceSources[occurrenceId]?.identityJson,
        )
        return identityType != null && identityType != "custom"
    }
    return !item.isSplittable && schedule.count { block ->
        block.canonicalItemId == itemId && block.occurrenceId == null
    } == 1 && unscheduledWork.none { work ->
        work.itemId == itemId && work.occurrenceId == null && work.remainingMinutes > 0
    }
}
