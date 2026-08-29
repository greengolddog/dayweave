package com.greengolddog.dayweave.model

import java.time.Duration
import java.time.Instant
import java.time.LocalTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlinx.serialization.Serializable

@Serializable
enum class AppDestination(val label: String) {
    TODAY("Today"),
    CALENDAR("Calendar"),
    INBOX("Inbox"),
    ASSISTANT("Assistant"),
    MORE("More"),
}

@Serializable
enum class ItemKind(val label: String) {
    EVENT("Event"),
    TASK("Task"),
    HABIT("Habit"),
    ROUTINE("Routine"),
    GOAL("Goal"),
    BREAK("Break"),
}

@Serializable
enum class ItemStatus(val label: String) {
    NOT_STARTED("Not started"),
    SCHEDULED("Scheduled"),
    ACTIVE("In progress"),
    PAUSED("Paused"),
    COMPLETED("Completed"),
    SKIPPED("Skipped"),
    CANCELED("Canceled"),
    BLOCKED("Blocked"),
}

@Serializable
enum class EnergyLevel(val label: String) {
    LOW("Low"),
    MEDIUM("Medium"),
    DEEP("Deep"),
}

@Serializable
enum class RecoveryBand(val label: String) {
    LOW("Low"),
    BALANCED("Balanced"),
    HIGH("High"),
}

@Serializable
enum class EnergySignalSource(val label: String) {
    HEALTH_CONNECT_SLEEP("Health Connect sleep estimate"),
    MANUAL_CHECK_IN("Manual check-in"),
}

/**
 * Privacy-minimal provider output retained in the encrypted planner snapshot.
 *
 * Provider record IDs, sleep bounds/stages, and other raw health measurements intentionally do
 * not cross this boundary. A future WHOOP provider can produce the same small contract.
 */
@Serializable
data class DerivedEnergySnapshot(
    val energy: EnergyLevel,
    val recovery: RecoveryBand,
    val source: EnergySignalSource,
    val calculatedAt: String,
)

@Serializable
data class ManualEnergyCheckIn(
    val energy: EnergyLevel,
    val checkedInAt: String,
)

data class EffectiveEnergySignal(
    val energy: EnergyLevel,
    val recovery: RecoveryBand?,
    val source: EnergySignalSource,
    val recordedAt: Instant,
)

@Serializable
data class ScheduleItem(
    val id: String,
    /** Effective sensitivity after canonical ancestor propagation. */
    val isSensitive: Boolean = false,
    val title: String,
    val kind: ItemKind,
    val startMinute: Int,
    val durationMinutes: Int,
    val status: ItemStatus,
    val project: String? = null,
    val energy: EnergyLevel = EnergyLevel.MEDIUM,
    val isFlexible: Boolean = true,
    val isHardConstraint: Boolean = false,
    val isSplittable: Boolean = false,
    val note: String = "",
    val actualMinutes: Int? = null,
    /** Canonical server identity. Null only for external fixed blocks or legacy local previews. */
    val canonicalItemId: String? = null,
    val occurrenceId: String? = null,
    val canonicalRevision: Long? = null,
    val sessionIndex: Int = 0,
    /** Exact composed instants retained for DST-safe stability and overlap calculations. */
    val absoluteStartAt: String? = null,
    val absoluteEndAt: String? = null,
    val planningZoneId: String? = null,
    /** Server block kind, kept separate from UI flexibility for stability replay. */
    val canonicalBlockKind: String? = null,
) {
    val endMinute: Int get() = startMinute + durationMinutes

    fun timeRange(): String {
        val startInstant = timelineInstant()
        val endInstant = absoluteEndAt?.let { raw ->
            runCatching { Instant.parse(raw) }.getOrNull()
        }
        val zone = planningZoneId?.let { raw ->
            runCatching { ZoneId.of(raw) }.getOrNull()
        }
        if (startInstant != null && endInstant != null && zone != null) {
            val start = startInstant.atZone(zone)
            val end = endInstant.atZone(zone)
            return if (start.offset != end.offset) {
                "${start.toLocalTime().format(TIME_FORMAT)} ${start.offset}–" +
                    "${end.toLocalTime().format(TIME_FORMAT)} ${end.offset}"
            } else {
                "${start.toLocalTime().format(TIME_FORMAT)}–${end.toLocalTime().format(TIME_FORMAT)}"
            }
        }
        return "${formatMinutes(startMinute)}–${formatMinutes(endMinute)}"
    }

    fun timelineInstant(): Instant? =
        absoluteStartAt?.let { raw -> runCatching { Instant.parse(raw) }.getOrNull() }

    private fun formatMinutes(total: Int): String {
        val normalized = total.coerceIn(0, 24 * 60 - 1)
        return LocalTime.of(normalized / 60, normalized % 60).format(TIME_FORMAT)
    }

    private companion object {
        val TIME_FORMAT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm")
    }
}

/**
 * Lossless encrypted cache of the canonical item wire contract.
 *
 * Structured recurrence and constraint documents remain raw JSON so a newer server field is not
 * silently discarded by an older presentation model. They are interpreted only by the server
 * scheduler; Android extracts a small, validated display subset after composition.
 */
@Serializable
data class CanonicalItemSnapshot(
    val id: String,
    /** Own sensitivity from the canonical item wire contract. */
    val isSensitive: Boolean = false,
    val kind: String,
    val status: String,
    val title: String,
    val notes: String? = null,
    val timezoneName: String,
    val durationSeconds: Long? = null,
    val deadlineAt: String? = null,
    val earliestStartAt: String? = null,
    val recurrenceJson: String? = null,
    val flexibleConstraintsJson: String,
    val splitPolicyJson: String,
    val importance: Int,
    val urgency: Int,
    val parentId: String? = null,
    val siblingOrder: Long,
    val isExecutable: Boolean,
    val revision: Long,
    val createdAt: String,
    val updatedAt: String,
    val completedAt: String? = null,
    val deletedAt: String? = null,
)

/** Resolves ancestor privacy and fails closed when a cached hierarchy is incomplete or cyclic. */
fun effectiveCanonicalSensitivity(
    items: List<CanonicalItemSnapshot>,
    itemId: String,
    pendingMutation: PendingCanonicalMutation? = null,
): Boolean {
    val byId = items.associateBy(CanonicalItemSnapshot::id)
    val visited = mutableSetOf<String>()
    var currentId: String? = itemId
    var sensitive = false
    while (currentId != null) {
        if (!visited.add(currentId)) return true
        val item = byId[currentId] ?: return true
        sensitive = sensitive || item.isSensitive || (
            pendingMutation?.itemId == item.id && pendingMutation.targetIsSensitive
            )
        currentId = item.parentId
    }
    return sensitive
}

@Serializable
data class RecurrenceOutcomeSnapshot(
    val itemId: String,
    val status: ItemStatus,
    val resolvedAt: String,
)

@Serializable
data class RecurrenceMoveSnapshot(
    val itemId: String,
    val startAt: String,
    val endAt: String,
    val movedAt: String,
)

/**
 * Durable uncertainty fence for a canonical item replacement.
 *
 * It is written before the request leaves the device and cleared only by an exact response or a
 * later authoritative delta+preview cycle. This lets startup reconcile a request whose response
 * was lost without inventing local success or issuing a different idempotency key.
 */
@Serializable
data class PendingCanonicalMutation(
    val idempotencyKey: String,
    val syncOrigin: String,
    /** Opaque credential/workspace binding; null is migration-only and never rebound. */
    val configurationId: String? = null,
    val itemId: String,
    val expectedRevision: Long,
    val targetStatus: String,
    /** Exact own-sensitivity value carried by the durably journaled replacement body. */
    val targetIsSensitive: Boolean = false,
    val startedAt: String,
    val replacementRequestJson: String,
    val focusedBlockId: String,
    val displayStatus: ItemStatus,
    val pauseLabel: String? = null,
    val pauseMinutes: Int? = null,
    /** Terminal execution whose one-shot parent status is being projected by this exact write. */
    val terminalExecutionSessionId: String? = null,
)

/** Durable, redacted copy of the server-owned execution lease. */
@Serializable
data class CanonicalExecutionSessionSnapshot(
    val id: String,
    val itemId: String,
    val itemRevision: Long,
    val occurrenceId: String? = null,
    val sessionIndex: Int,
    val plannedBlockId: String? = null,
    val sourceDeviceId: String,
    val status: String,
    val revision: Long,
    val accumulatedSeconds: Long,
    val actualSeconds: Long? = null,
    val startedAt: String,
    val runningSince: String? = null,
    val pausedAt: String? = null,
    val pauseUntil: String? = null,
    val pauseReason: String? = null,
    val endedAt: String? = null,
    val createdAt: String,
    val updatedAt: String,
    /**
     * Local provenance captured while this exact lease still matched its original composition.
     *
     * `null` is intentionally the migration/unknown value. A server payload cannot assert this;
     * the encrypted planner store sets it only after proving that the lease represented the sole,
     * fully scheduled, executable, non-recurring and indivisible leaf at [itemRevision].
     */
    val canonicalProjectionEligibleAtLeaseStart: Boolean? = null,
)

/**
 * Durable terminal execution fact retained independently from a composed schedule.
 *
 * A schedule preview is allowed to move or replace blocks, but it cannot erase a server-confirmed
 * completion/skip. One-shot, indivisible leaves additionally project their terminal status through
 * the canonical item replacement fence; recurring and split work remains scoped to this exact
 * occurrence/session identity.
 */
@Serializable
data class TerminalExecutionOutcomeSnapshot(
    val syncOrigin: String,
    val session: CanonicalExecutionSessionSnapshot,
    val requiresCanonicalItemProjection: Boolean,
    val canonicalProjectionRevision: Long? = null,
    /** Non-null when projection was intentionally resolved without a canonical status write. */
    val canonicalProjectionResolution: String? = null,
    /** Durable user-visible reason why the latest canonical item cannot be projected safely. */
    val canonicalProjectionConflict: String? = null,
    /** Explicit user approval to make exactly one safe retry from a persisted conflict. */
    val canonicalProjectionRetryAuthorizedAt: String? = null,
    val recordedAt: String,
)

/**
 * Exact execution request persisted before network I/O.
 *
 * Retries reuse both [idempotencyKey] and [requestJson]. A response timeout therefore cannot
 * make Android invent a second session or a different transition after process death.
 */
@Serializable
data class PendingExecutionCommand(
    val idempotencyKey: String,
    val syncOrigin: String,
    val configurationId: String? = null,
    val expectedRevision: Long,
    val sessionId: String,
    val itemId: String,
    val itemRevision: Long,
    val occurrenceId: String? = null,
    val sessionIndex: Int,
    /** Authoritative value from the lease; it can be null or refer to an older composed block. */
    val plannedBlockId: String? = null,
    /** Nullable only so a pre-integration encrypted snapshot decodes and then fails closed. */
    val sourceDeviceId: String? = null,
    val commandType: String,
    val requestJson: String,
    val focusedBlockId: String,
    val startedAt: String,
    /** Minted locally and durably only while staging this client's exact lease start. */
    val canonicalProjectionEligibleAtLeaseStart: Boolean = false,
)

@Serializable
data class UnscheduledWorkSnapshot(
    val itemId: String,
    val occurrenceId: String? = null,
    val remainingMinutes: Long,
    val reason: String,
)

/** One atomically persisted server reconciliation. */
data class CanonicalPlanUpdate(
    val items: List<CanonicalItemSnapshot>,
    val schedule: List<ScheduleItem>,
    val syncOrigin: String,
    /** Opaque credential/workspace binding that produced this cache generation. */
    val configurationId: String? = null,
    val deltaCursor: String,
    val inputDigest: String,
    val generatedAt: String,
    val planningZoneId: String,
    val rejectedItemCount: Int,
    val unscheduledItemCount: Int,
    val protectedFreeMinutes: Int,
    val dayScore: Int,
    val violationMessages: List<String>,
    val violationCount: Int,
    val errorViolationCount: Int,
    val unscheduledWork: List<UnscheduledWorkSnapshot>,
    val occurrenceSeriesItemIds: Map<String, String>,
    val message: String,
)

@Serializable
data class ActiveSession(
    val itemId: String,
    val elapsedMinutes: Int,
    val isPaused: Boolean,
    val pauseLabel: String? = null,
    val accumulatedSeconds: Long = elapsedMinutes.toLong() * 60L,
    val runningSinceEpochMillis: Long? = null,
    val pauseUntilEpochMillis: Long? = null,
    /** Set when a timed break expires; resuming remains an explicit user choice by default. */
    val timedBreakEnded: Boolean = false,
    /** Non-null only when timing is owned by the canonical cross-device execution lease. */
    val canonicalExecutionSessionId: String? = null,
)

@Serializable
enum class SuggestionDisposition {
    PENDING,
    APPROVED_FOR_INBOX,
    REJECTED,
    EXPIRED,
}

@Serializable
enum class SuggestionKind(val label: String) {
    SCHEDULE_CHANGE("Schedule change"),
    NEW_TASK("New task"),
    GOAL_BREAKDOWN("Goal breakdown"),
    CONSTRAINT_CHANGE("Constraint change"),
}

@Serializable
data class PlanningSuggestion(
    val id: String,
    val title: String,
    val summary: String,
    val source: String,
    val kind: SuggestionKind,
    val expiresInDays: Int,
    val disposition: SuggestionDisposition = SuggestionDisposition.PENDING,
    /** Present only for a server-backed proposal and used for optimistic concurrency. */
    val remoteRevision: Long? = null,
    /** Cached inside the encrypted planner snapshot so the offline draft remains reviewable. */
    val remotePayloadJson: String? = null,
)

@Serializable
enum class InboxSource(val label: String) {
    QUICK_CAPTURE("Quick capture"),
    EXTERNAL_PROPOSAL("External proposal"),
    GOOGLE_TASKS("Google Tasks"),
}

@Serializable
data class InboxItem(
    val id: String,
    /** Local draft sensitivity; inherited sensitivity starts once the draft becomes canonical. */
    val isSensitive: Boolean = false,
    val title: String,
    val source: InboxSource,
    val detail: String = "",
    val requiresReview: Boolean = true,
)

@Serializable
enum class ChatRole { USER, ASSISTANT }

@Serializable
data class ChatMessage(
    val id: String,
    val role: ChatRole,
    val text: String,
)

@Serializable
data class DayWeaveUiState(
    val destination: AppDestination = AppDestination.TODAY,
    val schedule: List<ScheduleItem> = emptyList(),
    val activeSession: ActiveSession? = null,
    val inbox: List<InboxItem> = emptyList(),
    val suggestions: List<PlanningSuggestion> = emptyList(),
    val messages: List<ChatMessage> = emptyList(),
    val scheduleMessage: String = "Capture something to compose your first day",
    val protectedFreeMinutes: Int = 90,
    val dayScore: Int = 0,
    val showCompleted: Boolean = true,
    val quietSuggestions: Boolean = true,
    val useDynamicColor: Boolean = false,
    /** User-controlled foreground Health Connect reads; never implies background access. */
    val healthConnectSyncEnabled: Boolean = false,
    /** Derived bands only. Raw Health Connect records are never persisted in planner state. */
    val derivedEnergySnapshot: DerivedEnergySnapshot? = null,
    /** A same-day manual check-in overrides the provider estimate and remains correctable. */
    val manualEnergyCheckIn: ManualEnergyCheckIn? = null,
    val canonicalItems: List<CanonicalItemSnapshot> = emptyList(),
    val canonicalSyncOrigin: String? = null,
    /** Credential/workspace binding for every canonical cursor/cache field above and below. */
    val canonicalConfigurationId: String? = null,
    val canonicalDeltaCursor: String? = null,
    val scheduleInputDigest: String? = null,
    val scheduleGeneratedAt: String? = null,
    val schedulePlanningZoneId: String? = null,
    val rejectedCanonicalItemCount: Int = 0,
    val unscheduledCanonicalItemCount: Int = 0,
    val scheduleViolationMessages: List<String> = emptyList(),
    val scheduleViolationCount: Int = 0,
    val scheduleErrorViolationCount: Int = 0,
    /** Per-occurrence outcomes retained until the API exposes cross-device occurrence mutations. */
    val recurrenceOutcomes: Map<String, RecurrenceOutcomeSnapshot> = emptyMap(),
    val recurrenceMoves: Map<String, RecurrenceMoveSnapshot> = emptyMap(),
    /** Last real completion instant by recurring canonical item. */
    val recurrenceCompletionAnchors: Map<String, String> = emptyMap(),
    val pendingCanonicalMutation: PendingCanonicalMutation? = null,
    /** Origin-bound global revision and active lease returned by `/v1/execution`. */
    val canonicalExecutionSyncOrigin: String? = null,
    val canonicalExecutionConfigurationId: String? = null,
    val canonicalExecutionRevision: Long = 0,
    val canonicalExecutionSession: CanonicalExecutionSessionSnapshot? = null,
    /** Newest 100 rows from the last stable paged history read, retained for rolling overlap. */
    val canonicalExecutionHistoryWindow: List<CanonicalExecutionSessionSnapshot> = emptyList(),
    /** Workspace revision at which [canonicalExecutionHistoryWindow] was captured. */
    val canonicalExecutionHistoryWindowRevision: Long? = null,
    /** True only while this page chain remains proven from one complete history baseline. */
    val canonicalExecutionHistoryContinuityEstablished: Boolean = false,
    /** False fences every canonical start until bounded history continuity is proven. */
    val canonicalExecutionHistoryVerified: Boolean = false,
    /** Bounded ledger preventing a confirmed execution outcome from being recomposed away. */
    val terminalExecutionOutcomes: Map<String, TerminalExecutionOutcomeSnapshot> = emptyMap(),
    val pendingExecutionCommand: PendingExecutionCommand? = null,
    /** Random device identity generated once and retained only inside encrypted planner state. */
    val executionDeviceId: String? = null,
    val unscheduledWork: List<UnscheduledWorkSnapshot> = emptyList(),
    /** Materialized occurrence id to the recurring root that owns its context/actions. */
    val occurrenceSeriesItemIds: Map<String, String> = emptyMap(),
) {
    val visibleSchedule: List<ScheduleItem>
        get() = schedule
            .filter { showCompleted || it.status != ItemStatus.COMPLETED }
            .sortedWith { left, right ->
                val leftInstant = left.timelineInstant()
                val rightInstant = right.timelineInstant()
                if (leftInstant != null && rightInstant != null) {
                    leftInstant.compareTo(rightInstant)
                } else {
                    left.startMinute.compareTo(right.startMinute)
                }
            }

    val activeItem: ScheduleItem?
        get() = activeSession?.let { session -> schedule.firstOrNull { it.id == session.itemId } }

    val completedCount: Int get() = schedule.count { it.status == ItemStatus.COMPLETED }
    val pendingSuggestionCount: Int
        get() = suggestions.count { it.disposition == SuggestionDisposition.PENDING }

    fun effectiveEnergySignal(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): EffectiveEnergySignal? {
        manualEnergyCheckIn?.let { checkIn ->
            val checkedInAt = runCatching { Instant.parse(checkIn.checkedInAt) }.getOrNull()
            if (
                checkedInAt != null &&
                checkedInAt.atZone(currentZone).toLocalDate() ==
                    reference.atZone(currentZone).toLocalDate()
            ) {
                return EffectiveEnergySignal(
                    energy = checkIn.energy,
                    recovery = null,
                    source = EnergySignalSource.MANUAL_CHECK_IN,
                    recordedAt = checkedInAt,
                )
            }
        }

        val snapshot = derivedEnergySnapshot ?: return null
        val calculatedAt = runCatching { Instant.parse(snapshot.calculatedAt) }.getOrNull()
            ?: return null
        val age = Duration.between(calculatedAt, reference)
        if (age.isNegative || age > AUTOMATIC_ENERGY_MAX_AGE) return null
        return EffectiveEnergySignal(
            energy = snapshot.energy,
            recovery = snapshot.recovery,
            source = snapshot.source,
            recordedAt = calculatedAt,
        )
    }

    /** Uses the current signal only for a non-mutating next-block fit hint. */
    fun energyFitCandidate(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): ScheduleItem? {
        val capacity = effectiveEnergySignal(reference, currentZone)?.energy ?: return null
        val capacityRank = capacity.rank()
        return visibleSchedule.firstOrNull { item ->
            item.status in setOf(ItemStatus.NOT_STARTED, ItemStatus.SCHEDULED) &&
                !item.isHardConstraint && item.energy.rank() <= capacityRank
        }
    }

    fun canonicalPlanningDate(): java.time.LocalDate? {
        val generated = scheduleGeneratedAt ?: return null
        val zone = schedulePlanningZoneId ?: return null
        return runCatching {
            Instant.parse(generated).atZone(ZoneId.of(zone)).toLocalDate()
        }.getOrNull()
    }

    fun isCanonicalPlanCurrent(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): Boolean {
        if (canonicalSyncOrigin == null) return true
        val zone = schedulePlanningZoneId?.let { raw ->
            runCatching { ZoneId.of(raw) }.getOrNull()
        } ?: return false
        return zone == currentZone &&
            canonicalPlanningDate() == reference.atZone(currentZone).toLocalDate()
    }

    companion object {
        private val AUTOMATIC_ENERGY_MAX_AGE: Duration = Duration.ofHours(18)

        fun preview(): DayWeaveUiState = DayWeaveUiState(
            schedule = listOf(
                ScheduleItem(
                    id = "morning-reset",
                    title = "Morning reset",
                    kind = ItemKind.ROUTINE,
                    startMinute = 7 * 60 + 30,
                    durationMinutes = 30,
                    status = ItemStatus.COMPLETED,
                    energy = EnergyLevel.LOW,
                    note = "Water, plan, and prepare",
                    actualMinutes = 27,
                ),
                ScheduleItem(
                    id = "walk-outside",
                    title = "Walk outside",
                    kind = ItemKind.HABIT,
                    startMinute = 8 * 60 + 10,
                    durationMinutes = 30,
                    status = ItemStatus.COMPLETED,
                    project = "Health",
                    energy = EnergyLevel.LOW,
                    note = "Habit target: 30 minutes",
                    actualMinutes = 31,
                ),
                ScheduleItem(
                    id = "architecture",
                    title = "Architecture deep work",
                    kind = ItemKind.TASK,
                    startMinute = 9 * 60,
                    durationMinutes = 90,
                    status = ItemStatus.ACTIVE,
                    project = "DayWeave",
                    energy = EnergyLevel.DEEP,
                    isSplittable = true,
                    note = "Finish sync boundary and review the scheduler contract.",
                ),
                ScheduleItem(
                    id = "coffee-break",
                    title = "Coffee & reset",
                    kind = ItemKind.BREAK,
                    startMinute = 10 * 60 + 30,
                    durationMinutes = 15,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.LOW,
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Protected break",
                ),
                ScheduleItem(
                    id = "planning-call",
                    title = "Weekly planning call",
                    kind = ItemKind.EVENT,
                    startMinute = 11 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    project = "DayWeave",
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Google Calendar · attendee event",
                ),
                ScheduleItem(
                    id = "scheduler-tests",
                    title = "Review scheduler tests",
                    kind = ItemKind.TASK,
                    startMinute = 12 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    project = "DayWeave",
                    energy = EnergyLevel.DEEP,
                    isSplittable = true,
                    note = "Can split into sessions of at least 20 minutes.",
                ),
                ScheduleItem(
                    id = "lunch",
                    title = "Lunch",
                    kind = ItemKind.BREAK,
                    startMinute = 13 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.LOW,
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Protected meal",
                ),
                ScheduleItem(
                    id = "reading",
                    title = "Read 20 pages",
                    kind = ItemKind.HABIT,
                    startMinute = 16 * 60,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                    project = "Learning",
                    note = "Preferred after 15:00",
                ),
            ),
            activeSession = ActiveSession(
                itemId = "architecture",
                elapsedMinutes = 38,
                isPaused = false,
            ),
            inbox = listOf(
                InboxItem(
                    id = "inbox-google-task",
                    title = "Renew travel insurance",
                    source = InboxSource.GOOGLE_TASKS,
                    detail = "Duration and scheduling constraints needed",
                ),
            ),
            suggestions = listOf(
                PlanningSuggestion(
                    id = "recovery-window",
                    title = "Protect a recovery window",
                    summary = "Move ‘Read 20 pages’ to 17:10 and keep 16:00–17:00 free after the dense work block.",
                    source = "DayWeave assistant",
                    kind = SuggestionKind.SCHEDULE_CHANGE,
                    expiresInDays = 7,
                ),
                PlanningSuggestion(
                    id = "external-goal-draft",
                    title = "Break down fitness goal",
                    summary = "A ChatGPT conversation proposed three weekly strength sessions and a Sunday review.",
                    source = "ChatGPT · Goals conversation",
                    kind = SuggestionKind.GOAL_BREAKDOWN,
                    expiresInDays = 7,
                ),
            ),
            messages = listOf(
                ChatMessage(
                    id = "welcome",
                    role = ChatRole.ASSISTANT,
                    text = "Your hard commitments fit. The afternoon is intentionally lighter because the morning has two deep-focus blocks.",
                ),
            ),
            scheduleMessage = "Schedule is balanced",
            dayScore = 82,
        )
    }
}

/**
 * A durable pending promotion may already have committed remotely. Its target and descendants
 * therefore become locally sensitive as soon as the fence exists and remain so after restart.
 * Pending declassification never lowers the confirmed classification.
 */
fun DayWeaveUiState.withPendingSensitivityHardened(): DayWeaveUiState {
    val pending = pendingCanonicalMutation?.takeIf(PendingCanonicalMutation::targetIsSensitive)
        ?: return this
    var changed = false
    val hardenedSchedule = schedule.map { block ->
        val canonicalId = block.canonicalItemId ?: return@map block
        val mustProtect = effectiveCanonicalSensitivity(canonicalItems, canonicalId, pending)
        if (!mustProtect || block.isSensitive) return@map block
        changed = true
        block.copy(isSensitive = true)
    }
    return if (changed) copy(schedule = hardenedSchedule) else this
}

private fun EnergyLevel.rank(): Int = when (this) {
    EnergyLevel.LOW -> 0
    EnergyLevel.MEDIUM -> 1
    EnergyLevel.DEEP -> 2
}
