package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.requireLegacyStructuralRepresentability
import com.greengolddog.dayweave.network.CanonicalItemReplacement
import com.greengolddog.dayweave.network.CreateCanonicalItemRequest

internal fun CanonicalItemDraft.toCanonicalItemReplacement(
    itemId: String,
    durationRequestShapeVersion: Int,
    structuralRequestShapeVersion: Int,
): CanonicalItemReplacement {
    val value = normalized().also { it.requireValid(itemId) }
    require(durationRequestShapeVersion in setOf(
        PendingCanonicalAuthoringMutation.LEGACY_DURATION_REQUEST_SHAPE_VERSION,
        PendingCanonicalAuthoringMutation.CURRENT_DURATION_REQUEST_SHAPE_VERSION,
    ))
    val emitsRichDuration = durationRequestShapeVersion ==
        PendingCanonicalAuthoringMutation.CURRENT_DURATION_REQUEST_SHAPE_VERSION
    require(structuralRequestShapeVersion in setOf(
        PendingCanonicalAuthoringMutation.LEGACY_STRUCTURAL_REQUEST_SHAPE_VERSION,
        PendingCanonicalAuthoringMutation.CURRENT_STRUCTURAL_REQUEST_SHAPE_VERSION,
    ))
    val emitsStructural = structuralRequestShapeVersion ==
        PendingCanonicalAuthoringMutation.CURRENT_STRUCTURAL_REQUEST_SHAPE_VERSION
    if (!emitsStructural) value.requireLegacyStructuralRepresentability()
    return CanonicalItemReplacement(
        isSensitive = value.isSensitive,
        kind = value.kind.name.lowercase(),
        status = value.placement.wireValue,
        title = value.title,
        notes = value.notes,
        timezoneName = value.timezoneName,
        durationSeconds = value.durationSeconds,
        durationKind = value.durationKind.takeIf { emitsRichDuration },
        durationMinSeconds = value.durationMinSeconds.takeIf { emitsRichDuration },
        durationMaxSeconds = value.durationMaxSeconds.takeIf { emitsRichDuration },
        durationSource = value.durationSource.takeIf { emitsRichDuration },
        deadlineAt = value.deadlineAt,
        deadlineKind = value.deadlineKind.takeIf { emitsStructural },
        deadlineDate = com.greengolddog.dayweave.network.CanonicalRequestNullable(value.deadlineDate)
            .takeIf { emitsStructural },
        deadlineStrength = com.greengolddog.dayweave.network.CanonicalRequestNullable(value.deadlineStrength)
            .takeIf { emitsStructural },
        deadlineSoftWeight = com.greengolddog.dayweave.network.CanonicalRequestNullable(value.deadlineSoftWeight)
            .takeIf { emitsStructural },
        hasOwnEffort = value.hasOwnEffort.takeIf { emitsStructural },
        earliestStartAt = value.earliestStartAt,
        recurrence = value.recurrence?.toCanonicalJson(),
        flexibleConstraints = value.constraints.toCanonicalJson(
            value.eventTiming,
            value.durationSeconds,
            value.timezoneName,
        ),
        splitPolicy = value.split.toCanonicalJson(value.durationSeconds),
        importance = value.importance,
        urgency = value.urgency,
        parentId = value.parentId,
        siblingOrder = value.siblingOrder,
    )
}

internal fun CanonicalItemDraft.toCreateCanonicalItemRequest(
    itemId: String,
    durationRequestShapeVersion: Int,
    structuralRequestShapeVersion: Int,
): CreateCanonicalItemRequest {
    val fields = toCanonicalItemReplacement(itemId, durationRequestShapeVersion, structuralRequestShapeVersion)
    return CreateCanonicalItemRequest(
        id = itemId,
        isSensitive = fields.isSensitive,
        kind = fields.kind,
        status = fields.status,
        title = fields.title,
        notes = fields.notes,
        timezoneName = fields.timezoneName,
        durationSeconds = fields.durationSeconds,
        durationKind = fields.durationKind,
        durationMinSeconds = fields.durationMinSeconds,
        durationMaxSeconds = fields.durationMaxSeconds,
        durationSource = fields.durationSource,
        deadlineAt = fields.deadlineAt,
        deadlineKind = fields.deadlineKind,
        deadlineDate = fields.deadlineDate,
        deadlineStrength = fields.deadlineStrength,
        deadlineSoftWeight = fields.deadlineSoftWeight,
        hasOwnEffort = fields.hasOwnEffort,
        earliestStartAt = fields.earliestStartAt,
        recurrence = fields.recurrence,
        flexibleConstraints = fields.flexibleConstraints,
        splitPolicy = fields.splitPolicy,
        importance = fields.importance,
        urgency = fields.urgency,
        parentId = fields.parentId,
        siblingOrder = fields.siblingOrder,
    )
}
