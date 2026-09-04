package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsSnapshot
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.HabitPauseSnapshot
import com.greengolddog.dayweave.model.HabitSupportiveFactCodeSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.ScheduleItemPresentationSlice
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.hasAtMostUnicodeScalars
import java.math.BigDecimal
import java.math.RoundingMode
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

internal const val MAX_PRESENTED_HABITS = 50
internal const val MAX_PRESENTED_TODAY_HABITS = 24
internal const val MAX_HABIT_QUANTITY_VALUE = 1_000_000_000_000L
internal const val MAX_HABIT_DURATION_SECONDS = 366L * 24L * 60L * 60L

internal enum class HabitEvidenceFallback {
    LEDGER_NOT_READY,
    LEGACY_SCHEDULE_BLOCK,
    AWAITING_CANONICAL_EVIDENCE,
    AMBIGUOUS_CANONICAL_EVIDENCE,
}

internal data class HabitTodayRow(
    val key: String,
    val habitId: String?,
    /** Ledger occurrence ID. This is intentionally not the planner occurrence ID. */
    val ledgerOccurrenceId: String?,
    val title: String,
    val isSensitive: Boolean,
    val timeLabel: String,
    val plannedMinutes: Int?,
    val occurrence: HabitOccurrenceSnapshot?,
    val fallback: HabitEvidenceFallback?,
) {
    val hasCanonicalEvidence: Boolean
        get() = habitId != null && ledgerOccurrenceId != null && occurrence != null &&
            fallback == null

    override fun toString(): String =
        "HabitTodayRow(key=$key, canonical=$hasCanonicalEvidence, content=<redacted>)"
}

internal data class HabitChoice(
    val id: String,
    val title: String,
    val isSensitive: Boolean,
) {
    override fun toString(): String = "HabitChoice(id=$id, content=<redacted>)"
}

internal enum class HabitStatisticsRange(val days: Long, val label: String) {
    THIRTY_DAYS(30, "30 days"),
    NINETY_DAYS(90, "90 days"),
    ONE_YEAR(365, "1 year"),
    ;

    fun bounds(endingOn: LocalDate): ClosedRange<LocalDate> {
        val boundedEnd = endingOn.coerceIn(MIN_HABIT_ANALYTICS_DATE, MAX_HABIT_ANALYTICS_DATE)
        val boundedStart = boundedEnd.minusDays(days - 1).coerceAtLeast(MIN_HABIT_ANALYTICS_DATE)
        return boundedStart..boundedEnd
    }
}

internal data class HabitOutcomeDraft(
    val status: HabitOutcomeStatusSnapshot,
    val progressPercent: String,
    val quantity: String,
    val unit: String,
    val actualMinutes: String,
    val note: String,
    /** Preserves exact seconds when a correction is saved without editing its display value. */
    val originalActualSeconds: Long? = null,
    val actualMinutesEdited: Boolean = false,
) {
    override fun toString(): String =
        "HabitOutcomeDraft(status=$status, progress=<redacted>, content=<redacted>)"

    companion object {
        fun forStatus(
            status: HabitOutcomeStatusSnapshot,
            outcome: HabitOutcomeSnapshot? = null,
        ): HabitOutcomeDraft = HabitOutcomeDraft(
            status = status,
            progressPercent = when (status) {
                HabitOutcomeStatusSnapshot.UNRESOLVED -> "0"
                HabitOutcomeStatusSnapshot.COMPLETED -> "100"
                HabitOutcomeStatusSnapshot.PARTIAL,
                HabitOutcomeStatusSnapshot.SKIPPED,
                -> formatBasisPoints(outcome?.progressBasisPoints ?: if (
                    status == HabitOutcomeStatusSnapshot.PARTIAL
                ) {
                    5_000
                } else {
                    0
                })
            },
            quantity = outcome?.quantity?.toString().orEmpty(),
            unit = outcome?.unit.orEmpty(),
            actualMinutes = outcome?.actualSeconds?.let(::formatSecondsAsEditableMinutes)
                .orEmpty(),
            note = outcome?.note.orEmpty(),
            originalActualSeconds = outcome?.actualSeconds,
        )

        fun correcting(outcome: HabitOutcomeSnapshot): HabitOutcomeDraft =
            forStatus(outcome.status, outcome)
    }
}

internal data class HabitOutcomeDraftValidation(
    val outcome: HabitOutcomeInputSnapshot? = null,
    val message: String? = null,
) {
    init {
        require((outcome == null) != (message == null))
    }

    override fun toString(): String =
        "HabitOutcomeDraftValidation(valid=${outcome != null}, content=<redacted>)"
}

internal fun projectTodayHabits(
    schedule: List<ScheduleItemPresentationSlice>,
    canonicalItems: List<CanonicalItemSnapshot>,
    ledger: HabitLedgerSnapshot,
    date: LocalDate,
): List<HabitTodayRow> {
    val habitSlices = schedule.filter { it.item.kind == ItemKind.HABIT }
    val itemById = canonicalItems.associateBy(CanonicalItemSnapshot::id)
    val scheduleGroups = habitSlices.groupBy { slice ->
        val item = slice.item
        if (item.canonicalItemId != null && item.occurrenceId != null) {
            "${item.canonicalItemId}|${item.occurrenceId}"
        } else {
            "local|${item.id}"
        }
    }
    val scheduledPlannerKeys = habitSlices.mapNotNull { slice ->
        val habitId = slice.item.canonicalItemId ?: return@mapNotNull null
        val plannerOccurrenceId = slice.item.occurrenceId ?: return@mapNotNull null
        habitId to plannerOccurrenceId
    }.toSet()
    val consumedLedgerIds = mutableSetOf<String>()
    val rows = scheduleGroups.values.map { slices ->
        val ordered = slices.sortedWith(
            compareBy<ScheduleItemPresentationSlice> { it.clippedStart }
                .thenBy { it.item.startMinute },
        )
        val first = ordered.first().item
        val habitId = first.canonicalItemId
        val plannerOccurrenceId = first.occurrenceId
        val revisions = ordered.map { it.item.canonicalRevision }.toSet()
        val revision = revisions.singleOrNull()
        val hasLegacyIdentity = habitId == null || plannerOccurrenceId == null ||
            revisions.any { it == null }
        val identityComplete = !hasLegacyIdentity && revisions.size == 1
        val matching = if (identityComplete && ledger.isBound) {
            ledger.occurrences.values.filter { occurrence ->
                occurrence.evidence.habitId == habitId &&
                    occurrence.evidence.plannerOccurrenceId == plannerOccurrenceId &&
                    occurrence.evidence.sourceItemRevision <= requireNotNull(revision)
            }
        } else {
            emptyList()
        }
        val occurrence = matching.singleOrNull()
        occurrence?.let { consumedLedgerIds += it.evidence.id }
        val fallback = when {
            !ledger.isBound -> HabitEvidenceFallback.LEDGER_NOT_READY
            hasLegacyIdentity -> HabitEvidenceFallback.LEGACY_SCHEDULE_BLOCK
            !identityComplete -> HabitEvidenceFallback.AMBIGUOUS_CANONICAL_EVIDENCE
            matching.size > 1 -> HabitEvidenceFallback.AMBIGUOUS_CANONICAL_EVIDENCE
            occurrence == null -> HabitEvidenceFallback.AWAITING_CANONICAL_EVIDENCE
            else -> null
        }
        val canonicalItem = habitId?.let(itemById::get)
        val effectiveSensitive = when {
            canonicalItem != null -> effectiveCanonicalSensitivity(canonicalItems, canonicalItem.id)
            else -> ordered.any { it.item.isSensitive }
        }
        HabitTodayRow(
            key = occurrence?.evidence?.id ?: "schedule:${ordered.first().item.id}",
            habitId = habitId,
            ledgerOccurrenceId = occurrence?.evidence?.id,
            title = first.title.ifBlank { "Habit" },
            isSensitive = effectiveSensitive,
            timeLabel = ordered.map(ScheduleItemPresentationSlice::startTimeLabel)
                .distinct()
                .take(3)
                .joinToString(" · "),
            plannedMinutes = occurrence?.evidence?.expectedDurationSeconds
                ?.let(::secondsToCeilingMinutes)
                ?: ordered.sumOf(ScheduleItemPresentationSlice::durationMinutes)
                    .takeIf { it > 0 },
            occurrence = occurrence,
            fallback = fallback,
        )
    }.toMutableList()

    ledger.occurrences.values
        .asSequence()
        .filter {
            it.evidence.id !in consumedLedgerIds && it.evidence.localDate == date.toString() &&
                (it.evidence.habitId to it.evidence.plannerOccurrenceId) !in scheduledPlannerKeys
        }
        .sortedBy { runCatching { Instant.parse(it.evidence.nominalStart) }.getOrNull() }
        .forEach { occurrence ->
            val canonicalItem = itemById[occurrence.evidence.habitId]
            val sensitive = canonicalItem?.let {
                effectiveCanonicalSensitivity(canonicalItems, it.id)
            } ?: true
            rows += HabitTodayRow(
                key = occurrence.evidence.id,
                habitId = occurrence.evidence.habitId,
                ledgerOccurrenceId = occurrence.evidence.id,
                title = canonicalItem?.title?.ifBlank { "Habit" } ?: "Private habit",
                isSensitive = sensitive,
                timeLabel = nominalTimeLabel(occurrence),
                plannedMinutes = occurrence.evidence.expectedDurationSeconds
                    ?.let(::secondsToCeilingMinutes),
                occurrence = occurrence,
                fallback = null,
            )
        }

    return rows.sortedWith(
        compareBy<HabitTodayRow> { row ->
            row.occurrence?.evidence?.nominalStart?.let {
                runCatching { Instant.parse(it) }.getOrNull()
            }
        }.thenBy { it.key },
    )
}

internal fun habitChoices(
    canonicalItems: List<CanonicalItemSnapshot>,
    schedule: List<ScheduleItemPresentationSlice>,
    ledger: HabitLedgerSnapshot = HabitLedgerSnapshot(),
): List<HabitChoice> {
    val canonical = canonicalItems.asSequence()
        .filter { it.kind == "habit" && it.deletedAt == null }
        .map { item ->
            HabitChoice(
                id = item.id,
                title = item.title.ifBlank { "Habit" },
                isSensitive = effectiveCanonicalSensitivity(canonicalItems, item.id),
            )
        }
        .toMutableList()
    val known = canonical.mapTo(mutableSetOf(), HabitChoice::id)
    schedule.asSequence()
        .map(ScheduleItemPresentationSlice::item)
        .filter { it.kind == ItemKind.HABIT && it.canonicalItemId != null }
        .forEach { item ->
            val id = requireNotNull(item.canonicalItemId)
            if (known.add(id)) {
                canonical += HabitChoice(
                    id = id,
                    title = item.title.ifBlank { "Habit" },
                    isSensitive = item.isSensitive,
                )
            }
        }
    ledger.occurrences.values.forEach { occurrence ->
        if (known.add(occurrence.evidence.habitId)) {
            canonical += HabitChoice(
                id = occurrence.evidence.habitId,
                title = "Private habit",
                isSensitive = true,
            )
        }
    }
    ledger.pauses.values.forEach { pause ->
        if (known.add(pause.habitId)) {
            canonical += HabitChoice(
                id = pause.habitId,
                title = "Private habit",
                isSensitive = true,
            )
        }
    }
    return canonical.sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.title })
        .take(MAX_PRESENTED_HABITS)
}

internal fun activePauseForHabit(
    ledger: HabitLedgerSnapshot,
    habitId: String,
): HabitPauseSnapshot? = ledger.pauses.values
    .asSequence()
    .filter { it.habitId == habitId && it.endedAt == null }
    .maxWithOrNull(compareBy<HabitPauseSnapshot> { it.revision }.thenBy { it.updatedAt })

internal fun pendingMutationForOccurrence(
    ledger: HabitLedgerSnapshot,
    ledgerOccurrenceId: String,
): PendingHabitMutation? = ledger.pendingMutations
    .filter { it.targetId == ledgerOccurrenceId }
    .maxByOrNull(PendingHabitMutation::createdAt)

internal fun reviewedHabitMutations(ledger: HabitLedgerSnapshot): List<PendingHabitMutation> =
    ledger.pendingMutations
        .filter { it.disposition != PendingHabitMutationDisposition.PENDING }
        .sortedByDescending(PendingHabitMutation::createdAt)

internal fun HabitOutcomeDraft.validate(occurredAt: String): HabitOutcomeDraftValidation {
    if (status == HabitOutcomeStatusSnapshot.UNRESOLVED) {
        val outcome = HabitOutcomeInputSnapshot(
            status = status,
            progressBasisPoints = 0,
            quantity = null,
            unit = null,
            actualSeconds = null,
            note = null,
            occurredAt = occurredAt,
        )
        return validatedOutcome(outcome)
    }

    val progressBasisPoints = when (status) {
        HabitOutcomeStatusSnapshot.COMPLETED -> 10_000
        HabitOutcomeStatusSnapshot.PARTIAL,
        HabitOutcomeStatusSnapshot.SKIPPED,
        -> parseProgressBasisPoints(progressPercent)
            ?: return invalidDraft("Enter progress as a percentage with up to two decimals")
        HabitOutcomeStatusSnapshot.UNRESOLVED -> error("handled above")
    }
    if (status == HabitOutcomeStatusSnapshot.PARTIAL && progressBasisPoints !in 1..9_999) {
        return invalidDraft("Partial progress must be between 0.01% and 99.99%")
    }
    if (status == HabitOutcomeStatusSnapshot.SKIPPED && progressBasisPoints !in 0..9_999) {
        return invalidDraft("Skipped progress must be between 0% and 99.99%")
    }

    val quantityValue = if (quantity.isEmpty()) {
        null
    } else {
        quantity.toLongOrNull()?.takeIf {
            it != Long.MIN_VALUE && kotlin.math.abs(it) <= MAX_HABIT_QUANTITY_VALUE
        } ?: return invalidDraft("Quantity must be a whole number from −1 trillion to 1 trillion")
    }
    val unitValue = unit.takeIf(String::isNotEmpty)
    if ((quantityValue == null) != (unitValue == null)) {
        return invalidDraft("Enter both quantity and unit, or leave both empty")
    }
    if (unitValue != null && !isValidHabitText(unitValue, 200, multiline = false)) {
        return invalidDraft("Unit must be 1–200 readable characters")
    }
    val actualSeconds = when {
        actualMinutes.isEmpty() -> null
        !actualMinutesEdited && originalActualSeconds != null -> originalActualSeconds
        else -> parseDurationSeconds(actualMinutes)
            ?: return invalidDraft("Duration must be non-negative minutes, precise to a second")
    }
    if (actualSeconds != null && actualSeconds !in 0..MAX_HABIT_DURATION_SECONDS) {
        return invalidDraft("Duration cannot exceed 366 days")
    }
    val noteValue = note.takeIf(String::isNotEmpty)
    if (noteValue != null && !isValidHabitText(noteValue, 10_000, multiline = true)) {
        return invalidDraft("Private note must be 1–10,000 readable characters")
    }

    return validatedOutcome(
        HabitOutcomeInputSnapshot(
            status = status,
            progressBasisPoints = progressBasisPoints,
            quantity = quantityValue,
            unit = unitValue,
            actualSeconds = actualSeconds,
            note = noteValue,
            occurredAt = occurredAt,
        ),
    )
}

internal fun HabitOutcomeDraft.selectStatus(
    selected: HabitOutcomeStatusSnapshot,
): HabitOutcomeDraft {
    val currentBasisPoints = parseProgressBasisPoints(progressPercent)
    val selectedProgress = when (selected) {
        HabitOutcomeStatusSnapshot.UNRESOLVED -> "0"
        HabitOutcomeStatusSnapshot.COMPLETED -> "100"
        HabitOutcomeStatusSnapshot.PARTIAL -> if (
            currentBasisPoints != null && currentBasisPoints in 1..9_999
        ) {
            progressPercent
        } else {
            "50"
        }
        HabitOutcomeStatusSnapshot.SKIPPED -> if (
            currentBasisPoints != null && currentBasisPoints in 0..9_999
        ) {
            progressPercent
        } else {
            "0"
        }
    }
    return copy(status = selected, progressPercent = selectedProgress)
}

internal fun analyticsFor(
    ledger: HabitLedgerSnapshot,
    habitId: String,
    bounds: ClosedRange<LocalDate>,
    bucket: HabitAnalyticsBucketSnapshot,
): HabitAnalyticsSnapshot? = ledger.analytics.values.singleOrNull {
    it.habitId == habitId && it.startDate == bounds.start.toString() &&
        it.endDate == bounds.endInclusive.toString() && it.bucket == bucket
}

internal fun supportiveHabitMessages(
    analytics: HabitAnalyticsSnapshot,
): List<String> = analytics.supportiveFactCodes.map { code ->
    when (code) {
        HabitSupportiveFactCodeSnapshot.NO_DATA ->
            "Your pattern will appear after the first eligible occurrence."
        HabitSupportiveFactCodeSnapshot.ACTIVE_STREAK ->
            "Your current rhythm is active. Keep the next step small and realistic."
        HabitSupportiveFactCodeSnapshot.STRONG_ADHERENCE ->
            "Your recent follow-through is strong. Consistency is doing the work."
        HabitSupportiveFactCodeSnapshot.FRESH_START_AVAILABLE ->
            "Today is a clean restart; a missed day does not erase your progress."
    }
}.distinct()

internal fun habitOutcomeLabel(outcome: HabitOutcomeSnapshot?): String = when (outcome?.status) {
    null, HabitOutcomeStatusSnapshot.UNRESOLVED -> "Not recorded"
    HabitOutcomeStatusSnapshot.PARTIAL -> "${formatBasisPoints(outcome.progressBasisPoints)}% done"
    HabitOutcomeStatusSnapshot.COMPLETED -> "Done"
    HabitOutcomeStatusSnapshot.SKIPPED -> if (outcome.progressBasisPoints > 0) {
        "Skipped after ${formatBasisPoints(outcome.progressBasisPoints)}%"
    } else {
        "Skipped"
    }
}

internal fun habitMutationLabel(mutation: PendingHabitMutation): String =
    when (mutation.disposition) {
        PendingHabitMutationDisposition.PENDING -> "Saved securely · waiting to sync"
        PendingHabitMutationDisposition.CONFLICT ->
            "Review needed · another device changed this habit"
        PendingHabitMutationDisposition.NOT_FOUND ->
            "Review needed · this occurrence is no longer available"
        PendingHabitMutationDisposition.REJECTED ->
            "Review needed · the saved update was rejected"
    }

internal fun formatBasisPoints(basisPoints: Int): String =
    BigDecimal.valueOf(basisPoints.toLong(), 2).stripTrailingZeros().toPlainString()

internal fun formatHabitDuration(seconds: Long): String {
    if (seconds <= 0) return "0m"
    val duration = Duration.ofSeconds(seconds)
    val days = duration.toDays()
    val hours = duration.minusDays(days).toHours()
    val minutes = duration.minusDays(days).minusHours(hours).toMinutes()
    return buildList {
        if (days > 0) add("${days}d")
        if (hours > 0) add("${hours}h")
        if (minutes > 0 || isEmpty()) add("${minutes}m")
    }.joinToString(" ")
}

private fun parseProgressBasisPoints(raw: String): Int? = runCatching {
    val percent = BigDecimal(raw)
    if (percent < BigDecimal.ZERO || percent > BigDecimal("100")) return null
    percent.movePointRight(2).longValueExact().takeIf { it in 0..10_000 }?.toInt()
}.getOrNull()

private fun parseDurationSeconds(raw: String): Long? = runCatching {
    val minutes = BigDecimal(raw)
    if (minutes < BigDecimal.ZERO) return null
    minutes.multiply(BigDecimal.valueOf(60)).longValueExact()
}.getOrNull()

private fun isValidHabitText(value: String, maximum: Int, multiline: Boolean): Boolean =
    value.isNotBlank() && value.hasAtMostUnicodeScalars(maximum) && value.none { character ->
        character.isISOControl() && !(multiline && character in setOf('\n', '\r', '\t'))
    }

private fun validatedOutcome(outcome: HabitOutcomeInputSnapshot): HabitOutcomeDraftValidation =
    runCatching { outcome.requireValid() }.fold(
        onSuccess = { HabitOutcomeDraftValidation(outcome = outcome) },
        onFailure = { invalidDraft("Check the outcome details and try again") },
    )

private fun invalidDraft(message: String) = HabitOutcomeDraftValidation(message = message)

private fun secondsToCeilingMinutes(seconds: Long): Int =
    ((seconds + 59) / 60).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

private fun nominalTimeLabel(occurrence: HabitOccurrenceSnapshot): String {
    val instant = runCatching { Instant.parse(occurrence.evidence.nominalStart) }.getOrNull()
        ?: return "Today"
    val zone = runCatching { ZoneId.of(occurrence.evidence.timezoneName) }.getOrNull()
        ?: return "Today"
    return DateTimeFormatter.ofPattern("HH:mm", Locale.getDefault()).format(instant.atZone(zone))
}

private fun formatSecondsAsEditableMinutes(seconds: Long): String =
    BigDecimal.valueOf(seconds)
        .divide(BigDecimal.valueOf(60), 2, RoundingMode.HALF_UP)
        .stripTrailingZeros()
        .toPlainString()

private val MIN_HABIT_ANALYTICS_DATE: LocalDate = LocalDate.of(1900, 1, 1)
private val MAX_HABIT_ANALYTICS_DATE: LocalDate = LocalDate.of(2200, 12, 31)
