package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.EditNote
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalAbsoluteWindowDraft
import com.greengolddog.dayweave.model.CanonicalBreakCategory
import com.greengolddog.dayweave.model.CanonicalBufferPolicyDraft
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalConstraintLevel
import com.greengolddog.dayweave.model.CanonicalConstraintStrengthDraft
import com.greengolddog.dayweave.model.CanonicalDailyWindowDraft
import com.greengolddog.dayweave.model.CanonicalDependencyDraft
import com.greengolddog.dayweave.model.CanonicalDependencyRelation
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalGoalMeasureDraft
import com.greengolddog.dayweave.model.CanonicalHabitTargetDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalQualifiedInstantDraft
import com.greengolddog.dayweave.model.CanonicalQualifiedMinutesDraft
import com.greengolddog.dayweave.model.CanonicalQualifiedStringDraft
import com.greengolddog.dayweave.model.CanonicalQualifiedWeekdaysDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
import com.greengolddog.dayweave.model.CanonicalRecurrencePeriod
import com.greengolddog.dayweave.model.CanonicalRecurrenceSemantics
import com.greengolddog.dayweave.model.CanonicalSchedulingConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalSplitDraft
import com.greengolddog.dayweave.model.CanonicalSplitKind
import com.greengolddog.dayweave.model.CanonicalWeekday
import com.greengolddog.dayweave.model.CanonicalWeeklyAllocationDraft
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.decodeCanonicalFlexibleConstraints
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.requireCanonicalInstant
import com.greengolddog.dayweave.state.canonicalDeviceTimezoneName
import java.time.Duration
import java.util.UUID
import kotlin.math.roundToInt
import kotlinx.coroutines.launch

internal enum class CanonicalItemEditorMode {
    CREATE,
    REPLACE,
    UPDATE_PENDING,
}

internal data class CanonicalItemEditorRoute(
    val itemId: String,
    val initialDraft: CanonicalItemDraft,
    val mode: CanonicalItemEditorMode,
    val mutationId: String? = null,
    val sourceInboxId: String? = null,
) {
    val routeId: String = mutationId ?: sourceInboxId ?: "$mode:$itemId"

    companion object {
        fun create(
            title: String = "",
            kind: ItemKind = ItemKind.TASK,
            isSensitive: Boolean = false,
        ): CanonicalItemEditorRoute = CanonicalItemEditorRoute(
            itemId = UUID.randomUUID().toString(),
            initialDraft = newCanonicalDetailedDraft(title, kind, isSensitive),
            mode = CanonicalItemEditorMode.CREATE,
        )

        fun fromInbox(item: InboxItem): CanonicalItemEditorRoute {
            val route = create(
                title = item.title,
                kind = ItemKind.TASK,
                isSensitive = item.isSensitive,
            )
            return route.copy(
                initialDraft = route.initialDraft.copy(
                    notes = item.detail.takeIf(String::isNotBlank),
                ),
                sourceInboxId = item.id,
            )
        }
    }
}

internal data class CanonicalParentOption(
    val id: String,
    val title: String,
)

internal data class CanonicalDependencyOption(
    val id: String,
    /** Already redacted when the referenced item is effectively sensitive. */
    val displayTitle: String,
    val status: String,
    val isSensitive: Boolean,
    val hasOpaqueDependencies: Boolean,
)

internal data class CanonicalDependencyGraphNode(
    val id: String,
    val title: String,
    val status: String,
    val kind: ItemKind,
    val hasRecurrence: Boolean,
    val parentId: String?,
    val siblingOrder: Long,
    val constraints: CanonicalFlexibleConstraintsDraft?,
    val isDeleted: Boolean,
    val hasOpaqueDependencies: Boolean,
)

internal data class CanonicalDependencyEditorContext(
    val editedItemId: String,
    val options: List<CanonicalDependencyOption>,
    private val baseNodes: Map<String, CanonicalDependencyGraphNode>,
) {
    val selectableOptions: List<CanonicalDependencyOption>
        get() = options.filterNot(CanonicalDependencyOption::hasOpaqueDependencies)

    private enum class GraphSafety {
        SAFE,
        CYCLE,
        CYCLE_UNPROVEN,
        RECURRING_BOUNDARY,
        RECURRING_BOUNDARY_UNPROVEN,
    }

    private enum class DependencyPath {
        ABSENT,
        PRESENT,
        UNKNOWN,
    }

    private data class DependencyGraphProjection(
        val explicitDependenciesByItem: Map<String, Set<String>>,
        val knownDependenciesByItem: Map<String, Set<String>>,
        val opaqueItemIds: Set<String>,
        val recurringOwnersByItem: Map<String, RecurringOwner>,
    )

    private sealed interface RecurringOwner {
        data object None : RecurringOwner
        data object Unknown : RecurringOwner
        data class Known(val itemId: String) : RecurringOwner
    }

    private enum class RecurringBoundarySafety {
        SAFE,
        CROSS_BOUNDARY,
        UNPROVEN,
    }

    fun option(itemId: String): CanonicalDependencyOption? =
        options.firstOrNull { it.id == itemId }

    fun cycleWarning(draft: CanonicalItemDraft): String? = when (graphSafety(draft)) {
        GraphSafety.SAFE -> null
        GraphSafety.CYCLE ->
            "These dependencies would create a cycle. Change or remove a predecessor before saving."
        GraphSafety.CYCLE_UNPROVEN ->
            "Dependency safety cannot be verified because a related item uses newer metadata."
        GraphSafety.RECURRING_BOUNDARY ->
            "A recurring predecessor can only be linked from within the same recurring subtree."
        GraphSafety.RECURRING_BOUNDARY_UNPROVEN ->
            "Dependency recurrence safety cannot be verified because related hierarchy metadata is unavailable."
    }

    fun candidateIssue(draft: CanonicalItemDraft, candidateItemId: String): String? =
        when (graphSafety(draft.withAdditionalDependency(candidateItemId))) {
            GraphSafety.SAFE -> null
            GraphSafety.CYCLE -> "Would create a cycle"
            GraphSafety.CYCLE_UNPROVEN -> "Cannot verify cycle safety"
            GraphSafety.RECURRING_BOUNDARY -> "Different recurring subtree"
            GraphSafety.RECURRING_BOUNDARY_UNPROVEN ->
                "Cannot verify recurring-subtree ownership"
        }

    fun wouldCreateCycle(
        draft: CanonicalItemDraft,
        candidateItemId: String,
    ): Boolean = candidateIssue(draft, candidateItemId) != null

    private fun CanonicalItemDraft.withAdditionalDependency(
        candidateItemId: String,
    ): CanonicalItemDraft {
        val scheduling = constraints.scheduling ?: CanonicalSchedulingConstraintsDraft()
        return copy(
            constraints = constraints.copy(
                scheduling = scheduling.copy(
                    dependencies = scheduling.dependencies + CanonicalDependencyDraft(
                        itemId = candidateItemId,
                        relation = CanonicalDependencyRelation.FINISH_TO_START,
                        strength = CanonicalConstraintStrengthDraft.hard(),
                    ),
                ),
            ),
        )
    }

    private fun graphSafety(draft: CanonicalItemDraft): GraphSafety {
        val before = projectGraph(baseNodes)
        val after = graphWith(draft)
        when (after.recurringBoundarySafety()) {
            RecurringBoundarySafety.CROSS_BOUNDARY ->
                return GraphSafety.RECURRING_BOUNDARY
            RecurringBoundarySafety.UNPROVEN ->
                return GraphSafety.RECURRING_BOUNDARY_UNPROVEN
            RecurringBoundarySafety.SAFE -> Unit
        }
        if (after.knownDependenciesByItem.hasCycle()) return GraphSafety.CYCLE
        val cannotProveAcyclic = after.addedEdgesComparedTo(before).any { (successor, predecessor) ->
            after.dependencyPath(start = predecessor, target = successor) == DependencyPath.UNKNOWN
        }
        return if (cannotProveAcyclic) GraphSafety.CYCLE_UNPROVEN else GraphSafety.SAFE
    }

    private fun graphWith(draft: CanonicalItemDraft): DependencyGraphProjection {
        val nodes = baseNodes.toMutableMap().also { projected ->
            projected[editedItemId] = CanonicalDependencyGraphNode(
                id = editedItemId,
                title = draft.title,
                status = draft.placement.wireValue,
                kind = draft.kind,
                hasRecurrence = draft.recurrence != null,
                parentId = draft.parentId,
                siblingOrder = draft.siblingOrder,
                constraints = draft.constraints,
                isDeleted = false,
                hasOpaqueDependencies = false,
            )
        }
        return projectGraph(nodes)
    }

    private fun projectGraph(
        nodes: Map<String, CanonicalDependencyGraphNode>,
    ): DependencyGraphProjection {
        val explicitGraph: Map<String, Set<String>> = nodes.values.associate { node ->
            node.id to node.constraints?.scheduling?.dependencies.orEmpty()
                .mapTo(linkedSetOf(), CanonicalDependencyDraft::itemId)
        }
        val graph = explicitGraph.toMutableMap()
        nodes.values
            .filter { node ->
                !node.isDeleted && node.kind == ItemKind.ROUTINE &&
                    node.constraints?.routineOrdered == true
            }
            .forEach { routine ->
                val children = nodes.values
                    .filter { !it.isDeleted && it.parentId == routine.id }
                    .sortedWith(
                        compareBy(
                            CanonicalDependencyGraphNode::siblingOrder,
                            CanonicalDependencyGraphNode::id,
                        ),
                    )
                children.zipWithNext { predecessor, successor ->
                    graph[successor.id] = graph.getValue(successor.id) + predecessor.id
                }
            }
        return DependencyGraphProjection(
            explicitDependenciesByItem = explicitGraph,
            knownDependenciesByItem = graph,
            opaqueItemIds = nodes.values.asSequence()
                .filter(CanonicalDependencyGraphNode::hasOpaqueDependencies)
                .map(CanonicalDependencyGraphNode::id)
                .toSet(),
            recurringOwnersByItem = recurringOwners(nodes),
        )
    }

    private fun recurringOwners(
        nodes: Map<String, CanonicalDependencyGraphNode>,
    ): Map<String, RecurringOwner> {
        val resolved = mutableMapOf<String, RecurringOwner>()
        nodes.keys.forEach { start ->
            if (start in resolved) return@forEach
            val path = mutableListOf<String>()
            val visiting = hashSetOf<String>()
            var current: String? = start
            var owner: RecurringOwner? = null
            while (owner == null) {
                val itemId = current
                if (itemId == null) {
                    owner = RecurringOwner.None
                    break
                }
                val cachedOwner = resolved[itemId]
                if (cachedOwner != null) {
                    owner = cachedOwner
                    break
                }
                if (!visiting.add(itemId)) {
                    owner = RecurringOwner.Unknown
                    break
                }
                val item = nodes[itemId]
                if (item == null) {
                    owner = RecurringOwner.Unknown
                    break
                }
                path += itemId
                current = item.parentId
            }
            var inheritedOwner = requireNotNull(owner)
            path.asReversed().forEach { itemId ->
                if (inheritedOwner == RecurringOwner.None && nodes.getValue(itemId).hasRecurrence) {
                    inheritedOwner = RecurringOwner.Known(itemId)
                }
                resolved[itemId] = inheritedOwner
            }
        }
        return resolved
    }

    private fun DependencyGraphProjection.recurringBoundarySafety(): RecurringBoundarySafety {
        var unproven = false
        explicitDependenciesByItem.forEach { (successorId, predecessorIds) ->
            predecessorIds.forEach { predecessorId ->
                when (val predecessorOwner = recurringOwnersByItem[predecessorId]
                    ?: RecurringOwner.Unknown) {
                    RecurringOwner.None -> Unit
                    RecurringOwner.Unknown -> unproven = true
                    is RecurringOwner.Known -> when (
                        val successorOwner = recurringOwnersByItem[successorId]
                            ?: RecurringOwner.Unknown
                    ) {
                        RecurringOwner.Unknown -> unproven = true
                        RecurringOwner.None -> return RecurringBoundarySafety.CROSS_BOUNDARY
                        is RecurringOwner.Known -> if (
                            successorOwner.itemId != predecessorOwner.itemId
                        ) {
                            return RecurringBoundarySafety.CROSS_BOUNDARY
                        }
                    }
                }
            }
        }
        return if (unproven) {
            RecurringBoundarySafety.UNPROVEN
        } else {
            RecurringBoundarySafety.SAFE
        }
    }

    private fun DependencyGraphProjection.addedEdgesComparedTo(
        previous: DependencyGraphProjection,
    ): Sequence<Pair<String, String>> = knownDependenciesByItem.asSequence().flatMap {
        (successor, predecessors) ->
        val previousPredecessors = previous.knownDependenciesByItem[successor].orEmpty()
        predecessors.asSequence()
            .filter { it !in previousPredecessors }
            .map { predecessor -> successor to predecessor }
    }

    private fun DependencyGraphProjection.dependencyPath(
        start: String,
        target: String,
    ): DependencyPath {
        val pending = ArrayDeque<String>().also { it.add(start) }
        val visited = hashSetOf<String>()
        var reachedOpaqueItem = false
        while (pending.isNotEmpty()) {
            val itemId = pending.removeLast()
            if (itemId == target) return DependencyPath.PRESENT
            if (!visited.add(itemId)) continue
            if (itemId in opaqueItemIds) reachedOpaqueItem = true
            knownDependenciesByItem[itemId].orEmpty().forEach(pending::add)
        }
        return if (reachedOpaqueItem) DependencyPath.UNKNOWN else DependencyPath.ABSENT
    }

    private fun Map<String, Set<String>>.hasCycle(): Boolean {
        val visiting = hashSetOf<String>()
        val visited = hashSetOf<String>()
        fun visit(itemId: String): Boolean {
            if (itemId in visiting) return true
            if (!visited.add(itemId)) return false
            visiting += itemId
            if (get(itemId).orEmpty().any(::visit)) return true
            visiting -= itemId
            return false
        }
        return keys.any(::visit)
    }
}

internal data class CanonicalStrengthForm(
    val level: CanonicalConstraintLevel = CanonicalConstraintLevel.SOFT,
    val weight: String = "100",
) {
    fun draft(label: String): CanonicalConstraintStrengthDraft =
        CanonicalConstraintStrengthDraft(
            level = level,
            weight = if (level == CanonicalConstraintLevel.SOFT) {
                weight.requiredLong("$label weight")
            } else {
                null
            },
        )

    companion object {
        fun from(value: CanonicalConstraintStrengthDraft) = CanonicalStrengthForm(
            level = value.level,
            weight = (value.weight ?: 100).toString(),
        )
    }
}

internal data class CanonicalDependencyForm(
    val itemId: String,
    val relation: CanonicalDependencyRelation = CanonicalDependencyRelation.FINISH_TO_START,
    val minimumLagMinutes: String = "0",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(
        CanonicalConstraintLevel.HARD,
    ),
) {
    fun draft() = CanonicalDependencyDraft(
        itemId = itemId,
        relation = relation,
        minimumLagMinutes = minimumLagMinutes.requiredLong("Dependency minimum lag"),
        strength = strength.draft("Dependency"),
    ).also(CanonicalDependencyDraft::requireValid)

    companion object {
        fun from(value: CanonicalDependencyDraft) = CanonicalDependencyForm(
            itemId = value.itemId,
            relation = value.relation,
            minimumLagMinutes = value.minimumLagMinutes.toString(),
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalInstantConstraintForm(
    val value: String = "",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(),
) {
    fun draft(label: String) = CanonicalQualifiedInstantDraft(
        value = value.required(label).also {
            requireCanonicalInstant(it, label)
        },
        strength = strength.draft(label),
    )

    companion object {
        fun from(value: CanonicalQualifiedInstantDraft) = CanonicalInstantConstraintForm(
            value = value.value,
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalMinutesConstraintForm(
    val value: String = "",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(),
) {
    fun draft(label: String) = CanonicalQualifiedMinutesDraft(
        value = value.requiredLong(label),
        strength = strength.draft(label),
    )

    companion object {
        fun from(value: CanonicalQualifiedMinutesDraft) = CanonicalMinutesConstraintForm(
            value = value.value.toString(),
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalDailyWindowForm(
    val weekdays: Set<CanonicalWeekday> = emptySet(),
    val startMinute: String = "540",
    val endMinute: String = "1020",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(),
) {
    fun draft() = CanonicalDailyWindowDraft(
        weekdays = CanonicalWeekday.entries.filter(weekdays::contains),
        startMinute = startMinute.requiredInt("Daily window start"),
        endMinute = endMinute.requiredInt("Daily window end"),
        strength = strength.draft("Daily window"),
    )

    companion object {
        fun from(value: CanonicalDailyWindowDraft) = CanonicalDailyWindowForm(
            weekdays = value.weekdays.toSet(),
            startMinute = value.startMinute.toString(),
            endMinute = value.endMinute.toString(),
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalAbsoluteWindowForm(
    val startsAt: String = "",
    val endsAt: String = "",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(),
) {
    fun draft(label: String) = CanonicalAbsoluteWindowDraft(
        startsAt = startsAt.required("$label start"),
        endsAt = endsAt.required("$label end"),
        strength = strength.draft(label),
    )

    companion object {
        fun from(value: CanonicalAbsoluteWindowDraft) = CanonicalAbsoluteWindowForm(
            startsAt = value.startsAt,
            endsAt = value.endsAt,
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalStringConstraintForm(
    val value: String = "",
    val strength: CanonicalStrengthForm = CanonicalStrengthForm(),
) {
    fun draft(label: String) = CanonicalQualifiedStringDraft(
        value = value.requiredMetadataText(label),
        strength = strength.draft(label),
    )

    companion object {
        fun from(value: CanonicalQualifiedStringDraft) = CanonicalStringConstraintForm(
            value = value.value,
            strength = CanonicalStrengthForm.from(value.strength),
        )
    }
}

internal data class CanonicalGoalMeasureForm(
    val name: String = "",
    val target: String = "1",
    val current: String = "0",
    val unit: String = "",
) {
    fun draft() = CanonicalGoalMeasureDraft(
        name = name.requiredMetadataText("Measure name"),
        target = target.requiredLong("Measure target"),
        current = current.requiredLong("Measure current value"),
        unit = unit.requiredMetadataText("Measure unit"),
    )

    companion object {
        fun from(value: CanonicalGoalMeasureDraft) = CanonicalGoalMeasureForm(
            name = value.name,
            target = value.target.toString(),
            current = value.current.toString(),
            unit = value.unit,
        )
    }
}

internal data class CanonicalItemEditorForm(
    val source: CanonicalItemDraft,
    val placement: CanonicalDraftPlacement,
    val kind: ItemKind,
    val isSensitive: Boolean,
    val title: String,
    val notes: String,
    val timezoneName: String,
    val hasDuration: Boolean,
    val durationSeconds: String,
    val earliestStartAt: String,
    val deadlineAt: String,
    val recurrenceKind: CanonicalRecurrenceKind?,
    val recurrenceCount: String,
    val recurrenceIntervalMinutes: String,
    val weekdays: Set<CanonicalWeekday>,
    val recurrencePeriod: CanonicalRecurrencePeriod,
    val recurrenceSemantics: CanonicalRecurrenceSemantics,
    val recurrenceMinimumSpacingMinutes: String,
    val recurrenceAnchorAt: String,
    val recurrenceRrule: String,
    val energy: EnergyLevel?,
    val energyStrength: CanonicalStrengthForm?,
    val tags: List<String>,
    val preferredStartMinute: String,
    val minimumGapMinutes: String,
    val maximumSessions: String,
    val maximumSplitDays: String,
    val schedulingSpecified: Boolean,
    val constraintEarliest: CanonicalInstantConstraintForm?,
    val constraintLatest: CanonicalInstantConstraintForm?,
    val minimumNotice: CanonicalMinutesConstraintForm?,
    val allowedWeekdays: Set<CanonicalWeekday>,
    val allowedWeekdaysStrength: CanonicalStrengthForm?,
    val preferredDailyWindows: List<CanonicalDailyWindowForm>,
    val preferredAbsoluteWindows: List<CanonicalAbsoluteWindowForm>,
    val forbiddenWindows: List<CanonicalAbsoluteWindowForm>,
    val requiredContexts: List<CanonicalStringConstraintForm>,
    val requiredLocation: CanonicalStringConstraintForm?,
    val dependencies: List<CanonicalDependencyForm>,
    val maximumDailyWork: CanonicalMinutesConstraintForm?,
    val maximumWeeklyWork: CanonicalMinutesConstraintForm?,
    val bufferBeforeMinutes: String,
    val bufferAfterMinutes: String,
    val bufferSpecified: Boolean,
    val bufferStrength: CanonicalStrengthForm?,
    val includesNullOccurrenceWindow: Boolean,
    val isSplittable: Boolean,
    val minimumChunkSeconds: String,
    val maximumChunkSeconds: String,
    val hasOwnEffort: Boolean,
    val hasOwnEffortSpecified: Boolean,
    val hasHabitTarget: Boolean,
    val habitTargetAmount: String,
    val habitTargetUnit: String,
    val preservesStreakWhenPaused: Boolean,
    val preservesStreakSpecified: Boolean,
    val routineOrdered: Boolean,
    val routineOrderedSpecified: Boolean,
    val goalMeasures: List<CanonicalGoalMeasureForm>,
    val goalMeasuresSpecified: Boolean,
    val hasGoalWeeklyAllocation: Boolean,
    val goalWeeklyMinimumMinutes: String,
    val goalWeeklyMaximumMinutes: String,
    val breakCategory: CanonicalBreakCategory,
    val breakCategorySpecified: Boolean,
    val breakMandatory: Boolean,
    val breakMandatorySpecified: Boolean,
    val breakPromptToResume: Boolean,
    val breakPromptSpecified: Boolean,
    val importance: Int,
    val urgency: Int,
    val parentId: String?,
    val siblingOrder: String,
    val eventStart: String,
    val eventEnd: String,
    val eventAllDay: Boolean,
    val eventTentative: Boolean,
    val eventBusy: Boolean,
) {
    val supportsRecurrence: Boolean
        get() = kind in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE)

    fun withKind(value: ItemKind): CanonicalItemEditorForm = copy(
        kind = value,
        recurrenceKind = when {
            value == ItemKind.HABIT && recurrenceKind == null -> CanonicalRecurrenceKind.DAILY
            value !in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE) -> null
            else -> recurrenceKind
        },
        isSplittable = isSplittable && value != ItemKind.EVENT,
    )

    /** Explicitly removes metadata that cannot coexist with an owned event timing block. */
    fun withoutEventFlexibleMetadata(): CanonicalItemEditorForm = copy(
        energy = null,
        energyStrength = null,
        tags = emptyList(),
        preferredStartMinute = "",
        minimumGapMinutes = "0",
        maximumSessions = "",
        maximumSplitDays = "",
        schedulingSpecified = false,
        constraintEarliest = null,
        constraintLatest = null,
        minimumNotice = null,
        allowedWeekdays = emptySet(),
        allowedWeekdaysStrength = null,
        preferredDailyWindows = emptyList(),
        preferredAbsoluteWindows = emptyList(),
        forbiddenWindows = emptyList(),
        requiredContexts = emptyList(),
        requiredLocation = null,
        dependencies = emptyList(),
        maximumDailyWork = null,
        maximumWeeklyWork = null,
        bufferBeforeMinutes = "0",
        bufferAfterMinutes = "0",
        bufferSpecified = false,
        bufferStrength = null,
        includesNullOccurrenceWindow = false,
        hasOwnEffort = false,
        hasOwnEffortSpecified = false,
    )

    fun draft(itemId: String): Result<CanonicalItemDraft> = runCatching {
        val ordinaryDuration = if (hasDuration) {
            durationSeconds.requiredLong("Duration")
        } else {
            null
        }
        val hasEventTiming = eventStart.isNotBlank() || eventEnd.isNotBlank()
        val event = if (
            kind == ItemKind.EVENT &&
            (hasEventTiming || placement == CanonicalDraftPlacement.PLANNED)
        ) {
            CanonicalEventTimingDraft(
                startsAt = eventStart.required(
                    if (placement == CanonicalDraftPlacement.PLANNED) {
                        "Event start is required before placing this item in the plan"
                    } else {
                        "Event start"
                    },
                ),
                endsAt = eventEnd.required(
                    if (placement == CanonicalDraftPlacement.PLANNED) {
                        "Event end is required before placing this item in the plan"
                    } else {
                        "Event end"
                    },
                ),
                allDay = eventAllDay,
                tentative = eventTentative,
                busy = eventBusy,
            )
        } else {
            null
        }
        val eventInterval = event?.let {
            Duration.between(
                requireCanonicalInstant(it.startsAt, "Event start"),
                requireCanonicalInstant(it.endsAt, "Event end"),
            )
        }
        val eventBoundsUnchanged = event != null &&
            source.eventTiming?.startsAt == event.startsAt &&
            source.eventTiming.endsAt == event.endsAt
        val eventDuration = when {
            event == null -> null
            eventBoundsUnchanged -> source.durationSeconds
            eventInterval?.nano == 0 &&
                eventInterval.seconds in 1..CanonicalItemDraft.MAX_DURATION_SECONDS ->
                eventInterval.seconds
            else -> null
        }
        val duration = if (event == null) ordinaryDuration else eventDuration
        val recurrence = recurrenceKind?.let { recurrenceType ->
            when (recurrenceType) {
                CanonicalRecurrenceKind.DAILY,
                CanonicalRecurrenceKind.MONTHLY,
                -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    occurrencesPerPeriod = recurrenceCount.requiredInt("Repeat count"),
                )
                CanonicalRecurrenceKind.WEEKLY -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    occurrencesPerPeriod = recurrenceCount.requiredInt("Repeat count"),
                    weekdays = CanonicalWeekday.entries.filter(weekdays::contains),
                )
                CanonicalRecurrenceKind.EVERY_INTERVAL,
                CanonicalRecurrenceKind.AFTER_COMPLETION,
                -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    intervalSeconds = Math.multiplyExact(
                        recurrenceIntervalMinutes.requiredLong("Repeat interval"),
                        60L,
                    ),
                )
                CanonicalRecurrenceKind.FREQUENCY -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    occurrencesPerPeriod = recurrenceCount.requiredInt("Frequency target"),
                    weekdays = if (recurrenceSemantics == CanonicalRecurrenceSemantics.CALENDAR) {
                        CanonicalWeekday.entries.filter(weekdays::contains)
                    } else {
                        emptyList()
                    },
                    period = recurrencePeriod,
                    semantics = recurrenceSemantics,
                    minimumSpacingMinutes = recurrenceMinimumSpacingMinutes
                        .optionalLong("Minimum recurrence spacing") ?: 0,
                    anchorAt = if (recurrenceSemantics == CanonicalRecurrenceSemantics.ROLLING) {
                        recurrenceAnchorAt.optional("Recurrence anchor")
                    } else {
                        null
                    },
                )
                CanonicalRecurrenceKind.CUSTOM -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    rrule = recurrenceRrule.required("Custom RRULE"),
                )
            }
        }
        val schedulingValue = CanonicalSchedulingConstraintsDraft(
            earliestStart = constraintEarliest?.draft("Flexible earliest start"),
            latestFinish = constraintLatest?.draft("Flexible latest finish"),
            minimumNotice = minimumNotice?.draft("Minimum notice"),
            allowedWeekdays = allowedWeekdaysStrength?.let { strength ->
                CanonicalQualifiedWeekdaysDraft(
                    value = CanonicalWeekday.entries.filter(allowedWeekdays::contains),
                    strength = strength.draft("Allowed weekdays"),
                )
            },
            preferredDailyWindows = preferredDailyWindows.map(CanonicalDailyWindowForm::draft),
            preferredAbsoluteWindows = preferredAbsoluteWindows.map {
                it.draft("Preferred absolute window")
            },
            forbiddenWindows = forbiddenWindows.map { it.draft("Forbidden window") },
            requiredContexts = requiredContexts.map { it.draft("Required context") },
            requiredLocation = requiredLocation?.draft("Required location"),
            dependencies = dependencies.map(CanonicalDependencyForm::draft),
            maximumDailyWork = maximumDailyWork?.draft("Maximum daily work"),
            maximumWeeklyWork = maximumWeeklyWork?.draft("Maximum weekly work"),
            buffers = if (bufferSpecified) {
                CanonicalBufferPolicyDraft(
                    beforeMinutes = bufferBeforeMinutes.requiredLong("Preparation buffer"),
                    afterMinutes = bufferAfterMinutes.requiredLong("Decompression buffer"),
                    strength = bufferStrength?.draft("Buffers"),
                )
            } else {
                null
            },
            includesNullOccurrenceWindow = includesNullOccurrenceWindow,
        )
        val hasSchedulingValues = schedulingValue != CanonicalSchedulingConstraintsDraft()
        val constraints = CanonicalFlexibleConstraintsDraft(
                energy = energy,
                energyStrength = energyStrength?.draft("Energy"),
                tags = tags,
                preferredStartMinute = preferredStartMinute.optionalInt("Preferred start"),
                minimumGapMinutes = if (isSplittable) {
                    minimumGapMinutes.optionalLong("Minimum gap") ?: 0
                } else {
                    0
                },
                maximumSessions = if (isSplittable) {
                    maximumSessions.optionalInt("Maximum sessions")
                } else {
                    null
                },
                maximumSplitDays = if (isSplittable) {
                    maximumSplitDays.optionalInt("Maximum split days")
                } else {
                    null
                },
                scheduling = schedulingValue.takeIf { schedulingSpecified || hasSchedulingValues },
                hasOwnEffort = if (
                    kind in setOf(ItemKind.ROUTINE, ItemKind.GOAL, ItemKind.EVENT)
                ) {
                    hasOwnEffort.takeIf { hasOwnEffortSpecified }
                } else {
                    source.constraints.hasOwnEffort
                },
                goalIds = source.constraints.goalIds,
                habitTarget = if (kind == ItemKind.HABIT && hasHabitTarget) {
                    CanonicalHabitTargetDraft(
                        amount = habitTargetAmount.requiredLong("Habit target"),
                        unit = habitTargetUnit.requiredMetadataText("Habit target unit"),
                    )
                } else {
                    null
                },
                preservesStreakWhenPaused = if (
                    kind == ItemKind.HABIT && preservesStreakSpecified
                ) preservesStreakWhenPaused else null,
                routineOrdered = if (
                    kind == ItemKind.ROUTINE && routineOrderedSpecified
                ) routineOrdered else null,
                goalMeasures = if (kind == ItemKind.GOAL && goalMeasuresSpecified) {
                    goalMeasures.map(CanonicalGoalMeasureForm::draft)
                } else {
                    null
                },
                goalWeeklyAllocation = if (kind == ItemKind.GOAL && hasGoalWeeklyAllocation) {
                    CanonicalWeeklyAllocationDraft(
                        minimumMinutes = goalWeeklyMinimumMinutes.requiredLong(
                            "Minimum weekly allocation",
                        ),
                        maximumMinutes = goalWeeklyMaximumMinutes.optionalLong(
                            "Maximum weekly allocation",
                        ),
                    )
                } else {
                    null
                },
                breakCategory = if (
                    kind == ItemKind.BREAK && breakCategorySpecified
                ) breakCategory else null,
                breakMandatory = if (
                    kind == ItemKind.BREAK && breakMandatorySpecified
                ) breakMandatory else null,
                breakPromptToResume = if (
                    kind == ItemKind.BREAK && breakPromptSpecified
                ) breakPromptToResume else null,
            )
        val split = if (event == null && isSplittable) {
            CanonicalSplitDraft(
                kind = CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = minimumChunkSeconds.requiredLong("Minimum chunk"),
                maximumChunkSeconds = maximumChunkSeconds.requiredLong("Maximum chunk"),
            )
        } else {
            CanonicalSplitDraft()
        }
        source.copy(
            placement = placement,
            kind = kind,
            isSensitive = isSensitive,
            title = title,
            notes = notes,
            timezoneName = timezoneName,
            durationSeconds = duration,
            deadlineAt = when {
                event == null -> deadlineAt.optional("Deadline")
                eventBoundsUnchanged -> source.deadlineAt
                else -> event.endsAt
            },
            earliestStartAt = when {
                event == null -> earliestStartAt.optional("Earliest start")
                eventBoundsUnchanged -> source.earliestStartAt
                else -> event.startsAt
            },
            recurrence = recurrence,
            constraints = constraints,
            split = split,
            importance = importance,
            urgency = urgency,
            parentId = parentId,
            siblingOrder = siblingOrder.requiredLong("Sibling order"),
            eventTiming = event,
        ).normalized().also { it.requireValid(itemId) }
    }

    fun validationIssue(itemId: String): String? = draft(itemId).exceptionOrNull()?.let {
        it.message?.takeIf(String::isNotBlank) ?: "Review the highlighted item details."
    }

    companion object {
        fun from(draft: CanonicalItemDraft): CanonicalItemEditorForm =
            CanonicalItemEditorForm(
                source = draft,
                placement = draft.placement,
                kind = draft.kind,
                isSensitive = draft.isSensitive,
                title = draft.title,
                notes = draft.notes.orEmpty(),
                timezoneName = draft.timezoneName,
                hasDuration = draft.durationSeconds != null,
                durationSeconds = (draft.durationSeconds ?: 30L * 60L).toString(),
                earliestStartAt = draft.earliestStartAt.orEmpty(),
                deadlineAt = draft.deadlineAt.orEmpty(),
                recurrenceKind = draft.recurrence?.kind,
                recurrenceCount = (draft.recurrence?.occurrencesPerPeriod ?: 1).toString(),
                recurrenceIntervalMinutes =
                    ((draft.recurrence?.intervalSeconds ?: 24L * 60L * 60L) / 60L).toString(),
                weekdays = draft.recurrence?.weekdays.orEmpty().toSet(),
                recurrencePeriod = draft.recurrence?.period ?: CanonicalRecurrencePeriod.WEEK,
                recurrenceSemantics = draft.recurrence?.semantics
                    ?: CanonicalRecurrenceSemantics.CALENDAR,
                recurrenceMinimumSpacingMinutes =
                    (draft.recurrence?.minimumSpacingMinutes ?: 0).toString(),
                recurrenceAnchorAt = draft.recurrence?.anchorAt.orEmpty(),
                recurrenceRrule = draft.recurrence?.rrule.orEmpty(),
                energy = draft.constraints.energy,
                energyStrength = draft.constraints.energyStrength?.let(CanonicalStrengthForm::from),
                tags = draft.constraints.tags,
                preferredStartMinute = draft.constraints.preferredStartMinute?.toString().orEmpty(),
                minimumGapMinutes = draft.constraints.minimumGapMinutes.toString(),
                maximumSessions = draft.constraints.maximumSessions?.toString().orEmpty(),
                maximumSplitDays = draft.constraints.maximumSplitDays?.toString().orEmpty(),
                schedulingSpecified = draft.constraints.scheduling != null,
                constraintEarliest = draft.constraints.scheduling?.earliestStart?.let(
                    CanonicalInstantConstraintForm::from,
                ),
                constraintLatest = draft.constraints.scheduling?.latestFinish?.let(
                    CanonicalInstantConstraintForm::from,
                ),
                minimumNotice = draft.constraints.scheduling?.minimumNotice?.let(
                    CanonicalMinutesConstraintForm::from,
                ),
                allowedWeekdays = draft.constraints.scheduling?.allowedWeekdays?.value
                    .orEmpty().toSet(),
                allowedWeekdaysStrength = draft.constraints.scheduling?.allowedWeekdays?.strength
                    ?.let(CanonicalStrengthForm::from),
                preferredDailyWindows = draft.constraints.scheduling?.preferredDailyWindows
                    .orEmpty().map(CanonicalDailyWindowForm::from),
                preferredAbsoluteWindows = draft.constraints.scheduling?.preferredAbsoluteWindows
                    .orEmpty().map(CanonicalAbsoluteWindowForm::from),
                forbiddenWindows = draft.constraints.scheduling?.forbiddenWindows
                    .orEmpty().map(CanonicalAbsoluteWindowForm::from),
                requiredContexts = draft.constraints.scheduling?.requiredContexts
                    .orEmpty().map(CanonicalStringConstraintForm::from),
                requiredLocation = draft.constraints.scheduling?.requiredLocation?.let(
                    CanonicalStringConstraintForm::from,
                ),
                dependencies = draft.constraints.scheduling?.dependencies.orEmpty().map(
                    CanonicalDependencyForm::from,
                ),
                maximumDailyWork = draft.constraints.scheduling?.maximumDailyWork?.let(
                    CanonicalMinutesConstraintForm::from,
                ),
                maximumWeeklyWork = draft.constraints.scheduling?.maximumWeeklyWork?.let(
                    CanonicalMinutesConstraintForm::from,
                ),
                bufferBeforeMinutes =
                    (draft.constraints.scheduling?.buffers?.beforeMinutes ?: 0).toString(),
                bufferAfterMinutes =
                    (draft.constraints.scheduling?.buffers?.afterMinutes ?: 0).toString(),
                bufferSpecified = draft.constraints.scheduling?.buffers != null,
                bufferStrength = draft.constraints.scheduling?.buffers?.strength?.let(
                    CanonicalStrengthForm::from,
                ),
                includesNullOccurrenceWindow =
                    draft.constraints.scheduling?.includesNullOccurrenceWindow ?: false,
                isSplittable = draft.split.kind == CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = (draft.split.minimumChunkSeconds ?: 15L * 60L).toString(),
                maximumChunkSeconds =
                    (draft.split.maximumChunkSeconds ?: draft.durationSeconds ?: 30L * 60L).toString(),
                hasOwnEffort = draft.constraints.hasOwnEffort ?: false,
                hasOwnEffortSpecified = draft.constraints.hasOwnEffort != null,
                hasHabitTarget = draft.constraints.habitTarget != null,
                habitTargetAmount = (draft.constraints.habitTarget?.amount ?: 1).toString(),
                habitTargetUnit = draft.constraints.habitTarget?.unit.orEmpty(),
                preservesStreakWhenPaused =
                    draft.constraints.preservesStreakWhenPaused ?: true,
                preservesStreakSpecified =
                    draft.constraints.preservesStreakWhenPaused != null,
                routineOrdered = draft.constraints.routineOrdered ?: false,
                routineOrderedSpecified = draft.constraints.routineOrdered != null,
                goalMeasures = draft.constraints.goalMeasures.orEmpty().map(
                    CanonicalGoalMeasureForm::from,
                ),
                goalMeasuresSpecified = draft.constraints.goalMeasures != null,
                hasGoalWeeklyAllocation = draft.constraints.goalWeeklyAllocation != null,
                goalWeeklyMinimumMinutes =
                    (draft.constraints.goalWeeklyAllocation?.minimumMinutes ?: 60).toString(),
                goalWeeklyMaximumMinutes =
                    draft.constraints.goalWeeklyAllocation?.maximumMinutes?.toString().orEmpty(),
                breakCategory = draft.constraints.breakCategory ?: CanonicalBreakCategory.OTHER,
                breakCategorySpecified = draft.constraints.breakCategory != null,
                breakMandatory = draft.constraints.breakMandatory ?: false,
                breakMandatorySpecified = draft.constraints.breakMandatory != null,
                breakPromptToResume = draft.constraints.breakPromptToResume ?: true,
                breakPromptSpecified = draft.constraints.breakPromptToResume != null,
                importance = draft.importance,
                urgency = draft.urgency,
                parentId = draft.parentId,
                siblingOrder = draft.siblingOrder.toString(),
                eventStart = draft.eventTiming?.startsAt.orEmpty(),
                eventEnd = draft.eventTiming?.endsAt.orEmpty(),
                eventAllDay = draft.eventTiming?.allDay ?: false,
                eventTentative = draft.eventTiming?.tentative ?: false,
                eventBusy = draft.eventTiming?.busy ?: true,
            )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CanonicalItemEditorSheet(
    route: CanonicalItemEditorRoute,
    parentOptions: List<CanonicalParentOption>,
    dependencyContext: CanonicalDependencyEditorContext,
    onDismiss: () -> Unit,
    onSave: suspend (CanonicalItemDraft) -> Boolean,
) {
    var form by remember(route.routeId) {
        mutableStateOf(CanonicalItemEditorForm.from(route.initialDraft))
    }
    var saveError by remember(route.routeId) { mutableStateOf<String?>(null) }
    var isSaving by remember(route.routeId) { mutableStateOf(false) }
    val coroutineScope = rememberCoroutineScope()
    val currentDraft = form.draft(route.itemId)
    val issue = currentDraft.exceptionOrNull()?.let {
        it.message?.takeIf(String::isNotBlank) ?: "Review the highlighted item details."
    } ?: currentDraft.getOrNull()?.let(dependencyContext::cycleWarning)
    ModalBottomSheet(onDismissRequest = { if (!isSaving) onDismiss() }) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 820.dp)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(Icons.Outlined.EditNote, contentDescription = null)
                Column {
                    Text(
                        when (route.mode) {
                            CanonicalItemEditorMode.CREATE -> "New detailed item"
                            CanonicalItemEditorMode.REPLACE -> "Edit canonical item"
                            CanonicalItemEditorMode.UPDATE_PENDING -> "Edit queued change"
                        },
                        style = MaterialTheme.typography.titleLarge,
                    )
                    Text(
                        "Saved to the encrypted journal before any sync.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            EditorSection("Identity") {
                OutlinedTextField(
                    value = form.title,
                    onValueChange = { form = form.copy(title = it) },
                    label = { Text("Title") },
                    modifier = Modifier.fillMaxWidth().testTag("canonical_editor_title"),
                    singleLine = true,
                )
                OutlinedTextField(
                    value = form.notes,
                    onValueChange = { form = form.copy(notes = it) },
                    label = { Text("Notes") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                )
                ChoiceRow {
                    ItemKind.entries.filterNot { it == ItemKind.PROJECT }.forEach { option ->
                        FilterChip(
                            selected = form.kind == option,
                            onClick = { form = form.withKind(option) },
                            label = { Text(option.label) },
                        )
                    }
                }
                LabeledSwitch(
                    title = "Sensitive",
                    detail = "Protect the title, notes, and inherited child context.",
                    checked = form.isSensitive,
                    onCheckedChange = { form = form.copy(isSensitive = it) },
                    testTag = "canonical_editor_sensitive",
                )
            }

            EditorSection("Planning") {
                ChoiceRow {
                    CanonicalDraftPlacement.entries.forEach { placement ->
                        FilterChip(
                            selected = form.placement == placement,
                            onClick = { form = form.copy(placement = placement) },
                            label = {
                                Text(if (placement == CanonicalDraftPlacement.INBOX) "Inbox" else "Planned")
                            },
                        )
                    }
                }
                OutlinedTextField(
                    value = form.timezoneName,
                    onValueChange = { form = form.copy(timezoneName = it) },
                    label = { Text("IANA timezone") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                if (form.kind != ItemKind.EVENT) {
                    LabeledSwitch(
                        title = "Duration estimate",
                        detail = "Inbox may stay unknown; schedulable Planned work needs an estimate.",
                        checked = form.hasDuration,
                        onCheckedChange = { form = form.copy(hasDuration = it) },
                    )
                    if (form.hasDuration) {
                        NumberField(
                            value = form.durationSeconds,
                            onValueChange = { form = form.copy(durationSeconds = it) },
                            label = "Duration (seconds)",
                        )
                    }
                    InstantField(
                        value = form.earliestStartAt,
                        onValueChange = { form = form.copy(earliestStartAt = it) },
                        label = "Hard earliest start (optional ISO-8601)",
                    )
                    InstantField(
                        value = form.deadlineAt,
                        onValueChange = { form = form.copy(deadlineAt = it) },
                        label = "Hard deadline (optional ISO-8601)",
                    )
                }
                PrioritySlider("Importance", form.importance) {
                    form = form.copy(importance = it)
                }
                PrioritySlider("Urgency", form.urgency) {
                    form = form.copy(urgency = it)
                }
            }

            if (form.kind == ItemKind.EVENT) {
                EditorSection("Exact event timing") {
                    Text(
                        "Events require explicit canonical instants; DayWeave will not invent them.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    InstantField(
                        value = form.eventStart,
                        onValueChange = { form = form.copy(eventStart = it) },
                        label = "Starts at (ISO-8601)",
                    )
                    InstantField(
                        value = form.eventEnd,
                        onValueChange = { form = form.copy(eventEnd = it) },
                        label = "Ends at (ISO-8601)",
                    )
                    LabeledSwitch("All day", "Bounds must be local midnight.", form.eventAllDay, {
                        form = form.copy(eventAllDay = it)
                    })
                    LabeledSwitch("Tentative", "Mark this firm block tentative.", form.eventTentative, {
                        form = form.copy(eventTentative = it)
                    })
                    LabeledSwitch(
                        "Publish as busy",
                        "Controls Google/calendar publication. DayWeave always reserves this local event.",
                        form.eventBusy,
                        { form = form.copy(eventBusy = it) },
                    )
                }
            }

            if (form.supportsRecurrence) {
                EditorSection("Recurrence") {
                    ChoiceRow {
                        FilterChip(
                            selected = form.recurrenceKind == null,
                            onClick = { form = form.copy(recurrenceKind = null) },
                            enabled = form.kind != ItemKind.HABIT ||
                                form.placement == CanonicalDraftPlacement.INBOX,
                            label = { Text("None") },
                        )
                        CanonicalRecurrenceKind.entries.forEach { option ->
                            FilterChip(
                                selected = form.recurrenceKind == option,
                                onClick = { form = form.copy(recurrenceKind = option) },
                                label = { Text(option.editorLabel()) },
                                modifier = Modifier.testTag(
                                    "canonical_editor_recurrence_${option.wireValue}",
                                ),
                            )
                        }
                    }
                    when (form.recurrenceKind) {
                        CanonicalRecurrenceKind.DAILY,
                        CanonicalRecurrenceKind.WEEKLY,
                        CanonicalRecurrenceKind.MONTHLY,
                        -> NumberField(
                            value = form.recurrenceCount,
                            onValueChange = { form = form.copy(recurrenceCount = it) },
                            label = "Occurrences per period",
                        )
                        CanonicalRecurrenceKind.EVERY_INTERVAL,
                        CanonicalRecurrenceKind.AFTER_COMPLETION,
                        -> NumberField(
                            value = form.recurrenceIntervalMinutes,
                            onValueChange = { form = form.copy(recurrenceIntervalMinutes = it) },
                            label = "Interval (minutes, max 527,040)",
                        )
                        CanonicalRecurrenceKind.FREQUENCY -> {
                            NumberField(
                                value = form.recurrenceCount,
                                onValueChange = { form = form.copy(recurrenceCount = it) },
                                label = "Target occurrences",
                            )
                            Text("Period", style = MaterialTheme.typography.labelLarge)
                            ChoiceRow {
                                CanonicalRecurrencePeriod.entries.forEach { period ->
                                    FilterChip(
                                        selected = form.recurrencePeriod == period,
                                        onClick = { form = form.copy(recurrencePeriod = period) },
                                        label = { Text(period.name.lowercase().replaceFirstChar(Char::uppercase)) },
                                    )
                                }
                            }
                            Text("Semantics", style = MaterialTheme.typography.labelLarge)
                            ChoiceRow {
                                CanonicalRecurrenceSemantics.entries.forEach { semantics ->
                                    FilterChip(
                                        selected = form.recurrenceSemantics == semantics,
                                        onClick = {
                                            form = form.copy(recurrenceSemantics = semantics)
                                        },
                                        label = {
                                            Text(
                                                semantics.name.lowercase()
                                                    .replaceFirstChar(Char::uppercase),
                                            )
                                        },
                                    )
                                }
                            }
                            NumberField(
                                value = form.recurrenceMinimumSpacingMinutes,
                                onValueChange = {
                                    form = form.copy(recurrenceMinimumSpacingMinutes = it)
                                },
                                label = "Minimum spacing (minutes, max 527,040)",
                            )
                            if (form.recurrenceSemantics == CanonicalRecurrenceSemantics.ROLLING) {
                                InstantField(
                                    value = form.recurrenceAnchorAt,
                                    onValueChange = { form = form.copy(recurrenceAnchorAt = it) },
                                    label = "Rolling anchor (optional ISO-8601)",
                                )
                            }
                        }
                        CanonicalRecurrenceKind.CUSTOM -> {
                            OutlinedTextField(
                                value = form.recurrenceRrule,
                                onValueChange = { form = form.copy(recurrenceRrule = it) },
                                label = { Text("Finite RRULE") },
                                supportingText = {
                                    Text(
                                        "Daily, weekly, or monthly; requires COUNT or date-only UNTIL.",
                                    )
                                },
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .testTag("canonical_editor_recurrence_rrule"),
                                singleLine = true,
                            )
                            Text(
                                "Supports INTERVAL, BYDAY, and BYMONTHDAY without ordinal weekdays.",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        null -> Unit
                    }
                    if (
                        form.recurrenceKind == CanonicalRecurrenceKind.WEEKLY ||
                        form.recurrenceKind == CanonicalRecurrenceKind.FREQUENCY &&
                        form.recurrenceSemantics == CanonicalRecurrenceSemantics.CALENDAR
                    ) {
                        Text("Eligible weekdays (empty means every day)")
                        ChoiceRow {
                            CanonicalWeekday.entries.forEach { weekday ->
                                FilterChip(
                                    selected = weekday in form.weekdays,
                                    onClick = {
                                        form = form.copy(
                                            weekdays = if (weekday in form.weekdays) {
                                                form.weekdays - weekday
                                            } else {
                                                form.weekdays + weekday
                                            },
                                        )
                                    },
                                    label = { Text(weekday.name.take(2)) },
                                )
                            }
                        }
                    }
                }
            }

            when (form.kind) {
                ItemKind.HABIT -> EditorSection("Habit behavior") {
                    LabeledSwitch(
                        title = "Quantity target",
                        detail = "Track a measurable amount for every habit occurrence.",
                        checked = form.hasHabitTarget,
                        onCheckedChange = { form = form.copy(hasHabitTarget = it) },
                        testTag = "canonical_editor_habit_target",
                    )
                    if (form.hasHabitTarget) {
                        NumberField(
                            form.habitTargetAmount,
                            { form = form.copy(habitTargetAmount = it) },
                            "Target amount",
                        )
                        OutlinedTextField(
                            value = form.habitTargetUnit,
                            onValueChange = { form = form.copy(habitTargetUnit = it) },
                            label = { Text("Target unit") },
                            modifier = Modifier.fillMaxWidth(),
                            singleLine = true,
                        )
                    }
                    LabeledSwitch(
                        title = "Preserve streak while paused",
                        detail = "A deliberate pause will not break the habit streak.",
                        checked = form.preservesStreakWhenPaused,
                        onCheckedChange = {
                            form = form.copy(
                                preservesStreakWhenPaused = it,
                                preservesStreakSpecified = true,
                            )
                        },
                        testTag = "canonical_editor_habit_pause_streak",
                    )
                }
                ItemKind.ROUTINE -> EditorSection("Routine behavior") {
                    LabeledSwitch(
                        title = "Ordered children",
                        detail = "Schedule child steps in their hierarchy order.",
                        checked = form.routineOrdered,
                        onCheckedChange = {
                            form = form.copy(routineOrdered = it, routineOrderedSpecified = true)
                        },
                        testTag = "canonical_editor_routine_ordered",
                    )
                    LabeledSwitch(
                        title = "Routine has its own effort",
                        detail = "Reserve the routine duration in addition to its child steps.",
                        checked = form.hasOwnEffort,
                        onCheckedChange = {
                            form = form.copy(hasOwnEffort = it, hasOwnEffortSpecified = true)
                        },
                        testTag = "canonical_editor_routine_own_effort",
                    )
                }
                ItemKind.GOAL -> EditorSection("Goal tracking") {
                    LabeledSwitch(
                        title = "Goal has its own effort",
                        detail = "Reserve the goal duration in addition to child work.",
                        checked = form.hasOwnEffort,
                        onCheckedChange = {
                            form = form.copy(hasOwnEffort = it, hasOwnEffortSpecified = true)
                        },
                        testTag = "canonical_editor_goal_own_effort",
                    )
                    Text("Measures", style = MaterialTheme.typography.labelLarge)
                    form.goalMeasures.forEachIndexed { index, measure ->
                        WindowCardLabel("Measure ${index + 1}") {
                            form = form.copy(
                                goalMeasures = form.goalMeasures.removing(index),
                                goalMeasuresSpecified = true,
                            )
                        }
                        OutlinedTextField(
                            value = measure.name,
                            onValueChange = {
                                form = form.copy(
                                    goalMeasures = form.goalMeasures.replacing(
                                        index,
                                        measure.copy(name = it),
                                    ),
                                )
                            },
                            label = { Text("Measure name") },
                            modifier = Modifier.fillMaxWidth(),
                            singleLine = true,
                        )
                        NumberField(measure.target, {
                            form = form.copy(
                                goalMeasures = form.goalMeasures.replacing(
                                    index,
                                    measure.copy(target = it),
                                ),
                            )
                        }, "Target")
                        NumberField(measure.current, {
                            form = form.copy(
                                goalMeasures = form.goalMeasures.replacing(
                                    index,
                                    measure.copy(current = it),
                                ),
                            )
                        }, "Current")
                        OutlinedTextField(
                            value = measure.unit,
                            onValueChange = {
                                form = form.copy(
                                    goalMeasures = form.goalMeasures.replacing(
                                        index,
                                        measure.copy(unit = it),
                                    ),
                                )
                            },
                            label = { Text("Unit") },
                            modifier = Modifier.fillMaxWidth(),
                            singleLine = true,
                        )
                    }
                    OutlinedButton(
                        onClick = {
                            form = form.copy(
                                goalMeasures = form.goalMeasures + CanonicalGoalMeasureForm(),
                                goalMeasuresSpecified = true,
                            )
                        },
                        modifier = Modifier.testTag("canonical_editor_goal_add_measure"),
                    ) { Text("Add measure") }
                    LabeledSwitch(
                        title = "Weekly allocation",
                        detail = "Set minimum and optional maximum goal minutes per week.",
                        checked = form.hasGoalWeeklyAllocation,
                        onCheckedChange = { form = form.copy(hasGoalWeeklyAllocation = it) },
                        testTag = "canonical_editor_goal_weekly_allocation",
                    )
                    if (form.hasGoalWeeklyAllocation) {
                        NumberField(form.goalWeeklyMinimumMinutes, {
                            form = form.copy(goalWeeklyMinimumMinutes = it)
                        }, "Minimum weekly minutes")
                        NumberField(form.goalWeeklyMaximumMinutes, {
                            form = form.copy(goalWeeklyMaximumMinutes = it)
                        }, "Maximum weekly minutes (optional)")
                    }
                }
                ItemKind.BREAK -> EditorSection("Break behavior") {
                    Text("Category", style = MaterialTheme.typography.labelLarge)
                    ChoiceRow {
                        CanonicalBreakCategory.entries.forEach { category ->
                            FilterChip(
                                selected = form.breakCategory == category,
                                onClick = {
                                    form = form.copy(
                                        breakCategory = category,
                                        breakCategorySpecified = true,
                                    )
                                },
                                label = {
                                    Text(
                                        category.name.lowercase().replaceFirstChar(Char::uppercase),
                                    )
                                },
                            )
                        }
                    }
                    LabeledSwitch(
                        title = "Mandatory break",
                        detail = "Treat this recovery interval as required.",
                        checked = form.breakMandatory,
                        onCheckedChange = {
                            form = form.copy(
                                breakMandatory = it,
                                breakMandatorySpecified = true,
                            )
                        },
                        testTag = "canonical_editor_break_mandatory",
                    )
                    LabeledSwitch(
                        title = "Prompt to resume",
                        detail = "Ask before returning to the interrupted work.",
                        checked = form.breakPromptToResume,
                        onCheckedChange = {
                            form = form.copy(
                                breakPromptToResume = it,
                                breakPromptSpecified = true,
                            )
                        },
                        testTag = "canonical_editor_break_resume_prompt",
                    )
                }
                ItemKind.EVENT, ItemKind.TASK, ItemKind.PROJECT -> Unit
            }

            EditorSection("Flexible constraints") {
                    if (form.kind == ItemKind.EVENT) {
                        Text(
                            "Inbox events can retain these details. An owned timing block must be " +
                                "the event's only scheduling metadata, so clear them explicitly " +
                                "before adding fixed bounds.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        OutlinedButton(
                            onClick = { form = form.withoutEventFlexibleMetadata() },
                            modifier = Modifier.testTag("canonical_editor_clear_event_metadata"),
                        ) { Text("Clear flexible metadata") }
                    }
                    Text("Energy", style = MaterialTheme.typography.labelLarge)
                    ChoiceRow {
                        FilterChip(
                            selected = form.energy == null,
                            onClick = {
                                form = form.copy(energy = null, energyStrength = null)
                            },
                            label = { Text("Any") },
                        )
                        EnergyLevel.entries.forEach { energy ->
                            FilterChip(
                                selected = form.energy == energy,
                                onClick = { form = form.copy(energy = energy) },
                                label = { Text(energy.label) },
                            )
                        }
                    }
                    if (form.energy != null) {
                        Text("Energy strength", style = MaterialTheme.typography.labelLarge)
                        ChoiceRow {
                            FilterChip(
                                selected = form.energyStrength == null,
                                onClick = { form = form.copy(energyStrength = null) },
                                label = { Text("Default") },
                            )
                            CanonicalConstraintLevel.entries.forEach { level ->
                                FilterChip(
                                    selected = form.energyStrength?.level == level,
                                    onClick = {
                                        form = form.copy(
                                            energyStrength = CanonicalStrengthForm(level),
                                        )
                                    },
                                    label = { Text(level.editorLabel()) },
                                )
                            }
                        }
                        form.energyStrength?.takeIf {
                            it.level == CanonicalConstraintLevel.SOFT
                        }?.let { strength ->
                            NumberField(
                                value = strength.weight,
                                onValueChange = {
                                    form = form.copy(
                                        energyStrength = strength.copy(weight = it),
                                    )
                                },
                                label = "Energy preference weight",
                            )
                        }
                    }
                    Text("Tags", style = MaterialTheme.typography.labelLarge)
                    form.tags.forEachIndexed { index, tag ->
                        WindowCardLabel("Tag ${index + 1}") {
                            form = form.copy(tags = form.tags.removing(index))
                        }
                        OutlinedTextField(
                            value = tag,
                            onValueChange = {
                                form = form.copy(tags = form.tags.replacing(index, it))
                            },
                            label = { Text("Tag") },
                            modifier = Modifier.fillMaxWidth(),
                            singleLine = true,
                        )
                    }
                    OutlinedButton(
                        onClick = { form = form.copy(tags = form.tags + "") },
                        modifier = Modifier.testTag("canonical_editor_add_tag"),
                    ) { Text("Add tag") }
                    NumberField(
                        value = form.preferredStartMinute,
                        onValueChange = { form = form.copy(preferredStartMinute = it) },
                        label = "Preferred minute of day (optional)",
                    )

                    OptionalInstantConstraintEditor(
                        title = "Flexible earliest start",
                        value = form.constraintEarliest,
                        onValueChange = {
                            form = form.copy(constraintEarliest = it, schedulingSpecified = true)
                        },
                    )
                    OptionalInstantConstraintEditor(
                        title = "Flexible latest finish",
                        value = form.constraintLatest,
                        onValueChange = {
                            form = form.copy(constraintLatest = it, schedulingSpecified = true)
                        },
                    )
                    OptionalMinutesConstraintEditor(
                        title = "Minimum notice (max 527,040 minutes)",
                        value = form.minimumNotice,
                        onValueChange = {
                            form = form.copy(minimumNotice = it, schedulingSpecified = true)
                        },
                    )

                    LabeledSwitch(
                        title = "Restrict weekdays",
                        detail = "Choose a hard rule or a weighted preference.",
                        checked = form.allowedWeekdaysStrength != null,
                        onCheckedChange = { enabled ->
                            form = form.copy(
                                allowedWeekdaysStrength = if (enabled) {
                                    CanonicalStrengthForm(CanonicalConstraintLevel.HARD)
                                } else {
                                    null
                                },
                                schedulingSpecified = true,
                            )
                        },
                    )
                    form.allowedWeekdaysStrength?.let { strength ->
                        ChoiceRow {
                            CanonicalWeekday.entries.forEach { weekday ->
                                FilterChip(
                                    selected = weekday in form.allowedWeekdays,
                                    onClick = {
                                        form = form.copy(
                                            allowedWeekdays = form.allowedWeekdays.toggle(weekday),
                                        )
                                    },
                                    label = { Text(weekday.name.take(2)) },
                                )
                            }
                        }
                        ConstraintStrengthEditor(strength) {
                            form = form.copy(allowedWeekdaysStrength = it)
                        }
                    }

                    Text("Preferred daily windows", style = MaterialTheme.typography.labelLarge)
                    form.preferredDailyWindows.forEachIndexed { index, window ->
                        WindowCardLabel("Daily window ${index + 1}") {
                            form = form.copy(
                                preferredDailyWindows = form.preferredDailyWindows.removing(index),
                            )
                        }
                        NumberField(window.startMinute, {
                            form = form.copy(
                                preferredDailyWindows = form.preferredDailyWindows.replacing(
                                    index,
                                    window.copy(startMinute = it),
                                ),
                            )
                        }, "Start minute")
                        NumberField(window.endMinute, {
                            form = form.copy(
                                preferredDailyWindows = form.preferredDailyWindows.replacing(
                                    index,
                                    window.copy(endMinute = it),
                                ),
                            )
                        }, "End minute")
                        ChoiceRow {
                            CanonicalWeekday.entries.forEach { weekday ->
                                FilterChip(
                                    selected = weekday in window.weekdays,
                                    onClick = {
                                        form = form.copy(
                                            preferredDailyWindows = form.preferredDailyWindows.replacing(
                                                index,
                                                window.copy(weekdays = window.weekdays.toggle(weekday)),
                                            ),
                                        )
                                    },
                                    label = { Text(weekday.name.take(2)) },
                                )
                            }
                        }
                        ConstraintStrengthEditor(window.strength) {
                            form = form.copy(
                                preferredDailyWindows = form.preferredDailyWindows.replacing(
                                    index,
                                    window.copy(strength = it),
                                ),
                            )
                        }
                    }
                    OutlinedButton(
                        onClick = {
                        form = form.copy(
                            preferredDailyWindows = form.preferredDailyWindows +
                                CanonicalDailyWindowForm(),
                            schedulingSpecified = true,
                        )
                        },
                        modifier = Modifier.testTag("canonical_editor_add_daily_window"),
                    ) { Text("Add daily window") }

                    AbsoluteWindowListEditor(
                        title = "Preferred absolute windows",
                        values = form.preferredAbsoluteWindows,
                        onValuesChange = {
                            form = form.copy(
                                preferredAbsoluteWindows = it,
                                schedulingSpecified = true,
                            )
                        },
                    )
                    AbsoluteWindowListEditor(
                        title = "Forbidden windows",
                        values = form.forbiddenWindows,
                        onValuesChange = {
                            form = form.copy(forbiddenWindows = it, schedulingSpecified = true)
                        },
                    )

                    Text("Required contexts", style = MaterialTheme.typography.labelLarge)
                    form.requiredContexts.forEachIndexed { index, context ->
                        WindowCardLabel("Context ${index + 1}") {
                            form = form.copy(
                                requiredContexts = form.requiredContexts.removing(index),
                            )
                        }
                        OutlinedTextField(
                            value = context.value,
                            onValueChange = {
                                form = form.copy(
                                    requiredContexts = form.requiredContexts.replacing(
                                        index,
                                        context.copy(value = it),
                                    ),
                                )
                            },
                            label = { Text("Context") },
                            modifier = Modifier.fillMaxWidth(),
                            singleLine = true,
                        )
                        ConstraintStrengthEditor(context.strength) {
                            form = form.copy(
                                requiredContexts = form.requiredContexts.replacing(
                                    index,
                                    context.copy(strength = it),
                                ),
                            )
                        }
                    }
                    OutlinedButton(
                        onClick = {
                        form = form.copy(
                            requiredContexts = form.requiredContexts + CanonicalStringConstraintForm(),
                            schedulingSpecified = true,
                        )
                        },
                        modifier = Modifier.testTag("canonical_editor_add_required_context"),
                    ) { Text("Add required context") }

                    OptionalStringConstraintEditor(
                        title = "Required location",
                        value = form.requiredLocation,
                        onValueChange = {
                            form = form.copy(requiredLocation = it, schedulingSpecified = true)
                        },
                    )
                    DependencyListEditor(
                        values = form.dependencies,
                        context = dependencyContext,
                        ownerDraft = currentDraft.getOrNull(),
                        onValuesChange = {
                            form = form.copy(dependencies = it, schedulingSpecified = true)
                        },
                    )
                    OptionalMinutesConstraintEditor(
                        title = "Maximum daily work",
                        value = form.maximumDailyWork,
                        onValueChange = {
                            form = form.copy(maximumDailyWork = it, schedulingSpecified = true)
                        },
                    )
                    OptionalMinutesConstraintEditor(
                        title = "Maximum weekly work",
                        value = form.maximumWeeklyWork,
                        onValueChange = {
                            form = form.copy(maximumWeeklyWork = it, schedulingSpecified = true)
                        },
                    )
                    LabeledSwitch(
                        title = "Preparation / recovery buffers",
                        detail = "Reserve time immediately before or after this work.",
                        checked = form.bufferSpecified,
                        onCheckedChange = { enabled ->
                            form = form.copy(
                                bufferSpecified = enabled,
                                bufferStrength = if (enabled) {
                                    form.bufferStrength ?: CanonicalStrengthForm()
                                } else {
                                    form.bufferStrength
                                },
                                schedulingSpecified = true,
                            )
                        },
                    )
                    if (form.bufferSpecified) {
                        NumberField(form.bufferBeforeMinutes, {
                            form = form.copy(bufferBeforeMinutes = it)
                        }, "Before buffer (minutes, max 527,040)")
                        NumberField(form.bufferAfterMinutes, {
                            form = form.copy(bufferAfterMinutes = it)
                        }, "After buffer (minutes, max 527,040)")
                        Text("Buffer strength", style = MaterialTheme.typography.labelLarge)
                        ChoiceRow {
                            FilterChip(
                                selected = form.bufferStrength == null,
                                onClick = { form = form.copy(bufferStrength = null) },
                                label = { Text("None") },
                            )
                            CanonicalConstraintLevel.entries.forEach { level ->
                                FilterChip(
                                    selected = form.bufferStrength?.level == level,
                                    onClick = {
                                        form = form.copy(
                                            bufferStrength = CanonicalStrengthForm(level),
                                        )
                                    },
                                    label = { Text(level.editorLabel()) },
                                )
                            }
                        }
                        form.bufferStrength?.takeIf {
                            it.level == CanonicalConstraintLevel.SOFT
                        }?.let { strength ->
                            NumberField(
                                value = strength.weight,
                                onValueChange = {
                                    form = form.copy(bufferStrength = strength.copy(weight = it))
                                },
                                label = "Buffer preference weight",
                            )
                        }
                    }
                }

                if (form.kind != ItemKind.EVENT) {
                    EditorSection("Split policy") {
                    LabeledSwitch(
                        title = "Splittable",
                        detail = "Allow the scheduler to compose multiple sessions.",
                        checked = form.isSplittable,
                        onCheckedChange = { form = form.copy(isSplittable = it) },
                        testTag = "canonical_editor_splittable",
                    )
                    if (form.isSplittable) {
                        NumberField(
                            value = form.minimumChunkSeconds,
                            onValueChange = { form = form.copy(minimumChunkSeconds = it) },
                            label = "Minimum chunk (seconds)",
                        )
                        NumberField(
                            value = form.maximumChunkSeconds,
                            onValueChange = { form = form.copy(maximumChunkSeconds = it) },
                            label = "Maximum chunk (seconds)",
                        )
                        NumberField(
                            value = form.maximumSessions,
                            onValueChange = { form = form.copy(maximumSessions = it) },
                            label = "Maximum sessions (optional)",
                        )
                        NumberField(
                            value = form.minimumGapMinutes,
                            onValueChange = { form = form.copy(minimumGapMinutes = it) },
                            label = "Minimum gap (minutes, max 527,040)",
                        )
                        NumberField(
                            value = form.maximumSplitDays,
                            onValueChange = { form = form.copy(maximumSplitDays = it) },
                            label = "Maximum days (optional)",
                            testTag = "canonical_editor_split_maximum_days",
                        )
                    }
                    }
                }

            EditorSection("Hierarchy") {
                ParentPicker(
                    selectedId = form.parentId,
                    options = parentOptions,
                    onSelected = { form = form.copy(parentId = it) },
                )
                NumberField(
                    value = form.siblingOrder,
                    onValueChange = { form = form.copy(siblingOrder = it) },
                    label = "Sibling order",
                )
            }

            (saveError ?: issue)?.let { message ->
                Row(
                    modifier = Modifier.fillMaxWidth().testTag("canonical_editor_diagnostic"),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Icon(
                        Icons.Outlined.ErrorOutline,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.error,
                    )
                    Text(
                        message,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(bottom = 28.dp),
                horizontalArrangement = Arrangement.End,
            ) {
                OutlinedButton(onClick = onDismiss, enabled = !isSaving) { Text("Cancel") }
                Spacer(Modifier.width(10.dp))
                Button(
                    onClick = {
                        val draft = form.draft(route.itemId).getOrNull() ?: return@Button
                        isSaving = true
                        saveError = null
                        coroutineScope.launch {
                            if (!onSave(draft)) {
                                saveError = "The exact change could not be saved. Review pending actions and try again."
                            }
                            isSaving = false
                        }
                    },
                    enabled = issue == null && !isSaving,
                    modifier = Modifier.testTag("canonical_editor_save"),
                ) {
                    Text(
                        when (route.mode) {
                            CanonicalItemEditorMode.CREATE -> if (isSaving) "Saving…" else "Queue item"
                            CanonicalItemEditorMode.REPLACE -> if (isSaving) "Saving…" else "Queue changes"
                            CanonicalItemEditorMode.UPDATE_PENDING ->
                                if (isSaving) "Saving…" else "Update queued change"
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun EditorSection(
    title: String,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(11.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        content()
        HorizontalDivider(modifier = Modifier.padding(top = 5.dp))
    }
}

@Composable
private fun ChoiceRow(content: @Composable RowScope.() -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        content = content,
    )
}

@Composable
private fun ConstraintStrengthEditor(
    value: CanonicalStrengthForm,
    onValueChange: (CanonicalStrengthForm) -> Unit,
) {
    Text("Strength", style = MaterialTheme.typography.labelLarge)
    ChoiceRow {
        CanonicalConstraintLevel.entries.forEach { level ->
            FilterChip(
                selected = value.level == level,
                onClick = { onValueChange(value.copy(level = level)) },
                label = { Text(level.editorLabel()) },
            )
        }
    }
    if (value.level == CanonicalConstraintLevel.SOFT) {
        NumberField(
            value = value.weight,
            onValueChange = { onValueChange(value.copy(weight = it)) },
            label = "Preference weight (0–1,000,000)",
        )
    }
}

@Composable
private fun OptionalInstantConstraintEditor(
    title: String,
    value: CanonicalInstantConstraintForm?,
    onValueChange: (CanonicalInstantConstraintForm?) -> Unit,
) {
    LabeledSwitch(
        title = title,
        detail = "A hard boundary must hold; a soft boundary may be traded off visibly.",
        checked = value != null,
        onCheckedChange = { onValueChange(if (it) CanonicalInstantConstraintForm() else null) },
        testTag = "canonical_editor_${title.testTagSegment()}",
    )
    value?.let { form ->
        InstantField(form.value, { onValueChange(form.copy(value = it)) }, title)
        ConstraintStrengthEditor(form.strength) {
            onValueChange(form.copy(strength = it))
        }
    }
}

@Composable
private fun OptionalMinutesConstraintEditor(
    title: String,
    value: CanonicalMinutesConstraintForm?,
    onValueChange: (CanonicalMinutesConstraintForm?) -> Unit,
) {
    LabeledSwitch(
        title = title,
        detail = "Enable a scheduler-aware minute limit.",
        checked = value != null,
        onCheckedChange = {
            onValueChange(if (it) CanonicalMinutesConstraintForm(value = "30") else null)
        },
        testTag = "canonical_editor_${title.testTagSegment()}",
    )
    value?.let { form ->
        NumberField(form.value, { onValueChange(form.copy(value = it)) }, "$title (minutes)")
        ConstraintStrengthEditor(form.strength) {
            onValueChange(form.copy(strength = it))
        }
    }
}

@Composable
private fun OptionalStringConstraintEditor(
    title: String,
    value: CanonicalStringConstraintForm?,
    onValueChange: (CanonicalStringConstraintForm?) -> Unit,
) {
    LabeledSwitch(
        title = title,
        detail = "Match this against the active availability profile.",
        checked = value != null,
        onCheckedChange = {
            onValueChange(if (it) CanonicalStringConstraintForm() else null)
        },
        testTag = "canonical_editor_${title.testTagSegment()}",
    )
    value?.let { form ->
        OutlinedTextField(
            value = form.value,
            onValueChange = { onValueChange(form.copy(value = it)) },
            label = { Text(title) },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
        )
        ConstraintStrengthEditor(form.strength) {
            onValueChange(form.copy(strength = it))
        }
    }
}

@Composable
private fun DependencyListEditor(
    values: List<CanonicalDependencyForm>,
    context: CanonicalDependencyEditorContext,
    ownerDraft: CanonicalItemDraft?,
    onValuesChange: (List<CanonicalDependencyForm>) -> Unit,
) {
    var isAdding by remember { mutableStateOf(false) }
    var search by remember { mutableStateOf("") }
    Text("Dependencies", style = MaterialTheme.typography.labelLarge)
    Text(
        "Choose everything that must start or finish first. Hard links block scheduling; " +
            "soft links influence the plan by their weight.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    values.forEachIndexed { index, dependency ->
        val option = context.option(dependency.itemId)
        WindowCardLabel("Predecessor ${index + 1}") {
            onValuesChange(values.removing(index))
        }
        Text(
            option?.displayTitle ?: "Unavailable item · ${dependency.itemId.take(8)}",
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.testTag("canonical_editor_dependency_title_$index"),
        )
        Text(
            option?.takeIf(CanonicalDependencyOption::hasOpaqueDependencies)?.let {
                "Dependency details require a newer DayWeave version; keep or remove this link."
            } ?: option?.let { "${canonicalStatusLabel(it.status)} predecessor" }
                ?: "This saved predecessor is unavailable locally; remove it to unlink it.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text("Relationship", style = MaterialTheme.typography.labelLarge)
        ChoiceRow {
            CanonicalDependencyRelation.entries.forEach { relation ->
                FilterChip(
                    selected = dependency.relation == relation,
                    onClick = {
                        onValuesChange(
                            values.replacing(index, dependency.copy(relation = relation)),
                        )
                    },
                    label = { Text(relation.editorLabel()) },
                    modifier = Modifier.testTag(
                        "canonical_editor_dependency_${index}_${relation.wireValue}",
                    ),
                )
            }
        }
        NumberField(
            value = dependency.minimumLagMinutes,
            onValueChange = {
                onValuesChange(
                    values.replacing(index, dependency.copy(minimumLagMinutes = it)),
                )
            },
            label = "Minimum lag (minutes, max 527,040)",
            testTag = "canonical_editor_dependency_lag_$index",
        )
        ConstraintStrengthEditor(dependency.strength) {
            onValuesChange(values.replacing(index, dependency.copy(strength = it)))
        }
        HorizontalDivider(modifier = Modifier.padding(vertical = 3.dp))
    }

    if (isAdding) {
        OutlinedTextField(
            value = search,
            onValueChange = { search = it },
            label = { Text("Search predecessors") },
            placeholder = { Text("Title, status, or item ID") },
            modifier = Modifier.fillMaxWidth().testTag("canonical_editor_dependency_search"),
            singleLine = true,
        )
        val selectedIds = values.mapTo(hashSetOf(), CanonicalDependencyForm::itemId)
        val normalizedSearch = search.trim().lowercase()
        val matches = context.selectableOptions.asSequence()
            .filter { it.id !in selectedIds }
            .filter { option ->
                normalizedSearch.isEmpty() ||
                    normalizedSearch in option.displayTitle.lowercase() ||
                    normalizedSearch in option.status.lowercase() ||
                    normalizedSearch in option.id.lowercase()
            }
            .take(MAX_DEPENDENCY_SEARCH_RESULTS)
            .toList()
        if (matches.isEmpty()) {
            Text(
                if (context.selectableOptions.all { it.id in selectedIds }) {
                    "Every available predecessor is already linked."
                } else {
                    "No predecessor matches this search."
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        matches.forEach { option ->
            val candidateIssue = if (ownerDraft == null) {
                "Finish correcting the draft first"
            } else {
                context.candidateIssue(ownerDraft, option.id)
            }
            OutlinedButton(
                onClick = {
                    onValuesChange(values + CanonicalDependencyForm(itemId = option.id))
                    search = ""
                    isAdding = false
                },
                enabled = candidateIssue == null,
                modifier = Modifier.fillMaxWidth().testTag(
                    "canonical_editor_dependency_option_${option.id}",
                ),
            ) {
                Column(modifier = Modifier.fillMaxWidth()) {
                    Text(option.displayTitle)
                    Text(
                        candidateIssue ?: canonicalStatusLabel(option.status),
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
        }
        OutlinedButton(onClick = { isAdding = false }) { Text("Close search") }
    } else {
        OutlinedButton(
            onClick = { isAdding = true },
            modifier = Modifier.testTag("canonical_editor_add_dependency"),
        ) { Text("Add predecessor") }
    }

    ownerDraft?.let(context::cycleWarning)?.let { warning ->
        Text(
            warning,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.testTag("canonical_editor_dependency_cycle"),
        )
    }
}

@Composable
private fun AbsoluteWindowListEditor(
    title: String,
    values: List<CanonicalAbsoluteWindowForm>,
    onValuesChange: (List<CanonicalAbsoluteWindowForm>) -> Unit,
) {
    Text(title, style = MaterialTheme.typography.labelLarge)
    values.forEachIndexed { index, window ->
        WindowCardLabel("Window ${index + 1}") {
            onValuesChange(values.removing(index))
        }
        InstantField(window.startsAt, {
            onValuesChange(values.replacing(index, window.copy(startsAt = it)))
        }, "Window start")
        InstantField(window.endsAt, {
            onValuesChange(values.replacing(index, window.copy(endsAt = it)))
        }, "Window end")
        ConstraintStrengthEditor(window.strength) {
            onValuesChange(values.replacing(index, window.copy(strength = it)))
        }
    }
    OutlinedButton(onClick = { onValuesChange(values + CanonicalAbsoluteWindowForm()) }) {
        Text("Add window")
    }
}

@Composable
private fun WindowCardLabel(title: String, onRemove: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = MaterialTheme.typography.titleSmall)
        OutlinedButton(onClick = onRemove) { Text("Remove") }
    }
}

@Composable
private fun LabeledSwitch(
    title: String,
    detail: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    testTag: String? = null,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            modifier = testTag?.let { Modifier.testTag(it) } ?: Modifier,
        )
    }
}

@Composable
private fun NumberField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    testTag: String? = null,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        modifier = Modifier.fillMaxWidth().then(
            testTag?.let { Modifier.testTag(it) } ?: Modifier,
        ),
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
    )
}

@Composable
private fun InstantField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        placeholder = { Text("2026-08-30T10:00:00Z") },
        modifier = Modifier.fillMaxWidth(),
        singleLine = true,
    )
}

@Composable
private fun PrioritySlider(
    label: String,
    value: Int,
    onValueChange: (Int) -> Unit,
) {
    Column {
        Text("$label · $value", style = MaterialTheme.typography.labelLarge)
        Slider(
            value = value.toFloat(),
            onValueChange = { onValueChange(it.roundToInt()) },
            valueRange = 0f..100f,
            steps = 99,
        )
    }
}

@Composable
private fun ParentPicker(
    selectedId: String?,
    options: List<CanonicalParentOption>,
    onSelected: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val title = options.firstOrNull { it.id == selectedId }?.title ?: "No parent"
    Box {
        OutlinedButton(onClick = { expanded = true }) { Text("Parent · $title") }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text("No parent") },
                onClick = {
                    onSelected(null)
                    expanded = false
                },
            )
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option.title) },
                    onClick = {
                        onSelected(option.id)
                        expanded = false
                    },
                )
            }
        }
    }
}

internal fun newCanonicalDetailedDraft(
    title: String = "",
    kind: ItemKind = ItemKind.TASK,
    isSensitive: Boolean = false,
): CanonicalItemDraft = CanonicalItemDraft(
    placement = CanonicalDraftPlacement.INBOX,
    kind = kind,
    isSensitive = isSensitive,
    title = title.trim(),
    timezoneName = canonicalDeviceTimezoneName(),
    recurrence = if (kind == ItemKind.HABIT) {
        CanonicalRecurrenceDraft(
            kind = CanonicalRecurrenceKind.DAILY,
            occurrencesPerPeriod = 1,
        )
    } else {
        null
    },
)

internal fun canonicalParentOptions(
    state: DayWeaveUiState,
    excludingItemId: String,
): List<CanonicalParentOption> {
    data class ParentNode(val id: String, val title: String, val parentId: String?)

    val nodes = state.canonicalItems
        .filter { it.deletedAt == null && it.status in setOf("inbox", "planned") }
        .associate { it.id to ParentNode(it.id, it.title, it.parentId) }
        .toMutableMap()
    state.pendingCanonicalAuthoringMutations.forEach { mutation ->
        when (mutation.operation) {
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.TRASH ->
                nodes.remove(mutation.itemId)
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.CREATE,
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.REPLACE,
            -> mutation.draft?.let { draft ->
                nodes[mutation.itemId] = ParentNode(mutation.itemId, draft.title, draft.parentId)
            }
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.RESTORE -> Unit
        }
    }
    val excluded = mutableSetOf(excludingItemId)
    var changed = true
    while (changed) {
        changed = false
        nodes.values.forEach { node ->
            if (node.parentId in excluded && excluded.add(node.id)) changed = true
        }
    }
    return nodes.values
        .filter { it.id !in excluded }
        .sortedWith(compareBy({ it.title.lowercase() }, { it.id }))
        .map { CanonicalParentOption(it.id, it.title) }
}

internal fun canonicalDependencyEditorContext(
    state: DayWeaveUiState,
    editedItemId: String,
): CanonicalDependencyEditorContext {
    val nodes = state.canonicalItems
        .associate { item ->
            val decodedConstraints = runCatching(item::decodeCanonicalFlexibleConstraints)
            item.id to CanonicalDependencyGraphNode(
                id = item.id,
                title = item.title,
                status = item.status,
                kind = ItemKind.entries.firstOrNull {
                    it.name.equals(item.kind, ignoreCase = true)
                } ?: ItemKind.TASK,
                hasRecurrence = item.recurrenceJson != null,
                parentId = item.parentId,
                siblingOrder = item.siblingOrder,
                constraints = decodedConstraints.getOrNull(),
                isDeleted = item.deletedAt != null,
                hasOpaqueDependencies = decodedConstraints.isFailure,
            )
        }
        .toMutableMap()
    state.canonicalRecentlyDeleted.forEach { record ->
        val item = record.lastKnownItem ?: return@forEach
        if (item.id !in nodes) {
            val decodedConstraints = runCatching(item::decodeCanonicalFlexibleConstraints)
            nodes[item.id] = CanonicalDependencyGraphNode(
                id = item.id,
                title = item.title,
                status = item.status,
                kind = ItemKind.entries.firstOrNull {
                    it.name.equals(item.kind, ignoreCase = true)
                } ?: ItemKind.TASK,
                hasRecurrence = item.recurrenceJson != null,
                parentId = item.parentId,
                siblingOrder = item.siblingOrder,
                constraints = decodedConstraints.getOrNull(),
                isDeleted = true,
                hasOpaqueDependencies = decodedConstraints.isFailure,
            )
        }
    }
    state.pendingCanonicalAuthoringMutations.forEach { mutation ->
        when (mutation.operation) {
            CanonicalAuthoringOperation.TRASH -> nodes[mutation.itemId]?.let { existing ->
                nodes[mutation.itemId] = existing.copy(isDeleted = true)
            }
            CanonicalAuthoringOperation.CREATE,
            CanonicalAuthoringOperation.REPLACE,
            -> mutation.draft?.let { draft ->
                nodes[mutation.itemId] = CanonicalDependencyGraphNode(
                    id = mutation.itemId,
                    title = draft.title,
                    status = draft.placement.wireValue,
                    kind = draft.kind,
                    hasRecurrence = draft.recurrence != null,
                    parentId = draft.parentId,
                    siblingOrder = draft.siblingOrder,
                    constraints = draft.constraints,
                    isDeleted = false,
                    hasOpaqueDependencies = false,
                )
            }
            CanonicalAuthoringOperation.RESTORE -> {
                val existing = nodes[mutation.itemId]
                    ?: mutation.baseItem?.let { item ->
                        val decodedConstraints = runCatching(
                            item::decodeCanonicalFlexibleConstraints,
                        )
                        CanonicalDependencyGraphNode(
                            id = item.id,
                            title = item.title,
                            status = item.status,
                            kind = ItemKind.entries.firstOrNull {
                                it.name.equals(item.kind, ignoreCase = true)
                            } ?: ItemKind.TASK,
                            hasRecurrence = item.recurrenceJson != null,
                            parentId = item.parentId,
                            siblingOrder = item.siblingOrder,
                            constraints = decodedConstraints.getOrNull(),
                            isDeleted = true,
                            hasOpaqueDependencies = decodedConstraints.isFailure,
                        )
                    }
                if (existing != null) {
                    nodes[mutation.itemId] = existing.copy(isDeleted = false)
                }
            }
        }
    }

    val options = nodes.values.asSequence()
        .filter { !it.isDeleted && it.id != editedItemId }
        .map { node ->
            val isSensitive = runCatching {
                effectiveCanonicalSensitivity(
                    items = state.canonicalItems,
                    itemId = node.id,
                    pendingMutation = state.pendingCanonicalMutation,
                    pendingAuthoringMutations = state.pendingCanonicalAuthoringMutations,
                )
            }.getOrDefault(true)
            CanonicalDependencyOption(
                id = node.id,
                displayTitle = if (isSensitive) {
                    "Sensitive item · ${node.id.take(8)}"
                } else {
                    node.title.trim().ifEmpty { "Untitled item" }
                },
                status = node.status,
                isSensitive = isSensitive,
                hasOpaqueDependencies = node.hasOpaqueDependencies,
            )
        }
        .sortedWith(compareBy({ it.displayTitle.lowercase() }, CanonicalDependencyOption::id))
        .toList()
    return CanonicalDependencyEditorContext(
        editedItemId = editedItemId,
        options = options,
        baseNodes = nodes,
    )
}

private fun String.required(label: String): String = trim().takeIf(String::isNotEmpty)
    ?: throw IllegalArgumentException("$label is required")

private fun String.requiredMetadataText(label: String): String = takeIf(String::isNotBlank)
    ?: throw IllegalArgumentException("$label is required")

private fun String.requiredLong(label: String): Long = required(label).toLongOrNull()
    ?: throw IllegalArgumentException("$label must be a whole number")

private fun String.requiredInt(label: String): Int = required(label).toIntOrNull()
    ?: throw IllegalArgumentException("$label must be a whole number")

private fun String.optionalInt(label: String): Int? = trim().takeIf(String::isNotEmpty)?.let {
    it.toIntOrNull() ?: throw IllegalArgumentException("$label must be a whole number")
}

private fun String.optionalLong(label: String): Long? = trim().takeIf(String::isNotEmpty)?.let {
    it.toLongOrNull() ?: throw IllegalArgumentException("$label must be a whole number")
}

private fun String.optional(label: String): String? = trim().takeIf(String::isNotEmpty)?.also {
    requireCanonicalInstant(it, label)
}

private fun <T> List<T>.replacing(index: Int, value: T): List<T> =
    toMutableList().also { it[index] = value }

private fun <T> List<T>.removing(index: Int): List<T> =
    toMutableList().also { it.removeAt(index) }

private fun <T> Set<T>.toggle(value: T): Set<T> =
    if (value in this) this - value else this + value

private fun String.testTagSegment(): String = lowercase().replace(Regex("[^a-z0-9]+"), "_")
    .trim('_')

private fun CanonicalConstraintLevel.editorLabel(): String = when (this) {
    CanonicalConstraintLevel.HARD -> "Hard"
    CanonicalConstraintLevel.SOFT -> "Soft"
}

private fun CanonicalDependencyRelation.editorLabel(): String = when (this) {
    CanonicalDependencyRelation.FINISH_TO_START -> "FS · Finish → start"
    CanonicalDependencyRelation.START_TO_START -> "SS · Start → start"
    CanonicalDependencyRelation.FINISH_TO_FINISH -> "FF · Finish → finish"
    CanonicalDependencyRelation.START_TO_FINISH -> "SF · Start → finish"
}

private const val MAX_DEPENDENCY_SEARCH_RESULTS = 24

private fun CanonicalRecurrenceKind.editorLabel(): String = when (this) {
    CanonicalRecurrenceKind.DAILY -> "Daily"
    CanonicalRecurrenceKind.WEEKLY -> "Weekly"
    CanonicalRecurrenceKind.MONTHLY -> "Monthly"
    CanonicalRecurrenceKind.EVERY_INTERVAL -> "Every interval"
    CanonicalRecurrenceKind.AFTER_COMPLETION -> "After completion"
    CanonicalRecurrenceKind.FREQUENCY -> "Flexible frequency"
    CanonicalRecurrenceKind.CUSTOM -> "Custom RRULE"
}
