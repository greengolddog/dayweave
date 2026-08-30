package com.greengolddog.dayweave.model

import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.Serializable

/** Target window and the user-visible risks that must be approved before a move. */
data class MoveLaterAssessment(
    val targetStart: Instant,
    val targetEnd: Instant,
    /** Every exact per-item deadline crossed by that item's shifted final block. */
    val crossedDeadlines: List<MoveLaterDeadlineRisk>,
    val overlappingHardBlocks: List<ScheduleItem>,
    val fitsSinglePlanningDay: Boolean,
    val placementMode: MoveLaterPlacementMode,
    /** The source itself is publication-pinned and needs an explicit user override. */
    val sourceRequiresOverride: Boolean,
    /** Canonical generations whose exact work/deadline semantics were assessed. */
    val sourceItemRevisions: Map<String, Long>,
    /** A one-shot scheduled move can encode approval by extending the canonical deadline. */
    val canonicalDeadlineRelaxation: Instant?,
) {
    val placementIsExact: Boolean get() = placementMode == MoveLaterPlacementMode.EXACT
    /** Compatibility summary for compact UI/tests; authorization uses [crossedDeadlines]. */
    val deadline: Instant?
        get() = crossedDeadlines.minOfOrNull { requireNotNull(parseMoveInstant(it.deadline)) }
    val deadlineIsHard: Boolean get() = crossedDeadlines.any(MoveLaterDeadlineRisk::isHard)
    val crossesDeadline: Boolean get() = crossedDeadlines.isNotEmpty()
    val crossesUnrelaxableHardDeadline: Boolean
        get() = crossedDeadlines.any(MoveLaterDeadlineRisk::isHard) &&
            canonicalDeadlineRelaxation == null
    val requiresConfirmation: Boolean
        get() = sourceRequiresOverride || crossesDeadline || overlappingHardBlocks.isNotEmpty()
}

enum class MoveLaterPlacementMode {
    /** An execution Defer publishes the exact remaining-work interval. */
    EXACT,

    /** A recurrence Move fixes only the outer occurrence window; the server recomposes leaves. */
    RECOMPOSED_WINDOW,

    /** A one-shot canonical mutation sets only earliest_start_at. */
    EARLIEST_START,
}

/** Exact risk envelope produced by the chooser and rechecked immediately before mutation. */
data class MoveLaterApprovalEnvelope(
    val targetStart: Instant,
    val maximumTargetEnd: Instant,
    val sourceOverrideApproved: Boolean,
    val sourceItemRevisions: Map<String, Long>,
    val deadlineRisks: Set<MoveLaterDeadlineRisk>,
    val hardConflicts: Set<MoveLaterConflictIdentity>,
)

/** Content-free exact identity for one per-item deadline crossing the user reviewed. */
@Serializable
data class MoveLaterDeadlineRisk(
    val itemId: String,
    val itemRevision: Long,
    val deadline: String,
    /** Latest reviewed bound relevant to this item under the placement mode. */
    val targetEnd: String,
    val isHard: Boolean,
    val isCanonicalField: Boolean,
)

/** Content-free exact identity for one fixed overlap that the user reviewed. */
@Serializable
data class MoveLaterConflictIdentity(
    val id: String,
    val canonicalItemId: String?,
    val canonicalRevision: Long?,
    val startAt: String,
    val endAt: String,
    val itemKind: ItemKind,
    val canonicalBlockKind: String?,
    val isFlexible: Boolean,
    val isHardConstraint: Boolean,
    val isSensitive: Boolean,
)

internal fun MoveLaterAssessment.toApprovalEnvelope(): MoveLaterApprovalEnvelope {
    return MoveLaterApprovalEnvelope(
        targetStart = targetStart,
        maximumTargetEnd = targetEnd,
        sourceOverrideApproved = sourceRequiresOverride,
        sourceItemRevisions = sourceItemRevisions,
        deadlineRisks = crossedDeadlines.toSet(),
        hardConflicts = overlappingHardBlocks.mapTo(hashSetOf()) { block ->
            block.moveLaterConflictIdentity()
        },
    )
}

/** New risks fail closed; risks that disappeared or an active lease that became shorter are safe. */
internal fun MoveLaterAssessment.isCoveredBy(
    approval: MoveLaterApprovalEnvelope?,
): Boolean {
    if (!requiresConfirmation && approval == null) return true
    approval ?: return false
    if (
        approval.targetStart != targetStart || targetEnd > approval.maximumTargetEnd ||
        sourceItemRevisions != approval.sourceItemRevisions ||
        sourceRequiresOverride && !approval.sourceOverrideApproved
    ) {
        return false
    }
    if (!crossedDeadlines.all { current ->
            approval.deadlineRisks.any { approved -> current.isCoveredBy(approved) }
        }
    ) {
        return false
    }
    return overlappingHardBlocks.all { block ->
        block.moveLaterConflictIdentity() in approval.hardConflicts
    }
}

private fun MoveLaterDeadlineRisk.isCoveredBy(approved: MoveLaterDeadlineRisk): Boolean =
    itemId == approved.itemId && itemRevision == approved.itemRevision &&
        deadline == approved.deadline && isHard == approved.isHard &&
        isCanonicalField == approved.isCanonicalField &&
        requireNotNull(parseMoveInstant(targetEnd)) <=
        requireNotNull(parseMoveInstant(approved.targetEnd))

private fun ScheduleItem.moveLaterConflictIdentity() = MoveLaterConflictIdentity(
    id = id,
    canonicalItemId = canonicalItemId,
    canonicalRevision = canonicalRevision,
    startAt = requireNotNull(absoluteStartAt),
    endAt = requireNotNull(absoluteEndAt),
    itemKind = kind,
    canonicalBlockKind = canonicalBlockKind,
    isFlexible = isFlexible,
    isHardConstraint = isHardConstraint,
    isSensitive = isSensitive,
)

internal fun PendingExecutionDeferIntent.savedApprovalEnvelope(): MoveLaterApprovalEnvelope? {
    val approvedEnd = approvedConflictTargetEnd?.let { raw ->
        runCatching { Instant.parse(raw) }.getOrNull()
    } ?: return null
    return MoveLaterApprovalEnvelope(
        targetStart = runCatching { Instant.parse(moveStart) }.getOrNull() ?: return null,
        maximumTargetEnd = approvedEnd,
        sourceOverrideApproved = approvedSourceOverride,
        sourceItemRevisions = approvedItemRevisions,
        deadlineRisks = approvedDeadlineRisks.toSet(),
        hardConflicts = approvedHardConflicts.toSet(),
    )
}

/** True only when Android can encode the source move without silently weakening its constraints. */
internal fun ScheduleItem.isRepresentableMoveLaterSource(): Boolean =
    canonicalItemId != null &&
        kind in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE, ItemKind.GOAL) &&
        canonicalBlockKind !in setOf("external_fixed", "calendar_event", "remote_execution_lease") &&
        (
            isFlexible && !isHardConstraint ||
                status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) &&
                canonicalBlockKind == "pinned"
            )

/**
 * Computes the exact move span used by the warning boundary.
 *
 * Active/paused work uses the authoritative lease's remaining seconds. A recurring scheduled
 * occurrence moves as one group, preserving the tapped block's relative offset. Returning null is
 * fail-closed: the caller cannot prove the exact source span or planning timezone.
 */
fun DayWeaveUiState.assessMoveLater(
    blockId: String,
    moveStart: Instant,
    referenceNow: Instant = Instant.now(),
): MoveLaterAssessment? {
    if (moveStart.nano != 0 || moveStart <= referenceNow) return null
    val focused = schedule.firstOrNull { it.id == blockId } ?: return null
    if (!focused.isRepresentableMoveLaterSource()) return null
    val itemId = focused.canonicalItemId ?: return null
    val sourceStart = focused.absoluteStartAt?.let(::parseMoveInstant) ?: return null
    val sourceEnd = focused.absoluteEndAt?.let(::parseMoveInstant) ?: return null
    if (sourceEnd <= sourceStart || moveStart <= sourceStart) return null
    val lease = canonicalExecutionSession?.takeIf { session ->
        session.status in OPEN_EXECUTION_STATUSES &&
            session.itemId == itemId && session.itemRevision == focused.canonicalRevision &&
            session.occurrenceId == focused.occurrenceId &&
            session.sessionIndex == focused.sessionIndex &&
            session.plannedBlockId == focused.id
    }
    val isExecutionMove = focused.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
    if (isExecutionMove && lease == null) return null

    val movedIds: Set<String>
    val movedCanonicalItemIds: Set<String>
    val targetEndsByCanonicalItemId: Map<String, Instant>
    val targetWindowStart: Instant
    val targetWindowEnd: Instant
    if (!isExecutionMove && focused.occurrenceId != null) {
        val occurrenceBlocks = schedule.filter { block ->
            block.occurrenceId == focused.occurrenceId
        }
        if (
            occurrenceBlocks.isEmpty() || occurrenceBlocks.any { block ->
                block.status != ItemStatus.SCHEDULED ||
                    !block.isRepresentableMoveLaterSource()
            } || unscheduledWork.any { work ->
                work.occurrenceId == focused.occurrenceId && work.remainingMinutes > 0
            }
        ) {
            return null
        }
        val shift = Duration.between(sourceStart, moveStart)
        if (shift.isNegative || shift.isZero || shift.nano != 0) return null
        val shifted = occurrenceBlocks.map { block ->
            val start = block.absoluteStartAt?.let(::parseMoveInstant) ?: return null
            val end = block.absoluteEndAt?.let(::parseMoveInstant) ?: return null
            if (end <= start) return null
            Triple(block, start.plus(shift), end.plus(shift))
        }
        movedIds = occurrenceBlocks.mapTo(hashSetOf(), ScheduleItem::id)
        movedCanonicalItemIds = occurrenceBlocks.mapNotNullTo(hashSetOf()) {
            it.canonicalItemId
        }
        if (movedCanonicalItemIds.isEmpty()) return null
        targetWindowStart = shifted.minOf { it.second }
        targetWindowEnd = shifted.maxOf { it.third }
        // The recurrence command carries only the outer window. Core may place any leaf as late
        // as its end, so every leaf deadline is conservatively checked against that same bound.
        targetEndsByCanonicalItemId = movedCanonicalItemIds.associateWith { targetWindowEnd }
    } else {
        val sourceSeconds = runCatching { Duration.between(sourceStart, sourceEnd).seconds }
            .getOrNull()
            ?.takeIf { it > 0 } ?: return null
        val remainingSeconds = lease?.let { session ->
            val runningSeconds = if (session.status == "active") {
                val runningSince = session.runningSince?.let(::parseMoveInstant) ?: return null
                Duration.between(runningSince, referenceNow).seconds.coerceAtLeast(0)
            } else {
                0L
            }
            runCatching {
                Math.subtractExact(
                    sourceSeconds,
                    Math.addExact(session.accumulatedSeconds, runningSeconds),
                )
            }.getOrNull()?.takeIf { it > 0 } ?: return null
        } ?: sourceSeconds
        movedIds = setOf(focused.id)
        movedCanonicalItemIds = setOf(itemId)
        targetWindowStart = moveStart
        targetWindowEnd = runCatching { moveStart.plusSeconds(remainingSeconds) }.getOrNull()
            ?: return null
        targetEndsByCanonicalItemId = mapOf(itemId to targetWindowEnd)
    }

    val zone = listOfNotNull(schedulePlanningZoneId, focused.planningZoneId)
        .firstNotNullOfOrNull { raw -> runCatching { ZoneId.of(raw) }.getOrNull() }
        ?: return null
    val targetDate = targetWindowStart.atZone(zone).toLocalDate()
    val loadedPlanningDate = canonicalPlanningDate() ?: return null
    val horizonStart = targetDate.atStartOfDay(zone).toInstant()
    val horizonEnd = targetDate.plusDays(1).atStartOfDay(zone).toInstant()
    val fitsSinglePlanningDay = targetDate == loadedPlanningDate &&
        targetWindowStart >= horizonStart && targetWindowEnd <= horizonEnd

    val itemsById = canonicalItems.associateBy(CanonicalItemSnapshot::id)
    val crossedDeadlines = movedCanonicalItemIds.mapNotNull { movedItemId ->
        val canonicalItem = itemsById[movedItemId] ?: return null
        val boundaryResult = canonicalItem.moveLaterDeadlineBoundary() ?: return null
        val boundary = boundaryResult.getOrNull() ?: return@mapNotNull null
        val itemTargetEnd = targetEndsByCanonicalItemId[movedItemId] ?: return null
        if (itemTargetEnd <= boundary.instant) return@mapNotNull null
        MoveLaterDeadlineRisk(
            itemId = movedItemId,
            itemRevision = canonicalItem.revision,
            deadline = boundary.instant.toString(),
            targetEnd = itemTargetEnd.toString(),
            isHard = boundary.isHard,
            isCanonicalField = boundary.isCanonicalField,
        )
    }.sortedWith(compareBy(MoveLaterDeadlineRisk::deadline, MoveLaterDeadlineRisk::itemId))
    val placementMode = when {
        isExecutionMove -> MoveLaterPlacementMode.EXACT
        focused.occurrenceId != null -> MoveLaterPlacementMode.RECOMPOSED_WINDOW
        else -> MoveLaterPlacementMode.EARLIEST_START
    }
    val hardOverlaps = if (placementMode == MoveLaterPlacementMode.EXACT) {
        schedule.filter { other ->
            other.id !in movedIds && other.isImmutableMoveObstacle() &&
                other.absoluteStartAt?.let(::parseMoveInstant)?.let { otherStart ->
                    other.absoluteEndAt?.let(::parseMoveInstant)?.let { otherEnd ->
                        otherStart < targetWindowEnd && otherEnd > targetWindowStart
                    }
                } == true
        }
    } else {
        emptyList()
    }
    val relaxDeadline = crossedDeadlines.singleOrNull()?.takeIf { risk ->
        risk.isCanonicalField && !isExecutionMove && focused.occurrenceId == null
    }?.targetEnd?.let(::parseMoveInstant)
    return MoveLaterAssessment(
        targetStart = targetWindowStart,
        targetEnd = targetWindowEnd,
        crossedDeadlines = crossedDeadlines,
        overlappingHardBlocks = hardOverlaps,
        fitsSinglePlanningDay = fitsSinglePlanningDay,
        placementMode = placementMode,
        sourceRequiresOverride = !focused.isFlexible || focused.isHardConstraint ||
            focused.canonicalBlockKind == "pinned",
        sourceItemRevisions = movedCanonicalItemIds.associateWith { itemIdForRevision ->
            requireNotNull(itemsById[itemIdForRevision]).revision
        },
        canonicalDeadlineRelaxation = relaxDeadline,
    )
}

private fun ScheduleItem.isImmutableMoveObstacle(): Boolean =
    isHardConstraint || !isFlexible || kind == ItemKind.EVENT ||
        canonicalBlockKind in setOf("pinned", "calendar_event", "external_fixed")

private fun parseMoveInstant(raw: String): Instant? = runCatching { Instant.parse(raw) }
    .getOrNull()
    ?.takeIf { it.toString() == raw }

private data class MoveLaterDeadlineBoundary(
    val instant: Instant,
    val isHard: Boolean,
    val isCanonicalField: Boolean,
)

/**
 * Returns a successful nullable boundary, or null when deadline metadata cannot be proven safe.
 * Canonical `deadline_at` is hard. Rich scheduler metadata may instead carry a hard or soft
 * `constraints.latest_finish`; the compose contract rejects defining both.
 */
private fun CanonicalItemSnapshot.moveLaterDeadlineBoundary(): Result<MoveLaterDeadlineBoundary?>? {
    val canonical = deadlineAt?.let { raw ->
        val instant = parseMoveInstant(raw) ?: return null
        MoveLaterDeadlineBoundary(instant, isHard = true, isCanonicalField = true)
    }
    val root = runCatching {
        MOVE_LATER_JSON.parseToJsonElement(flexibleConstraintsJson) as? JsonObject
            ?: error("Flexible constraints must be an object")
    }.getOrNull() ?: return null
    val constraintsElement = root["constraints"]
    if (constraintsElement == null || constraintsElement is JsonNull) {
        return Result.success(canonical)
    }
    val constraints = constraintsElement as? JsonObject ?: return null
    val deadlineElement = constraints["latest_finish"]
    if (deadlineElement == null || deadlineElement is JsonNull) {
        return Result.success(canonical)
    }
    if (canonical != null) return null
    val qualified = deadlineElement as? JsonObject ?: return null
    if (qualified.keys != setOf("value", "strength")) return null
    val rawValue = (qualified["value"] as? JsonPrimitive)
        ?.takeIf { it.isString }
        ?.contentOrNull ?: return null
    val instant = runCatching { Instant.parse(rawValue) }.getOrNull() ?: return null
    val strength = qualified["strength"] as? JsonObject ?: return null
    val level = (strength["level"] as? JsonPrimitive)
        ?.takeIf { it.isString }
        ?.contentOrNull ?: return null
    val isHard = when (level) {
        "hard" -> {
            if (strength.keys != setOf("level")) return null
            true
        }
        "soft" -> {
            if (strength.keys != setOf("level", "weight")) return null
            val weight = (strength["weight"] as? JsonPrimitive)
                ?.takeUnless { it.isString }
                ?.intOrNull ?: return null
            if (weight !in 0..1_000_000) return null
            false
        }
        else -> return null
    }
    return Result.success(
        MoveLaterDeadlineBoundary(
            instant = instant,
            isHard = isHard,
            isCanonicalField = false,
        ),
    )
}

private val OPEN_EXECUTION_STATUSES = setOf("active", "paused")
private val MOVE_LATER_JSON = Json { ignoreUnknownKeys = false }
