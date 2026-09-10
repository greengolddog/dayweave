package com.greengolddog.dayweave.model

import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*

const val MAX_ROUTINE_PLANNING_WITNESS_BYTES = 16 * 1024 * 1024
internal const val MAX_ROUTINE_PLANNING_SOURCES = 10_000
internal val ROUTINE_PLANNING_JSON = Json {
    encodeDefaults = true
    explicitNulls = true
    ignoreUnknownKeys = false
    classDiscriminator = "status"
}

/** Private fixed-input evidence, never a publication or execution capability. */
@Serializable
data class RoutinePlanningWitnessRequest(
    @SerialName("schema_version") val schemaVersion: Int,
    val schedule: RoutinePlanningSchedule,
    @SerialName("expected_source_item_revisions") val expectedSourceItemRevisions: Map<String, Long>,
    @SerialName("terminal_cursor") val terminalCursor: String,
) {
    fun requireValid() {
        require(schemaVersion == 1)
        requirePlanningSourceMap(expectedSourceItemRevisions)
        requirePlanningCursor(terminalCursor)
        schedule.requireValid()
    }
    override fun toString() = "RoutinePlanningWitnessRequest(<protected>)"
}

@Serializable
enum class RoutinePlanningRemoteReason {
    @SerialName("first_publication_required") FIRST_PUBLICATION_REQUIRED,
    @SerialName("execution_evidence_required") EXECUTION_EVIDENCE_REQUIRED,
    @SerialName("retained_manual_placement_required") RETAINED_MANUAL_PLACEMENT_REQUIRED,
    @SerialName("source_ineligible") SOURCE_INELIGIBLE,
    @SerialName("calendar_projection_incomplete") CALENDAR_PROJECTION_INCOMPLETE,
    @SerialName("composition_unsupported") COMPOSITION_UNSUPPORTED,
}

@Serializable
sealed class RoutinePlanningWitnessResult {
    @Serializable @SerialName("qualified")
    data class Qualified(val witness: RoutinePlanningWitness) : RoutinePlanningWitnessResult() {
        override fun toString() = "RoutinePlanningWitnessResult.Qualified(<protected>)"
    }
    @Serializable @SerialName("remote_required")
    data class RemoteRequired(val reason: RoutinePlanningRemoteReason) : RoutinePlanningWitnessResult()
}

@Serializable
data class RoutinePlanningWitnessResponse(
    @SerialName("schema_version") val schemaVersion: Int,
    val result: RoutinePlanningWitnessResult,
) {
    fun requireValid() {
        require(schemaVersion == 1)
        (result as? RoutinePlanningWitnessResult.Qualified)?.witness?.requireValid()
    }
}

@Serializable
data class RoutinePlanningWitness(
    @SerialName("workspace_id") val workspaceId: String,
    @SerialName("user_id") val userId: String,
    @SerialName("request_fingerprint") val requestFingerprint: String,
    @SerialName("witness_fingerprint") val witnessFingerprint: String,
    @SerialName("local_input_fingerprint") val localInputFingerprint: String,
    @SerialName("calendar_projection_fingerprint") val calendarProjectionFingerprint: String,
    @SerialName("source_item_revisions") val sourceItemRevisions: Map<String, Long>,
    @SerialName("terminal_cursor") val terminalCursor: String,
    val schedule: RoutinePlanningSchedule,
    @SerialName("occurrence_lifecycle") val occurrenceLifecycle: RoutinePlanningLifecycle,
    @SerialName("execution_snapshot_revision") val executionSnapshotRevision: Long,
    @SerialName("habit_change_head") val habitChangeHead: Long,
    @SerialName("published_schedule_revision_id") val publishedScheduleRevisionId: String?,
) {
    fun requireValid() {
        requirePlanningUuid(workspaceId); requirePlanningUuid(userId)
        requirePlanningFingerprint(requestFingerprint, "routine-witness-request-sha256:")
        requirePlanningFingerprint(witnessFingerprint, "routine-witness-capture-sha256:")
        requirePlanningFingerprint(calendarProjectionFingerprint, "routine-witness-calendar-sha256:")
        requirePlanningFingerprint(localInputFingerprint, "local-sha256:")
        requirePlanningSourceMap(sourceItemRevisions)
        requirePlanningCursor(terminalCursor)
        require(executionSnapshotRevision >= 0 && habitChangeHead >= 0)
        publishedScheduleRevisionId?.let(::requirePlanningUuid)
        schedule.requireValid()
        require(schedule.manualPlacements.isEmpty() && schedule.manualPlacementReleases.isEmpty())
        occurrenceLifecycle.requireValid(sourceItemRevisions)
    }

    /** The hash is opaque server evidence. Request ownership comes from the protected HTTP call. */
    fun requireMatches(request: RoutinePlanningWitnessRequest, workspaceId: String, userId: String,
        items: List<CanonicalItemSnapshot>,
    ) {
        requireValid(); request.requireValid()
        require(this.workspaceId == workspaceId && this.userId == userId)
        require(sourceItemRevisions == request.expectedSourceItemRevisions && terminalCursor == request.terminalCursor)
        requireCurrentSources(items)
        schedule.requireNormalizationOf(request.schedule, items, occurrenceLifecycle)
    }

    /** Iterative full-source join includes unscheduled/Inbox descendants, independent of cached GETs. */
    fun requireCurrentSources(items: List<CanonicalItemSnapshot>) {
        require(items.size <= MAX_ROUTINE_PLANNING_SOURCES)
        val sources = items.associateBy { it.id }
        require(sources.size == items.size && sources.mapValues { it.value.revision } == sourceItemRevisions)
        val children = HashMap<String, MutableSet<String>>()
        items.forEach { item ->
            requirePlanningUuid(item.id)
            require(item.revision > 0 && item.deletedAt == null)
            item.parentId?.let { parent ->
                requirePlanningUuid(parent); require(parent in sources && parent != item.id)
                children.getOrPut(parent) { HashSet() }.add(item.id)
            }
        }
        // Validate the entire forest, not only the members in a requested horizon.
        val ready = ArrayDeque(items.filter { it.parentId == null }.map { it.id })
        val visited = HashSet<String>()
        while (ready.isNotEmpty()) {
            val id = ready.removeFirst(); require(visited.add(id))
            children[id]?.forEach(ready::addLast)
        }
        require(visited.size == items.size)
        occurrenceLifecycle.instances.forEach { instance ->
            val root = requireNotNull(sources[instance.rootItemId])
            require(root.kind in setOf("task", "routine") && root.recurrenceJson != null)
            val expected = HashSet<String>()
            ready.addLast(root.id)
            while (ready.isNotEmpty()) {
                val id = ready.removeFirst(); require(expected.add(id))
                children[id]?.forEach(ready::addLast)
            }
            require(instance.members.map { it.itemId }.toSet() == expected)
            instance.members.forEach { member ->
                val item = sources.getValue(member.itemId)
                require(member.sourceRevision == item.revision)
                require(member.parentId == if (member.itemId == root.id) null else item.parentId)
            }
        }
    }
    override fun toString() = "RoutinePlanningWitness(<protected>)"
}

@Serializable
data class RoutinePlanningLifecycle(
    @SerialName("snapshot_revision") val snapshotRevision: Long,
    val instances: List<RoutinePlanningLifecycleInstance>,
) {
    fun requireValid(sources: Map<String, Long>) {
        require(snapshotRevision >= 0 && (snapshotRevision > 0 || instances.isEmpty()))
        require(instances.size <= MAX_ROUTINE_PLANNING_SOURCES)
        require(instances.sumOf { it.members.size.toLong() } <= MAX_ROUTINE_PLANNING_SOURCES)
        val seen = HashSet<String>()
        instances.forEach { instance ->
            requirePlanningUuid(instance.rootItemId); requirePlanningOccurrenceId(instance.occurrenceId)
            require(seen.add(instance.occurrenceId))
            requirePlanningIdentity(instance.identity)
            require(instance.members.isNotEmpty())
            val members = instance.members.associateBy { it.itemId }
            require(members.size == instance.members.size && instance.rootItemId in members)
            val children = HashMap<String, MutableList<String>>()
            instance.members.forEach { member ->
                requirePlanningUuid(member.itemId)
                require(member.sourceRevision > 0 && sources[member.itemId] == member.sourceRevision)
                require(member.status in setOf("not_started", "scheduled", "completed", "skipped", "canceled", "blocked"))
                if (member.itemId == instance.rootItemId) require(member.parentId == null)
                else {
                    val parent = requireNotNull(member.parentId)
                    requirePlanningUuid(parent); require(parent in members && parent != member.itemId)
                    children.getOrPut(parent) { ArrayList() }.add(member.itemId)
                }
            }
            val queue = ArrayDeque<String>(); queue.addLast(instance.rootItemId)
            val visited = HashSet<String>()
            while (queue.isNotEmpty()) {
                val id = queue.removeFirst(); require(visited.add(id))
                children[id]?.forEach(queue::addLast)
            }
            require(visited.size == members.size)
        }
    }
    override fun toString() = "RoutinePlanningLifecycle(<protected>)"
}

@Serializable
data class RoutinePlanningLifecycleInstance(
    @SerialName("root_item_id") val rootItemId: String,
    @SerialName("occurrence_id") val occurrenceId: String,
    val identity: JsonObject,
    val members: List<RoutinePlanningLifecycleMember>,
)

@Serializable
data class RoutinePlanningLifecycleMember(
    @SerialName("item_id") val itemId: String,
    @SerialName("parent_id") val parentId: String?,
    @SerialName("source_revision") val sourceRevision: Long,
    val status: String,
)

/** Fixed diagnostics intentionally discard private parser/serializer cause chains. */
class RoutinePlanningWitnessProtocolException : IllegalArgumentException("Planning evidence could not be verified")

internal inline fun <reified T> decodeExactRoutinePlanningWitness(body: String): T = try {
    requirePlanningJson(body)
    val raw = ROUTINE_PLANNING_JSON.parseToJsonElement(body)
    val decoded = ROUTINE_PLANNING_JSON.decodeFromJsonElement<T>(raw)
    require(ROUTINE_PLANNING_JSON.encodeToJsonElement(decoded) == raw)
    decoded
} catch (_: Exception) { throw RoutinePlanningWitnessProtocolException() }

internal fun requirePlanningJson(body: String) {
    require(body.toByteArray(StandardCharsets.UTF_8).size in 1..MAX_ROUTINE_PLANNING_WITNESS_BYTES)
    requireStrictItemProgressJson(body, integersOnly = true, maxDepth = 128)
    val pending = ArrayDeque<JsonElement>(); pending.addLast(ROUTINE_PLANNING_JSON.parseToJsonElement(body))
    var count = 0
    while (pending.isNotEmpty()) {
        require(++count <= 1_000_000)
        when (val value = pending.removeLast()) {
            is JsonObject -> { value.keys.forEach(::requirePlanningUnicode); value.values.forEach(pending::addLast) }
            is JsonArray -> value.forEach(pending::addLast)
            is JsonPrimitive -> if (value.isString) requirePlanningUnicode(value.content)
        }
    }
}

internal fun decodePlanningUtf8(bytes: ByteArray): String = StandardCharsets.UTF_8.newDecoder()
    .onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
    .decode(ByteBuffer.wrap(bytes)).toString()

internal fun requirePlanningUnicode(value: String) {
    require('\u0000' !in value)
    var index = 0
    while (index < value.length) {
        val char = value[index++]
        if (char.isHighSurrogate()) require(index < value.length && value[index++].isLowSurrogate())
        else require(!char.isLowSurrogate())
    }
}

internal fun requirePlanningUuid(value: String) { requireCanonicalUuid(value, "planning identity") }
internal fun requirePlanningOccurrenceId(value: String) {
    requirePlanningUuid(value)
    val id = UUID.fromString(value); require(id.version() == 5 && id.variant() == 2)
}
internal fun requirePlanningSourceMap(values: Map<String, Long>) {
    require(values.size <= MAX_ROUTINE_PLANNING_SOURCES)
    values.forEach { (id, revision) -> requirePlanningUuid(id); require(revision > 0) }
}
internal fun requirePlanningCursor(value: String) { require(value.length in 1..4096 && value.all { it.code in 0..127 }) }
internal fun requirePlanningFingerprint(value: String, prefix: String) {
    require(value.startsWith(prefix) && value.length == prefix.length + 64 && value.drop(prefix.length).all { it in '0'..'9' || it in 'a'..'f' })
}
