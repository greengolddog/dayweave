package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.PrivacyTip
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.unit.dp
import android.text.format.DateFormat
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ScheduleDisplayHorizon
import com.greengolddog.dayweave.model.ScheduleItemPresentationSlice
import com.greengolddog.dayweave.ui.components.color
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeFormatterBuilder
import java.time.format.FormatStyle
import java.time.format.TextStyle
import java.time.temporal.ChronoUnit
import java.util.Locale

@Composable
fun CalendarScreen(
    state: DayWeaveUiState,
    reference: Instant,
    currentZone: ZoneId,
    modifier: Modifier = Modifier,
) {
    val use24HourFormat = DateFormat.is24HourFormat(LocalContext.current)
    val isCurrentPlan = state.isCanonicalPlanCurrent(reference, currentZone)
    val isDisplayCurrent = state.isScheduleDisplayCurrent(reference, currentZone)
    val isPublishedReplica = state.isPublishedScheduleDisplayCurrent(reference, currentZone)
    val isLocalPlan = isDisplayCurrent && !isPublishedReplica
    val isReadOnlyPublishedReplica = isPublishedReplica && !isCurrentPlan
    val displayHorizon = if (isDisplayCurrent) {
        state.scheduleDisplayHorizon(reference, currentZone)
    } else {
        null
    }
    val visibleTimeline = if (isDisplayCurrent) {
        state.visibleScheduleSlicesForFirmHorizon(reference, currentZone)
    } else {
        emptyList()
    }
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column {
                    Text("Firm plan", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        if (!isDisplayCurrent) {
                            "Cached schedule is stale and hidden until the firm horizon is recomposed"
                        } else if (displayHorizon == null) {
                            "No firm plan yet · compose after capturing or syncing work"
                        } else if (isReadOnlyPublishedReplica) {
                            "Published ${state.schedulePlanningZoneId.orEmpty()} horizon · " +
                                "read-only in this device time zone"
                        } else if (isLocalPlan) {
                            "On-device firm plan · sync before canonical actions"
                        } else if (isCurrentPlan) {
                            "Canonical firm-horizon preview · Google connection is configured separately"
                        } else {
                            "Firm-horizon preview"
                        },
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Icon(
                    Icons.Outlined.CalendarMonth,
                    contentDescription = "Firm schedule preview",
                    tint = MaterialTheme.colorScheme.secondary,
                )
            }
        }

        displayHorizon?.let { horizon ->
            item { FirmHorizonStrip(horizon, reference) }
        }

        item {
            Surface(
                color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
                shape = MaterialTheme.shapes.large,
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(14.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Outlined.Lock, contentDescription = null)
                    Column {
                        Text(
                            "2-hour near-term stability",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            "Assignments entirely inside this two-hour window are pinned; longer " +
                                "split plans use soft stability.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }

        item {
            Text("Firm horizon", style = MaterialTheme.typography.titleLarge)
        }

        if (visibleTimeline.isEmpty()) {
            item {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.surface,
                    ),
                ) {
                    Text(
                        if (displayHorizon != null) {
                            "No scheduled blocks in this firm horizon."
                        } else if (isDisplayCurrent) {
                            "Compose a schedule to create the first firm horizon."
                        } else {
                            "Recompose the schedule to reveal the firm horizon."
                        },
                        modifier = Modifier.padding(16.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        } else {
            items(
                items = visibleTimeline,
                key = ::firmHorizonTimelineKey,
            ) { slice ->
                val item = slice.item
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.surface,
                    ),
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Text(
                                firmHorizonTimelineLabel(
                                    slice = slice,
                                    timezone = displayHorizon?.timezone,
                                    use24HourFormat = use24HourFormat,
                            ),
                            style = MaterialTheme.typography.labelMedium,
                            modifier = Modifier.weight(0.3f),
                        )
                        Box(
                            Modifier
                                .weight((slice.durationMinutes / 30f).coerceIn(0.4f, 3f))
                                .height(25.dp)
                                .background(item.kind.color(), MaterialTheme.shapes.small),
                        )
                        Text(
                            listOfNotNull(item.title, slice.continuationLabel)
                                .joinToString(" · "),
                            modifier = Modifier.weight(1f),
                            style = MaterialTheme.typography.bodySmall,
                            maxLines = 1,
                        )
                        if (item.isSensitive) {
                            Icon(
                                Icons.Outlined.PrivacyTip,
                                contentDescription = "Sensitive item",
                                modifier = Modifier.size(16.dp),
                                tint = MaterialTheme.colorScheme.tertiary,
                            )
                        }
                    }
                }
            }
        }

        item {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                ItemKind.entries.filter { it != ItemKind.GOAL }.forEach { kind ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.size(8.dp).background(kind.color(), MaterialTheme.shapes.small))
                        Text(
                            kind.label,
                            modifier = Modifier.padding(start = 4.dp),
                            style = MaterialTheme.typography.labelSmall,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun FirmHorizonStrip(horizon: ScheduleDisplayHorizon, reference: Instant) {
    val today = reference.atZone(horizon.timezone).toLocalDate()
    val dates = firmHorizonDates(horizon)
    val currentIndex = dates.indexOf(today)
    val listState = rememberLazyListState(
        initialFirstVisibleItemIndex = currentIndex.coerceAtLeast(0),
    )
    LaunchedEffect(horizon.start, horizon.end, horizon.timezone, today) {
        if (currentIndex >= 0) listState.scrollToItem(currentIndex)
    }
    LazyRow(
        modifier = Modifier.fillMaxWidth(),
        state = listState,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        items(
            items = dates,
            key = LocalDate::toEpochDay,
        ) { date ->
            val selected = date == today
            Surface(
                modifier = Modifier
                    .width(54.dp)
                    .clearAndSetSemantics {
                        this.selected = selected
                        contentDescription = firmHorizonDateContentDescription(
                            date = date,
                            isToday = selected,
                        )
                    },
                shape = MaterialTheme.shapes.medium,
                color = if (selected) MaterialTheme.colorScheme.primaryContainer else Color.Transparent,
            ) {
                Column(
                    modifier = Modifier.padding(vertical = 10.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(
                        date.dayOfWeek.getDisplayName(TextStyle.SHORT, Locale.getDefault()),
                        style = MaterialTheme.typography.labelSmall,
                    )
                    Text(date.dayOfMonth.toString(), style = MaterialTheme.typography.titleMedium)
                }
            }
        }
    }
}

internal fun firmHorizonDates(horizon: ScheduleDisplayHorizon): List<LocalDate> {
    require(horizon.start < horizon.end)
    val startDate = horizon.start.atZone(horizon.timezone).toLocalDate()
    val lastDate = horizon.end.minusNanos(1).atZone(horizon.timezone).toLocalDate()
    val dayCount = Math.addExact(ChronoUnit.DAYS.between(startDate, lastDate), 1L)
    require(dayCount in 1L..MAX_DISPLAY_HORIZON_DATES)
    return (0L until dayCount).map(startDate::plusDays)
}

internal fun firmHorizonTimelineLabel(
    slice: ScheduleItemPresentationSlice,
    timezone: ZoneId?,
    locale: Locale = Locale.getDefault(),
    use24HourFormat: Boolean = true,
): String {
    val clippedStart = slice.clippedStart
    if (clippedStart == null || timezone == null) return "Unplaced · ${slice.startTimeLabel}"
    val local = clippedStart.atZone(timezone)
    val date = local.format(
        DateTimeFormatterBuilder()
            .appendPattern("EEE, ")
            .appendLocalized(FormatStyle.MEDIUM, null)
            .toFormatter(locale),
    )
    val time = local.format(
        DateTimeFormatter.ofPattern(if (use24HourFormat) "HH:mm" else "h:mm a", locale),
    )
    return "$date · $time"
}

internal fun firmHorizonTimelineKey(slice: ScheduleItemPresentationSlice): String = buildString {
    append(slice.item.id.length)
    append(':')
    append(slice.item.id)
    append('|')
    append(slice.clippedStart?.toEpochMilli() ?: Long.MIN_VALUE)
    append('|')
    append(slice.clippedEnd?.toEpochMilli() ?: Long.MIN_VALUE)
}

internal fun firmHorizonDateContentDescription(
    date: LocalDate,
    isToday: Boolean,
    locale: Locale = Locale.getDefault(),
): String {
    val fullDate = date.format(
        DateTimeFormatter.ofLocalizedDate(FormatStyle.FULL).withLocale(locale),
    )
    return if (isToday) "Today, $fullDate" else fullDate
}

private const val MAX_DISPLAY_HORIZON_DATES = 92L
