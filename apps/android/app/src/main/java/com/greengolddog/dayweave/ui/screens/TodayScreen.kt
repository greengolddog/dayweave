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
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.ui.components.ActiveItemActions
import com.greengolddog.dayweave.ui.components.MetricCard
import com.greengolddog.dayweave.ui.components.ScheduleItemCard
import com.greengolddog.dayweave.sync.CanonicalSyncState
import java.time.LocalDate
import java.time.format.DateTimeFormatter

@Composable
fun TodayScreen(
    state: DayWeaveUiState,
    syncState: CanonicalSyncState,
    onStart: (String) -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onComplete: () -> Unit,
    onSkip: () -> Unit,
    onLater: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val isCurrentPlan = state.isCanonicalPlanCurrent()
    val visibleTimeline = if (isCurrentPlan) state.visibleSchedule else emptyList()
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
                    if (isCurrentPlan) state.scheduleMessage else syncState.message,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        if (!isCurrentPlan) {
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
            ScheduleItemCard(item = item, onStart = { onStart(item.id) })
        }

        if (visibleTimeline.isEmpty() && isCurrentPlan) {
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
