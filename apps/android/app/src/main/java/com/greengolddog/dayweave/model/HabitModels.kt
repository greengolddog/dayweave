package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.RemoteHabitAnalytics
import com.greengolddog.dayweave.network.RemoteHabitAnalyticsBucket
import com.greengolddog.dayweave.network.RemoteHabitMissedCancellationReason
import com.greengolddog.dayweave.network.RemoteHabitMissedPolicy
import com.greengolddog.dayweave.network.RemoteHabitMissedResolution
import com.greengolddog.dayweave.network.RemoteHabitMissedResolutionAction
import com.greengolddog.dayweave.network.RemoteHabitMissedResumeAction
import com.greengolddog.dayweave.network.RemoteHabitOccurrence
import com.greengolddog.dayweave.network.RemoteHabitOutcome
import com.greengolddog.dayweave.network.RemoteHabitOutcomeStatus
import com.greengolddog.dayweave.network.RemoteHabitPause
import com.greengolddog.dayweave.network.RemoteHabitSupportiveFactCode
import com.greengolddog.dayweave.network.RemoteHabitTrendBucket
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject

@Serializable
enum class HabitOutcomeStatusSnapshot {
    @SerialName("unresolved")
    UNRESOLVED,

    @SerialName("partial")
    PARTIAL,

    @SerialName("completed")
    COMPLETED,

    @SerialName("skipped")
    SKIPPED,
}

@Serializable
data class HabitOutcomeInputSnapshot(
    val status: HabitOutcomeStatusSnapshot,
    @SerialName("progress_basis_points") val progressBasisPoints: Int,
    val quantity: Long?,
    val unit: String?,
    @SerialName("actual_seconds") val actualSeconds: Long?,
    val note: String?,
    @SerialName("occurred_at") val occurredAt: String,
) {
    fun requireValid() {
        require(progressBasisPoints in 0..10_000)
        when (status) {
            HabitOutcomeStatusSnapshot.UNRESOLVED -> require(
                progressBasisPoints == 0 && quantity == null && unit == null &&
                    actualSeconds == null && note == null,
            )
            HabitOutcomeStatusSnapshot.PARTIAL -> require(progressBasisPoints in 1..9_999)
            HabitOutcomeStatusSnapshot.COMPLETED -> require(progressBasisPoints == 10_000)
            HabitOutcomeStatusSnapshot.SKIPPED -> require(progressBasisPoints in 0..9_999)
        }
        require(quantity == null == (unit == null))
        quantity?.let(::requireSignedHabitQuantity)
        unit?.let { requireHabitText(it, MAX_HABIT_UNIT_CHARS, multiline = false) }
        actualSeconds?.let { require(it in 0..MAX_HABIT_SECONDS) }
        note?.let { requireHabitText(it, MAX_HABIT_NOTE_CHARS, multiline = true) }
        requireHabitInstant(occurredAt)
    }
}

@Serializable
data class HabitOutcomeCommandSnapshot(
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_revision") val expectedRevision: Long,
    val outcome: HabitOutcomeInputSnapshot,
) {
    fun encoded(): String {
        requireHabitUuid(operationId)
        require(expectedRevision >= 0)
        outcome.requireValid()
        return HABIT_COMMAND_JSON.encodeToString(this)
    }
}

@Serializable
data class HabitPauseStartCommandSnapshot(
    @SerialName("operation_id") val operationId: String,
    @SerialName("pause_id") val pauseId: String,
    @SerialName("expected_revision") val expectedRevision: Long,
    @SerialName("started_at") val startedAt: String,
) {
    fun encoded(): String {
        requireHabitUuid(operationId)
        requireHabitUuid(pauseId)
        require(expectedRevision == 0L)
        requireHabitInstant(startedAt)
        return HABIT_COMMAND_JSON.encodeToString(this)
    }
}

@Serializable
data class HabitPauseResumeCommandSnapshot(
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_revision") val expectedRevision: Long,
    @SerialName("ended_at") val endedAt: String,
) {
    fun encoded(): String {
        requireHabitUuid(operationId)
        require(expectedRevision > 0)
        requireHabitInstant(endedAt)
        return HABIT_COMMAND_JSON.encodeToString(this)
    }
}

@Serializable
enum class HabitMissedExplicitActionSnapshot {
    @SerialName("skip")
    SKIP,

    @SerialName("carry")
    CARRY,

    @SerialName("reduce_frequency")
    REDUCE_FREQUENCY,
}

@Serializable
data class HabitMissedResolveCommandSnapshot(
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_revision") val expectedRevision: Long,
    val action: HabitMissedExplicitActionSnapshot,
) {
    fun encoded(): String {
        requireHabitUuid(operationId)
        require(expectedRevision > 0)
        return HABIT_COMMAND_JSON.encodeToString(this)
    }
}

@Serializable
data class HabitMissedReconcileCommandSnapshot(
    @SerialName("operation_id") val operationId: String,
) {
    fun encoded(): String {
        requireHabitUuid(operationId)
        return HABIT_COMMAND_JSON.encodeToString(this)
    }
}

@Serializable
data class HabitOutcomeSnapshot(
    val revision: Long,
    val status: HabitOutcomeStatusSnapshot,
    val progressBasisPoints: Int,
    val quantity: Long?,
    val unit: String?,
    val actualSeconds: Long?,
    val note: String?,
    val occurredAt: String,
    val updatedAt: String,
) {
    fun requireValid() {
        require(revision > 0)
        require(progressBasisPoints in 0..10_000)
        when (status) {
            HabitOutcomeStatusSnapshot.UNRESOLVED -> require(
                progressBasisPoints == 0 && quantity == null && unit == null &&
                    actualSeconds == null && note == null,
            )
            HabitOutcomeStatusSnapshot.PARTIAL -> require(progressBasisPoints in 1..9_999)
            HabitOutcomeStatusSnapshot.COMPLETED -> require(progressBasisPoints == 10_000)
            HabitOutcomeStatusSnapshot.SKIPPED -> require(progressBasisPoints in 0..9_999)
        }
        require(quantity == null == (unit == null))
        quantity?.let(::requireSignedHabitQuantity)
        unit?.let { requireHabitText(it, MAX_HABIT_UNIT_CHARS, multiline = false) }
        actualSeconds?.let { require(it in 0..MAX_HABIT_SECONDS) }
        note?.let { requireHabitText(it, MAX_HABIT_NOTE_CHARS, multiline = true) }
        requireHabitInstant(occurredAt)
        requireHabitInstant(updatedAt)
    }

    override fun toString(): String =
        "HabitOutcomeSnapshot(revision=$revision, status=$status, " +
            "progressBasisPoints=$progressBasisPoints, content=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteHabitOutcome) = HabitOutcomeSnapshot(
            revision = remote.revision,
            status = when (remote.status) {
                RemoteHabitOutcomeStatus.UNRESOLVED -> HabitOutcomeStatusSnapshot.UNRESOLVED
                RemoteHabitOutcomeStatus.PARTIAL -> HabitOutcomeStatusSnapshot.PARTIAL
                RemoteHabitOutcomeStatus.COMPLETED -> HabitOutcomeStatusSnapshot.COMPLETED
                RemoteHabitOutcomeStatus.SKIPPED -> HabitOutcomeStatusSnapshot.SKIPPED
            },
            progressBasisPoints = remote.progressBasisPoints,
            quantity = remote.quantity,
            unit = remote.unit,
            actualSeconds = remote.actualSeconds,
            note = remote.note,
            occurredAt = remote.occurredAt,
            updatedAt = remote.updatedAt,
        ).also(HabitOutcomeSnapshot::requireValid)
    }
}

@Serializable
enum class HabitMissedPolicySnapshot {
    @SerialName("skip")
    SKIP,

    @SerialName("carry")
    CARRY,

    @SerialName("reduce_frequency")
    REDUCE_FREQUENCY,

    @SerialName("ask")
    ASK,
}

@Serializable
enum class HabitMissedCancellationReasonSnapshot {
    @SerialName("source_completed")
    SOURCE_COMPLETED,

    @SerialName("source_skipped")
    SOURCE_SKIPPED,

    @SerialName("source_paused")
    SOURCE_PAUSED,

    @SerialName("source_obsolete")
    SOURCE_OBSOLETE,
}

@Serializable
enum class HabitMissedResumeActionSnapshot {
    @SerialName("decision_required")
    DECISION_REQUIRED,

    @SerialName("skip")
    SKIP,

    @SerialName("carry")
    CARRY,

    @SerialName("reduce_frequency")
    REDUCE_FREQUENCY,
}

@Serializable
sealed class HabitMissedResolutionActionSnapshot {
    @Serializable
    @SerialName("decision_required")
    data object DecisionRequired : HabitMissedResolutionActionSnapshot()

    @Serializable
    @SerialName("reduction_pending")
    data object ReductionPending : HabitMissedResolutionActionSnapshot()

    @Serializable
    @SerialName("cancelled")
    data class Cancelled(
        val reason: HabitMissedCancellationReasonSnapshot,
        val resumeAction: HabitMissedResumeActionSnapshot,
    ) : HabitMissedResolutionActionSnapshot()

    @Serializable
    @SerialName("skip")
    data object Skip : HabitMissedResolutionActionSnapshot()

    @Serializable
    @SerialName("carry")
    data class Carry(
        val windowStart: String,
        val windowEnd: String,
    ) : HabitMissedResolutionActionSnapshot()

    @Serializable
    @SerialName("reduce_frequency")
    data class ReduceFrequency(
        val suppressedPlannerOccurrenceIds: List<String>,
    ) : HabitMissedResolutionActionSnapshot()
}

@Serializable
data class HabitMissedResolutionSnapshot(
    val occurrenceEvidenceId: String,
    val habitId: String,
    val sourcePlannerOccurrenceId: String,
    val revision: Long,
    val configuredPolicy: HabitMissedPolicySnapshot,
    val action: HabitMissedResolutionActionSnapshot,
    val createdAt: String,
    val updatedAt: String,
) {
    fun requireValid() {
        requireHabitUuid(occurrenceEvidenceId)
        requireHabitUuid(habitId)
        requireHabitUuid(sourcePlannerOccurrenceId)
        require(UUID.fromString(sourcePlannerOccurrenceId).isRfc4122Version5())
        require(revision > 0)
        val created = requireHabitInstant(createdAt)
        val updated = requireHabitInstant(updatedAt)
        require(updated >= created)
        when (val value = action) {
            HabitMissedResolutionActionSnapshot.DecisionRequired ->
                require(configuredPolicy == HabitMissedPolicySnapshot.ASK)
            HabitMissedResolutionActionSnapshot.ReductionPending -> require(
                configuredPolicy == HabitMissedPolicySnapshot.REDUCE_FREQUENCY ||
                    (configuredPolicy == HabitMissedPolicySnapshot.ASK && revision >= 2),
            )
            HabitMissedResolutionActionSnapshot.Skip -> require(
                configuredPolicy == HabitMissedPolicySnapshot.SKIP ||
                    (configuredPolicy == HabitMissedPolicySnapshot.ASK && revision >= 2),
            )
            is HabitMissedResolutionActionSnapshot.Carry -> {
                require(
                    configuredPolicy == HabitMissedPolicySnapshot.CARRY ||
                        (configuredPolicy == HabitMissedPolicySnapshot.ASK && revision >= 2),
                )
                val start = requireHabitInstant(value.windowStart)
                val end = requireHabitInstant(value.windowEnd)
                require(start == updated && end > start)
                require(Duration.between(start, end) <= MAX_HABIT_MISSED_WINDOW)
            }
            is HabitMissedResolutionActionSnapshot.ReduceFrequency -> {
                require(
                    configuredPolicy == HabitMissedPolicySnapshot.REDUCE_FREQUENCY ||
                        (configuredPolicy == HabitMissedPolicySnapshot.ASK && revision >= 2),
                )
                require(value.suppressedPlannerOccurrenceIds.size == 1)
                value.suppressedPlannerOccurrenceIds.forEach { occurrenceId ->
                    requireHabitUuid(occurrenceId)
                    require(UUID.fromString(occurrenceId).isRfc4122Version5())
                }
                require(sourcePlannerOccurrenceId !in value.suppressedPlannerOccurrenceIds)
            }
            is HabitMissedResolutionActionSnapshot.Cancelled -> {
                require(revision >= 2)
                if (configuredPolicy != HabitMissedPolicySnapshot.ASK) {
                    require(
                        value.resumeAction == when (configuredPolicy) {
                            HabitMissedPolicySnapshot.SKIP -> HabitMissedResumeActionSnapshot.SKIP
                            HabitMissedPolicySnapshot.CARRY -> HabitMissedResumeActionSnapshot.CARRY
                            HabitMissedPolicySnapshot.REDUCE_FREQUENCY ->
                                HabitMissedResumeActionSnapshot.REDUCE_FREQUENCY
                            HabitMissedPolicySnapshot.ASK -> error("unreachable")
                        },
                    )
                }
            }
        }
    }

    internal fun matchesExplicitAction(requested: HabitMissedExplicitActionSnapshot): Boolean =
        when (val value = action) {
            HabitMissedResolutionActionSnapshot.Skip ->
                requested == HabitMissedExplicitActionSnapshot.SKIP
            is HabitMissedResolutionActionSnapshot.Carry ->
                requested == HabitMissedExplicitActionSnapshot.CARRY
            HabitMissedResolutionActionSnapshot.ReductionPending,
            is HabitMissedResolutionActionSnapshot.ReduceFrequency,
            -> requested == HabitMissedExplicitActionSnapshot.REDUCE_FREQUENCY
            is HabitMissedResolutionActionSnapshot.Cancelled -> value.resumeAction == when (requested) {
                HabitMissedExplicitActionSnapshot.SKIP -> HabitMissedResumeActionSnapshot.SKIP
                HabitMissedExplicitActionSnapshot.CARRY -> HabitMissedResumeActionSnapshot.CARRY
                HabitMissedExplicitActionSnapshot.REDUCE_FREQUENCY ->
                    HabitMissedResumeActionSnapshot.REDUCE_FREQUENCY
            }
            HabitMissedResolutionActionSnapshot.DecisionRequired -> false
        }

    override fun toString(): String =
        "HabitMissedResolutionSnapshot(occurrenceEvidenceId=$occurrenceEvidenceId, " +
            "revision=$revision, action=${action::class.simpleName}, content=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteHabitMissedResolution) = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = remote.occurrenceEvidenceId,
            habitId = remote.habitId,
            sourcePlannerOccurrenceId = remote.sourcePlannerOccurrenceId,
            revision = remote.revision,
            configuredPolicy = when (remote.configuredPolicy) {
                RemoteHabitMissedPolicy.SKIP -> HabitMissedPolicySnapshot.SKIP
                RemoteHabitMissedPolicy.CARRY -> HabitMissedPolicySnapshot.CARRY
                RemoteHabitMissedPolicy.REDUCE_FREQUENCY ->
                    HabitMissedPolicySnapshot.REDUCE_FREQUENCY
                RemoteHabitMissedPolicy.ASK -> HabitMissedPolicySnapshot.ASK
            },
            action = remote.action.toSnapshot(),
            createdAt = remote.createdAt,
            updatedAt = remote.updatedAt,
        ).also(HabitMissedResolutionSnapshot::requireValid)
    }
}

private fun RemoteHabitMissedResolutionAction.toSnapshot(): HabitMissedResolutionActionSnapshot =
    when (this) {
        RemoteHabitMissedResolutionAction.DecisionRequired ->
            HabitMissedResolutionActionSnapshot.DecisionRequired
        RemoteHabitMissedResolutionAction.ReductionPending ->
            HabitMissedResolutionActionSnapshot.ReductionPending
        RemoteHabitMissedResolutionAction.Skip -> HabitMissedResolutionActionSnapshot.Skip
        is RemoteHabitMissedResolutionAction.Carry ->
            HabitMissedResolutionActionSnapshot.Carry(windowStart, windowEnd)
        is RemoteHabitMissedResolutionAction.ReduceFrequency ->
            HabitMissedResolutionActionSnapshot.ReduceFrequency(
                suppressedPlannerOccurrenceIds,
            )
        is RemoteHabitMissedResolutionAction.Cancelled ->
            HabitMissedResolutionActionSnapshot.Cancelled(
                reason = when (reason) {
                    RemoteHabitMissedCancellationReason.SOURCE_COMPLETED ->
                        HabitMissedCancellationReasonSnapshot.SOURCE_COMPLETED
                    RemoteHabitMissedCancellationReason.SOURCE_SKIPPED ->
                        HabitMissedCancellationReasonSnapshot.SOURCE_SKIPPED
                    RemoteHabitMissedCancellationReason.SOURCE_PAUSED ->
                        HabitMissedCancellationReasonSnapshot.SOURCE_PAUSED
                    RemoteHabitMissedCancellationReason.SOURCE_OBSOLETE ->
                        HabitMissedCancellationReasonSnapshot.SOURCE_OBSOLETE
                },
                resumeAction = when (resumeAction) {
                    RemoteHabitMissedResumeAction.DECISION_REQUIRED ->
                        HabitMissedResumeActionSnapshot.DECISION_REQUIRED
                    RemoteHabitMissedResumeAction.SKIP -> HabitMissedResumeActionSnapshot.SKIP
                    RemoteHabitMissedResumeAction.CARRY -> HabitMissedResumeActionSnapshot.CARRY
                    RemoteHabitMissedResumeAction.REDUCE_FREQUENCY ->
                        HabitMissedResumeActionSnapshot.REDUCE_FREQUENCY
                },
            )
    }

@Serializable
data class HabitOccurrenceEvidenceSnapshot(
    val id: String,
    val habitId: String,
    val plannerOccurrenceId: String,
    val sourceScheduleRevisionId: String,
    val sourceItemRevision: Long,
    val policyFingerprint: String,
    val identity: JsonObject,
    val nominalStart: String,
    val nominalEnd: String,
    val windowStart: String,
    val windowEnd: String,
    val localDate: String,
    val timezoneName: String,
    val expectedDurationSeconds: Long?,
    val expectedQuantity: Long?,
    val expectedUnit: String?,
) {
    fun requireValid() {
        listOf(id, habitId, plannerOccurrenceId, sourceScheduleRevisionId)
            .forEach(::requireHabitUuid)
        require(UUID.fromString(plannerOccurrenceId).isRfc4122Version5())
        require(id != plannerOccurrenceId)
        require(sourceItemRevision > 0)
        require(policyFingerprint.matches(HABIT_FINGERPRINT_PATTERN))
        require(identity.isNotEmpty() && identity.size <= MAX_HABIT_IDENTITY_FIELDS)
        require(identity.toString().length <= MAX_HABIT_IDENTITY_CHARS)
        val nominalStartInstant = requireHabitEvidenceInstant(nominalStart)
        val nominalEndInstant = requireHabitEvidenceInstant(nominalEnd)
        val windowStartInstant = requireHabitEvidenceInstant(windowStart)
        val windowEndInstant = requireHabitEvidenceInstant(windowEnd)
        require(nominalStartInstant < nominalEndInstant)
        require(windowStartInstant < windowEndInstant)
        require(nominalStartInstant >= windowStartInstant && nominalEndInstant <= windowEndInstant)
        val occurrenceDate = requireHabitDate(localDate)
        require(occurrenceDate.year in MIN_HABIT_EVIDENCE_YEAR..MAX_HABIT_EVIDENCE_YEAR)
        val timezone = requireCanonicalTimezoneName(timezoneName)
        require(
            identity.matchesHabitEvidenceContext(
                occurrenceDate,
                timezone,
                nominalStartInstant,
                nominalEndInstant,
            ),
        )
        expectedDurationSeconds?.let { require(it in 1..MAX_HABIT_SECONDS) }
        expectedQuantity?.let { require(it in 1..MAX_HABIT_QUANTITY) }
        require(expectedQuantity == null == (expectedUnit == null))
        expectedUnit?.let { requireHabitText(it, MAX_HABIT_UNIT_CHARS, multiline = false) }
    }

    override fun toString(): String =
        "HabitOccurrenceEvidenceSnapshot(id=$id, habitId=$habitId, " +
            "localDate=$localDate, policy=<redacted>)"
}

@Serializable
data class HabitOccurrenceSnapshot(
    val evidence: HabitOccurrenceEvidenceSnapshot,
    val outcome: HabitOutcomeSnapshot?,
    val missedResolution: HabitMissedResolutionSnapshot? = null,
) {
    fun requireValid() {
        evidence.requireValid()
        outcome?.let { recorded ->
            recorded.requireValid()
            if (recorded.quantity != null && evidence.expectedUnit != null) {
                require(recorded.unit == evidence.expectedUnit)
            }
        }
        missedResolution?.let { resolution ->
            resolution.requireValid()
            require(resolution.occurrenceEvidenceId == evidence.id)
            require(resolution.habitId == evidence.habitId)
            require(resolution.sourcePlannerOccurrenceId == evidence.plannerOccurrenceId)
        }
    }

    override fun toString(): String =
        "HabitOccurrenceSnapshot(evidence=$evidence, outcome=${outcome?.status})"

    companion object {
        fun fromRemote(remote: RemoteHabitOccurrence) = HabitOccurrenceSnapshot(
            evidence = HabitOccurrenceEvidenceSnapshot(
                id = remote.evidence.id,
                habitId = remote.evidence.habitId,
                plannerOccurrenceId = remote.evidence.plannerOccurrenceId,
                sourceScheduleRevisionId = remote.evidence.sourceScheduleRevisionId,
                sourceItemRevision = remote.evidence.sourceItemRevision,
                policyFingerprint = remote.evidence.policyFingerprint,
                identity = remote.evidence.identity,
                nominalStart = remote.evidence.nominalStart,
                nominalEnd = remote.evidence.nominalEnd,
                windowStart = remote.evidence.windowStart,
                windowEnd = remote.evidence.windowEnd,
                localDate = remote.evidence.localDate,
                timezoneName = remote.evidence.timezoneName,
                expectedDurationSeconds = remote.evidence.expectedDurationSeconds,
                expectedQuantity = remote.evidence.expectedQuantity,
                expectedUnit = remote.evidence.expectedUnit,
            ),
            outcome = remote.outcome?.let(HabitOutcomeSnapshot::fromRemote),
            missedResolution = remote.missedResolution?.let(
                HabitMissedResolutionSnapshot::fromRemote,
            ),
        ).also(HabitOccurrenceSnapshot::requireValid)
    }
}

@Serializable
data class HabitPauseSnapshot(
    val id: String,
    val habitId: String,
    val revision: Long,
    val startedAt: String,
    val endedAt: String?,
    val preservesStreak: Boolean,
    val createdAt: String,
    val updatedAt: String,
) {
    fun requireValid() {
        requireHabitUuid(id)
        requireHabitUuid(habitId)
        require(revision > 0)
        val start = requireHabitInstant(startedAt)
        val end = endedAt?.let(::requireHabitInstant)
        val created = requireHabitInstant(createdAt)
        val updated = requireHabitInstant(updatedAt)
        require(end == null || end > start)
        // The API accepts bounded client clock skew, so started/ended may follow server record time.
        require(created <= updated)
    }

    companion object {
        fun fromRemote(remote: RemoteHabitPause) = HabitPauseSnapshot(
            id = remote.id,
            habitId = remote.habitId,
            revision = remote.revision,
            startedAt = remote.startedAt,
            endedAt = remote.endedAt,
            preservesStreak = remote.preservesStreak,
            createdAt = remote.createdAt,
            updatedAt = remote.updatedAt,
        ).also(HabitPauseSnapshot::requireValid)
    }
}

@Serializable
enum class HabitAnalyticsBucketSnapshot {
    @SerialName("day")
    DAY,

    @SerialName("week")
    WEEK,

    @SerialName("month")
    MONTH,
}

@Serializable
enum class HabitSupportiveFactCodeSnapshot {
    @SerialName("no_data")
    NO_DATA,

    @SerialName("active_streak")
    ACTIVE_STREAK,

    @SerialName("strong_adherence")
    STRONG_ADHERENCE,

    @SerialName("fresh_start_available")
    FRESH_START_AVAILABLE,
}

@Serializable
data class HabitQuantityTotalSnapshot(
    val unit: String,
    val amount: Long,
)

@Serializable
data class HabitTrendBucketSnapshot(
    val startDate: String,
    val endDate: String,
    val expected: Long,
    val eligible: Long,
    val completed: Long,
    val partial: Long,
    val skipped: Long,
    val missed: Long,
    val excused: Long,
    val unresolved: Long,
    val adherenceBasisPoints: Int,
    val actualSecondsTotal: Long,
    val quantityTotals: List<HabitQuantityTotalSnapshot>,
) {
    fun requireValid(
        withinStart: LocalDate,
        withinEnd: LocalDate,
        bucket: HabitAnalyticsBucketSnapshot,
    ) {
        val start = requireHabitDate(startDate)
        val end = requireHabitDate(endDate)
        require(start <= end && start >= withinStart && end <= withinEnd)
        val calendarStart = bucket.calendarBucketStart(start)
        require(start == maxOf(withinStart, calendarStart))
        require(end == minOf(withinEnd, bucket.calendarBucketEnd(calendarStart)))
        requireHabitTotals(
            expected,
            eligible,
            completed,
            partial,
            skipped,
            missed,
            excused,
            unresolved,
            adherenceBasisPoints,
            actualSecondsTotal,
            quantityTotals,
        )
    }

    companion object {
        fun fromRemote(remote: RemoteHabitTrendBucket) = HabitTrendBucketSnapshot(
            startDate = remote.startDate,
            endDate = remote.endDate,
            expected = remote.expected,
            eligible = remote.eligible,
            completed = remote.completed,
            partial = remote.partial,
            skipped = remote.skipped,
            missed = remote.missed,
            excused = remote.excused,
            unresolved = remote.unresolved,
            adherenceBasisPoints = remote.adherenceBasisPoints,
            actualSecondsTotal = remote.actualSecondsTotal,
            quantityTotals = remote.quantityTotals.map {
                HabitQuantityTotalSnapshot(it.unit, it.amount)
            },
        )
    }
}

@Serializable
data class HabitAnalyticsSnapshot(
    val habitId: String,
    val startDate: String,
    val endDate: String,
    val bucket: HabitAnalyticsBucketSnapshot,
    val expected: Long,
    val eligible: Long,
    val completed: Long,
    val partial: Long,
    val skipped: Long,
    val missed: Long,
    val excused: Long,
    val unresolved: Long,
    val adherenceBasisPoints: Int,
    val actualSecondsTotal: Long,
    val quantityTotals: List<HabitQuantityTotalSnapshot>,
    val currentStreak: Int,
    val longestStreak: Int,
    val trends: List<HabitTrendBucketSnapshot>,
    val supportiveFactCodes: List<HabitSupportiveFactCodeSnapshot>,
) {
    val cacheKey: String
        get() = "$habitId:$startDate:$endDate:${bucket.name.lowercase()}"

    fun requireValid() {
        requireHabitUuid(habitId)
        val start = requireHabitDate(startDate)
        val end = requireHabitDate(endDate)
        require(start <= end && end.toEpochDay() - start.toEpochDay() < 366)
        requireHabitTotals(
            expected,
            eligible,
            completed,
            partial,
            skipped,
            missed,
            excused,
            unresolved,
            adherenceBasisPoints,
            actualSecondsTotal,
            quantityTotals,
        )
        require(currentStreak in 0..366 && longestStreak in currentStreak..366)
        require(trends.size <= MAX_HABIT_TRENDS)
        var previousEnd: LocalDate? = null
        trends.forEach { trend ->
            trend.requireValid(start, end, bucket)
            val trendStart = LocalDate.parse(trend.startDate)
            previousEnd?.let { require(trendStart > it) }
            previousEnd = LocalDate.parse(trend.endDate)
        }
        requireTrendTotalsMatchAggregate()
        require(supportiveFactCodes.distinct().size == supportiveFactCodes.size)
        val expectedFacts = mutableSetOf<HabitSupportiveFactCodeSnapshot>()
        if (expected == 0L) expectedFacts += HabitSupportiveFactCodeSnapshot.NO_DATA
        if (currentStreak > 0) expectedFacts += HabitSupportiveFactCodeSnapshot.ACTIVE_STREAK
        if (eligible > 0 && adherenceBasisPoints >= 8_000) {
            expectedFacts += HabitSupportiveFactCodeSnapshot.STRONG_ADHERENCE
        }
        if (missed > 0) expectedFacts += HabitSupportiveFactCodeSnapshot.FRESH_START_AVAILABLE
        require(supportiveFactCodes.toSet() == expectedFacts)
        require(estimatedHabitCacheBytes() <= MAX_CACHED_HABIT_ANALYTICS_BYTES)
    }

    companion object {
        fun fromRemote(remote: RemoteHabitAnalytics) = HabitAnalyticsSnapshot(
            habitId = remote.habitId,
            startDate = remote.startDate,
            endDate = remote.endDate,
            bucket = when (remote.bucket) {
                RemoteHabitAnalyticsBucket.DAY -> HabitAnalyticsBucketSnapshot.DAY
                RemoteHabitAnalyticsBucket.WEEK -> HabitAnalyticsBucketSnapshot.WEEK
                RemoteHabitAnalyticsBucket.MONTH -> HabitAnalyticsBucketSnapshot.MONTH
            },
            expected = remote.expected,
            eligible = remote.eligible,
            completed = remote.completed,
            partial = remote.partial,
            skipped = remote.skipped,
            missed = remote.missed,
            excused = remote.excused,
            unresolved = remote.unresolved,
            adherenceBasisPoints = remote.adherenceBasisPoints,
            actualSecondsTotal = remote.actualSecondsTotal,
            quantityTotals = remote.quantityTotals.map {
                HabitQuantityTotalSnapshot(it.unit, it.amount)
            },
            currentStreak = remote.currentStreak,
            longestStreak = remote.longestStreak,
            trends = remote.trends.map(HabitTrendBucketSnapshot::fromRemote),
            supportiveFactCodes = remote.supportiveFactCodes.map {
                when (it) {
                    RemoteHabitSupportiveFactCode.NO_DATA -> HabitSupportiveFactCodeSnapshot.NO_DATA
                    RemoteHabitSupportiveFactCode.ACTIVE_STREAK ->
                        HabitSupportiveFactCodeSnapshot.ACTIVE_STREAK
                    RemoteHabitSupportiveFactCode.STRONG_ADHERENCE ->
                        HabitSupportiveFactCodeSnapshot.STRONG_ADHERENCE
                    RemoteHabitSupportiveFactCode.FRESH_START_AVAILABLE ->
                        HabitSupportiveFactCodeSnapshot.FRESH_START_AVAILABLE
                }
            },
        ).also(HabitAnalyticsSnapshot::requireValid)
    }
}

@Serializable
enum class PendingHabitMutationKind {
    @SerialName("outcome")
    OUTCOME,

    @SerialName("start_pause")
    START_PAUSE,

    @SerialName("resume_pause")
    RESUME_PAUSE,

    @SerialName("missed_resolution")
    MISSED_RESOLUTION,
}

@Serializable
enum class PendingHabitMutationDisposition {
    @SerialName("pending")
    PENDING,

    @SerialName("conflict")
    CONFLICT,

    @SerialName("not_found")
    NOT_FOUND,

    @SerialName("rejected")
    REJECTED,
}

/** Exact request authority retained only inside the SQLCipher planner snapshot. */
@Serializable
data class PendingHabitMutation(
    val schemaVersion: Int,
    val kind: PendingHabitMutationKind,
    val habitId: String,
    val targetId: String,
    val expectedRevision: Long,
    val idempotencyKey: String,
    val requestJson: String,
    val createdAt: String,
    val syncOrigin: String,
    val configurationId: String,
    val disposition: PendingHabitMutationDisposition =
        PendingHabitMutationDisposition.PENDING,
) {
    fun requireValid() {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        requireHabitUuid(habitId)
        requireHabitUuid(targetId)
        requireHabitUuid(idempotencyKey)
        require(expectedRevision in 0 until Long.MAX_VALUE)
        require(kind != PendingHabitMutationKind.START_PAUSE || expectedRevision == 0L)
        require(kind != PendingHabitMutationKind.MISSED_RESOLUTION || expectedRevision > 0L)
        require(requestJson.length in 2..MAX_HABIT_REQUEST_CHARS)
        requireHabitInstant(createdAt)
        require(syncOrigin.length in 1..MAX_HABIT_ORIGIN_CHARS)
        require(syncOrigin.none(Char::isISOControl))
        require(configurationId.length in 1..MAX_HABIT_CONFIGURATION_CHARS)
        require(configurationId.none(Char::isISOControl))
        requireRequestMatchesEnvelope()
    }

    private fun requireRequestMatchesEnvelope() {
        try {
            when (kind) {
                PendingHabitMutationKind.OUTCOME -> {
                    val command = HABIT_COMMAND_JSON.decodeFromString<HabitOutcomeCommandSnapshot>(
                        requestJson,
                    )
                    require(command.operationId == idempotencyKey)
                    require(command.expectedRevision == expectedRevision)
                    command.outcome.requireValid()
                }
                PendingHabitMutationKind.START_PAUSE -> {
                    val command = HABIT_COMMAND_JSON.decodeFromString<HabitPauseStartCommandSnapshot>(
                        requestJson,
                    )
                    require(command.operationId == idempotencyKey)
                    require(command.pauseId == targetId)
                    require(command.expectedRevision == expectedRevision)
                    require(command.expectedRevision == 0L)
                    requireHabitInstant(command.startedAt)
                }
                PendingHabitMutationKind.RESUME_PAUSE -> {
                    val command =
                        HABIT_COMMAND_JSON.decodeFromString<HabitPauseResumeCommandSnapshot>(
                            requestJson,
                        )
                    require(command.operationId == idempotencyKey)
                    require(command.expectedRevision == expectedRevision)
                    require(command.expectedRevision > 0)
                    requireHabitInstant(command.endedAt)
                }
                PendingHabitMutationKind.MISSED_RESOLUTION -> {
                    val command =
                        HABIT_COMMAND_JSON.decodeFromString<HabitMissedResolveCommandSnapshot>(
                            requestJson,
                        )
                    require(command.operationId == idempotencyKey)
                    require(command.expectedRevision == expectedRevision)
                    require(command.expectedRevision > 0)
                }
            }
        } catch (error: SerializationException) {
            throw IllegalArgumentException("Habit request journal is malformed", error)
        }
    }

    internal fun decodedOutcomeCommand(): HabitOutcomeCommandSnapshot {
        require(kind == PendingHabitMutationKind.OUTCOME)
        requireValid()
        return HABIT_COMMAND_JSON.decodeFromString(requestJson)
    }

    internal fun decodedPauseStartCommand(): HabitPauseStartCommandSnapshot {
        require(kind == PendingHabitMutationKind.START_PAUSE)
        requireValid()
        return HABIT_COMMAND_JSON.decodeFromString(requestJson)
    }

    internal fun decodedPauseResumeCommand(): HabitPauseResumeCommandSnapshot {
        require(kind == PendingHabitMutationKind.RESUME_PAUSE)
        requireValid()
        return HABIT_COMMAND_JSON.decodeFromString(requestJson)
    }

    internal fun decodedMissedResolutionCommand(): HabitMissedResolveCommandSnapshot {
        require(kind == PendingHabitMutationKind.MISSED_RESOLUTION)
        requireValid()
        return HABIT_COMMAND_JSON.decodeFromString(requestJson)
    }

    override fun toString(): String =
        "PendingHabitMutation(kind=$kind, habitId=$habitId, targetId=$targetId, " +
            "expectedRevision=$expectedRevision, request=<redacted>, binding=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
    }
}

/** Exact no-op reconciliation request retained until its server receipt is safely replayable. */
@Serializable
data class PendingHabitMissedReconcile(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val idempotencyKey: String,
    val requestJson: String,
    val limit: Int,
    val createdAt: String,
) {
    fun requireValid() {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        requireHabitUuid(idempotencyKey)
        require(requestJson.length in 2..MAX_HABIT_REQUEST_CHARS)
        require(limit in 1..MAX_HABIT_RECONCILE_LIMIT)
        requireHabitInstant(createdAt)
        try {
            val command = HABIT_COMMAND_JSON
                .decodeFromString<HabitMissedReconcileCommandSnapshot>(requestJson)
            require(command.operationId == idempotencyKey)
        } catch (error: SerializationException) {
            throw IllegalArgumentException("Habit reconciliation journal is malformed", error)
        }
    }

    override fun toString(): String =
        "PendingHabitMissedReconcile(limit=$limit, request=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
    }
}

/** Origin-bound encrypted offline cache and exact replay outbox for the habit ledger. */
@Serializable
data class HabitLedgerSnapshot(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val syncOrigin: String? = null,
    val configurationId: String? = null,
    val deltaCursor: String? = null,
    /** True only after the cursor was committed from a terminal (`has_more = false`) page. */
    val deltaCaughtUp: Boolean = false,
    val occurrences: Map<String, HabitOccurrenceSnapshot> = emptyMap(),
    val pauses: Map<String, HabitPauseSnapshot> = emptyMap(),
    val analytics: Map<String, HabitAnalyticsSnapshot> = emptyMap(),
    val pendingMutations: List<PendingHabitMutation> = emptyList(),
    val pendingMissedReconcile: PendingHabitMissedReconcile? = null,
) {
    val isBound: Boolean
        get() = syncOrigin != null && configurationId != null

    fun requireValid() {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        require((syncOrigin == null) == (configurationId == null))
        if (!isBound) {
            require(deltaCursor == null)
            require(!deltaCaughtUp)
            require(occurrences.isEmpty() && pauses.isEmpty() && analytics.isEmpty())
            require(pendingMutations.isEmpty())
            require(pendingMissedReconcile == null)
            return
        }
        require(syncOrigin?.length in 1..MAX_HABIT_ORIGIN_CHARS)
        require(syncOrigin?.none(Char::isISOControl) == true)
        require(configurationId?.length in 1..MAX_HABIT_CONFIGURATION_CHARS)
        require(configurationId?.none(Char::isISOControl) == true)
        deltaCursor?.let {
            require(it.length in 1..MAX_HABIT_CURSOR_CHARS)
            require(it.all(Char::isAsciiBase64UrlCharacter))
        }
        require(!deltaCaughtUp || deltaCursor != null)
        require(occurrences.size <= MAX_CACHED_HABIT_OCCURRENCES)
        require(
            occurrences.values.sumOf(HabitOccurrenceSnapshot::estimatedHabitCacheBytes) <=
                MAX_CACHED_HABIT_OCCURRENCE_BYTES,
        )
        require(
            occurrences.values.map { it.evidence.plannerOccurrenceId }.distinct().size ==
                occurrences.size,
        )
        occurrences.forEach { (id, occurrence) ->
            require(id == occurrence.evidence.id)
            occurrence.requireValid()
        }
        require(pauses.size <= MAX_CACHED_HABIT_PAUSES)
        pauses.forEach { (id, pause) ->
            require(id == pause.id)
            pause.requireValid()
        }
        pauses.values.groupBy(HabitPauseSnapshot::habitId).values.forEach { habitPauses ->
            val ordered = habitPauses.sortedWith(
                compareBy<HabitPauseSnapshot> { Instant.parse(it.startedAt) }.thenBy { it.id },
            )
            require(ordered.count { it.endedAt == null } <= 1)
            ordered.zipWithNext().forEach { (previous, next) ->
                val previousEnd = previous.endedAt?.let(Instant::parse)
                require(previousEnd != null && previousEnd <= Instant.parse(next.startedAt))
            }
        }
        require(analytics.size <= MAX_CACHED_HABIT_ANALYTICS)
        require(
            analytics.values.sumOf(HabitAnalyticsSnapshot::estimatedHabitCacheBytes) <=
                MAX_CACHED_HABIT_ANALYTICS_BYTES,
        )
        analytics.forEach { (key, value) ->
            require(key == value.cacheKey)
            value.requireValid()
        }
        require(pendingMutations.size <= MAX_PENDING_HABIT_MUTATIONS)
        require(pendingMutations.map { it.idempotencyKey }.distinct().size == pendingMutations.size)
        require(
            pendingMutations.sumOf { it.requestJson.length.toLong() } <=
                MAX_PENDING_HABIT_REQUEST_CHARS.toLong(),
        )
        pendingMutations.forEach { pending ->
            pending.requireValid()
            require(pending.syncOrigin == syncOrigin)
            require(pending.configurationId == configurationId)
        }
        pendingMissedReconcile?.let { pending ->
            pending.requireValid()
            require(!deltaCaughtUp)
            require(pendingMutations.none { it.idempotencyKey == pending.idempotencyKey })
        }
        requireValidPendingMutationRelations()
    }

    override fun toString(): String =
        "HabitLedgerSnapshot(bound=$isBound, deltaCaughtUp=$deltaCaughtUp, " +
            "occurrenceCount=${occurrences.size}, " +
            "pauseCount=${pauses.size}, analyticsCount=${analytics.size}, " +
            "pendingCount=${pendingMutations.size}, " +
            "pendingReconcile=${pendingMissedReconcile != null}, content=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        internal const val MAX_CACHED_OCCURRENCES = MAX_CACHED_HABIT_OCCURRENCES
        internal const val MAX_CACHED_PAUSES = MAX_CACHED_HABIT_PAUSES
        internal const val MAX_CACHED_ANALYTICS = MAX_CACHED_HABIT_ANALYTICS
        internal const val MAX_CACHED_OCCURRENCE_BYTES =
            MAX_CACHED_HABIT_OCCURRENCE_BYTES
        internal const val MAX_CACHED_ANALYTICS_BYTES = MAX_CACHED_HABIT_ANALYTICS_BYTES
    }
}

/**
 * Only unresolved writes carry replay authority. Reviewed failures remain inspectable even when a
 * later authoritative delta has removed or advanced their original target.
 */
private fun HabitLedgerSnapshot.requireValidPendingMutationRelations() {
    val pending = pendingMutations.filter {
        it.disposition == PendingHabitMutationDisposition.PENDING
    }
    require(pending.map { it.habitId to it.targetId }.distinct().size == pending.size)
    val pendingPauseMutations = pending.filter {
        it.kind == PendingHabitMutationKind.START_PAUSE ||
            it.kind == PendingHabitMutationKind.RESUME_PAUSE
    }
    require(
        pendingPauseMutations.map(PendingHabitMutation::habitId).distinct().size ==
            pendingPauseMutations.size,
    )
    pending.forEach { mutation ->
        when (mutation.kind) {
            PendingHabitMutationKind.OUTCOME -> {
                val occurrence = requireNotNull(occurrences[mutation.targetId])
                require(occurrence.evidence.habitId == mutation.habitId)
                require((occurrence.outcome?.revision ?: 0L) == mutation.expectedRevision)
                val outcome = mutation.decodedOutcomeCommand().outcome
                if (outcome.quantity != null && occurrence.evidence.expectedUnit != null) {
                    require(outcome.unit == occurrence.evidence.expectedUnit)
                }
            }
            PendingHabitMutationKind.START_PAUSE -> {
                require(mutation.targetId !in pauses)
                require(pauses.values.none {
                    it.habitId == mutation.habitId && it.endedAt == null
                })
            }
            PendingHabitMutationKind.RESUME_PAUSE -> {
                val pause = requireNotNull(pauses[mutation.targetId])
                require(pause.habitId == mutation.habitId)
                require(pause.endedAt == null && pause.revision == mutation.expectedRevision)
            }
            PendingHabitMutationKind.MISSED_RESOLUTION -> {
                val occurrence = requireNotNull(occurrences[mutation.targetId])
                require(occurrence.evidence.habitId == mutation.habitId)
                val resolution = requireNotNull(occurrence.missedResolution)
                require(resolution.revision == mutation.expectedRevision)
                require(resolution.configuredPolicy == HabitMissedPolicySnapshot.ASK)
                require(resolution.action == HabitMissedResolutionActionSnapshot.DecisionRequired)
            }
        }
    }
}

private fun HabitAnalyticsSnapshot.requireTrendTotalsMatchAggregate() {
    require(trends.sumOf(HabitTrendBucketSnapshot::expected) == expected)
    require(trends.sumOf(HabitTrendBucketSnapshot::eligible) == eligible)
    require(trends.sumOf(HabitTrendBucketSnapshot::completed) == completed)
    require(trends.sumOf(HabitTrendBucketSnapshot::partial) == partial)
    require(trends.sumOf(HabitTrendBucketSnapshot::skipped) == skipped)
    require(trends.sumOf(HabitTrendBucketSnapshot::missed) == missed)
    require(trends.sumOf(HabitTrendBucketSnapshot::excused) == excused)
    require(trends.sumOf(HabitTrendBucketSnapshot::unresolved) == unresolved)
    require(trends.sumOf(HabitTrendBucketSnapshot::actualSecondsTotal) == actualSecondsTotal)
    val trendQuantities = mutableMapOf<String, Long>()
    trends.flatMap(HabitTrendBucketSnapshot::quantityTotals).forEach { total ->
        val combined = runCatching {
            Math.addExact(trendQuantities[total.unit] ?: 0L, total.amount)
        }.getOrNull()
        require(combined != null)
        trendQuantities[total.unit] = combined
    }
    require(quantityTotals.associate { it.unit to it.amount } == trendQuantities)
}

private fun HabitAnalyticsBucketSnapshot.calendarBucketStart(date: LocalDate): LocalDate =
    when (this) {
        HabitAnalyticsBucketSnapshot.DAY -> date
        HabitAnalyticsBucketSnapshot.WEEK -> date.minusDays(date.dayOfWeek.value.toLong() - 1L)
        HabitAnalyticsBucketSnapshot.MONTH -> date.withDayOfMonth(1)
    }

private fun HabitAnalyticsBucketSnapshot.calendarBucketEnd(start: LocalDate): LocalDate =
    when (this) {
        HabitAnalyticsBucketSnapshot.DAY -> start
        HabitAnalyticsBucketSnapshot.WEEK -> start.plusDays(6)
        HabitAnalyticsBucketSnapshot.MONTH -> start.plusMonths(1).minusDays(1)
    }

internal fun HabitOccurrenceSnapshot.estimatedHabitCacheBytes(): Long =
    HABIT_CACHE_ENTRY_OVERHEAD_BYTES +
        evidence.id.conservativeJsonStorageBytes() +
        evidence.habitId.conservativeJsonStorageBytes() +
        evidence.plannerOccurrenceId.conservativeJsonStorageBytes() +
        evidence.sourceScheduleRevisionId.conservativeJsonStorageBytes() +
        evidence.policyFingerprint.conservativeJsonStorageBytes() +
        evidence.identity.toString().conservativeJsonStorageBytes() +
        evidence.nominalStart.conservativeJsonStorageBytes() +
        evidence.nominalEnd.conservativeJsonStorageBytes() +
        evidence.windowStart.conservativeJsonStorageBytes() +
        evidence.windowEnd.conservativeJsonStorageBytes() +
        evidence.localDate.conservativeJsonStorageBytes() +
        evidence.timezoneName.conservativeJsonStorageBytes() +
        (evidence.expectedUnit?.conservativeJsonStorageBytes() ?: 0L) +
        (outcome?.let { value ->
            HABIT_CACHE_ENTRY_OVERHEAD_BYTES +
                (value.unit?.conservativeJsonStorageBytes() ?: 0L) +
                (value.note?.conservativeJsonStorageBytes() ?: 0L) +
                value.occurredAt.conservativeJsonStorageBytes() +
                value.updatedAt.conservativeJsonStorageBytes()
        } ?: 0L) +
        (missedResolution?.let { value ->
            HABIT_CACHE_ENTRY_OVERHEAD_BYTES +
                value.occurrenceEvidenceId.conservativeJsonStorageBytes() +
                value.habitId.conservativeJsonStorageBytes() +
                value.sourcePlannerOccurrenceId.conservativeJsonStorageBytes() +
                value.createdAt.conservativeJsonStorageBytes() +
                value.updatedAt.conservativeJsonStorageBytes() +
                when (val action = value.action) {
                    is HabitMissedResolutionActionSnapshot.Carry ->
                        action.windowStart.conservativeJsonStorageBytes() +
                            action.windowEnd.conservativeJsonStorageBytes()
                    is HabitMissedResolutionActionSnapshot.ReduceFrequency ->
                        action.suppressedPlannerOccurrenceIds.sumOf {
                            it.conservativeJsonStorageBytes()
                        }
                    else -> 0L
                }
        } ?: 0L)

internal fun HabitAnalyticsSnapshot.estimatedHabitCacheBytes(): Long =
    HABIT_CACHE_ENTRY_OVERHEAD_BYTES +
        habitId.conservativeJsonStorageBytes() +
        startDate.conservativeJsonStorageBytes() +
        endDate.conservativeJsonStorageBytes() +
        quantityTotals.sumOf { total ->
            HABIT_CACHE_LIST_ENTRY_OVERHEAD_BYTES + total.unit.conservativeJsonStorageBytes()
        } +
        trends.sumOf { trend ->
            HABIT_CACHE_LIST_ENTRY_OVERHEAD_BYTES +
                trend.startDate.conservativeJsonStorageBytes() +
                trend.endDate.conservativeJsonStorageBytes() +
                trend.quantityTotals.sumOf { total ->
                    HABIT_CACHE_LIST_ENTRY_OVERHEAD_BYTES +
                        total.unit.conservativeJsonStorageBytes()
                }
        } + supportiveFactCodes.size * HABIT_CACHE_LIST_ENTRY_OVERHEAD_BYTES

private fun String.conservativeJsonStorageBytes(): Long =
    toByteArray(Charsets.UTF_8).size.toLong() * 2L + 2L

@Suppress("LongParameterList")
private fun requireHabitTotals(
    expected: Long,
    eligible: Long,
    completed: Long,
    partial: Long,
    skipped: Long,
    missed: Long,
    excused: Long,
    unresolved: Long,
    adherenceBasisPoints: Int,
    actualSecondsTotal: Long,
    quantityTotals: List<HabitQuantityTotalSnapshot>,
) {
    val counts = listOf(expected, eligible, completed, partial, skipped, missed, excused, unresolved)
    require(counts.all { it in 0..MAX_HABIT_ANALYTICS_OCCURRENCES })
    require(eligible + excused == expected)
    require(completed + partial + skipped + missed + unresolved == eligible)
    require(adherenceBasisPoints in 0..10_000)
    require(actualSecondsTotal in 0..MAX_HABIT_ANALYTICS_SECONDS)
    require(quantityTotals.size <= MAX_HABIT_QUANTITY_TOTALS)
    require(quantityTotals.map { it.unit }.distinct().size == quantityTotals.size)
    quantityTotals.forEach { total ->
        requireHabitText(total.unit, MAX_HABIT_UNIT_CHARS, multiline = false)
        requireSignedHabitQuantity(total.amount, MAX_HABIT_ANALYTICS_QUANTITY)
    }
}

private fun requireHabitUuid(value: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed != UUID(0L, 0L) && parsed.toString() == value)
}

private fun requireHabitInstant(value: String): Instant {
    val parsed = Instant.parse(value)
    require(parsed.toString() == value)
    require(parsed.nano % 1_000 == 0) { "Habit timestamps cannot exceed microsecond precision" }
    return parsed
}

private fun requireHabitEvidenceInstant(value: String): Instant =
    requireHabitInstant(value).also { instant ->
        require(
            instant.atOffset(ZoneOffset.UTC).year in
                MIN_HABIT_EVIDENCE_INSTANT_YEAR..MAX_HABIT_EVIDENCE_INSTANT_YEAR,
        )
    }

private fun requireHabitDate(value: String): LocalDate {
    val parsed = LocalDate.parse(value)
    require(parsed.toString() == value)
    return parsed
}

private fun requireHabitText(value: String, maxChars: Int, multiline: Boolean) {
    require(value.isNotBlank() && value.hasAtMostUnicodeScalars(maxChars))
    require(value.none { it.isISOControl() && !(multiline && it in setOf('\n', '\r', '\t')) })
}

private fun requireSignedHabitQuantity(
    value: Long,
    maximum: Long = MAX_HABIT_QUANTITY,
) {
    require(value != Long.MIN_VALUE && kotlin.math.abs(value) <= maximum)
}

private val HABIT_FINGERPRINT_PATTERN = Regex("sha256:[0-9a-f]{64}")
private val HABIT_COMMAND_JSON = Json {
    encodeDefaults = true
    explicitNulls = true
    ignoreUnknownKeys = false
}
private const val MAX_HABIT_NOTE_CHARS = 10_000
private const val MAX_HABIT_UNIT_CHARS = 200
private const val MAX_HABIT_IDENTITY_FIELDS = 32
private const val MAX_HABIT_IDENTITY_CHARS = 16 * 1024
private const val MIN_HABIT_EVIDENCE_YEAR = 1_900
private const val MAX_HABIT_EVIDENCE_YEAR = 2_200
private const val MIN_HABIT_EVIDENCE_INSTANT_YEAR = 1
private const val MAX_HABIT_EVIDENCE_INSTANT_YEAR = 9_999
private const val MAX_HABIT_QUANTITY = 1_000_000_000_000L
private const val MAX_HABIT_SECONDS = 366L * 24 * 60 * 60
private val MAX_HABIT_MISSED_WINDOW: Duration = Duration.ofDays(366)
private const val MAX_HABIT_ANALYTICS_OCCURRENCES = 50_000L
private const val MAX_HABIT_ANALYTICS_SECONDS = MAX_HABIT_ANALYTICS_OCCURRENCES * MAX_HABIT_SECONDS
private const val MAX_HABIT_ANALYTICS_QUANTITY =
    MAX_HABIT_ANALYTICS_OCCURRENCES * MAX_HABIT_QUANTITY
private const val MAX_HABIT_QUANTITY_TOTALS = 200
private const val MAX_HABIT_TRENDS = 366
private const val MAX_HABIT_REQUEST_CHARS = 64 * 1024
private const val MAX_HABIT_ORIGIN_CHARS = 4_096
private const val MAX_HABIT_CONFIGURATION_CHARS = 4_096
private fun Char.isAsciiBase64UrlCharacter(): Boolean =
    this in 'a'..'z' || this in 'A'..'Z' || this in '0'..'9' || this == '-' || this == '_'

private const val MAX_HABIT_CURSOR_CHARS = 256
private const val MAX_CACHED_HABIT_OCCURRENCES = 10_000
private const val MAX_CACHED_HABIT_PAUSES = 2_000
private const val MAX_CACHED_HABIT_ANALYTICS = 256
private const val MAX_CACHED_HABIT_OCCURRENCE_BYTES = 16L * 1024 * 1024
private const val MAX_CACHED_HABIT_ANALYTICS_BYTES = 4L * 1024 * 1024
private const val HABIT_CACHE_ENTRY_OVERHEAD_BYTES = 1_024L
private const val HABIT_CACHE_LIST_ENTRY_OVERHEAD_BYTES = 512L
private const val MAX_PENDING_HABIT_MUTATIONS = 256
private const val MAX_PENDING_HABIT_REQUEST_CHARS = 2 * 1024 * 1024
private const val MAX_HABIT_RECONCILE_LIMIT = 200
