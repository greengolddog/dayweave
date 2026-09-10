package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalBlockedReasonKind
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.RoutinePlanningWitness
import com.greengolddog.dayweave.model.RoutinePlanningSchedule
import com.greengolddog.dayweave.model.RoutinePlanningLifecycle
import com.greengolddog.dayweave.model.decodePlanningUtf8
import com.greengolddog.dayweave.model.requirePlanningJson
import com.greengolddog.dayweave.model.requirePlanningInstant
import com.greengolddog.dayweave.model.requirePlanningUuid
import com.greengolddog.dayweave.model.requirePlanningOccurrenceId
import com.greengolddog.dayweave.model.requirePlanningIdentity
import com.greengolddog.dayweave.model.requireValidStructuralMetadata
import com.greengolddog.dayweave.network.RemoteIgnoredPreviousAssignment
import com.greengolddog.dayweave.network.RemotePlanDecision
import com.greengolddog.dayweave.network.RemotePlanOccurrence
import com.greengolddog.dayweave.network.RemotePlanScore
import com.greengolddog.dayweave.network.RemotePlanViolation
import com.greengolddog.dayweave.network.RemoteRejectedScheduleItem
import com.greengolddog.dayweave.network.RemoteScheduleBlock
import com.greengolddog.dayweave.network.RemoteSchedulePlan
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.RemoteUnscheduledWork
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import java.io.ByteArrayOutputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.encodeToStream
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

data class LocalScheduleComposition(
    val localInputFingerprint: String,
    val scheduleRequestFingerprint: String = "",
    val sourceItemCount: Int,
    val sourceItemRevisions: Map<String, Long>,
    val acceptedItemCount: Int,
    val rejectedItems: List<RemoteRejectedScheduleItem>,
    val ignoredPreviousAssignments: List<RemoteIgnoredPreviousAssignment>,
    val plan: RemoteSchedulePlan,
) {
    fun asRemotePreview(): RemoteSchedulePreview = RemoteSchedulePreview(
        inputDigest = localInputFingerprint,
        sourceItemCount = sourceItemCount,
        sourceItemRevisions = sourceItemRevisions,
        acceptedItemCount = acceptedItemCount,
        rejectedItems = rejectedItems,
        ignoredPreviousAssignments = ignoredPreviousAssignments,
        plan = plan,
    )
}

fun interface LocalScheduleComposer {
    suspend fun compose(
        items: List<CanonicalItemSnapshot>,
        request: SchedulePreviewRequest,
    ): LocalScheduleComposition
}

/** Deliberately distinct from v1: a positive occurrence head cannot disappear in a generic result. */
data class RoutineOccurrenceLocalComposition(
    val composition: LocalScheduleComposition,
    val occurrenceSnapshotRevision: Long,
) {
    override fun toString() = "RoutineOccurrenceLocalComposition(<protected>)"
}

fun interface RoutineLifecycleScheduleComposer {
    suspend fun compose(items: List<CanonicalItemSnapshot>, witness: RoutinePlanningWitness): RoutineOccurrenceLocalComposition
}

class LocalScheduleCompositionProtocolException :
    IllegalStateException("Bundled scheduler returned an invalid response")

class LocalScheduleCompositionRejectedException(
    val code: String,
) : IllegalStateException("Bundled scheduler rejected the composition request")

class LocalScheduleCompositionRequestTooLargeException :
    IllegalStateException("Bundled scheduler request exceeds the fixed local limit")

class LocalScheduleCompositionRequestException :
    IllegalStateException("Bundled scheduler request is invalid")

/** The only Kotlin declaration whose ABI is consumed by the pinned Rust JNI bridge. */
internal object RustSchedulerNative {
    init {
        System.loadLibrary("dayweave_android_ffi")
    }

    external fun process(request: ByteArray): ByteArray?
}

fun interface RustSchedulerByteArrayBridge {
    fun process(request: ByteArray): ByteArray?
}

/** Bounded, content-private JNI adapter around the existing scheduler-helper protocol. */
class RustScheduleComposer(
    private val bridge: RustSchedulerByteArrayBridge =
        RustSchedulerByteArrayBridge(RustSchedulerNative::process),
    private val beforeBridge: suspend () -> Unit = {},
) : LocalScheduleComposer, RoutineLifecycleScheduleComposer {
    override suspend fun compose(
        items: List<CanonicalItemSnapshot>,
        request: SchedulePreviewRequest,
    ): LocalScheduleComposition = withContext(Dispatchers.Default) {
        ensureActive()
        val requestBytes = encodeRequest(items, request)
        beforeBridge()
        ensureActive()
        val response = bridge.process(requestBytes)
            ?: throw LocalScheduleCompositionProtocolException()
        ensureActive()
        decodeResponse(response).copy(
            scheduleRequestFingerprint = requestBytes.sha256Fingerprint(),
        )
    }

    /** This explicit entry point never retries with helper v1 or installs its result. */
    override suspend fun compose(items: List<CanonicalItemSnapshot>, witness: RoutinePlanningWitness): RoutineOccurrenceLocalComposition = withContext(Dispatchers.Default) {
        ensureActive()
        val requestBytes = encodeWitnessRequest(items, witness)
        beforeBridge(); ensureActive()
        val response = bridge.process(requestBytes) ?: throw LocalScheduleCompositionProtocolException()
        ensureActive()
        val decoded = decodeV2Response(response, witness)
        ensureActive()
        decoded.copy(composition = decoded.composition.copy(scheduleRequestFingerprint = requestBytes.sha256Fingerprint()))
    }

    @OptIn(ExperimentalSerializationApi::class)
    internal fun encodeWitnessRequest(items: List<CanonicalItemSnapshot>, witness: RoutinePlanningWitness, byteLimit: Int = MAX_MESSAGE_BYTES): ByteArray {
        require(byteLimit in 1..MAX_MESSAGE_BYTES)
        val output = BoundedRequestOutput(byteLimit)
        try {
            witness.requireValid(); witness.requireCurrentSources(items)
            output.writeAscii("{\"protocol\":\"dayweave.scheduler.helper\",\"version\":2,\"operation\":\"compose\",\"request\":{\"canonical_items\":[")
            items.forEachIndexed { index, item ->
                if (index > 0) output.write(','.code)
                JSON.encodeToStream(HelperCanonicalItem.serializer(), item.toHelperItem(), output)
            }
            output.writeAscii(REQUEST_SCHEDULE_SEPARATOR)
            JSON.encodeToStream(RoutinePlanningSchedule.serializer(), witness.schedule, output)
            output.writeAscii(",\"occurrence_lifecycle\":")
            JSON.encodeToStream(RoutinePlanningLifecycle.serializer(), witness.occurrenceLifecycle, output)
            output.writeAscii(REQUEST_SUFFIX)
            return output.finish()
        } catch (_: LocalScheduleCompositionRequestTooLargeException) { throw LocalScheduleCompositionRequestTooLargeException() }
        catch (error: Exception) {
            if (error.hasRequestTooLargeCause()) throw LocalScheduleCompositionRequestTooLargeException()
            throw LocalScheduleCompositionRequestException()
        }
    }

    internal fun decodeV2Response(bytes: ByteArray, witness: RoutinePlanningWitness): RoutineOccurrenceLocalComposition {
        try {
            witness.requireValid()
            require(bytes.size in 1..MAX_MESSAGE_BYTES && bytes.last() == '\n'.code.toByte())
            val text = decodePlanningUtf8(bytes.copyOf(bytes.size - 1)); requirePlanningJson(text)
            val root = JSON.parseToJsonElement(text).jsonObject
            root.requireExactKeys("protocol", "version", "result")
            require(root.getValue("protocol").jsonPrimitive.isString && root.string("protocol") == PROTOCOL)
            require(!root.getValue("version").jsonPrimitive.isString && root.getValue("version").jsonPrimitive.content == "2")
            val result = root.getValue("result").jsonObject
            require(result.getValue("type").jsonPrimitive.isString)
            if (result.string("type") == "error") {
                result.requireExactKeys("type", "error")
                val error = result.getValue("error").jsonObject; error.requireExactKeys("code", "message")
                require(error.getValue("code").jsonPrimitive.isString && error.getValue("message").jsonPrimitive.isString)
                val code = error.string("code")
                require(code in V2_ERROR_CODES && error.string("message").isNotBlank())
                throw LocalScheduleCompositionRejectedException(code)
            }
            require(result.string("type") == "composition"); result.requireExactKeys("type", "composition")
            val raw = result.getValue("composition").jsonObject
            val exact = JSON.decodeFromJsonElement<HelperV2Composition>(raw)
            require(JSON.encodeToJsonElement(exact) == raw)
            require(exact.occurrenceSnapshotRevision == witness.occurrenceLifecycle.snapshotRevision)
            val legacyShape = JsonObject(raw.filterKeys { it != "occurrence_snapshot_revision" })
            val decoded = decodeComposition(JsonObject(mapOf("type" to result.getValue("type"), "composition" to legacyShape)))
            require(decoded.localInputFingerprint == witness.localInputFingerprint)
            require(decoded.sourceItemRevisions == witness.sourceItemRevisions && decoded.sourceItemCount == witness.sourceItemRevisions.size)
            require(decoded.acceptedItemCount == decoded.sourceItemCount)
            require(decoded.rejectedItems.isEmpty()) // Qualified server capture rejects ineligible sources.
            requireV2Plan(decoded.plan, witness)
            return RoutineOccurrenceLocalComposition(decoded, exact.occurrenceSnapshotRevision)
        } catch (error: LocalScheduleCompositionRejectedException) { throw error }
        catch (_: Exception) { throw LocalScheduleCompositionProtocolException() }
    }

    private fun requireV2Plan(plan: RemoteSchedulePlan, witness: RoutinePlanningWitness) {
        val schedule = witness.schedule
        require(requirePlanningInstant(plan.asOf) == requirePlanningInstant(schedule.asOf))
        val start = requirePlanningInstant(plan.horizonStart); val end = requirePlanningInstant(plan.horizonEnd)
        require(start == requirePlanningInstant(schedule.horizonStart) && end == requirePlanningInstant(schedule.horizonEnd))
        require(plan.score.scheduledMinutes in 0..4_294_967_295L && plan.score.unscheduledMinutes in 0..4_294_967_295L && plan.score.movedMinutes in 0..4_294_967_295L)
        val occurrences = plan.occurrences.associateBy { it.id }
        require(occurrences.size == plan.occurrences.size && occurrences.size <= 10_000)
        occurrences.values.forEach {
            requirePlanningOccurrenceId(it.id); requirePlanningUuid(it.seriesItemId)
            require(it.seriesItemId in witness.sourceItemRevisions && it.ordinal in 0..4_294_967_295L)
            requirePlanningIdentity(it.identity)
            require(requirePlanningInstant(it.nominalStart) < requirePlanningInstant(it.nominalEnd))
            require(requirePlanningInstant(it.windowStart) < requirePlanningInstant(it.windowEnd))
            require(it.state in setOf("generated", "completed", "paused", "skipped"))
            it.localDate?.let { date -> require(java.time.LocalDate.parse(date).toString() == date) }
        }
        val managed = witness.occurrenceLifecycle.instances.associateBy { it.occurrenceId }
        managed.forEach { (id, instance) ->
            val occurrence = requireNotNull(occurrences[id])
            require(occurrence.seriesItemId == instance.rootItemId && occurrence.identity == instance.identity && occurrence.state == "generated")
        }
        val ids = HashSet<String>()
        plan.blocks.forEach { block ->
            requirePlanningUuid(block.id); require(ids.add(block.id))
            require(block.kind in setOf("planned", "pinned", "calendar_event", "external_fixed"))
            require(block.title.isNotBlank() && block.title.length <= 4096)
            block.explanations.forEach { require(it.code in V2_EXPLANATION_CODES && it.message.isNotBlank() && it.message.length <= 4096) }
            val blockStart = requirePlanningInstant(block.start); val blockEnd = requirePlanningInstant(block.end)
            require(blockStart < blockEnd && blockStart >= start && blockEnd <= end && block.sessionIndex in 0..65_535)
            block.itemId?.let { requirePlanningUuid(it); require(it in witness.sourceItemRevisions) }
            block.externalBlockId?.let(::requirePlanningUuid)
            block.occurrenceId?.let { id ->
                requirePlanningOccurrenceId(id); require(id in occurrences && block.itemId != null)
                managed[id]?.let { require(it.members.any { member -> member.itemId == block.itemId }) }
            }
        }
        plan.unscheduled.forEach {
            requirePlanningUuid(it.itemId); require(it.itemId in witness.sourceItemRevisions && it.remaining in 0..4_294_967_295L)
            require(it.reason in setOf("missing_duration", "no_capacity", "hard_constraint", "blocked", "dependency_unavailable", "dependency_cycle", "session_limit"))
            require(it.message.isNotBlank() && it.message.length <= 4096); it.occurrenceId?.let { id -> require(id in occurrences) }
        }
        plan.decisions.forEach {
            requirePlanningUuid(it.itemId); require(it.itemId in witness.sourceItemRevisions)
            require(it.kind in setOf("container_rolled_up", "terminal_item_ignored", "fixed_event_retained", "scheduled", "partially_scheduled", "kept_pinned"))
            require(it.message.isNotBlank() && it.message.length <= 4096); it.occurrenceId?.let { id -> require(id in occurrences) }
        }
        plan.violations.forEach { violation ->
            require(violation.kind in setOf("soft_constraint", "fixed_overlap", "pinned_conflict", "deadline_risk", "dependency", "buffer_compressed", "capacity"))
            require(violation.severity in setOf("warning", "error") && violation.message.isNotBlank() && violation.message.length <= 4096)
            violation.itemIds.forEach { requirePlanningUuid(it); require(it in witness.sourceItemRevisions) }
            violation.occurrenceIds.forEach { requirePlanningOccurrenceId(it); require(it in occurrences) }
            violation.start?.let(::requirePlanningInstant); violation.end?.let(::requirePlanningInstant)
        }
    }

    @OptIn(ExperimentalSerializationApi::class)
    internal fun encodeRequest(
        items: List<CanonicalItemSnapshot>,
        request: SchedulePreviewRequest,
        byteLimit: Int = MAX_MESSAGE_BYTES,
    ): ByteArray {
        require(byteLimit in 1..MAX_MESSAGE_BYTES)
        val output = BoundedRequestOutput(byteLimit)
        try {
            output.writeAscii(REQUEST_PREFIX)
            items.forEachIndexed { index, item ->
                if (index > 0) output.write(','.code)
                JSON.encodeToStream(HelperCanonicalItem.serializer(), item.toHelperItem(), output)
            }
            output.writeAscii(REQUEST_SCHEDULE_SEPARATOR)
            JSON.encodeToStream(SchedulePreviewRequest.serializer(), request, output)
            output.writeAscii(REQUEST_SUFFIX)
            return output.finish()
        } catch (_: LocalScheduleCompositionRequestTooLargeException) {
            throw LocalScheduleCompositionRequestTooLargeException()
        } catch (error: Exception) {
            if (error.hasRequestTooLargeCause()) {
                throw LocalScheduleCompositionRequestTooLargeException()
            }
            // Stored JSON/parser failures may include private item content. Discard the complete
            // cause chain and expose only a fixed request diagnostic.
            throw LocalScheduleCompositionRequestException()
        }
    }

    internal fun decodeResponse(bytes: ByteArray): LocalScheduleComposition {
        try {
            require(bytes.size in 1..MAX_MESSAGE_BYTES)
            require(bytes.last() == '\n'.code.toByte())
            val payload = bytes.copyOf(bytes.size - 1)
            val text = StrictJsonContract.decodeAndVerify(payload)
            val root = JSON.parseToJsonElement(text).jsonObject
            root.requireExactKeys("protocol", "version", "result")
            require(root.string("protocol") == PROTOCOL)
            require(root.getValue("version").jsonPrimitive.intOrNull == VERSION)
            val result = root.getValue("result").jsonObject
            return when (result.string("type")) {
                "composition" -> decodeComposition(result)
                "error" -> {
                    result.requireExactKeys("type", "error")
                    val error = result.getValue("error").jsonObject
                    error.requireExactKeys("code", "message")
                    val code = error.string("code")
                    require(code.matches(ERROR_CODE_PATTERN))
                    require(error.string("message").isNotBlank())
                    throw LocalScheduleCompositionRejectedException(code)
                }
                else -> error("Unexpected bundled scheduler result")
            }
        } catch (error: LocalScheduleCompositionRejectedException) {
            throw error
        } catch (_: Exception) {
            // Parser and serializer failures may embed response snippets. Never retain them in an
            // exception cause, log, or crash-report chain.
            throw LocalScheduleCompositionProtocolException()
        }
    }

    private fun decodeComposition(result: JsonObject): LocalScheduleComposition {
        result.requireExactKeys("type", "composition")
        val composition = result.getValue("composition").jsonObject
        composition.requireExactKeys(
            "local_input_fingerprint",
            "source_item_count",
            "source_item_revisions",
            "accepted_item_count",
            "rejected_items",
            "ignored_previous_assignments",
            "plan",
        )
        composition.getValue("rejected_items").jsonArray.forEach {
            it.jsonObject.requireExactKeys("item_id", "is_sensitive", "title", "reason")
        }
        composition.getValue("ignored_previous_assignments").jsonArray.forEach {
            it.jsonObject.requireExactKeys(
                "item_id",
                "requested_revision",
                "current_revision",
                "reason",
            )
        }
        val plan = composition.getValue("plan").jsonObject
        requireExactPlanShape(plan)
        val fingerprint = composition.string("local_input_fingerprint")
        require(
            fingerprint.length == LOCAL_FINGERPRINT_PREFIX.length + 64 &&
                fingerprint.startsWith(LOCAL_FINGERPRINT_PREFIX) &&
                fingerprint.drop(LOCAL_FINGERPRINT_PREFIX.length).all {
                    it in '0'..'9' || it in 'a'..'f'
                },
        )
        return LocalScheduleComposition(
            localInputFingerprint = fingerprint,
            sourceItemCount = composition.getValue("source_item_count").jsonPrimitive.intOrNull
                ?: error("Invalid source count"),
            sourceItemRevisions = JSON.decodeFromJsonElement(
                composition.getValue("source_item_revisions"),
            ),
            acceptedItemCount =
                composition.getValue("accepted_item_count").jsonPrimitive.intOrNull
                    ?: error("Invalid accepted count"),
            rejectedItems = JSON.decodeFromJsonElement(
                composition.getValue("rejected_items"),
            ),
            ignoredPreviousAssignments = JSON.decodeFromJsonElement(
                composition.getValue("ignored_previous_assignments"),
            ),
            plan = JSON.decodeFromJsonElement(plan),
        )
    }

    private fun requireExactPlanShape(plan: JsonObject) {
        plan.requireExactKeys(
            "as_of",
            "horizon_start",
            "horizon_end",
            "blocks",
            "unscheduled",
            "decisions",
            "violations",
            "score",
            "occurrences",
        )
        plan.getValue("blocks").jsonArray.forEach { block ->
            block.jsonObject.requireExactKeys(
                "id",
                "is_sensitive",
                "item_id",
                "occurrence_id",
                "external_block_id",
                "title",
                "start",
                "end",
                "session_index",
                "kind",
                "explanations",
            )
            block.jsonObject.getValue("explanations").jsonArray.forEach {
                it.jsonObject.requireExactKeys("code", "message")
            }
        }
        plan.getValue("unscheduled").jsonArray.forEach {
            it.jsonObject.requireExactKeys(
                "item_id",
                "occurrence_id",
                "remaining",
                "reason",
                "message",
            )
        }
        plan.getValue("decisions").jsonArray.forEach {
            it.jsonObject.requireExactKeys("item_id", "occurrence_id", "kind", "message")
        }
        plan.getValue("violations").jsonArray.forEach {
            it.jsonObject.requireExactKeys(
                "kind",
                "severity",
                "item_ids",
                "occurrence_ids",
                "start",
                "end",
                "penalty",
                "message",
            )
        }
        plan.getValue("score").jsonObject.requireExactKeys(
            "scheduled_minutes",
            "unscheduled_minutes",
            "soft_penalty",
            "moved_minutes",
        )
        plan.getValue("occurrences").jsonArray.forEach {
            it.jsonObject.requireExactKeys(
                "id",
                "series_item_id",
                "identity",
                "nominal_start",
                "nominal_end",
                "window_start",
                "window_end",
                "local_date",
                "ordinal",
                "state",
            )
        }
    }

    private fun CanonicalItemSnapshot.toHelperItem(): HelperCanonicalItem {
        requireValidStructuralMetadata()
        require(durationKind.isSupported && durationSource?.isSupported != false)
        require(deadlineKind.isSupported && deadlineStrength?.isSupported != false)
        require(blockedReasonKind?.isSupported != false)
        require(kind in HELPER_SUPPORTED_ITEM_KINDS && status in HELPER_SUPPORTED_ITEM_STATUSES) {
            "The bundled scheduler cannot normalize unknown canonical semantics"
        }
        return HelperCanonicalItem(
            id = id,
            isSensitive = isSensitive,
            kind = kind,
            status = status,
            title = title,
            // Notes do not affect deterministic placement and never cross the native boundary.
            notes = null,
            timezoneName = timezoneName,
            durationKind = durationKind,
            durationSeconds = durationSeconds,
            durationMinSeconds = durationMinSeconds,
            durationMaxSeconds = durationMaxSeconds,
            durationSource = durationSource,
            deadlineKind = deadlineKind,
            deadlineDate = deadlineDate,
            deadlineAt = deadlineAt,
            deadlineStrength = deadlineStrength,
            deadlineSoftWeight = deadlineSoftWeight,
            earliestStartAt = earliestStartAt,
            recurrence = recurrenceJson?.let(::parseStoredJson),
            flexibleConstraints = parseStoredJson(flexibleConstraintsJson),
            hasOwnEffort = hasOwnEffort,
            splitPolicy = parseStoredJson(splitPolicyJson),
            importance = importance,
            urgency = urgency,
            parentId = parentId,
            siblingOrder = siblingOrder,
            isExecutable = isExecutable,
            revision = revision,
            createdAt = createdAt,
            updatedAt = updatedAt,
            completedAt = completedAt,
            deletedAt = deletedAt,
            blockedReasonKind = blockedReasonKind,
            blockedByItemId = blockedByItemId,
            blockedReason = blockedReason,
        )
    }

    private fun parseStoredJson(raw: String): JsonElement {
        val bytes = raw.toByteArray(Charsets.UTF_8)
        require(bytes.size <= MAX_EMBEDDED_JSON_BYTES)
        StrictJsonContract.decodeAndVerify(bytes)
        return JSON.parseToJsonElement(raw)
    }

    private fun JsonObject.string(key: String): String =
        getValue(key).jsonPrimitive.contentOrNull ?: error("$key is not a string")

    private fun JsonObject.requireExactKeys(vararg expected: String) {
        require(keys == expected.toSet())
    }

    private fun ByteArray.sha256Fingerprint(): String =
        "sha256:" + MessageDigest.getInstance("SHA-256").digest(this).joinToString("") { byte ->
            "%02x".format(byte.toInt() and 0xff)
        }

    private companion object {
        const val PROTOCOL = "dayweave.scheduler.helper"
        const val VERSION = 1
        const val MAX_MESSAGE_BYTES = 16 * 1024 * 1024
        const val MAX_EMBEDDED_JSON_BYTES = 1024 * 1024
        const val REQUEST_PREFIX =
            "{\"protocol\":\"dayweave.scheduler.helper\",\"version\":1," +
                "\"operation\":\"compose\",\"request\":{\"canonical_items\":["
        const val REQUEST_SCHEDULE_SEPARATOR = "],\"schedule\":"
        const val REQUEST_SUFFIX = "}}"
        const val LOCAL_FINGERPRINT_PREFIX = "local-sha256:"
        val ERROR_CODE_PATTERN = Regex("[a-z][a-z0-9_]{0,63}")
        val V2_ERROR_CODES = setOf("request_too_large", "invalid_utf8", "invalid_json", "duplicate_json_key", "json_depth_exceeded", "unsupported_protocol", "unsupported_version", "unsupported_operation", "invalid_request", "resource_limit_exceeded", "response_too_large", "invalid_horizon", "invalid_granularity", "duplicate_item", "invalid_item", "invalid_window", "missing_previous_item", "invalid_hierarchy", "invalid_recurrence", "internal_failure")
        val V2_EXPLANATION_CODES = setOf("fixed_event", "pinned", "hard_deadline", "goal_progress", "habit_or_routine", "priority", "preferred_window", "context_match", "energy_match", "dependency", "stable_time", "earliest_available", "split_session")
        val HELPER_SUPPORTED_ITEM_KINDS = setOf(
            "event", "task", "habit", "routine", "goal", "project", "break",
        )
        val HELPER_SUPPORTED_ITEM_STATUSES = setOf(
            "inbox", "planned", "scheduled", "in_progress", "paused", "blocked", "completed",
            "skipped", "cancelled",
        )
        val JSON = Json {
            encodeDefaults = true
            explicitNulls = true
            ignoreUnknownKeys = false
        }
    }
}

@Serializable
private data class HelperV2Composition(
    @SerialName("local_input_fingerprint") val localInputFingerprint: String,
    @SerialName("source_item_count") val sourceItemCount: Int,
    @SerialName("source_item_revisions") val sourceItemRevisions: Map<String, Long>,
    @SerialName("accepted_item_count") val acceptedItemCount: Int,
    @SerialName("rejected_items") val rejectedItems: List<RemoteRejectedScheduleItem>,
    @SerialName("ignored_previous_assignments") val ignoredPreviousAssignments: List<RemoteIgnoredPreviousAssignment>,
    val plan: RemoteSchedulePlan,
    @SerialName("occurrence_snapshot_revision") val occurrenceSnapshotRevision: Long,
)

@Serializable
private data class HelperCanonicalItem(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val kind: String,
    val status: String,
    val title: String,
    val notes: String?,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("duration_kind") val durationKind: CanonicalDurationKind,
    @SerialName("duration_seconds") val durationSeconds: Long?,
    @SerialName("duration_min_seconds") val durationMinSeconds: Long?,
    @SerialName("duration_max_seconds") val durationMaxSeconds: Long?,
    @SerialName("duration_source") val durationSource: CanonicalDurationSource?,
    @SerialName("deadline_kind") val deadlineKind: CanonicalDeadlineKind,
    @SerialName("deadline_date") val deadlineDate: String?,
    @SerialName("deadline_at") val deadlineAt: String?,
    @SerialName("deadline_strength") val deadlineStrength: CanonicalDeadlineStrength?,
    @SerialName("deadline_soft_weight") val deadlineSoftWeight: Long?,
    @SerialName("earliest_start_at") val earliestStartAt: String?,
    val recurrence: JsonElement?,
    @SerialName("flexible_constraints") val flexibleConstraints: JsonElement,
    @SerialName("has_own_effort") val hasOwnEffort: Boolean,
    @SerialName("split_policy") val splitPolicy: JsonElement,
    val importance: Int,
    val urgency: Int,
    @SerialName("parent_id") val parentId: String?,
    @SerialName("sibling_order") val siblingOrder: Long,
    @SerialName("is_executable") val isExecutable: Boolean,
    val revision: Long,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
    @SerialName("completed_at") val completedAt: String?,
    @SerialName("deleted_at") val deletedAt: String?,
    @SerialName("blocked_reason_kind") val blockedReasonKind: CanonicalBlockedReasonKind?,
    @SerialName("blocked_by_item_id") val blockedByItemId: String?,
    @SerialName("blocked_reason") val blockedReason: String?,
)

private class BoundedRequestOutput(
    private val byteLimit: Int,
) : ByteArrayOutputStream(minOf(byteLimit, INITIAL_CAPACITY)) {
    override fun write(value: Int) {
        if (count >= byteLimit) throw LocalScheduleCompositionRequestTooLargeException()
        super.write(value)
    }

    override fun write(bytes: ByteArray, offset: Int, length: Int) {
        if (length < 0 || offset < 0 || offset > bytes.size - length) {
            throw IndexOutOfBoundsException()
        }
        if (length > byteLimit - count) {
            throw LocalScheduleCompositionRequestTooLargeException()
        }
        super.write(bytes, offset, length)
    }

    fun writeAscii(value: String) {
        val bytes = value.toByteArray(Charsets.US_ASCII)
        write(bytes, 0, bytes.size)
    }

    fun finish(): ByteArray = toByteArray()

    private companion object {
        const val INITIAL_CAPACITY = 64 * 1024
    }
}

private fun Throwable.hasRequestTooLargeCause(): Boolean {
    var current: Throwable? = this
    repeat(8) {
        if (current is LocalScheduleCompositionRequestTooLargeException) return true
        current = current?.cause ?: return false
    }
    return false
}

/** Detects duplicate keys and resource abuse before kotlinx.serialization materializes the tree. */
private object StrictJsonContract {
    private const val MAX_DEPTH = 128
    private const val MAX_VALUES = 1_000_000

    fun decodeAndVerify(bytes: ByteArray): String {
        val decoder = StandardCharsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
        val text = decoder.decode(ByteBuffer.wrap(bytes)).toString()
        require('\u0000' !in text)
        Parser(text).parse()
        return text
    }

    private class Parser(private val input: String) {
        private var index = 0
        private var values = 0

        fun parse() {
            require(input.isNotEmpty())
            parseValue(0)
            require(index == input.length)
        }

        private fun parseValue(depth: Int) {
            require(depth <= MAX_DEPTH && ++values <= MAX_VALUES && index < input.length)
            when (input[index]) {
                '{' -> parseObject(depth + 1)
                '[' -> parseArray(depth + 1)
                '"' -> parseString()
                't' -> literal("true")
                'f' -> literal("false")
                'n' -> literal("null")
                '-', in '0'..'9' -> parseNumber()
                else -> error("Invalid JSON value")
            }
        }

        private fun parseObject(depth: Int) {
            require(input[index++] == '{')
            skipWhitespace()
            if (take('}')) return
            val keys = hashSetOf<String>()
            while (true) {
                require(index < input.length && input[index] == '"')
                val key = parseString()
                require(keys.add(key))
                skipWhitespace()
                require(take(':'))
                skipWhitespace()
                parseValue(depth)
                skipWhitespace()
                if (take('}')) return
                require(take(','))
                skipWhitespace()
            }
        }

        private fun parseArray(depth: Int) {
            require(input[index++] == '[')
            skipWhitespace()
            if (take(']')) return
            while (true) {
                parseValue(depth)
                skipWhitespace()
                if (take(']')) return
                require(take(','))
                skipWhitespace()
            }
        }

        private fun parseString(): String {
            val start = index
            require(input[index++] == '"')
            var escaped = false
            while (index < input.length) {
                val character = input[index++]
                when {
                    character == '"' -> {
                        val literal = input.substring(start, index)
                        return Json.parseToJsonElement(literal).jsonPrimitive.content
                    }
                    character.code < 0x20 -> error("Control character in JSON string")
                    character == '\\' -> {
                        escaped = true
                        require(index < input.length)
                        when (input[index++]) {
                            '"', '\\', '/', 'b', 'f', 'n', 'r', 't' -> Unit
                            'u' -> repeat(4) {
                                require(index < input.length && input[index++].isHexDigit())
                            }
                            else -> error("Invalid JSON escape")
                        }
                    }
                }
            }
            require(!escaped)
            error("Unterminated JSON string")
        }

        private fun parseNumber() {
            take('-')
            require(index < input.length)
            if (take('0')) {
                require(index >= input.length || input[index] !in '0'..'9')
            } else {
                require(index < input.length && input[index] in '1'..'9')
                while (index < input.length && input[index] in '0'..'9') index++
            }
            if (take('.')) {
                require(index < input.length && input[index] in '0'..'9')
                while (index < input.length && input[index] in '0'..'9') index++
            }
            if (index < input.length && input[index] in charArrayOf('e', 'E')) {
                index++
                if (index < input.length && input[index] in charArrayOf('+', '-')) index++
                require(index < input.length && input[index] in '0'..'9')
                while (index < input.length && input[index] in '0'..'9') index++
            }
        }

        private fun literal(expected: String) {
            require(input.regionMatches(index, expected, 0, expected.length))
            index += expected.length
        }

        private fun skipWhitespace() {
            while (index < input.length && input[index] in charArrayOf(' ', '\t', '\r', '\n')) {
                index++
            }
        }

        private fun take(character: Char): Boolean {
            if (index >= input.length || input[index] != character) return false
            index++
            return true
        }

        private fun Char.isHexDigit(): Boolean =
            this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'
    }
}
