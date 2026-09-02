package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.ProposalApplicationHttpRequest
import com.greengolddog.dayweave.network.SchedulePublishHttpRequest
import java.security.MessageDigest
import java.time.DayOfWeek
import java.time.Duration
import java.time.Instant
import java.time.LocalTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

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
    /** Server-assigned execution identity. Null for local helpers and legacy cached blocks. */
    val sessionIndex: Int? = null,
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

/** A view-only clipped interval. [item] retains the exact proof-bound publication identity. */
data class ScheduleItemPresentationSlice(
    val item: ScheduleItem,
    val clippedStart: Instant?,
    val clippedEnd: Instant?,
    val startTimeLabel: String,
    val weekStartLabel: String,
    val durationMinutes: Int,
    val durationLabel: String,
    val continuationLabel: String? = null,
)

private const val PRESENTATION_MINUTES_PER_DAY = 24 * 60
private val SLICE_TIME_FORMAT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm")
private val SLICE_WEEK_FORMAT: DateTimeFormatter =
    DateTimeFormatter.ofPattern("EEE HH:mm", Locale.getDefault())

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
    pendingAuthoringMutations: List<PendingCanonicalAuthoringMutation> = emptyList(),
): Boolean {
    val ownSensitivity = mutableMapOf<String, Boolean>()
    val possibleParents = mutableMapOf<String, MutableSet<String>>()
    fun retain(item: CanonicalItemSnapshot) {
        ownSensitivity[item.id] = ownSensitivity[item.id] == true || item.isSensitive
        item.parentId?.let { possibleParents.getOrPut(item.id) { mutableSetOf() }.add(it) }
    }
    items.forEach(::retain)
    for (mutation in pendingAuthoringMutations) {
        if (runCatching { mutation.requireValid() }.isFailure) return true
        mutation.baseItem?.let(::retain)
        when (mutation.operation) {
            CanonicalAuthoringOperation.CREATE,
            CanonicalAuthoringOperation.REPLACE,
            -> {
                val draft = mutation.draft ?: return true
                ownSensitivity[mutation.itemId] =
                    ownSensitivity[mutation.itemId] == true || draft.isSensitive
                draft.parentId?.let {
                    possibleParents.getOrPut(mutation.itemId) { mutableSetOf() }.add(it)
                }
            }
            CanonicalAuthoringOperation.TRASH -> Unit
            CanonicalAuthoringOperation.RESTORE -> if (mutation.baseItem == null) {
                // A bodyless restore has an unknown own mark and ancestor path.
                ownSensitivity[mutation.itemId] = true
            }
        }
    }
    pendingMutation?.takeIf(PendingCanonicalMutation::targetIsSensitive)?.let {
        ownSensitivity[it.itemId] = true
    }

    // Pending reparenting retains both old and proposed ancestor paths. It can therefore raise
    // privacy immediately but can never declassify a confirmed item before reconciliation.
    val colors = mutableMapOf<String, Int>()
    val stack = mutableListOf(itemId to false)
    while (stack.isNotEmpty()) {
        val (currentId, exiting) = stack.removeAt(stack.lastIndex)
        if (exiting) {
            colors[currentId] = 2
            continue
        }
        when (colors[currentId]) {
            1 -> return true
            2 -> continue
        }
        val sensitive = ownSensitivity[currentId] ?: return true
        if (sensitive) return true
        colors[currentId] = 1
        stack.add(currentId to true)
        possibleParents[currentId].orEmpty().forEach { parentId ->
            stack.add(parentId to false)
        }
    }
    return false
}

@Serializable
data class RecurrenceOutcomeSnapshot(
    val itemId: String,
    val status: ItemStatus,
    val resolvedAt: String,
)

@Serializable
data class RecurrenceOccurrenceSourceSnapshot(
    val itemId: String,
    val itemRevision: Long,
    /** Nullable only for migration; a new Move requires a validated server-issued identity. */
    val identityJson: String? = null,
    val nominalStart: String,
    val nominalEnd: String,
    val localDate: String? = null,
    val ordinal: Long,
)

@Serializable
data class RecurrenceMoveSnapshot(
    val itemId: String,
    val startAt: String,
    val endAt: String,
    val movedAt: String,
    /** Exact source identity required to restore this occurrence outside its nominal horizon. */
    val source: RecurrenceOccurrenceSourceSnapshot? = null,
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
    /** Exact future placement requested when a server-owned session closes as deferred. */
    val moveStart: String? = null,
    val moveEnd: String? = null,
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
 * Durable closed execution fact retained independently from a composed schedule.
 *
 * A schedule preview is allowed to move or replace blocks, but it cannot erase a server-confirmed
 * closure. One-shot, indivisible leaves additionally project a completion/skip through the
 * canonical item replacement fence; deferred, recurring, and split work remain scoped to the exact
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

/** Content-free fixed-block identity returned by the authoritative defer assessor. */
@Serializable
data class ExecutionDeferConflictSnapshot(
    @SerialName("block_id")
    val blockId: String,
    @SerialName("item_id")
    val itemId: String? = null,
    @SerialName("occurrence_id")
    val occurrenceId: String? = null,
    @SerialName("external_block_id")
    val externalBlockId: String? = null,
    val kind: String,
    val start: String,
    val end: String,
)

/** Content-free policy violation returned by the authoritative defer assessor. */
@Serializable
data class ExecutionDeferViolationSnapshot(
    val code: String,
    @SerialName("item_ids")
    val itemIds: List<String>,
    @SerialName("occurrence_ids")
    val occurrenceIds: List<String>,
    @SerialName("conflicting_block_ids")
    val conflictingBlockIds: List<String>,
    @SerialName("conflicting_blocks")
    val conflictingBlocks: List<ExecutionDeferConflictSnapshot>,
    val start: String,
    val end: String,
    @SerialName("boundary_start")
    val boundaryStart: String? = null,
    @SerialName("boundary_end")
    val boundaryEnd: String? = null,
    val message: String,
)

/**
 * Exact server response authorizing one paused-session Defer candidate.
 *
 * This remains encrypted with the intent so a conflict review can survive process death. It has
 * no item title, notes, calendar title, or other user content.
 */
@Serializable
data class ExecutionDeferAssessmentSnapshot(
    @SerialName("session_id")
    val sessionId: String,
    @SerialName("execution_revision")
    val executionRevision: Long,
    @SerialName("session_revision")
    val sessionRevision: Long,
    @SerialName("item_id")
    val itemId: String,
    @SerialName("item_revision")
    val itemRevision: Long,
    @SerialName("occurrence_id")
    val occurrenceId: String? = null,
    @SerialName("source_session_index")
    val sourceSessionIndex: Int,
    @SerialName("replacement_session_index")
    val replacementSessionIndex: Int,
    @SerialName("source_schedule_revision_id")
    val sourceScheduleRevisionId: String,
    @SerialName("source_block_id")
    val sourceBlockId: String,
    @SerialName("actual_seconds")
    val actualSeconds: Long,
    @SerialName("credited_source_seconds")
    val creditedSourceSeconds: Long,
    @SerialName("planned_duration_seconds")
    val plannedDurationSeconds: Long,
    @SerialName("remaining_duration_seconds")
    val remainingDurationSeconds: Long,
    @SerialName("move_start")
    val moveStart: String,
    @SerialName("move_end")
    val moveEnd: String,
    @SerialName("environment_digest")
    val environmentDigest: String,
    @SerialName("assessment_digest")
    val assessmentDigest: String,
    @SerialName("approval_required")
    val approvalRequired: Boolean,
    val violations: List<ExecutionDeferViolationSnapshot>,
    @SerialName("expires_at")
    val expiresAt: String,
)

/**
 * Durable user intent spanning the active-to-paused-to-deferred execution transition.
 *
 * Pause and Defer are separate server commands. Persisting this exact source/target tuple before
 * Pause prevents a lost pause response or process death from forgetting the user's selected time.
 */
@Serializable
data class PendingExecutionDeferIntent(
    /** Zero is migration-only; legacy locally assessed intents are abandoned fail-closed. */
    val schemaVersion: Int = 0,
    val syncOrigin: String,
    val configurationId: String? = null,
    val sessionId: String,
    val itemId: String,
    val itemRevision: Long,
    val occurrenceId: String? = null,
    val sessionIndex: Int,
    val plannedBlockId: String,
    val sourceDeviceId: String,
    val focusedBlockId: String,
    val sourceStart: String,
    val sourceEnd: String,
    val moveStart: String,
    val stagedAt: String,
    /** Exact authoritative response; null while Pause or assessment is still pending. */
    val assessment: ExecutionDeferAssessmentSnapshot? = null,
    /** Set only by an explicit tap for the exact current [assessment] digest. */
    val approvedAssessmentDigest: String? = null,
    /** Legacy local-warning fields retained only so older encrypted snapshots can fail closed. */
    val approvedConflictTargetEnd: String? = null,
    val approvedDeadlineRisks: List<MoveLaterDeadlineRisk> = emptyList(),
    val approvedSourceOverride: Boolean = false,
    val approvedItemRevisions: Map<String, Long> = emptyMap(),
    val approvedHardBlockIds: List<String> = emptyList(),
    val approvedHardConflicts: List<MoveLaterConflictIdentity> = emptyList(),
)

@Serializable
data class UnscheduledWorkSnapshot(
    val itemId: String,
    val occurrenceId: String? = null,
    val remainingMinutes: Long,
    val reason: String,
)

/** One atomically persisted server reconciliation. */
@Serializable
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
    val occurrenceSources: Map<String, RecurrenceOccurrenceSourceSnapshot> = emptyMap(),
    val message: String,
)

/**
 * Encrypted, device-local evidence for a schedule composed by the bundled deterministic core.
 *
 * This is display provenance only. It deliberately contains no server input digest, published
 * revision, publication proof, or execution authority, and therefore can never authorize a
 * canonical execution command or a schedule publication.
 */
@Serializable
data class LocalScheduleCompositionProvenanceSnapshot(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val syncOrigin: String,
    val configurationId: String,
    val deltaCursor: String,
    val localInputFingerprint: String,
    /** Digest of the exact helper request, including work windows/config/fixed blocks. */
    val scheduleRequestFingerprint: String,
    /** Digest of every mutable planner input that must invalidate display provenance. */
    val stateInputFingerprint: String,
    val generatedAt: String,
    val asOf: String,
    val horizonStart: String,
    val horizonEnd: String,
    val timezoneName: String,
    val sourceItemRevisions: Map<String, Long>,
) {
    fun hasValidShape(): Boolean = runCatching {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        requireBoundedText(syncOrigin, MAX_BINDING_CHARS)
        requireBoundedText(configurationId, MAX_BINDING_CHARS)
        require(
            deltaCursor.toByteArray(Charsets.UTF_8).size in 1..MAX_CURSOR_BYTES &&
                deltaCursor.all { character ->
                    character.code in 0x21..0x7e && character != '"' && character != '\\'
                },
        )
        require(
            localInputFingerprint.length == LOCAL_FINGERPRINT_PREFIX.length + 64 &&
                localInputFingerprint.startsWith(LOCAL_FINGERPRINT_PREFIX) &&
                localInputFingerprint.drop(LOCAL_FINGERPRINT_PREFIX.length).all {
                    it in '0'..'9' || it in 'a'..'f'
                },
        )
        requireSha256(scheduleRequestFingerprint)
        requireSha256(stateInputFingerprint)
        val generated = requireCanonicalInstant(generatedAt)
        val requestAsOf = requireCanonicalInstant(asOf)
        val start = requireCanonicalInstant(horizonStart)
        val end = requireCanonicalInstant(horizonEnd)
        require(generated == requestAsOf && start <= requestAsOf && requestAsOf < end)
        val zone = ZoneId.of(timezoneName)
        val horizonDate = start.atZone(zone).toLocalDate()
        require(
            start == horizonDate.atStartOfDay(zone).toInstant() &&
                end == horizonDate.plusDays(1).atStartOfDay(zone).toInstant() &&
                requestAsOf.atZone(zone).toLocalDate() == horizonDate,
        )
        require(sourceItemRevisions.size <= MAX_SOURCE_ITEMS)
        sourceItemRevisions.forEach { (id, revision) ->
            val parsed = UUID.fromString(id)
            require(parsed.toString() == id && revision > 0)
        }
    }.isSuccess

    fun matchesState(state: DayWeaveUiState): Boolean {
        if (state.hasMemoizedLocalScheduleCompositionValidation(this)) return true
        if (!hasValidShape()) return false
        if (!state.scheduleCompositionProfile.hasValidShape()) return false
        if (
            state.canonicalSyncOrigin != syncOrigin ||
            state.canonicalConfigurationId != configurationId ||
            state.canonicalDeltaCursor != deltaCursor ||
            state.scheduleGeneratedAt != generatedAt ||
            state.schedulePlanningZoneId != timezoneName ||
            state.pendingSchedulePublication != null ||
            state.publishedScheduleRevision != null ||
            state.publishedScheduleProof != null ||
            state.scheduleInputDigest != null
        ) {
            return false
        }
        val revisions = state.canonicalItems.associate { it.id to it.revision }
        if (revisions != sourceItemRevisions) return false
        if (state.localScheduleCompositionStateFingerprint() != stateInputFingerprint) return false
        val matchesBlocks = state.schedule.all { block ->
            val itemId = block.canonicalItemId ?: return@all true
            val revision = block.canonicalRevision ?: return@all false
            sourceItemRevisions[itemId] == revision
        }
        if (matchesBlocks) state.memoizeLocalScheduleCompositionValidation(this)
        return matchesBlocks
    }

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        private const val MAX_BINDING_CHARS = 4_096
        private const val MAX_CURSOR_BYTES = 256
        private const val MAX_SOURCE_ITEMS = 10_000
        private const val LOCAL_FINGERPRINT_PREFIX = "local-sha256:"
        private const val SHA256_PREFIX = "sha256:"

        private fun requireBoundedText(raw: String, limit: Int) {
            require(raw.isNotBlank() && raw.length <= limit && raw.none(Char::isISOControl))
        }

        private fun requireCanonicalInstant(raw: String): Instant = Instant.parse(raw).also {
            require(it.toString() == raw)
        }

        private fun requireSha256(raw: String) {
            require(
                raw.length == SHA256_PREFIX.length + 64 && raw.startsWith(SHA256_PREFIX) &&
                    raw.drop(SHA256_PREFIX.length).all {
                        it in '0'..'9' || it in 'a'..'f'
                    },
            )
        }
    }
}

/** Encrypted scheduling policy used by both remote and bundled deterministic composition. */
@Serializable
data class ScheduleCompositionProfileSnapshot(
    val dayStartMinute: Int = 7 * 60,
    val dayEndMinute: Int = 22 * 60,
    val slotGranularityMinutes: Int = 5,
    val stabilityWeight: Int = 4,
    val defaultSoftWeight: Int = 100,
) {
    fun hasValidShape(): Boolean =
        dayStartMinute in 0 until MINUTES_PER_DAY &&
            dayEndMinute in 1..MINUTES_PER_DAY && dayEndMinute > dayStartMinute &&
            slotGranularityMinutes in 1..60 &&
            stabilityWeight in 0..1_000_000 &&
            defaultSoftWeight in 0..1_000_000

    private companion object {
        const val MINUTES_PER_DAY = 24 * 60
    }
}

@Serializable
private data class LocalScheduleMutableInputSnapshot(
    val scheduleCompositionProfile: ScheduleCompositionProfileSnapshot,
    val canonicalItems: List<CanonicalItemSnapshot>,
    val schedule: List<ScheduleItem>,
    val recurrenceOutcomes: Map<String, RecurrenceOutcomeSnapshot>,
    val recurrenceMoves: Map<String, RecurrenceMoveSnapshot>,
    val recurrenceCompletionAnchors: Map<String, String>,
    val occurrenceSeriesItemIds: Map<String, String>,
    val recurrenceOccurrenceSources: Map<String, RecurrenceOccurrenceSourceSnapshot>,
    val canonicalExecutionSyncOrigin: String?,
    val canonicalExecutionConfigurationId: String?,
    val canonicalExecutionRevision: Long,
    val canonicalExecutionSession: CanonicalExecutionSessionSnapshot?,
    val canonicalExecutionHistoryWindow: List<CanonicalExecutionSessionSnapshot>,
    val canonicalExecutionHistoryWindowRevision: Long?,
    val canonicalExecutionHistoryContinuityEstablished: Boolean,
    val terminalExecutionOutcomes: Map<String, TerminalExecutionOutcomeSnapshot>,
    val hasPendingExecutionCommand: Boolean,
    val hasPendingExecutionDeferIntent: Boolean,
    val hasActiveSession: Boolean,
    val hasPendingCanonicalMutation: Boolean,
    val hasPendingCanonicalAuthoringMutation: Boolean,
    val hasPendingProposalApplicationMutation: Boolean,
)

/** Content-free deterministic fence for every mutable input currently used by previewRequest. */
fun DayWeaveUiState.localScheduleCompositionStateFingerprint(): String {
    localScheduleCompositionFingerprintMemo.get()?.let { return it }
    LOCAL_COMPOSITION_FINGERPRINT_COMPUTATIONS.incrementAndGet()
    val snapshot = LocalScheduleMutableInputSnapshot(
        scheduleCompositionProfile = scheduleCompositionProfile,
        canonicalItems = canonicalItems.sortedBy(CanonicalItemSnapshot::id),
        schedule = schedule,
        recurrenceOutcomes = recurrenceOutcomes.toSortedMap(),
        recurrenceMoves = recurrenceMoves.toSortedMap(),
        recurrenceCompletionAnchors = recurrenceCompletionAnchors.toSortedMap(),
        occurrenceSeriesItemIds = occurrenceSeriesItemIds.toSortedMap(),
        recurrenceOccurrenceSources = recurrenceOccurrenceSources.toSortedMap(),
        canonicalExecutionSyncOrigin = canonicalExecutionSyncOrigin,
        canonicalExecutionConfigurationId = canonicalExecutionConfigurationId,
        canonicalExecutionRevision = canonicalExecutionRevision,
        canonicalExecutionSession = canonicalExecutionSession,
        canonicalExecutionHistoryWindow = canonicalExecutionHistoryWindow.sortedBy { it.revision },
        canonicalExecutionHistoryWindowRevision = canonicalExecutionHistoryWindowRevision,
        canonicalExecutionHistoryContinuityEstablished =
            canonicalExecutionHistoryContinuityEstablished,
        terminalExecutionOutcomes = terminalExecutionOutcomes.toSortedMap(),
        hasPendingExecutionCommand = pendingExecutionCommand != null,
        hasPendingExecutionDeferIntent = pendingExecutionDeferIntent != null,
        hasActiveSession = activeSession != null,
        hasPendingCanonicalMutation = pendingCanonicalMutation != null,
        hasPendingCanonicalAuthoringMutation = pendingCanonicalAuthoringMutations.isNotEmpty(),
        hasPendingProposalApplicationMutation = pendingProposalApplicationMutation != null,
    )
    val bytes = LOCAL_COMPOSITION_FINGERPRINT_JSON.encodeToString(snapshot)
        .toByteArray(Charsets.UTF_8)
    val fingerprint = "sha256:" + MessageDigest.getInstance("SHA-256")
        .digest(bytes).joinToString("") {
        byte -> "%02x".format(byte.toInt() and 0xff)
    }
    localScheduleCompositionFingerprintMemo.compareAndSet(null, fingerprint)
    return localScheduleCompositionFingerprintMemo.get() ?: fingerprint
}

internal fun localScheduleCompositionFingerprintComputationCount(): Long =
    LOCAL_COMPOSITION_FINGERPRINT_COMPUTATIONS.get()

private val LOCAL_COMPOSITION_FINGERPRINT_COMPUTATIONS = AtomicLong(0)

private val LOCAL_COMPOSITION_FINGERPRINT_JSON = Json {
    encodeDefaults = true
    explicitNulls = true
}

@Serializable
data class PublishedScheduleRevisionSnapshot(
    val id: String,
    val revision: String,
    val revisionNumber: ULong,
    val inputDigest: String,
    val horizonStart: String,
    val horizonEnd: String,
    val timezoneName: String,
    val publishedAt: String,
)

/** Exact immutable identity of one block accepted for schedule publication. */
@Serializable
data class PublishedScheduleBlockProofSnapshot(
    val id: String,
    val itemId: String? = null,
    val itemRevision: Long? = null,
    val occurrenceId: String? = null,
    val sessionIndex: Int? = null,
    val start: String,
    val end: String,
    val kind: String,
    /** SHA-256 of every publication-static [ScheduleItem] field, excluding execution projection. */
    val immutableDigest: String? = null,
) {
    fun hasValidShape(
        horizonStart: Instant,
        horizonEnd: Instant,
        requireFullSeal: Boolean,
    ): Boolean = runCatching {
        requireCanonicalUuid(id, "published schedule block")
        if (itemId == null) {
            require(itemRevision == null && occurrenceId == null)
            require(!requireFullSeal || kind in setOf("calendar_event", "external_fixed"))
        } else {
            requireCanonicalUuid(itemId, "published schedule item")
            occurrenceId?.let { requireCanonicalUuid(it, "published schedule occurrence") }
            require(requireNotNull(itemRevision) > 0)
        }
        require(requireNotNull(sessionIndex) in 0..UShort.MAX_VALUE.toInt())
        require(kind.isNotBlank() && kind.length <= 128 && kind.none(Char::isISOControl))
        if (requireFullSeal) requireScheduleDigest(requireNotNull(immutableDigest))
        val blockStart = Instant.parse(start)
        val blockEnd = Instant.parse(end)
        require(blockStart < blockEnd)
        // Calendar and pinned blocks may retain their original overnight bounds. Publication
        // authority therefore requires exact overlap with the receipt horizon, not containment.
        require(blockStart < horizonEnd && horizonStart < blockEnd)
    }.isSuccess

    fun matches(block: ScheduleItem): Boolean =
        block.id == id &&
            block.canonicalItemId == itemId &&
            block.canonicalRevision == itemRevision &&
            block.occurrenceId == occurrenceId &&
            block.sessionIndex == sessionIndex &&
            block.canonicalBlockKind == kind &&
            sameInstant(block.absoluteStartAt, start) &&
            sameInstant(block.absoluteEndAt, end) &&
            immutableDigest?.let { it == publishedScheduleBlockImmutableDigest(block) } != false

    private fun sameInstant(left: String?, right: String): Boolean =
        left?.let { raw -> runCatching { Instant.parse(raw) }.getOrNull() } ==
            runCatching { Instant.parse(right) }.getOrNull()

    companion object {
        fun from(block: ScheduleItem): PublishedScheduleBlockProofSnapshot =
            PublishedScheduleBlockProofSnapshot(
                id = block.id,
                itemId = block.canonicalItemId,
                itemRevision = block.canonicalRevision,
                occurrenceId = block.occurrenceId,
                sessionIndex = block.sessionIndex,
                start = requireNotNull(block.absoluteStartAt),
                end = requireNotNull(block.absoluteEndAt),
                kind = requireNotNull(block.canonicalBlockKind),
                immutableDigest = publishedScheduleBlockImmutableDigest(block),
            )
    }
}

@Serializable
private data class PublishedScheduleBlockDigestSnapshot(
    val id: String,
    val isSensitive: Boolean,
    val title: String,
    val itemKind: String,
    val startMinute: Int,
    val durationMinutes: Int,
    val project: String?,
    val energy: String,
    val isFlexible: Boolean,
    val isHardConstraint: Boolean,
    val isSplittable: Boolean,
    val note: String,
    val canonicalItemId: String?,
    val occurrenceId: String?,
    val canonicalRevision: Long?,
    val sessionIndex: Int?,
    val absoluteStartAt: String?,
    val absoluteEndAt: String?,
    val planningZoneId: String?,
    val canonicalBlockKind: String?,
)

private fun publishedScheduleBlockImmutableDigest(block: ScheduleItem): String {
    // Status and actual minutes are server execution projections applied after publication.
    val immutable = PublishedScheduleBlockDigestSnapshot(
        id = block.id,
        isSensitive = block.isSensitive,
        title = block.title,
        itemKind = block.kind.name,
        startMinute = block.startMinute,
        durationMinutes = block.durationMinutes,
        project = block.project,
        energy = block.energy.name,
        isFlexible = block.isFlexible,
        isHardConstraint = block.isHardConstraint,
        isSplittable = block.isSplittable,
        note = block.note,
        canonicalItemId = block.canonicalItemId,
        occurrenceId = block.occurrenceId,
        canonicalRevision = block.canonicalRevision,
        sessionIndex = block.sessionIndex,
        absoluteStartAt = block.absoluteStartAt,
        absoluteEndAt = block.absoluteEndAt,
        planningZoneId = block.planningZoneId,
        canonicalBlockKind = block.canonicalBlockKind,
    )
    val bytes = PUBLISHED_SCHEDULE_BLOCK_DIGEST_JSON.encodeToString(immutable)
        .toByteArray(Charsets.UTF_8)
    return "sha256:" + MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") {
        byte -> "%02x".format(byte.toInt() and 0xff)
    }
}

private fun requireScheduleDigest(value: String) {
    require(
        value.length == 71 && value.startsWith("sha256:") &&
            value.drop(7).all { it in '0'..'9' || it in 'a'..'f' },
    )
}

private val PUBLISHED_SCHEDULE_BLOCK_DIGEST_JSON = Json {
    encodeDefaults = true
    explicitNulls = true
}

private val SERVER_NAMED_TIMEZONE_IDS = ZoneId.getAvailableZoneIds()

/**
 * Durable positive authority for the exact candidate installed after a validated, non-replayed
 * schedule publication. It is encrypted as part of the same planner snapshot generation.
 */
@Serializable
data class PublishedScheduleProofSnapshot(
    val schemaVersion: Int,
    val syncOrigin: String,
    val configurationId: String,
    val revision: PublishedScheduleRevisionSnapshot,
    val asOf: String,
    val blocks: List<PublishedScheduleBlockProofSnapshot>,
) {
    fun hasValidShape(): Boolean = runCatching {
        require(schemaVersion in LEGACY_SCHEMA_VERSION..CURRENT_SCHEMA_VERSION)
        require(
            syncOrigin.isNotBlank() && syncOrigin.length <= 4_096 &&
                syncOrigin.none(Char::isISOControl),
        )
        require(
            configurationId.isNotBlank() && configurationId.length <= 4_096 &&
                configurationId.none(Char::isISOControl),
        )
        val revisionId = UUID.fromString(revision.id)
        require(revisionId != NIL_PUBLICATION_UUID && revisionId.toString() == revision.id)
        require(revision.revisionNumber > 0uL)
        require(revision.revision == "${revision.revisionNumber}:${revision.id}")
        require(
            revision.inputDigest.length == 71 &&
                revision.inputDigest.startsWith("sha256:") &&
                revision.inputDigest.drop(7).all { it in '0'..'9' || it in 'a'..'f' },
        )
        val exactAsOf = Instant.parse(asOf)
        val horizonStart = Instant.parse(revision.horizonStart)
        val horizonEnd = Instant.parse(revision.horizonEnd)
        require(horizonStart <= exactAsOf && exactAsOf < horizonEnd)
        require(
            revision.timezoneName.isNotBlank() && revision.timezoneName.length <= 255 &&
                revision.timezoneName.none(Char::isISOControl),
        )
        require(revision.timezoneName in SERVER_NAMED_TIMEZONE_IDS)
        requireNotNull(runCatching { ZoneId.of(revision.timezoneName) }.getOrNull())
        requireNotNull(runCatching { Instant.parse(revision.publishedAt) }.getOrNull())
        require(blocks.size <= MAX_PUBLISHED_BLOCKS)
        require(blocks.map { it.id }.distinct().size == blocks.size)
        require(blocks.all {
            it.hasValidShape(
                horizonStart = horizonStart,
                horizonEnd = horizonEnd,
                requireFullSeal = schemaVersion >= FULL_PLAN_SCHEMA_VERSION,
            )
        })
    }.isSuccess

    /**
     * Only the current full-plan proof seals immutable context rows as well as executable items.
     * Legacy proofs remain decodable for migration, but cannot make a plan current or actionable.
     */
    fun hasCurrentImmutablePlanSeal(): Boolean =
        schemaVersion == CURRENT_SCHEMA_VERSION && hasValidShape()

    fun matchesStateBinding(state: DayWeaveUiState): Boolean =
        hasValidShape() &&
            state.canonicalSyncOrigin == syncOrigin &&
            state.canonicalConfigurationId == configurationId &&
            state.publishedScheduleRevision == revision &&
            state.scheduleInputDigest == revision.inputDigest &&
            sameInstant(state.scheduleGeneratedAt, asOf) &&
            state.schedulePlanningZoneId == revision.timezoneName

    fun matches(block: ScheduleItem): Boolean =
        hasCurrentImmutablePlanSeal() &&
            blocks.singleOrNull { it.id == block.id }?.matches(block) == true

    /**
     * Verifies the complete canonical plan accepted by publication, rather than granting
     * authority block-by-block while an unproved canonical placement is also present. Execution
     * lease projections arrive after publication and local rows have no canonical item identity,
     * so neither belongs to the publication-backed set.
     */
    fun matchesPublishedPlan(schedule: List<ScheduleItem>): Boolean {
        if (!hasValidShape()) return false
        val publicationBacked = schedule.filter { block ->
            block.canonicalBlockKind != null &&
                block.canonicalBlockKind != REMOTE_EXECUTION_LEASE_KIND &&
                (schemaVersion >= FULL_PLAN_SCHEMA_VERSION || block.canonicalItemId != null)
        }
        if (publicationBacked.size != blocks.size) return false
        val scheduleById = publicationBacked.associateBy(ScheduleItem::id)
        if (scheduleById.size != publicationBacked.size) return false
        return blocks.all { proof -> scheduleById[proof.id]?.let(proof::matches) == true }
    }

    private fun sameInstant(left: String?, right: String): Boolean =
        left?.let { raw -> runCatching { Instant.parse(raw) }.getOrNull() } ==
            runCatching { Instant.parse(right) }.getOrNull()

    companion object {
        const val CURRENT_SCHEMA_VERSION = 2
        private const val LEGACY_SCHEMA_VERSION = 1
        private const val FULL_PLAN_SCHEMA_VERSION = 2
        private const val MAX_PUBLISHED_BLOCKS = 10_000
        private const val REMOTE_EXECUTION_LEASE_KIND = "remote_execution_lease"
        private val NIL_PUBLICATION_UUID = UUID(0L, 0L)
    }
}

/**
 * Crash-replay journal written before a schedule publication can leave the device.
 *
 * [candidate] is intentionally not installed into the active canonical cache until the exact
 * request has a strictly validated server receipt.
 */
@Serializable
data class PendingSchedulePublication(
    val schemaVersion: Int,
    val idempotencyKey: String,
    val syncOrigin: String,
    val configurationId: String? = null,
    val preparedAt: String,
    val request: SchedulePublishHttpRequest,
    val candidate: CanonicalPlanUpdate,
)

@Serializable
enum class ProposalApplicationMutationKind {
    APPLY,
    UNDO,
}

/**
 * Exact, binding-scoped proposal application request persisted before network I/O.
 *
 * Review diffs deliberately remain ephemeral because they can contain sensitive item content.
 * This journal retains only the content-bound review hash or application revision plus the exact
 * request bytes needed to resolve a lost response without minting a second operation.
 */
@Serializable
data class PendingProposalApplicationMutation(
    val schemaVersion: Int,
    val kind: ProposalApplicationMutationKind,
    val idempotencyKey: String,
    val syncOrigin: String,
    val configurationId: String? = null,
    val proposalId: String,
    val expectedProposalRevision: Long,
    /** Ordered command identity from the reviewed preview/retained receipt. */
    val expectedCommandIds: List<String>,
    val previewId: String? = null,
    val expectedReviewHash: String? = null,
    val applicationId: String? = null,
    val expectedApplicationRevision: Long? = null,
    val preparedAt: String,
    val request: ProposalApplicationHttpRequest,
)

@Serializable
enum class ProposalApplicationStatusSnapshot {
    APPLIED,
    UNDONE,
}

/** Content-free durable receipt used for recovery, status display, and the bounded undo fence. */
@Serializable
data class ProposalApplicationReceiptSnapshot(
    val schemaVersion: Int,
    val syncOrigin: String,
    val configurationId: String? = null,
    val applicationId: String,
    val proposalId: String,
    val appliedProposalRevision: Long,
    val applicationRevision: Long,
    val status: ProposalApplicationStatusSnapshot,
    val commandIds: List<String>,
    val affectedItemIds: List<String>,
    val appliedAt: String,
    val undoExpiresAt: String,
    val undoneAt: String? = null,
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
    TRANSACTIONALLY_APPLIED,
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
    /** Exact external conversation/reference label retained for review provenance. */
    val remoteSourceReference: String? = null,
    val remoteCreatedAt: String? = null,
    val remoteExpiresAt: String? = null,
    /** String-valued payload schema. Reserved unknown versions remain visible but non-actionable. */
    val remotePayloadSchema: String? = null,
)

const val DAYWEAVE_PROPOSAL_CHANGE_SET_SCHEMA_V1 = "dayweave.proposal-change-set/1"

val PlanningSuggestion.usesReservedChangeSetNamespace: Boolean
    get() = remotePayloadSchema?.startsWith("dayweave.proposal-change-set/") == true

val PlanningSuggestion.isApplicationReady: Boolean
    get() = remoteRevision != null &&
        remotePayloadSchema == DAYWEAVE_PROPOSAL_CHANGE_SET_SCHEMA_V1

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
    /** Durable work window and scheduler weights; changing any field invalidates local provenance. */
    val scheduleCompositionProfile: ScheduleCompositionProfileSnapshot =
        ScheduleCompositionProfileSnapshot(),
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
    /** Non-null only between durable staging and a strictly validated publish receipt. */
    val pendingSchedulePublication: PendingSchedulePublication? = null,
    /** Exact apply/undo request awaiting an authoritative result. */
    val pendingProposalApplicationMutation: PendingProposalApplicationMutation? = null,
    /** Content-free receipts keyed by their single proposal identifier. */
    val proposalApplications: Map<String, ProposalApplicationReceiptSnapshot> = emptyMap(),
    /** Receipt proving that [scheduleInputDigest] was published under this exact binding. */
    val publishedScheduleRevision: PublishedScheduleRevisionSnapshot? = null,
    /** Exact, encrypted publication authority. Legacy revision receipts are not actionable. */
    val publishedScheduleProof: PublishedScheduleProofSnapshot? = null,
    val scheduleInputDigest: String? = null,
    /** Display-only evidence for a bundled-core composition; never publication authority. */
    val localScheduleCompositionProvenance: LocalScheduleCompositionProvenanceSnapshot? = null,
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
    /** Local create/edit/delete/restore queue; submitted entries are remote uncertainty fences. */
    val pendingCanonicalAuthoringMutations: List<PendingCanonicalAuthoringMutation> = emptyList(),
    /** Bounded full-item/tombstone records supporting reviewable restore after deletion. */
    val canonicalRecentlyDeleted: List<CanonicalRecentlyDeletedRecord> = emptyList(),
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
    /** Lifetime ledger preventing a confirmed closed execution from being recomposed away. */
    val terminalExecutionOutcomes: Map<String, TerminalExecutionOutcomeSnapshot> = emptyMap(),
    val pendingExecutionCommand: PendingExecutionCommand? = null,
    /** User-level move request retained while Pause and Defer reconcile across process death. */
    val pendingExecutionDeferIntent: PendingExecutionDeferIntent? = null,
    /**
     * Opaque at-most-once claim for the most recent timed-break notification delivery.
     *
     * The digest binds only redacted execution identity/revision/deadline fields. Keeping it in
     * the encrypted planner snapshot prevents a restored WorkManager job from repeatedly
     * resurfacing the same break after delivery, an ambiguous platform failure, or process death.
     */
    val lastBreakEndNotificationAttemptDigest: String? = null,
    /** Opaque encrypted consume-once receipt for the last exact notification tap handled. */
    val lastConsumedBreakEndNotificationDigest: String? = null,
    /** Separate opaque receipt for the last stale tap rejected without retargeting execution. */
    val lastRejectedBreakEndNotificationDigest: String? = null,
    /** Exact encrypted acknowledgement that this ended break should remain paused without nagging. */
    val acknowledgedBreakEndDigest: String? = null,
    /** Random device identity generated once and retained only inside encrypted planner state. */
    val executionDeviceId: String? = null,
    val unscheduledWork: List<UnscheduledWorkSnapshot> = emptyList(),
    /** Materialized occurrence id to the recurring root that owns its context/actions. */
    val occurrenceSeriesItemIds: Map<String, String> = emptyMap(),
    /** Exact source envelopes for visible occurrence-scoped actions. */
    val recurrenceOccurrenceSources: Map<String, RecurrenceOccurrenceSourceSnapshot> = emptyMap(),
) {
    /** Non-serialized memo: a copied state starts untrusted unless the store transfers it safely. */
    @Transient
    internal val localScheduleCompositionFingerprintMemo = AtomicReference<String?>(null)
    @Transient
    private val localScheduleCompositionValidationMemo =
        AtomicReference<LocalScheduleCompositionProvenanceSnapshot?>(null)

    internal fun inheritLocalScheduleCompositionMemo(previous: DayWeaveUiState) {
        if (!hasSameLocalScheduleCompositionInputsByReference(previous)) return
        previous.localScheduleCompositionFingerprintMemo.get()?.let {
            localScheduleCompositionFingerprintMemo.compareAndSet(null, it)
        }
        previous.localScheduleCompositionValidationMemo.get()?.let { provenance ->
            if (localScheduleCompositionProvenance === provenance) {
                localScheduleCompositionValidationMemo.compareAndSet(null, provenance)
            }
        }
    }

    private fun hasSameLocalScheduleCompositionInputsByReference(
        previous: DayWeaveUiState,
    ): Boolean =
        scheduleCompositionProfile == previous.scheduleCompositionProfile &&
            canonicalItems === previous.canonicalItems &&
            schedule === previous.schedule &&
            recurrenceOutcomes === previous.recurrenceOutcomes &&
            recurrenceMoves === previous.recurrenceMoves &&
            recurrenceCompletionAnchors === previous.recurrenceCompletionAnchors &&
            occurrenceSeriesItemIds === previous.occurrenceSeriesItemIds &&
            recurrenceOccurrenceSources === previous.recurrenceOccurrenceSources &&
            canonicalExecutionSyncOrigin == previous.canonicalExecutionSyncOrigin &&
            canonicalExecutionConfigurationId == previous.canonicalExecutionConfigurationId &&
            canonicalExecutionRevision == previous.canonicalExecutionRevision &&
            canonicalExecutionSession == previous.canonicalExecutionSession &&
            canonicalExecutionHistoryWindow === previous.canonicalExecutionHistoryWindow &&
            canonicalExecutionHistoryWindowRevision ==
            previous.canonicalExecutionHistoryWindowRevision &&
            canonicalExecutionHistoryContinuityEstablished ==
            previous.canonicalExecutionHistoryContinuityEstablished &&
            terminalExecutionOutcomes === previous.terminalExecutionOutcomes &&
            (pendingExecutionCommand != null) == (previous.pendingExecutionCommand != null) &&
            (pendingExecutionDeferIntent != null) ==
            (previous.pendingExecutionDeferIntent != null) &&
            (activeSession != null) == (previous.activeSession != null) &&
            (pendingCanonicalMutation != null) ==
            (previous.pendingCanonicalMutation != null) &&
            pendingCanonicalAuthoringMutations.isEmpty() ==
            previous.pendingCanonicalAuthoringMutations.isEmpty() &&
            (pendingProposalApplicationMutation != null) ==
            (previous.pendingProposalApplicationMutation != null) &&
            canonicalSyncOrigin == previous.canonicalSyncOrigin &&
            canonicalConfigurationId == previous.canonicalConfigurationId &&
            canonicalDeltaCursor == previous.canonicalDeltaCursor &&
            scheduleGeneratedAt == previous.scheduleGeneratedAt &&
            schedulePlanningZoneId == previous.schedulePlanningZoneId &&
            pendingSchedulePublication === previous.pendingSchedulePublication &&
            publishedScheduleRevision === previous.publishedScheduleRevision &&
            publishedScheduleProof === previous.publishedScheduleProof &&
            scheduleInputDigest == previous.scheduleInputDigest &&
            localScheduleCompositionProvenance ===
            previous.localScheduleCompositionProvenance

    internal fun hasMemoizedLocalScheduleCompositionValidation(
        provenance: LocalScheduleCompositionProvenanceSnapshot,
    ): Boolean =
        localScheduleCompositionProvenance === provenance &&
            localScheduleCompositionValidationMemo.get() === provenance

    internal fun memoizeLocalScheduleCompositionValidation(
        provenance: LocalScheduleCompositionProvenanceSnapshot,
    ) {
        if (localScheduleCompositionProvenance === provenance) {
            localScheduleCompositionValidationMemo.compareAndSet(null, provenance)
        }
    }

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

    /** Today renders only blocks intersecting its local calendar day, even for a long replica. */
    fun visibleScheduleForDay(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): List<ScheduleItem> = visibleScheduleSlicesForDay(reference, currentZone)
        .map(ScheduleItemPresentationSlice::item)

    fun visibleScheduleSlicesForDay(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): List<ScheduleItemPresentationSlice> {
        val date = reference.atZone(currentZone).toLocalDate()
        val dayStart = date.atStartOfDay(currentZone).toInstant()
        val dayEnd = date.plusDays(1).atStartOfDay(currentZone).toInstant()
        return visibleScheduleSlicesIntersecting(
            intervalStart = dayStart,
            intervalEnd = dayEnd,
            displayZone = currentZone,
            isDaySlice = true,
        )
    }

    /** Calendar renders the same Monday-based local week shown by its seven-day strip. */
    fun visibleScheduleForWeek(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
        firstDayOfWeek: DayOfWeek = DayOfWeek.MONDAY,
    ): List<ScheduleItem> = visibleScheduleSlicesForWeek(
        reference,
        currentZone,
        firstDayOfWeek,
    ).map(ScheduleItemPresentationSlice::item)

    fun visibleScheduleSlicesForWeek(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
        firstDayOfWeek: DayOfWeek = DayOfWeek.MONDAY,
    ): List<ScheduleItemPresentationSlice> {
        val date = reference.atZone(currentZone).toLocalDate()
        val daysSinceWeekStart =
            (date.dayOfWeek.value - firstDayOfWeek.value + 7) % 7
        val weekStartDate = date.minusDays(daysSinceWeekStart.toLong())
        val weekStart = weekStartDate.atStartOfDay(currentZone).toInstant()
        val weekEnd = weekStartDate.plusDays(7).atStartOfDay(currentZone).toInstant()
        return visibleScheduleSlicesIntersecting(
            intervalStart = weekStart,
            intervalEnd = weekEnd,
            displayZone = currentZone,
            isDaySlice = false,
        )
    }

    private fun visibleScheduleSlicesIntersecting(
        intervalStart: Instant,
        intervalEnd: Instant,
        displayZone: ZoneId,
        isDaySlice: Boolean,
    ): List<ScheduleItemPresentationSlice> {
        return visibleSchedule.mapNotNull { block ->
            if (block.absoluteStartAt == null && block.absoluteEndAt == null) {
                return@mapNotNull ScheduleItemPresentationSlice(
                    item = block,
                    clippedStart = null,
                    clippedEnd = null,
                    startTimeLabel = block.timeRange().substringBefore('–'),
                    weekStartLabel = block.timeRange().substringBefore('–'),
                    durationMinutes = block.durationMinutes,
                    durationLabel = "${block.durationMinutes}m",
                )
            }
            val start = block.absoluteStartAt?.let { raw ->
                runCatching { Instant.parse(raw) }.getOrNull()
            } ?: return@mapNotNull null
            val end = block.absoluteEndAt?.let { raw ->
                runCatching { Instant.parse(raw) }.getOrNull()
            } ?: return@mapNotNull null
            if (start >= end || end <= intervalStart || start >= intervalEnd) {
                return@mapNotNull null
            }
            val clippedStart = maxOf(start, intervalStart)
            val clippedEnd = minOf(end, intervalEnd)
            val duration = Duration.between(clippedStart, clippedEnd)
            val wholeMinutes = duration.seconds / 60
            val durationMinutes = (
                wholeMinutes + if (duration.seconds % 60 != 0L || duration.nano != 0) 1 else 0
                ).coerceIn(1, Int.MAX_VALUE.toLong()).toInt()
            val continuesBefore = start < intervalStart
            val continuesAfter = end > intervalEnd
            val spansMultipleDisplayDays =
                start.atZone(displayZone).toLocalDate() !=
                end.minusNanos(1).atZone(displayZone).toLocalDate()
            val localStart = clippedStart.atZone(displayZone)
            val continuation = when {
                isDaySlice && continuesBefore && continuesAfter -> "Ongoing all day"
                isDaySlice && continuesBefore -> "Ongoing · ends today"
                isDaySlice && continuesAfter -> "Continues tomorrow"
                continuesBefore && continuesAfter -> "Ongoing"
                continuesBefore -> "Started earlier"
                continuesAfter -> "Continues"
                spansMultipleDisplayDays -> "Multi-day"
                else -> null
            }
            val durationLabel = when {
                isDaySlice && continuesBefore && continuesAfter -> "All day"
                durationMinutes >= PRESENTATION_MINUTES_PER_DAY &&
                    durationMinutes % PRESENTATION_MINUTES_PER_DAY == 0 ->
                    "${durationMinutes / PRESENTATION_MINUTES_PER_DAY}d"
                else -> "${durationMinutes}m"
            }
            ScheduleItemPresentationSlice(
                item = block,
                clippedStart = clippedStart,
                clippedEnd = clippedEnd,
                startTimeLabel = localStart.toLocalTime().format(SLICE_TIME_FORMAT),
                weekStartLabel = localStart.format(SLICE_WEEK_FORMAT),
                durationMinutes = durationMinutes,
                durationLabel = durationLabel,
                continuationLabel = continuation,
            )
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
        return visibleScheduleForDay(reference, currentZone).firstOrNull { item ->
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
        if (pendingSchedulePublication != null) return false
        if (canonicalSyncOrigin == null) return true
        return isPublishedScheduleDisplayCurrent(reference, currentZone, requireSameZone = true)
    }

    /**
     * A proof-bound server replica remains useful for display throughout its published horizon.
     * A replica composed in another IANA zone is deliberately read-only, but must not disappear.
     */
    fun isPublishedScheduleDisplayCurrent(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): Boolean = isPublishedScheduleDisplayCurrent(reference, currentZone, requireSameZone = false)

    private fun isPublishedScheduleDisplayCurrent(
        reference: Instant,
        currentZone: ZoneId,
        requireSameZone: Boolean,
    ): Boolean {
        if (pendingSchedulePublication != null || canonicalSyncOrigin == null) return false
        val proof = publishedScheduleProof ?: return false
        if (
            !proof.hasCurrentImmutablePlanSeal() || !proof.matchesStateBinding(this) ||
            !proof.matchesPublishedPlan(schedule)
        ) {
            return false
        }
        val zone = schedulePlanningZoneId?.let { raw ->
            runCatching { ZoneId.of(raw) }.getOrNull()
        } ?: return false
        if (proof.revision.timezoneName != zone.id || requireSameZone && zone != currentZone) {
            return false
        }
        val horizonStart = runCatching {
            Instant.parse(proof.revision.horizonStart)
        }.getOrNull() ?: return false
        val horizonEnd = runCatching {
            Instant.parse(proof.revision.horizonEnd)
        }.getOrNull() ?: return false
        if (horizonStart >= horizonEnd) return false
        val date = reference.atZone(currentZone).toLocalDate()
        val dayStart = date.atStartOfDay(currentZone).toInstant()
        val dayEnd = date.plusDays(1).atStartOfDay(currentZone).toInstant()
        return horizonStart < dayEnd && dayStart < horizonEnd
    }

    /** Allows a current bundled-core plan to remain visible while all server actions stay locked. */
    fun isScheduleDisplayCurrent(
        reference: Instant = Instant.now(),
        currentZone: ZoneId = ZoneId.systemDefault(),
    ): Boolean {
        if (isCanonicalPlanCurrent(reference, currentZone)) return true
        if (isPublishedScheduleDisplayCurrent(reference, currentZone)) return true
        val provenance = localScheduleCompositionProvenance ?: return false
        if (!provenance.matchesState(this)) return false
        val zone = runCatching { ZoneId.of(provenance.timezoneName) }.getOrNull() ?: return false
        val horizonDate = runCatching {
            Instant.parse(provenance.horizonStart).atZone(zone).toLocalDate()
        }.getOrNull() ?: return false
        return zone == currentZone && horizonDate == reference.atZone(currentZone).toLocalDate()
    }

    /** Exact publication authority for one unchanged canonical server block. */
    fun hasPublishedExecutionAuthority(block: ScheduleItem): Boolean {
        if (pendingSchedulePublication != null || block.sessionIndex == null) return false
        val proof = publishedScheduleProof ?: return false
        if (
            !proof.hasCurrentImmutablePlanSeal() || !proof.matchesStateBinding(this) ||
            !proof.matchesPublishedPlan(schedule) || !proof.matches(block)
        ) {
            return false
        }
        val itemId = block.canonicalItemId ?: return false
        val itemRevision = block.canonicalRevision ?: return false
        return canonicalItems.singleOrNull { it.id == itemId }?.let { item ->
            item.revision == itemRevision && item.isExecutable && item.deletedAt == null
        } == true
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

/** Fail-closed newest-session test shared by projection and conflict entry points. */
internal fun DayWeaveUiState.isNewestExecutionForProjection(
    session: CanonicalExecutionSessionSnapshot,
): Boolean = runCatching {
    val active = canonicalExecutionSession?.takeIf { candidate ->
        candidate.hasSameExecutionProjectionKey(session)
    }
    if (active != null) return@runCatching active.id == session.id
    val newest = terminalExecutionOutcomes.values.asSequence()
        .map(TerminalExecutionOutcomeSnapshot::session)
        .filter { candidate -> candidate.hasSameExecutionProjectionKey(session) }
        .maxWithOrNull(
            compareBy<CanonicalExecutionSessionSnapshot> {
                Instant.parse(it.updatedAt)
            }.thenBy { it.id },
        )
    newest?.id == session.id
}.getOrDefault(false)

/**
 * Returns true while any server-owned execution transition can still mutate this occurrence.
 *
 * A split occurrence may contain scheduled siblings beside its active block. Moving the whole
 * occurrence from one of those siblings would otherwise detach the authoritative lease from its
 * published source. Pending commands/intents are included because response loss can temporarily
 * hide the lease transition from the visible timeline.
 */
internal fun DayWeaveUiState.hasOpenOrPendingExecutionForOccurrence(
    occurrenceId: String,
): Boolean = canonicalExecutionSession?.let { session ->
    session.occurrenceId == occurrenceId && session.status in setOf("active", "paused")
} == true || pendingExecutionCommand?.occurrenceId == occurrenceId ||
    pendingExecutionDeferIntent?.occurrenceId == occurrenceId || schedule.any { block ->
        block.occurrenceId == occurrenceId &&
            block.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
    }

private fun CanonicalExecutionSessionSnapshot.hasSameExecutionProjectionKey(
    other: CanonicalExecutionSessionSnapshot,
): Boolean =
    itemId == other.itemId &&
        itemRevision == other.itemRevision &&
        occurrenceId == other.occurrenceId &&
        sessionIndex == other.sessionIndex

/**
 * A durable pending promotion may already have committed remotely. Its target and descendants
 * therefore become locally sensitive as soon as the fence exists and remain so after restart.
 * Pending declassification never lowers the confirmed classification.
 */
fun DayWeaveUiState.withPendingSensitivityHardened(): DayWeaveUiState {
    var changed = false
    val hardenedSchedule = schedule.map { block ->
        val canonicalId = block.canonicalItemId ?: return@map block
        val mustProtect = effectiveCanonicalSensitivity(
            canonicalItems,
            canonicalId,
            pendingCanonicalMutation,
            pendingCanonicalAuthoringMutations,
        )
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
