package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.PauseCircle
import androidx.compose.material.icons.outlined.PlayCircle
import androidx.compose.material.icons.outlined.QueryStats
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsSnapshot
import com.greengolddog.dayweave.model.HabitMissedExplicitActionSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.model.hasAtMostUnicodeScalars
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.HabitSyncState
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.temporal.ChronoUnit
import java.util.Locale
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

private const val MAX_PRESENTED_MISSED_DECISIONS = 12

private data class HabitOutcomeEditorTarget(
    val row: HabitTodayRow,
    val draft: HabitOutcomeDraft,
    val isSaving: Boolean = false,
    val saveError: String? = null,
) {
    override fun toString(): String =
        "HabitOutcomeEditorTarget(key=${row.key}, content=<redacted>)"
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun TodayHabitsSection(
    state: DayWeaveUiState,
    syncState: HabitSyncState,
    reference: Instant,
    currentZone: ZoneId,
    onRefresh: () -> Boolean,
    onRecordOutcome: (String, String, Long, HabitOutcomeInputSnapshot) -> Deferred<Boolean>,
    onResolveMissed: (
        String,
        String,
        Long,
        HabitMissedExplicitActionSnapshot,
    ) -> Deferred<Boolean>,
    onStartPause: (String) -> Boolean,
    onResumePause: (String, String) -> Boolean,
    onDiscardReviewedMutation: (String) -> Boolean,
    modifier: Modifier = Modifier,
    now: () -> Instant = Instant::now,
) {
    val date = reference.atZone(currentZone).toLocalDate()
    val schedule = if (state.isScheduleDisplayCurrent(reference, currentZone)) {
        state.visibleScheduleSlicesForDay(reference, currentZone)
    } else {
        emptyList()
    }
    val rows = remember(schedule, state.canonicalItems, state.habitLedger, date) {
        projectTodayHabits(schedule, state.canonicalItems, state.habitLedger, date)
    }
    val displayedRows = rows.take(MAX_PRESENTED_TODAY_HABITS)
    val reviewed = remember(state.habitLedger.pendingMutations) {
        reviewedHabitMutations(state.habitLedger)
    }
    val missedDecisions = remember(
        state.canonicalItems,
        state.habitLedger,
        state.publishedOccurrenceMembershipProof,
        state.publishedScheduleRevisionHint,
        state.canonicalSyncOrigin,
        state.canonicalConfigurationId,
    ) {
        missedHabitDecisions(
            state.canonicalItems,
            state.habitLedger,
            state.publishedOccurrenceMembershipProof,
            state.publishedScheduleRevisionHint,
            state.canonicalSyncOrigin,
            state.canonicalConfigurationId,
        )
    }
    val coroutineScope = rememberCoroutineScope()
    var editorTarget by remember { mutableStateOf<HabitOutcomeEditorTarget?>(null) }
    var quickOutcomeSaveInFlight by remember { mutableStateOf(false) }
    var missedDecisionSaveInFlightId by remember { mutableStateOf<String?>(null) }
    var discardTarget by remember { mutableStateOf<PendingHabitMutation?>(null) }
    var discardAdmissionError by remember { mutableStateOf<String?>(null) }
    var actionAdmissionMessage by remember { mutableStateOf<String?>(null) }

    editorTarget?.let { target ->
        HabitOutcomeEditorDialog(
            target = target,
            onTargetChange = { editorTarget = it },
            onDismiss = { editorTarget = null },
            onSave = { draft ->
                val editedTarget = target.copy(
                    draft = draft,
                    isSaving = false,
                    saveError = null,
                )
                val occurredAt = target.row.occurrence?.outcome?.occurredAt
                    ?: canonicalHabitInstant(now())
                val validated = draft.validate(occurredAt)
                val outcome = validated.outcome
                if (outcome == null) {
                    editorTarget = editedTarget.copy(saveError = validated.message)
                } else {
                    val habitId = target.row.habitId
                    val occurrenceId = target.row.ledgerOccurrenceId
                    if (habitId != null && occurrenceId != null) {
                        val savingTarget = editedTarget.copy(isSaving = true)
                        editorTarget = savingTarget
                        coroutineScope.launch {
                            val saved = onRecordOutcome(
                                habitId,
                                occurrenceId,
                                target.row.occurrence?.outcome?.revision ?: 0,
                                outcome,
                            ).await()
                            if (editorTarget == savingTarget) {
                                editorTarget = if (saved) {
                                    null
                                } else {
                                    savingTarget.copy(
                                        isSaving = false,
                                        saveError = HABIT_OUTCOME_NOT_SAVED_MESSAGE,
                                    )
                                }
                            }
                        }
                    } else {
                        editorTarget = editedTarget.copy(
                            saveError = HABIT_ACTION_NOT_SUBMITTED_MESSAGE,
                        )
                    }
                }
            },
        )
    }
    discardTarget?.let { mutation ->
        AlertDialog(
            onDismissRequest = { discardTarget = null },
            title = { Text("Discard saved habit update?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "This removes only the encrypted local update that needs review. " +
                            "The server’s current habit history stays unchanged.",
                    )
                    discardAdmissionError?.let { message ->
                        Text(
                            message,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.semantics {
                                liveRegion = LiveRegionMode.Assertive
                            },
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        val admitted = onDiscardReviewedMutation(mutation.idempotencyKey)
                        if (admitted) {
                            discardTarget = null
                            discardAdmissionError = null
                        } else {
                            discardAdmissionError = HABIT_ACTION_NOT_SUBMITTED_MESSAGE
                        }
                    },
                    modifier = Modifier.testTag("habit_confirm_discard_reviewed"),
                ) {
                    Text("Discard saved update")
                }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        discardTarget = null
                        discardAdmissionError = null
                    },
                ) { Text("Keep for review") }
            },
        )
    }

    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    "Today’s habits",
                    style = MaterialTheme.typography.titleLarge,
                    modifier = Modifier.semantics { heading() },
                )
                Text(
                    habitSyncSummary(syncState, state.habitLedger.isBound),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.semantics {
                        liveRegion = LiveRegionMode.Polite
                    },
                )
            }
            if (syncState.isBusy) {
                CircularProgressIndicator(
                    modifier = Modifier.width(24.dp),
                    strokeWidth = 2.dp,
                )
            } else {
                IconButton(
                    onClick = {
                        actionAdmissionMessage = habitActionAdmissionMessage(onRefresh())
                    },
                    modifier = Modifier
                        .testTag("habit_refresh_today")
                        .semantics { contentDescription = "Refresh habit history" },
                ) {
                    Icon(Icons.Outlined.Refresh, contentDescription = null)
                }
            }
        }

        actionAdmissionMessage?.let { message ->
            Text(
                message,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
            )
        }

        if (rows.isEmpty()) {
            Card(
                colors = CardDefaults.cardColors(
                    containerColor = MaterialTheme.colorScheme.surfaceVariant,
                ),
            ) {
                Column(
                    modifier = Modifier.fillMaxWidth().padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text("No habit occurrence is attached to today")
                    Text(
                        "Habits still appear in your timeline when the planner schedules them. " +
                            "Refresh after publishing a firm plan to attach completion history.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        } else {
            displayedRows.forEach { row ->
                val occurrenceMutation = row.ledgerOccurrenceId?.let {
                    pendingMutationForOccurrence(state.habitLedger, it)
                }
                val openPause = row.habitId?.let { activePauseForHabit(state.habitLedger, it) }
                val pauseMutation = row.habitId?.let { habitId ->
                    state.habitLedger.pendingMutations
                        .filter {
                            it.habitId == habitId &&
                                (it.kind == PendingHabitMutationKind.START_PAUSE ||
                                    it.kind == PendingHabitMutationKind.RESUME_PAUSE)
                        }
                        .maxByOrNull(PendingHabitMutation::createdAt)
                }
                HabitTodayCard(
                    row = row,
                    occurrenceMutation = occurrenceMutation,
                    openPause = openPause,
                    pauseMutation = pauseMutation,
                    actionsEnabled = !syncState.isBusy && !quickOutcomeSaveInFlight,
                    onDone = {
                        val habitId = row.habitId
                        val occurrenceId = row.ledgerOccurrenceId
                        val outcome = HabitOutcomeDraft
                            .forStatus(HabitOutcomeStatusSnapshot.COMPLETED)
                            .validate(canonicalHabitInstant(now()))
                            .outcome
                        if (habitId != null && occurrenceId != null && outcome != null) {
                            quickOutcomeSaveInFlight = true
                            actionAdmissionMessage = null
                            coroutineScope.launch {
                                try {
                                    actionAdmissionMessage = habitActionAdmissionMessage(
                                        onRecordOutcome(
                                            habitId,
                                            occurrenceId,
                                            row.occurrence?.outcome?.revision ?: 0,
                                            outcome,
                                        ).await(),
                                    )
                                } finally {
                                    quickOutcomeSaveInFlight = false
                                }
                            }
                        }
                    },
                    onDoneWithDetails = {
                        editorTarget = HabitOutcomeEditorTarget(
                            row,
                            HabitOutcomeDraft.forStatus(HabitOutcomeStatusSnapshot.COMPLETED),
                        )
                    },
                    onPartial = {
                        editorTarget = HabitOutcomeEditorTarget(
                            row,
                            HabitOutcomeDraft.forStatus(HabitOutcomeStatusSnapshot.PARTIAL),
                        )
                    },
                    onSkipped = {
                        editorTarget = HabitOutcomeEditorTarget(
                            row,
                            HabitOutcomeDraft.forStatus(HabitOutcomeStatusSnapshot.SKIPPED),
                        )
                    },
                    onCorrect = {
                        val outcome = row.occurrence?.outcome
                        if (outcome != null) {
                            editorTarget = HabitOutcomeEditorTarget(
                                row,
                                HabitOutcomeDraft.correcting(outcome),
                            )
                        }
                    },
                    onStartPause = {
                        actionAdmissionMessage = habitActionAdmissionMessage(
                            row.habitId?.let(onStartPause) ?: false,
                        )
                    },
                    onResumePause = {
                        val habitId = row.habitId
                        if (habitId != null && openPause != null) {
                            actionAdmissionMessage = habitActionAdmissionMessage(
                                onResumePause(habitId, openPause.id),
                            )
                        }
                    },
                )
            }
            if (rows.size > displayedRows.size) {
                Text(
                    "+${rows.size - displayedRows.size} more occurrences are available in " +
                        "the encrypted ledger",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        if (missedDecisions.isNotEmpty()) {
            MissedHabitReviewQueue(
                decisions = missedDecisions,
                ledger = state.habitLedger,
                savingOccurrenceId = missedDecisionSaveInFlightId,
                actionsEnabled = !syncState.isBusy && missedDecisionSaveInFlightId == null,
                onResolve = { row, action ->
                    val resolution = requireNotNull(row.occurrence.missedResolution)
                    missedDecisionSaveInFlightId = row.occurrence.evidence.id
                    actionAdmissionMessage = null
                    coroutineScope.launch {
                        try {
                            actionAdmissionMessage = habitActionAdmissionMessage(
                                onResolveMissed(
                                    row.occurrence.evidence.habitId,
                                    row.occurrence.evidence.id,
                                    resolution.revision,
                                    action,
                                ).await(),
                            )
                        } finally {
                            if (missedDecisionSaveInFlightId == row.occurrence.evidence.id) {
                                missedDecisionSaveInFlightId = null
                            }
                        }
                    }
                },
            )
        }

        if (reviewed.isNotEmpty()) {
            HabitReviewQueue(
                mutations = reviewed,
                onDiscard = {
                    discardAdmissionError = null
                    discardTarget = it
                },
                actionsEnabled = !syncState.isBusy,
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MissedHabitReviewQueue(
    decisions: List<HabitMissedDecisionRow>,
    ledger: com.greengolddog.dayweave.model.HabitLedgerSnapshot,
    savingOccurrenceId: String?,
    actionsEnabled: Boolean,
    onResolve: (HabitMissedDecisionRow, HabitMissedExplicitActionSnapshot) -> Unit,
) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.tertiaryContainer.copy(alpha = 0.56f),
        ),
        modifier = Modifier.fillMaxWidth().testTag("habit_missed_review_queue"),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                "A gentle check-in",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.semantics { heading() },
            )
            Text(
                "These habit windows passed without a final outcome. Choose what feels right; " +
                    "your earlier history stays unchanged.",
                style = MaterialTheme.typography.bodySmall,
            )
            decisions.take(MAX_PRESENTED_MISSED_DECISIONS).forEach { row ->
                val occurrenceId = row.occurrence.evidence.id
                val pending = ledger.pendingMutations.firstOrNull {
                    it.targetId == occurrenceId &&
                        it.kind == PendingHabitMutationKind.MISSED_RESOLUTION
                }
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    row.title,
                                    style = MaterialTheme.typography.titleSmall,
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (row.isSensitive) {
                                    Spacer(Modifier.width(6.dp))
                                    Icon(
                                        Icons.Outlined.Lock,
                                        contentDescription = "Private habit",
                                    )
                                }
                            }
                            Text(
                                "Window ended ${row.occurrence.evidence.localDate}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        if (savingOccurrenceId == occurrenceId) {
                            CircularProgressIndicator(
                                modifier = Modifier.width(20.dp),
                                strokeWidth = 2.dp,
                            )
                        }
                    }
                    if (pending != null) {
                        Text(
                            habitMutationLabel(pending),
                            style = MaterialTheme.typography.labelMedium,
                            color = if (
                                pending.disposition == PendingHabitMutationDisposition.PENDING
                            ) {
                                MaterialTheme.colorScheme.tertiary
                            } else {
                                MaterialTheme.colorScheme.error
                            },
                            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                        )
                    }
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        OutlinedButton(
                            onClick = {
                                onResolve(row, HabitMissedExplicitActionSnapshot.SKIP)
                            },
                            enabled = actionsEnabled && pending == null,
                            modifier = Modifier.testTag("habit_missed_skip"),
                        ) { Text("Skip") }
                        Button(
                            onClick = {
                                onResolve(row, HabitMissedExplicitActionSnapshot.CARRY)
                            },
                            enabled = actionsEnabled && pending == null,
                            modifier = Modifier.testTag("habit_missed_carry"),
                        ) { Text("Will do later") }
                        OutlinedButton(
                            onClick = {
                                onResolve(
                                    row,
                                    HabitMissedExplicitActionSnapshot.REDUCE_FREQUENCY,
                                )
                            },
                            enabled = actionsEnabled && pending == null,
                            modifier = Modifier.testTag("habit_missed_reduce_frequency"),
                        ) { Text("Reduce frequency") }
                    }
                    Text(
                        "DayWeave asks the server to choose any new time or occurrence safely.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                HorizontalDivider(
                    color = MaterialTheme.colorScheme.onTertiaryContainer.copy(alpha = 0.18f),
                )
            }
            if (decisions.size > MAX_PRESENTED_MISSED_DECISIONS) {
                Text(
                    "+${decisions.size - MAX_PRESENTED_MISSED_DECISIONS} more check-ins are " +
                        "kept securely",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

@Composable
private fun HabitTodayCard(
    row: HabitTodayRow,
    occurrenceMutation: PendingHabitMutation?,
    openPause: com.greengolddog.dayweave.model.HabitPauseSnapshot?,
    pauseMutation: PendingHabitMutation?,
    actionsEnabled: Boolean,
    onDone: () -> Unit,
    onDoneWithDetails: () -> Unit,
    onPartial: () -> Unit,
    onSkipped: () -> Unit,
    onCorrect: () -> Unit,
    onStartPause: () -> Unit,
    onResumePause: () -> Unit,
) {
    val outcome = row.occurrence?.outcome
    val mutationPending = occurrenceMutation != null
    val canRecord = row.hasCanonicalEvidence && actionsEnabled && !mutationPending
    Card(
        colors = CardDefaults.cardColors(
            containerColor = when {
                occurrenceMutation?.disposition != null &&
                    occurrenceMutation.disposition != PendingHabitMutationDisposition.PENDING ->
                    MaterialTheme.colorScheme.errorContainer
                outcome?.status == HabitOutcomeStatusSnapshot.COMPLETED ->
                    MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.52f)
                else -> MaterialTheme.colorScheme.surface
            },
        ),
        modifier = Modifier
            .fillMaxWidth()
            .testTag("habit_today_card")
            .semantics {
                stateDescription = habitOutcomeLabel(outcome)
            },
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.Top,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Icon(
                    if (outcome?.status == HabitOutcomeStatusSnapshot.COMPLETED) {
                        Icons.Outlined.CheckCircle
                    } else {
                        Icons.Outlined.Schedule
                    },
                    contentDescription = null,
                    tint = if (outcome?.status == HabitOutcomeStatusSnapshot.COMPLETED) {
                        MaterialTheme.colorScheme.primary
                    } else {
                        MaterialTheme.colorScheme.secondary
                    },
                )
                Column(modifier = Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            row.title,
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (row.isSensitive) {
                            Spacer(Modifier.width(6.dp))
                            Icon(
                                Icons.Outlined.Lock,
                                contentDescription = "Private habit",
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                    Text(
                        buildList {
                            if (row.timeLabel.isNotBlank()) add(row.timeLabel)
                            row.plannedMinutes?.let { add("${it}m planned") }
                            val evidence = row.occurrence?.evidence
                            if (evidence?.expectedQuantity != null && evidence.expectedUnit != null) {
                                add("${evidence.expectedQuantity} ${evidence.expectedUnit} target")
                            }
                        }.joinToString(" · ").ifBlank { "Scheduled habit" },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Text(
                    habitOutcomeLabel(outcome),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }

            outcome?.takeIf { it.status != HabitOutcomeStatusSnapshot.UNRESOLVED }?.let {
                LinearProgressIndicator(
                    progress = { it.progressBasisPoints / 10_000f },
                    modifier = Modifier.fillMaxWidth(),
                )
                val details = buildList {
                    it.actualSeconds?.let { seconds -> add(formatHabitDuration(seconds)) }
                    if (it.quantity != null && it.unit != null) add("${it.quantity} ${it.unit}")
                    if (it.note != null) add("Private note saved")
                }
                if (details.isNotEmpty()) {
                    Text(
                        details.joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            row.fallback?.let { fallback ->
                Text(
                    fallbackMessage(fallback),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            occurrenceMutation?.let { mutation ->
                Text(
                    habitMutationLabel(mutation),
                    style = MaterialTheme.typography.labelMedium,
                    color = if (mutation.disposition == PendingHabitMutationDisposition.PENDING) {
                        MaterialTheme.colorScheme.tertiary
                    } else {
                        MaterialTheme.colorScheme.error
                    },
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
            }

            if (row.hasCanonicalEvidence) {
                if (outcome == null || outcome.status == HabitOutcomeStatusSnapshot.UNRESOLVED) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Button(
                            onClick = onDone,
                            enabled = canRecord,
                            modifier = Modifier.weight(1f).testTag("habit_done"),
                        ) {
                            Text("Done")
                        }
                        OutlinedButton(
                            onClick = onPartial,
                            enabled = canRecord,
                            modifier = Modifier.weight(1f).testTag("habit_partial"),
                        ) {
                            Text("Partial")
                        }
                        OutlinedButton(
                            onClick = onSkipped,
                            enabled = canRecord,
                            modifier = Modifier.weight(1f).testTag("habit_skipped"),
                        ) {
                            Text("Skipped")
                        }
                    }
                    TextButton(
                        onClick = onDoneWithDetails,
                        enabled = canRecord,
                        modifier = Modifier.fillMaxWidth().testTag("habit_done_with_details"),
                    ) {
                        Text("Done with details")
                    }
                } else {
                    OutlinedButton(
                        onClick = onCorrect,
                        enabled = canRecord,
                        modifier = Modifier.fillMaxWidth().testTag("habit_correct"),
                    ) {
                        Icon(Icons.Outlined.Edit, contentDescription = null)
                        Spacer(Modifier.width(8.dp))
                        Text("Correct outcome")
                    }
                }
            }

            HorizontalDivider()
            HabitPauseControl(
                openPause = openPause,
                pauseMutation = pauseMutation,
                actionsEnabled = actionsEnabled && row.habitId != null,
                onStartPause = onStartPause,
                onResumePause = onResumePause,
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun HabitOutcomeEditorDialog(
    target: HabitOutcomeEditorTarget,
    onTargetChange: (HabitOutcomeEditorTarget) -> Unit,
    onDismiss: () -> Unit,
    onSave: (HabitOutcomeDraft) -> Unit,
) {
    val draft = target.draft
    fun update(transform: (HabitOutcomeDraft) -> HabitOutcomeDraft) {
        if (!target.isSaving) {
            onTargetChange(target.copy(draft = transform(draft), saveError = null))
        }
    }
    AlertDialog(
        onDismissRequest = { if (!target.isSaving) onDismiss() },
        title = {
            Column {
                Text(if (target.row.occurrence?.outcome == null) "Record habit" else "Correct habit")
                Text(
                    target.row.title,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        },
        text = {
            Column(
                modifier = Modifier.heightIn(max = 500.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text("Outcome", style = MaterialTheme.typography.labelLarge)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(
                        HabitOutcomeStatusSnapshot.COMPLETED to "Done",
                        HabitOutcomeStatusSnapshot.PARTIAL to "Partial",
                        HabitOutcomeStatusSnapshot.SKIPPED to "Skipped",
                        HabitOutcomeStatusSnapshot.UNRESOLVED to "Clear",
                    ).forEach { (status, label) ->
                        FilterChip(
                            selected = draft.status == status,
                            onClick = { update { it.selectStatus(status) } },
                            enabled = !target.isSaving,
                            label = { Text(label) },
                            modifier = Modifier.testTag("habit_editor_${status.name.lowercase()}")
                                .semantics {
                                    stateDescription = if (draft.status == status) {
                                        "$label selected"
                                    } else {
                                        "$label not selected"
                                    }
                                },
                        )
                    }
                }
                if (draft.status in setOf(
                        HabitOutcomeStatusSnapshot.PARTIAL,
                        HabitOutcomeStatusSnapshot.SKIPPED,
                    )
                ) {
                    OutlinedTextField(
                        value = draft.progressPercent,
                        onValueChange = { value ->
                            if (value.length <= MAX_PROGRESS_INPUT_CHARS) {
                                update { it.copy(progressPercent = value) }
                            }
                        },
                        label = { Text("Progress (%)") },
                        supportingText = { Text("Use up to two decimal places") },
                        enabled = !target.isSaving,
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Decimal,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.fillMaxWidth().testTag("habit_editor_progress"),
                    )
                }
                if (draft.status != HabitOutcomeStatusSnapshot.UNRESOLVED) {
                    val evidence = target.row.occurrence?.evidence
                    if (evidence?.expectedQuantity != null && evidence.expectedUnit != null) {
                        Text(
                            "Target · ${evidence.expectedQuantity} ${evidence.expectedUnit}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedTextField(
                            value = draft.quantity,
                            onValueChange = { value ->
                                if (value.length <= MAX_QUANTITY_INPUT_CHARS) {
                                    update { it.copy(quantity = value) }
                                }
                            },
                            label = { Text("Quantity") },
                            supportingText = { Text("Signed whole number") },
                            enabled = !target.isSaving,
                            singleLine = true,
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Text,
                                imeAction = ImeAction.Next,
                            ),
                            modifier = Modifier.weight(1f).testTag("habit_editor_quantity"),
                        )
                        OutlinedTextField(
                            value = draft.unit,
                            onValueChange = { value ->
                                if (value.hasAtMostUnicodeScalars(MAX_UNIT_INPUT_CHARS)) {
                                    update { it.copy(unit = value) }
                                }
                            },
                            label = { Text("Unit") },
                            enabled = !target.isSaving,
                            singleLine = true,
                            keyboardOptions = KeyboardOptions(
                                keyboardType = KeyboardType.Text,
                                imeAction = ImeAction.Next,
                            ),
                            modifier = Modifier.weight(1f).testTag("habit_editor_unit"),
                        )
                    }
                    OutlinedTextField(
                        value = draft.actualMinutes,
                        onValueChange = { value ->
                            if (value.length <= MAX_DURATION_INPUT_CHARS) {
                                update {
                                    it.copy(actualMinutes = value, actualMinutesEdited = true)
                                }
                            }
                        },
                        label = { Text("Actual duration (minutes)") },
                        supportingText = { Text("Decimals are accepted when they equal whole seconds") },
                        enabled = !target.isSaving,
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Decimal,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.fillMaxWidth().testTag("habit_editor_duration"),
                    )
                    OutlinedTextField(
                        value = draft.note,
                        onValueChange = { value ->
                            if (value.hasAtMostUnicodeScalars(MAX_PRIVATE_NOTE_CHARS)) {
                                update { it.copy(note = value) }
                            }
                        },
                        label = { Text("Private note (encrypted)") },
                        supportingText = {
                            Text("Stored on authenticated devices; never included in statistics")
                        },
                        enabled = !target.isSaving,
                        minLines = 2,
                        maxLines = 5,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Text,
                            imeAction = ImeAction.Default,
                        ),
                        modifier = Modifier.fillMaxWidth().testTag("habit_editor_private_note"),
                    )
                } else {
                    Text(
                        "Clearing removes this recorded outcome after it synchronizes. " +
                            "The scheduled occurrence remains.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                target.saveError?.let {
                    Text(
                        it,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier
                            .testTag("habit_editor_save_error")
                            .semantics { liveRegion = LiveRegionMode.Assertive },
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onSave(draft) },
                enabled = !target.isSaving,
                modifier = Modifier.testTag("habit_editor_save"),
            ) {
                Text(if (target.isSaving) "Saving…" else "Save outcome")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !target.isSaving) { Text("Cancel") }
        },
    )
}

@Composable
private fun HabitPauseControl(
    openPause: com.greengolddog.dayweave.model.HabitPauseSnapshot?,
    pauseMutation: PendingHabitMutation?,
    actionsEnabled: Boolean,
    onStartPause: () -> Unit,
    onResumePause: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(
            if (openPause == null) Icons.Outlined.PauseCircle else Icons.Outlined.PlayCircle,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.secondary,
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                when {
                    pauseMutation != null -> habitMutationLabel(pauseMutation)
                    openPause == null -> "Habit tracking is active"
                    openPause.preservesStreak -> "Paused · streak protected"
                    else -> "Paused · streak follows habit policy"
                },
                style = MaterialTheme.typography.bodySmall,
                color = if (
                    pauseMutation?.disposition != null &&
                    pauseMutation.disposition != PendingHabitMutationDisposition.PENDING
                ) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
        }
        TextButton(
            onClick = if (openPause == null) onStartPause else onResumePause,
            enabled = actionsEnabled && pauseMutation == null,
            modifier = Modifier.testTag(
                if (openPause == null) "habit_pause" else "habit_resume",
            ),
        ) {
            Text(if (openPause == null) "Pause" else "Resume")
        }
    }
}

@Composable
private fun HabitReviewQueue(
    mutations: List<PendingHabitMutation>,
    onDiscard: (PendingHabitMutation) -> Unit,
    actionsEnabled: Boolean,
) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.errorContainer,
        ),
        modifier = Modifier.fillMaxWidth().testTag("habit_review_queue"),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                "Habit updates need review",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.semantics { heading() },
            )
            Text(
                "DayWeave kept these encrypted local updates and did not overwrite newer history.",
                style = MaterialTheme.typography.bodySmall,
            )
            mutations.take(5).forEach { mutation ->
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        habitMutationLabel(mutation),
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight = FontWeight.Medium,
                    )
                    Text(
                        when (mutation.kind) {
                            PendingHabitMutationKind.OUTCOME -> "Occurrence outcome"
                            PendingHabitMutationKind.START_PAUSE -> "Pause request"
                            PendingHabitMutationKind.RESUME_PAUSE -> "Resume request"
                            PendingHabitMutationKind.MISSED_RESOLUTION ->
                                "Missed habit choice"
                        },
                        style = MaterialTheme.typography.bodySmall,
                    )
                    TextButton(
                        onClick = { onDiscard(mutation) },
                        enabled = actionsEnabled,
                        modifier = Modifier.testTag("habit_discard_reviewed"),
                    ) {
                        Text("Discard saved update")
                    }
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.onErrorContainer.copy(alpha = 0.2f))
            }
            if (mutations.size > 5) {
                Text(
                    "+${mutations.size - 5} more saved updates",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun HabitStatisticsSection(
    state: DayWeaveUiState,
    syncState: HabitSyncState,
    reference: Instant,
    currentZone: ZoneId,
    onRefreshAnalytics: (
        String,
        LocalDate,
        LocalDate,
        HabitAnalyticsBucketSnapshot,
    ) -> Boolean,
    onStartPause: (String) -> Boolean,
    onResumePause: (String, String) -> Boolean,
    modifier: Modifier = Modifier,
) {
    val date = reference.atZone(currentZone).toLocalDate()
    val schedule = if (state.isScheduleDisplayCurrent(reference, currentZone)) {
        state.visibleScheduleSlicesForDay(reference, currentZone)
    } else {
        emptyList()
    }
    val choices = remember(state.canonicalItems, state.habitLedger, schedule) {
        habitChoices(state.canonicalItems, schedule, state.habitLedger)
    }
    var selectedHabitId by rememberSaveable { mutableStateOf<String?>(null) }
    var rangeName by rememberSaveable { mutableStateOf(HabitStatisticsRange.NINETY_DAYS.name) }
    var bucketName by rememberSaveable { mutableStateOf(HabitAnalyticsBucketSnapshot.WEEK.name) }
    val selectedRange = HabitStatisticsRange.entries
        .firstOrNull { it.name == rangeName } ?: HabitStatisticsRange.NINETY_DAYS
    val selectedBucket = HabitAnalyticsBucketSnapshot.entries
        .firstOrNull { it.name == bucketName } ?: HabitAnalyticsBucketSnapshot.WEEK
    val selectedChoice = choices.firstOrNull { it.id == selectedHabitId } ?: choices.firstOrNull()
    val bounds = selectedRange.bounds(date)
    val analytics = selectedChoice?.let {
        analyticsFor(state.habitLedger, it.id, bounds, selectedBucket)
    }
    val refreshWindow = habitAnalyticsFreshnessWindow(reference)
    val refreshRequestKey = selectedChoice?.let { choice ->
        listOf(
            state.habitLedger.syncOrigin,
            state.habitLedger.configurationId,
            choice.id,
            bounds.start,
            bounds.endInclusive,
            selectedBucket,
            refreshWindow,
        ).joinToString("|")
    }
    // Screen activation intentionally starts a fresh bounded revalidation. This is not saveable:
    // restoring an old admission marker could suppress the first refresh after recreation.
    var lastAdmittedRefreshKey by remember { mutableStateOf<String?>(null) }
    var actionAdmissionMessage by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(choices.map(HabitChoice::id), selectedHabitId) {
        if (selectedHabitId !in choices.map(HabitChoice::id)) {
            selectedHabitId = choices.firstOrNull()?.id
        }
    }
    LaunchedEffect(
        selectedChoice?.id,
        bounds.start,
        bounds.endInclusive,
        selectedBucket,
        state.habitLedger.isBound,
        syncState.isBusy,
        refreshWindow,
        refreshRequestKey,
    ) {
        if (
            selectedChoice != null && state.habitLedger.isBound && !syncState.isBusy &&
            refreshRequestKey != null && lastAdmittedRefreshKey != refreshRequestKey
        ) {
            val admitted = awaitHabitAnalyticsRefreshAdmission(
                shouldContinue = {
                    currentCoroutineContext().isActive &&
                        lastAdmittedRefreshKey != refreshRequestKey
                },
                actionBusy = { syncState.isBusy },
                launch = {
                    onRefreshAnalytics(
                        selectedChoice.id,
                        bounds.start,
                        bounds.endInclusive,
                        selectedBucket,
                    )
                },
            )
            if (admitted) lastAdmittedRefreshKey = refreshRequestKey
        }
    }

    Card(modifier = modifier.fillMaxWidth().testTag("habit_statistics")) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Row(
                    modifier = Modifier.weight(1f),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Icon(
                        Icons.Outlined.QueryStats,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.primary,
                    )
                    Column {
                        Text(
                            "Habit statistics",
                            style = MaterialTheme.typography.titleLarge,
                            modifier = Modifier.semantics { heading() },
                        )
                        Text(
                            "Private, bounded history from the habit ledger",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                if (syncState.isBusy) {
                    CircularProgressIndicator(
                        modifier = Modifier.width(24.dp),
                        strokeWidth = 2.dp,
                    )
                } else {
                    IconButton(
                        onClick = {
                            selectedChoice?.let {
                                val admitted = onRefreshAnalytics(
                                    it.id, bounds.start, bounds.endInclusive, selectedBucket,
                                )
                                actionAdmissionMessage =
                                    habitActionAdmissionMessage(admitted)
                                if (admitted) lastAdmittedRefreshKey = refreshRequestKey
                            }
                        },
                        enabled = selectedChoice != null,
                        modifier = Modifier.testTag("habit_refresh_statistics").semantics {
                            contentDescription = "Refresh selected habit statistics"
                        },
                    ) {
                        Icon(Icons.Outlined.Refresh, contentDescription = null)
                    }
                }
            }

            if (choices.isEmpty()) {
                Text(
                    "Create a habit and publish its schedule to see adherence and streaks here.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                Text("Habit", style = MaterialTheme.typography.labelLarge)
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    choices.forEach { choice ->
                        FilterChip(
                            selected = selectedChoice?.id == choice.id,
                            onClick = { selectedHabitId = choice.id },
                            label = {
                                Text(
                                    choice.title,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            },
                            leadingIcon = if (choice.isSensitive) {
                                { Icon(Icons.Outlined.Lock, contentDescription = "Private") }
                            } else {
                                null
                            },
                            modifier = Modifier.testTag("habit_statistics_choice"),
                        )
                    }
                }

                Text("Range", style = MaterialTheme.typography.labelLarge)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    HabitStatisticsRange.entries.forEach { range ->
                        FilterChip(
                            selected = selectedRange == range,
                            onClick = { rangeName = range.name },
                            label = { Text(range.label) },
                            modifier = Modifier.testTag("habit_range_${range.name.lowercase()}"),
                        )
                    }
                }
                Text("Trend buckets", style = MaterialTheme.typography.labelLarge)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    HabitAnalyticsBucketSnapshot.entries.forEach { bucket ->
                        FilterChip(
                            selected = selectedBucket == bucket,
                            onClick = { bucketName = bucket.name },
                            label = {
                                Text(bucket.name.lowercase().replaceFirstChar(Char::uppercaseChar))
                            },
                            modifier = Modifier.testTag(
                                "habit_bucket_${bucket.name.lowercase()}",
                            ),
                        )
                    }
                }

                selectedChoice?.let { choice ->
                    val openPause = activePauseForHabit(state.habitLedger, choice.id)
                    val pauseMutation = state.habitLedger.pendingMutations
                        .filter {
                            it.habitId == choice.id &&
                                (it.kind == PendingHabitMutationKind.START_PAUSE ||
                                    it.kind == PendingHabitMutationKind.RESUME_PAUSE)
                        }
                        .maxByOrNull(PendingHabitMutation::createdAt)
                    HabitPauseControl(
                        openPause = openPause,
                        pauseMutation = pauseMutation,
                        actionsEnabled = !syncState.isBusy,
                        onStartPause = {
                            actionAdmissionMessage = habitActionAdmissionMessage(
                                onStartPause(choice.id),
                            )
                        },
                        onResumePause = {
                            actionAdmissionMessage = habitActionAdmissionMessage(
                                openPause?.let { onResumePause(choice.id, it.id) } ?: false,
                            )
                        },
                    )
                }

                actionAdmissionMessage?.let { message ->
                    Text(
                        message,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.semantics {
                            liveRegion = LiveRegionMode.Assertive
                        },
                    )
                }

                HorizontalDivider()
                when {
                    analytics != null -> HabitAnalyticsContent(analytics)
                    syncState.isBusy -> Text(
                        "Refreshing this bounded range…",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    else -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text("No cached statistics for this range yet")
                        Text(
                            "Refresh to calculate them without exposing private notes.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                Text(
                    syncState.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = syncStateColor(syncState.phase),
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
            }
        }
    }
}

@Composable
private fun HabitAnalyticsContent(analytics: HabitAnalyticsSnapshot) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        val supportive = supportiveHabitMessages(analytics)
        supportive.forEach { message ->
            Card(
                colors = CardDefaults.cardColors(
                    containerColor = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.55f),
                ),
            ) {
                Text(
                    message,
                    modifier = Modifier.fillMaxWidth().padding(12.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            HabitStatisticMetric(
                value = "${formatBasisPoints(analytics.adherenceBasisPoints)}%",
                label = "adherence",
                modifier = Modifier.weight(1f),
            )
            HabitStatisticMetric(
                value = analytics.currentStreak.toString(),
                label = "current streak",
                modifier = Modifier.weight(1f),
            )
            HabitStatisticMetric(
                value = analytics.longestStreak.toString(),
                label = "best streak",
                modifier = Modifier.weight(1f),
            )
        }
        Text(
            "${analytics.completed} done · ${analytics.partial} partial · " +
                "${analytics.skipped} skipped · ${analytics.missed} missed · " +
                "${analytics.excused} excused",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            "${formatHabitDuration(analytics.actualSecondsTotal)} recorded across " +
                "${analytics.eligible} eligible occurrences",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (analytics.quantityTotals.isNotEmpty()) {
            Text(
                buildList {
                    analytics.quantityTotals.take(MAX_PRESENTED_QUANTITY_TOTALS).forEach {
                        add("${it.amount} ${it.unit}")
                    }
                    if (analytics.quantityTotals.size > MAX_PRESENTED_QUANTITY_TOTALS) {
                        add(
                            "+${analytics.quantityTotals.size - MAX_PRESENTED_QUANTITY_TOTALS} units",
                        )
                    }
                }.joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (analytics.trends.isNotEmpty()) {
            Text(
                "Recent trend",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.semantics { heading() },
            )
            analytics.trends.takeLast(12).forEach { trend ->
                Column(
                    modifier = Modifier.semantics {
                        stateDescription =
                            "${friendlyTrendRange(trend.startDate, trend.endDate)}, " +
                            "${formatBasisPoints(trend.adherenceBasisPoints)} percent adherence"
                    },
                    verticalArrangement = Arrangement.spacedBy(3.dp),
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(
                            friendlyTrendRange(trend.startDate, trend.endDate),
                            style = MaterialTheme.typography.labelMedium,
                        )
                        Text(
                            "${formatBasisPoints(trend.adherenceBasisPoints)}%",
                            style = MaterialTheme.typography.labelMedium,
                        )
                    }
                    LinearProgressIndicator(
                        progress = { trend.adherenceBasisPoints / 10_000f },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }
    }
}

@Composable
private fun HabitStatisticMetric(
    value: String,
    label: String,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.semantics(mergeDescendants = true) {
            contentDescription = "$label, $value"
        },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(value, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun syncStateColor(phase: CanonicalSyncPhase): Color = when (phase) {
    CanonicalSyncPhase.ERROR,
    CanonicalSyncPhase.AUTH_REQUIRED,
    -> MaterialTheme.colorScheme.error
    CanonicalSyncPhase.OFFLINE -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

private fun habitSyncSummary(state: HabitSyncState, isBound: Boolean): String = when {
    state.isBusy -> state.message
    !isBound -> "Waiting for canonical habit history · timeline controls remain available"
    else -> state.message
}

private fun fallbackMessage(fallback: HabitEvidenceFallback): String = when (fallback) {
    HabitEvidenceFallback.LEDGER_NOT_READY ->
        "Canonical history is not attached yet. Use this habit’s existing timeline controls."
    HabitEvidenceFallback.LEGACY_SCHEDULE_BLOCK ->
        "This cached block has no authoritative occurrence identity. Use timeline controls."
    HabitEvidenceFallback.AWAITING_CANONICAL_EVIDENCE ->
        "Waiting for the published occurrence. Refresh, or use timeline controls for now."
    HabitEvidenceFallback.AMBIGUOUS_CANONICAL_EVIDENCE ->
        "This occurrence needs a safe history refresh before an outcome can be recorded."
}

private fun canonicalHabitInstant(instant: Instant): String =
    instant.truncatedTo(ChronoUnit.MICROS).toString()

/** Five-minute windows refresh time-sensitive missed/unresolved analytics without request churn. */
internal fun habitAnalyticsFreshnessWindow(reference: Instant): Long =
    Math.floorDiv(reference.epochSecond, HABIT_ANALYTICS_FRESHNESS_SECONDS)

/** Retries only local admission for a bounded period; one admitted request ends all retries. */
internal suspend fun awaitHabitAnalyticsRefreshAdmission(
    shouldContinue: suspend () -> Boolean,
    actionBusy: () -> Boolean,
    launch: () -> Boolean,
    maxAttempts: Int = HABIT_ANALYTICS_ADMISSION_MAX_ATTEMPTS,
    waitForRetry: suspend () -> Unit = { delay(HABIT_ANALYTICS_ADMISSION_RETRY_MILLIS) },
): Boolean {
    require(maxAttempts > 0)
    repeat(maxAttempts) { attempt ->
        if (!shouldContinue()) return false
        if (!actionBusy() && launch()) return true
        if (attempt < maxAttempts - 1) waitForRetry()
    }
    return false
}

internal fun habitActionAdmissionMessage(admitted: Boolean): String? =
    if (admitted) null else HABIT_ACTION_NOT_SUBMITTED_MESSAGE

private fun friendlyTrendRange(start: String, end: String): String {
    val startDate = runCatching { LocalDate.parse(start) }.getOrNull() ?: return "Period"
    val endDate = runCatching { LocalDate.parse(end) }.getOrNull() ?: return "Period"
    val formatter = java.time.format.DateTimeFormatter.ofPattern("MMM d", Locale.getDefault())
    return if (startDate == endDate) {
        startDate.format(formatter)
    } else {
        "${startDate.format(formatter)}–${endDate.format(formatter)}"
    }
}

private const val MAX_PROGRESS_INPUT_CHARS = 8
private const val MAX_QUANTITY_INPUT_CHARS = 14
private const val MAX_UNIT_INPUT_CHARS = 200
private const val MAX_DURATION_INPUT_CHARS = 16
private const val MAX_PRIVATE_NOTE_CHARS = 10_000
private const val MAX_PRESENTED_QUANTITY_TOTALS = 6
private const val HABIT_ANALYTICS_FRESHNESS_SECONDS = 5L * 60L
private const val HABIT_ANALYTICS_ADMISSION_RETRY_MILLIS = 1_000L
private const val HABIT_ANALYTICS_ADMISSION_MAX_ATTEMPTS = 30
private const val HABIT_OUTCOME_NOT_SAVED_MESSAGE =
    "The exact outcome was not saved. Review the latest habit history and try again."
private const val HABIT_ACTION_NOT_SUBMITTED_MESSAGE =
    "Another planner action is finishing. Your habit action was not submitted; try again."
