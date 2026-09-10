package com.greengolddog.dayweave.model

import java.io.OutputStream
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneOffset
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.json.*

const val MAX_ROUTINE_OCCURRENCE_MEMBERS = 10_000
const val MAX_ROUTINE_OCCURRENCE_BYTES = 8 * 1024 * 1024
internal const val MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES = 16_384

/** All occurrence data is protected; the wire has no subtree privacy witness. */
@Serializable
data class RoutineOccurrenceMemberDefinition(
    @SerialName("item_id") val itemId: String,
    @SerialName("parent_id") val parentId: String?,
    @SerialName("source_revision") val sourceRevision: Long,
    val title: String,
    val kind: String,
    val recurs: Boolean,
    @SerialName("sibling_order") val siblingOrder: Int,
    @SerialName("required_for_parent") val requiredForParent: Boolean,
    @SerialName("initial_open") val initialOpen: ItemCompletionReopening,
) {
    fun requireValid() {
        requireCanonicalUuid(itemId, "occurrence member")
        parentId?.let { requireCanonicalUuid(it, "occurrence parent"); require(it != itemId) }
        require(sourceRevision > 0 && siblingOrder in 0..1_000_000)
        requireProgressText(title, 500)
        require(kind in setOf("task", "event", "habit", "goal", "project", "routine", "break"))
        require(kind != "habit" || recurs)
        initialOpen.requireValid(itemId)
    }
}

@Serializable
data class RoutineOccurrenceManifest(
    @SerialName("schema_version") val schemaVersion: Int,
    /** HTTP route identity; deliberately different from the planner occurrence UUID. */
    val id: String,
    @SerialName("series_item_id") val seriesItemId: String,
    @SerialName("occurrence_id") val occurrenceId: String,
    val identity: JsonObject,
    @SerialName("nominal_start") val nominalStart: String,
    @SerialName("nominal_end") val nominalEnd: String,
    @SerialName("window_start") val windowStart: String,
    @SerialName("window_end") val windowEnd: String,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("definition_hash") val definitionHash: String,
    val members: List<RoutineOccurrenceMemberDefinition>,
) {
    fun requireValid() { validatedTopology(); requireRoutineEncodedSize(serializer(), this) }

    internal fun validatedTopology(): RoutineOccurrenceTopology {
        require(schemaVersion == 1)
        requireCanonicalUuid(id, "occurrence instance")
        requireCanonicalUuid(seriesItemId, "occurrence series")
        requireCanonicalUuid(occurrenceId, "planner occurrence")
        require(UUID.fromString(occurrenceId).isRfc4122Version5() && id != occurrenceId)
        requireCompletionHash(definitionHash)
        val start = requireCompletionInstant(nominalStart)
        val end = requireCompletionInstant(nominalEnd)
        require(start < end && requireCompletionInstant(windowStart) < requireCompletionInstant(windowEnd))
        val timezone = requireCanonicalTimezoneName(timezoneName)
        requireRoutineIdentity(identity, start.atZone(timezone).toLocalDate(), end.minusNanos(1_000).atZone(timezone).toLocalDate())
        require(members.size in 1..MAX_ROUTINE_OCCURRENCE_MEMBERS)
        val definitions = members.associateBy { it.itemId }
        require(definitions.size == members.size)
        members.forEach(RoutineOccurrenceMemberDefinition::requireValid)
        val root = requireNotNull(definitions[seriesItemId])
        require(root.kind in setOf("task", "routine") && root.recurs && root.parentId == null)
        val children = HashMap<String, MutableList<String>>()
        members.forEach { member ->
            if (member.itemId != seriesItemId) require(member.parentId != null)
            member.parentId?.let { parent -> require(parent in definitions); children.getOrPut(parent) { mutableListOf() }.add(member.itemId) }
        }
        val order = ArrayList<String>(members.size)
        val unqualified = HashSet<String>()
        val pending = ArrayDeque<Pair<String, Boolean>>()
        pending.addLast(seriesItemId to false)
        val seen = HashSet<String>()
        while (pending.isNotEmpty()) {
            val (current, inherited) = pending.removeLast()
            require(seen.add(current))
            val nested = inherited || current != seriesItemId && definitions.getValue(current).recurs
            if (nested) unqualified.add(current)
            order.add(current)
            children[current]?.forEach { pending.addLast(it to nested) }
        }
        require(order.size == members.size) // Also rejects disconnected cycles.
        return RoutineOccurrenceTopology(definitions, children, order, unqualified)
    }
}

internal data class RoutineOccurrenceTopology(
    val definitions: Map<String, RoutineOccurrenceMemberDefinition>,
    val children: Map<String, List<String>>,
    val preorder: List<String>,
    val unqualified: Set<String>,
)

@Serializable
data class RoutineOccurrenceMemberState(
    @SerialName("item_id") val itemId: String,
    val revision: Long,
    val status: String,
    @SerialName("required_for_parent") val requiredForParent: Boolean,
    val mode: ItemCompletionMode,
    val open: ItemCompletionReopening,
    val provenance: ItemCompletionProvenance?,
    @SerialName("completed_at") val completedAt: String?,
    @SerialName("updated_at") val updatedAt: String,
) {
    internal fun requireValid(instanceRevision: Long, isParent: Boolean) {
        require(revision in 1..instanceRevision)
        require(status in ROUTINE_STATUSES)
        ItemCompletionPolicyState(itemId, revision, requiredForParent, mode, provenance, updatedAt).requireValid()
        open.requireValid(itemId)
        require(status !in ROUTINE_OPEN_STATUSES || status == open.status)
        require((status == "completed") == (completedAt != null))
        completedAt?.let(::requireCompletionInstant)
        if (!isParent) require(mode == ItemCompletionMode.AUTOMATIC && provenance == null)
        if (isParent && status !in ROUTINE_OPEN_STATUSES) require(provenance != null)
        provenance?.let { require(status == "completed" && it.reopen == open) }
    }
}

@Serializable
data class RoutineOccurrenceAggregate(
    val manifest: RoutineOccurrenceManifest,
    val revision: Long,
    val members: List<RoutineOccurrenceMemberState>,
) {
    fun requireValid() { validatedCounts(); requireRoutineEncodedSize(serializer(), this) }

    internal fun validatedCounts(): Pair<RoutineOccurrenceTopology, Map<String, ItemCompletionCounts>> {
        val topology = manifest.validatedTopology()
        require(revision > 0 && members.size == topology.definitions.size)
        val states = members.associateBy { it.itemId }
        require(states.size == members.size && states.keys == topology.definitions.keys)
        members.forEach { it.requireValid(revision, topology.children[it.itemId].orEmpty().isNotEmpty()) }
        val counts = HashMap<String, ItemCompletionCounts>()
        for (id in topology.preorder.asReversed()) {
            val state = states.getValue(id)
            var total = 0L; var complete = 0L; var incomplete = 0L; var unknown = 0L
            topology.children[id].orEmpty().forEach { childId ->
                val child = states.getValue(childId)
                if (child.requiredForParent) {
                    val branch = counts.getValue(childId)
                    total = Math.addExact(total, Math.addExact(branch.requiredDescendants, 1))
                    complete = Math.addExact(complete, branch.completed)
                    incomplete = Math.addExact(incomplete, branch.incomplete)
                    unknown = Math.addExact(unknown, branch.occurrenceEvidenceRequired)
                    when { childId in topology.unqualified -> unknown++
                        child.status == "completed" -> complete++
                        else -> incomplete++ }
                }
            }
            val count = ItemCompletionCounts(total, complete, incomplete, unknown)
            count.requireValid(); require(total < members.size)
            counts[id] = count
            if (id !in topology.unqualified) when (state.mode) {
                ItemCompletionMode.COMPLETE -> require(state.status == "completed" && state.provenance?.kind == ItemCompletionProvenanceKind.MANUAL)
                ItemCompletionMode.KEEP_OPEN -> require(state.status in ROUTINE_OPEN_STATUSES && state.provenance == null)
                ItemCompletionMode.AUTOMATIC -> if (topology.children[id].orEmpty().isNotEmpty()) {
                    if (total > 0 && incomplete == 0L && unknown == 0L) {
                        require(state.status == "completed" && state.provenance?.kind == ItemCompletionProvenanceKind.AUTOMATIC)
                    } else require(state.status in ROUTINE_OPEN_STATUSES && state.provenance == null)
                }
            }
        }
        return topology to counts
    }
}

@Serializable
enum class RoutineOccurrenceReason {
    @SerialName("unchanged") UNCHANGED,
    @SerialName("outcome_recorded") OUTCOME_RECORDED,
    @SerialName("reopened") REOPENED,
    @SerialName("policy_reviewed") POLICY_REVIEWED,
    @SerialName("occurrence_evidence_required") OCCURRENCE_EVIDENCE_REQUIRED,
    @SerialName("automatically_completed") AUTOMATICALLY_COMPLETED,
    @SerialName("automatically_reopened") AUTOMATICALLY_REOPENED,
    @SerialName("manually_completed") MANUALLY_COMPLETED,
    @SerialName("manually_kept_open") MANUALLY_KEPT_OPEN,
    @SerialName("manual_completion_released") MANUAL_COMPLETION_RELEASED,
}

@Serializable
data class RoutineOccurrenceMemberEvaluation(
    @SerialName("item_id") val itemId: String,
    val counts: ItemCompletionCounts,
    @SerialName("occurrence_evidence_required") val occurrenceEvidenceRequired: Boolean,
    val reason: RoutineOccurrenceReason,
)

@Serializable
data class RoutineOccurrenceSnapshot(
    @SerialName("schema_version") val schemaVersion: Int,
    val aggregate: RoutineOccurrenceAggregate,
    @SerialName("evidence_hash") val evidenceHash: String,
    @SerialName("fresh_edit_eligible") val freshEditEligible: Boolean,
    val members: List<RoutineOccurrenceMemberEvaluation>,
) {
    fun requireValid() {
        requireStructure()
        requireRoutineEncodedSize(serializer(), this)
    }

    internal fun requireStructure() {
        require(schemaVersion == 1)
        requireCompletionHash(evidenceHash)
        val (topology, expected) = aggregate.validatedCounts()
        require(members.size == expected.size && members.map { it.itemId }.toSet() == expected.keys)
        members.forEach {
            it.counts.requireValid()
            require(it.counts == expected.getValue(it.itemId))
            require(it.occurrenceEvidenceRequired == (it.itemId in topology.unqualified))
        }
    }
}

@Serializable
sealed class RoutineOccurrenceAction {
    @Serializable @SerialName("set_outcome")
    data class SetOutcome(val status: String) : RoutineOccurrenceAction()
    @Serializable @SerialName("reopen")
    data class Reopen(val open: ItemCompletionReopening) : RoutineOccurrenceAction()
    @Serializable @SerialName("set_policy")
    data class SetPolicy(@SerialName("required_for_parent") val requiredForParent: Boolean, val mode: ItemCompletionMode) : RoutineOccurrenceAction()
}

@Serializable
data class RoutineOccurrenceRequest(
    @SerialName("schema_version") val schemaVersion: Int = 1,
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_instance_revision") val expectedInstanceRevision: Long,
    @SerialName("expected_member_revision") val expectedMemberRevision: Long,
    @SerialName("expected_evidence_hash") val expectedEvidenceHash: String,
    val action: RoutineOccurrenceAction,
) {
    fun requireValid(memberId: String) {
        requireCanonicalUuid(memberId, "occurrence target")
        requireCanonicalUuid(operationId, "occurrence operation")
        require(schemaVersion == 1 && expectedInstanceRevision > 0 && expectedMemberRevision > 0)
        requireCompletionHash(expectedEvidenceHash)
        when (action) {
            is RoutineOccurrenceAction.SetOutcome -> require(action.status in setOf("completed", "skipped"))
            is RoutineOccurrenceAction.Reopen -> action.open.requireValid(memberId)
            is RoutineOccurrenceAction.SetPolicy -> Unit
        }
    }
}

@Serializable
data class RoutineOccurrenceMutationResult(
    @SerialName("operation_id") val operationId: String,
    val replayed: Boolean,
    val occurrence: RoutineOccurrenceSnapshot,
) {
    fun requireValid() {
        requireCanonicalUuid(operationId, "occurrence operation")
        occurrence.requireStructure()
        require(occurrence.aggregate.revision >= 2 && occurrence.freshEditEligible)
        requireRoutineEncodedSize(serializer(), this)
    }

    fun requireMatches(instanceId: String, memberId: String, request: RoutineOccurrenceRequest) {
        requireValid(); request.requireValid(memberId)
        require(operationId == request.operationId && occurrence.aggregate.manifest.id == instanceId)
        require(occurrence.aggregate.revision == Math.addExact(request.expectedInstanceRevision, 1))
        val member = occurrence.aggregate.members.single { it.itemId == memberId }
        require(member.revision == Math.addExact(request.expectedMemberRevision, 1))
        val evaluation = occurrence.members.single { it.itemId == memberId }
        require(!evaluation.occurrenceEvidenceRequired)
        val isParent = occurrence.aggregate.manifest.members.any { it.parentId == memberId }
        when (val action = request.action) {
            is RoutineOccurrenceAction.SetOutcome -> require(!isParent && member.status == action.status && evaluation.reason == RoutineOccurrenceReason.OUTCOME_RECORDED)
            is RoutineOccurrenceAction.Reopen -> require(!isParent && member.open == action.open && member.status == action.open.status && evaluation.reason == RoutineOccurrenceReason.REOPENED)
            is RoutineOccurrenceAction.SetPolicy -> require(member.requiredForParent == action.requiredForParent && member.mode == action.mode)
        }
    }
}

@Serializable
data class RoutineOccurrenceChange(val sequence: Long, val occurrence: RoutineOccurrenceSnapshot)

@Serializable
data class RoutineOccurrencePage(
    @SerialName("schema_version") val schemaVersion: Int,
    val changes: List<RoutineOccurrenceChange>,
    val cursor: String,
    @SerialName("has_more") val hasMore: Boolean,
) {
    fun requireValid(isCurrentList: Boolean = false, requestedLimit: Int = 100) {
        require(schemaVersion == 1 && requestedLimit in 1..100 && changes.size <= requestedLimit)
        requireRoutineCursor(cursor)
        require(!hasMore || changes.isNotEmpty())
        var previous = 0L
        val previousInstances = HashMap<String, RoutineOccurrenceAggregate>()
        changes.forEach { change ->
            require(change.sequence > previous); previous = change.sequence
            change.occurrence.requireStructure()
            val id = change.occurrence.aggregate.manifest.id
            val old = previousInstances.put(id, change.occurrence.aggregate)
            require(old == null || !isCurrentList && change.occurrence.aggregate.revision > old.revision && change.occurrence.aggregate.manifest == old.manifest)
        }
        requireRoutineEncodedSize(serializer(), this)
    }
}

/** Count compact UTF-8 output without materializing an oversized local page. */
@OptIn(ExperimentalSerializationApi::class)
private fun <T> requireRoutineEncodedSize(serializer: SerializationStrategy<T>, value: T) {
    ITEM_PROGRESS_JSON.encodeToStream(serializer, value, RoutineOccurrenceSizeCounter())
}

private class RoutineOccurrenceSizeCounter : OutputStream() {
    private var size = 0
    override fun write(value: Int) { add(1) }
    override fun write(bytes: ByteArray, offset: Int, length: Int) { add(length) }
    private fun add(count: Int) {
        require(count >= 0 && size <= MAX_ROUTINE_OCCURRENCE_BYTES - count)
        size += count
    }
}

/** JSON nesting stays bounded; logical tree depth is flat and has no fixed cap. */
internal inline fun <reified T> decodeExactRoutineOccurrence(body: String): T {
    require(body.toByteArray(Charsets.UTF_8).size <= MAX_ROUTINE_OCCURRENCE_BYTES)
    return decodeExactItemProgress(body)
}

internal fun requireRoutineCursor(value: String) {
    // Accepted characters are exactly one UTF-8 byte each; no whitespace or Unicode aliases.
    require(value.length in 1..512 && value.all { it.code in 33..126 })
}

private val ROUTINE_OPEN_STATUSES = setOf("inbox", "planned", "blocked")
private val ROUTINE_STATUSES = ROUTINE_OPEN_STATUSES + setOf("completed", "skipped", "cancelled")

private fun requireRoutineIdentity(value: JsonObject, nominalDate: LocalDate, nominalLastDate: LocalDate) {
    fun text(key: String) = value.getValue(key).jsonPrimitive.also { require(it.isString) }.content
    fun number(key: String) = value.getValue(key).jsonPrimitive.also { require(!it.isString && Regex("(?:0|[1-9][0-9]*|-[1-9][0-9]*)").matches(it.content)) }.content.toLong()
    fun fields(vararg keys: String) { require(value.keys == setOf("type", *keys)) }
    fun calendar() { require(nominalDate == nominalLastDate) }
    fun ordinal() { require(number("bucket_ordinal") in 0..65_534) }
    fun date() { calendar(); require(text("date") == nominalDate.toString()) }
    when (text("type")) {
        "calendar_day" -> { fields("date", "bucket_ordinal"); ordinal(); date() }
        "calendar_week" -> { fields("week_key", "bucket_ordinal"); ordinal(); calendar()
            val week = number("week_key"); require(week in Int.MIN_VALUE.toLong()..Int.MAX_VALUE.toLong())
            val last = Math.addExact(Math.toIntExact(week), 6)
            require(nominalDate.toEpochDay() + 2_440_588 in week..last.toLong()) }
        "calendar_month" -> { fields("year", "month", "bucket_ordinal"); ordinal(); calendar()
            require(number("year") == nominalDate.year.toLong() && number("month") == nominalDate.monthValue.toLong()) }
        "rolling_minutes" -> { fields("index", "anchor"); require(number("index") in 0..4_294_967_295); requireRoutineAnchor(text("anchor")) }
        "after_completion" -> { fields("anchor"); requireRoutineAnchor(text("anchor")) }
        "rolling_month" -> { fields("cycle", "index", "anchor"); require(number("cycle") in 0..Int.MAX_VALUE.toLong() && number("index") in 0..65_534); requireRoutineAnchor(text("anchor")) }
        "custom_rule" -> { fields("rule_id", "sequence", "date"); date(); require(number("sequence") in 0..9_999)
            requireCanonicalUuid(text("rule_id"), "occurrence rule"); require(UUID.fromString(text("rule_id")).isRfc4122Version5()) }
        else -> error("Unsupported occurrence identity")
    }
}

private fun requireRoutineAnchor(value: String): Instant {
    val match = requireNotNull(Regex("([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,6})?)(Z|([+-])([0-9]{2}):([0-9]{2}))").matchEntire(value))
    val local = LocalDateTime.parse(match.groupValues[1]); require(local.year in 1..9_999)
    val offset = if (match.groupValues[2] == "Z") 0 else {
        val hours = match.groupValues[4].toInt(); val minutes = match.groupValues[5].toInt()
        require(hours <= 23 && minutes <= 59)
        (hours * 3_600 + minutes * 60) * if (match.groupValues[3] == "-") -1 else 1
    }
    return local.toInstant(ZoneOffset.UTC).minusSeconds(offset.toLong())
}
