package com.greengolddog.dayweave.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.ui.theme.BreakColor
import com.greengolddog.dayweave.ui.theme.EventColor
import com.greengolddog.dayweave.ui.theme.GoalColor
import com.greengolddog.dayweave.ui.theme.HabitColor
import com.greengolddog.dayweave.ui.theme.RoutineColor
import com.greengolddog.dayweave.ui.theme.TaskColor

@Composable
fun MetricCard(
    value: String,
    label: String,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier,
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.55f),
        shape = RoundedCornerShape(14.dp),
    ) {
        Column(modifier = Modifier.padding(horizontal = 14.dp, vertical = 11.dp)) {
            Text(value, style = MaterialTheme.typography.titleMedium)
            Text(
                label,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}

@Composable
fun ScheduleItemCard(
    item: ScheduleItem,
    onStart: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val kindColor = item.kind.color()
    Card(
        modifier = modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
    ) {
        Row(
            modifier = Modifier.padding(14.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Column(horizontalAlignment = Alignment.End, modifier = Modifier.width(54.dp)) {
                Text(item.timeRange().substringBefore('–'), style = MaterialTheme.typography.labelLarge)
                Text(
                    "${item.durationMinutes}m",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            Box(
                Modifier
                    .width(5.dp)
                    .height(76.dp)
                    .background(kindColor, RoundedCornerShape(4.dp)),
            )

            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        item.title,
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.titleMedium,
                        textDecoration = if (item.status == ItemStatus.COMPLETED) {
                            TextDecoration.LineThrough
                        } else {
                            null
                        },
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (item.isHardConstraint) {
                        Icon(
                            Icons.Default.Lock,
                            contentDescription = "Hard constraint",
                            modifier = Modifier.size(15.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        item.project ?: item.kind.label,
                        style = MaterialTheme.typography.labelMedium,
                        color = kindColor,
                    )
                    Text("· ${item.energy.label} energy", style = MaterialTheme.typography.labelMedium)
                    if (item.isSplittable) {
                        Icon(
                            Icons.AutoMirrored.Outlined.CallSplit,
                            contentDescription = "Splittable",
                            modifier = Modifier.size(15.dp),
                        )
                    }
                }

                Text(
                    item.note,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )

                if (item.status == ItemStatus.SCHEDULED && item.kind != ItemKind.EVENT) {
                    AssistChip(
                        onClick = onStart,
                        label = { Text("Start") },
                        leadingIcon = { Icon(Icons.Default.PlayArrow, contentDescription = null) },
                    )
                }
            }
        }
    }
}

@Composable
fun ActiveSessionBar(
    item: ScheduleItem,
    session: ActiveSession,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onComplete: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = MaterialTheme.colorScheme.primaryContainer,
        tonalElevation = 5.dp,
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Box(
                modifier = Modifier
                    .size(9.dp)
                    .background(
                        if (session.isPaused) MaterialTheme.colorScheme.tertiary else MaterialTheme.colorScheme.primary,
                        CircleShape,
                    ),
            )
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    if (session.isPaused) "PAUSED" else "FOCUSING · ${session.elapsedMinutes} MIN",
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(item.title, style = MaterialTheme.typography.titleMedium, maxLines = 1)
                session.pauseLabel?.let {
                    Text(it, style = MaterialTheme.typography.bodySmall)
                }
            }
            FilledTonalIconButton(onClick = if (session.isPaused) onResume else onPause) {
                Icon(
                    if (session.isPaused) Icons.Default.PlayArrow else Icons.Default.Pause,
                    contentDescription = if (session.isPaused) "Resume" else "Pause",
                )
            }
            IconButton(onClick = onComplete) {
                Icon(Icons.Default.Check, contentDescription = "Complete")
            }
        }
    }
}

@Composable
fun ActiveItemActions(
    isPaused: Boolean,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onComplete: () -> Unit,
    onSkip: () -> Unit,
    onLater: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Button(onClick = if (isPaused) onResume else onPause, modifier = Modifier.weight(1f)) {
                Icon(if (isPaused) Icons.Default.PlayArrow else Icons.Default.Pause, contentDescription = null)
                Spacer(Modifier.width(8.dp))
                Text(if (isPaused) "Resume" else "Take a break")
            }
            OutlinedButton(onClick = onComplete, modifier = Modifier.weight(1f)) {
                Icon(Icons.Default.Check, contentDescription = null)
                Spacer(Modifier.width(8.dp))
                Text("Complete")
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            OutlinedButton(onClick = onSkip, modifier = Modifier.weight(1f)) { Text("Skipped") }
            OutlinedButton(onClick = onLater, modifier = Modifier.weight(1f)) { Text("Will do later") }
        }
    }
}

@Composable
fun EnergyLabel(item: ScheduleItem) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Outlined.Bolt, contentDescription = null, modifier = Modifier.size(15.dp))
        Text(item.energy.label, style = MaterialTheme.typography.labelMedium)
    }
}

fun ItemKind.color(): Color = when (this) {
    ItemKind.EVENT -> EventColor
    ItemKind.TASK -> TaskColor
    ItemKind.HABIT -> HabitColor
    ItemKind.ROUTINE -> RoutineColor
    ItemKind.GOAL -> GoalColor
    ItemKind.BREAK -> BreakColor
}
