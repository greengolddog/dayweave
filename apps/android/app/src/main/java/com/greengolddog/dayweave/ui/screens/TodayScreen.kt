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
import java.time.LocalDate
import java.time.format.DateTimeFormatter

@Composable
fun TodayScreen(
    state: DayWeaveUiState,
    onStart: (String) -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onComplete: () -> Unit,
    onSkip: () -> Unit,
    onLater: () -> Unit,
    modifier: Modifier = Modifier,
) {
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
                    state.scheduleMessage,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        item {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                MetricCard(
                    value = "${state.completedCount}/${state.schedule.size}",
                    label = "done",
                    modifier = Modifier.weight(1f),
                )
                MetricCard(
                    value = "${state.protectedFreeMinutes}m",
                    label = "protected",
                    modifier = Modifier.weight(1f),
                )
                MetricCard(
                    value = state.dayScore.toString(),
                    label = "day score",
                    modifier = Modifier.weight(1f),
                )
            }
        }

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
                        tint = MaterialTheme.colorScheme.secondary,
                    )
                    Text(
                        "Hard limits protected",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }

        items(state.visibleSchedule, key = { it.id }) { item ->
            ScheduleItemCard(item = item, onStart = { onStart(item.id) })
        }
    }
}
