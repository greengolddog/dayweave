package com.greengolddog.dayweave.data

import android.content.Context
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.withPendingSensitivityHardened
import com.greengolddog.dayweave.network.requireScheduleInputDigest
import com.greengolddog.dayweave.network.validateSchedulePublishHttpRequest
import java.time.Instant
import java.util.UUID
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

interface PlannerStateRepository {
    suspend fun load(): DayWeaveUiState?
    suspend fun save(state: DayWeaveUiState)
}

/** Defers Keystore and database initialization until the store's IO-scoped restore starts. */
class EncryptedRoomPlannerStateRepository(context: Context) : PlannerStateRepository {
    private val applicationContext = context.applicationContext
    private val delegate: RoomPlannerStateRepository by lazy {
        val database = PlannerDatabaseFactory.createEncrypted(applicationContext)
        RoomPlannerStateRepository(database.plannerSnapshotDao())
    }

    override suspend fun load(): DayWeaveUiState? = delegate.load()

    override suspend fun save(state: DayWeaveUiState) = delegate.save(state)
}

class RoomPlannerStateRepository(
    private val dao: PlannerSnapshotDao,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) : PlannerStateRepository {
    override suspend fun load(): DayWeaveUiState? = dao.load()?.let { snapshot ->
        when (snapshot.payloadFormat) {
            PlannerSnapshotFormats.JSON_V5 -> decodeCurrentSnapshot(snapshot.payload)
            PlannerSnapshotFormats.JSON_V4 -> {
                val migrated = decodeVersionFourSnapshot(snapshot.payload)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V3 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    requireExistingSensitivity = true,
                )
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V2 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    allowPreSensitivityJournal = true,
                )
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V1 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    allowPreSensitivityJournal = true,
                )
                save(migrated)
                migrated
            }
            else -> error("Unsupported planner snapshot format")
        }
    }

    override suspend fun save(state: DayWeaveUiState) {
        validateSchedulePublicationState(state)
        dao.save(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = SNAPSHOT_JSON.encodeToString(state),
                updatedAtEpochMillis = nowEpochMillis(),
                payloadFormat = PlannerSnapshotFormats.JSON_V5,
            ),
        )
    }

    private fun decodeCurrentSnapshot(payload: String): DayWeaveUiState {
        val root = SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject
        if (!root.containsKey("pendingSchedulePublication") ||
            !root.containsKey("publishedScheduleRevision")) {
            throw SerializationException("Current schedule publication fields are required")
        }
        requireExplicitSensitivity(root, "schedule")
        requireExplicitSensitivity(root, "canonicalItems")
        requireExplicitSensitivity(root, "inbox")
        when (val pendingElement = root["pendingCanonicalMutation"]) {
            null, JsonNull -> Unit
            is JsonObject -> {
                val target = pendingElement["targetIsSensitive"]
                    ?.jsonPrimitive
                    ?.booleanOrNull
                if (target == null) {
                    throw SerializationException(
                        "pendingCanonicalMutation.targetIsSensitive is required by the current " +
                        "snapshot contract",
                    )
                }
                if (target != journaledPendingSensitivity(pendingElement)) {
                    throw SerializationException(
                        "pendingCanonicalMutation.targetIsSensitive does not match its exact " +
                            "replacement body",
                    )
                }
            }
            else -> throw SerializationException("pendingCanonicalMutation must be an object")
        }
        return SNAPSHOT_JSON.decodeFromJsonElement<DayWeaveUiState>(root)
            .withPendingSensitivityHardened()
            .also(::validateSchedulePublicationState)
    }

    /** V4 already required explicit sensitivity and an exact pending replacement target. */
    private fun decodeVersionFourSnapshot(payload: String): DayWeaveUiState {
        val root = SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject
        requireExplicitSensitivity(root, "schedule")
        requireExplicitSensitivity(root, "canonicalItems")
        requireExplicitSensitivity(root, "inbox")
        when (val pendingElement = root["pendingCanonicalMutation"]) {
            null, JsonNull -> Unit
            is JsonObject -> {
                val target = pendingElement["targetIsSensitive"]
                    ?.jsonPrimitive
                    ?.booleanOrNull
                    ?: throw SerializationException(
                        "pendingCanonicalMutation.targetIsSensitive is required by the v4 " +
                            "snapshot contract",
                    )
                if (target != journaledPendingSensitivity(pendingElement)) {
                    throw SerializationException(
                        "pendingCanonicalMutation.targetIsSensitive does not match its exact " +
                            "replacement body",
                    )
                }
            }
            else -> throw SerializationException("pendingCanonicalMutation must be an object")
        }
        return SNAPSHOT_JSON.decodeFromJsonElement<DayWeaveUiState>(root)
            .withPendingSensitivityHardened()
            .also(::validateSchedulePublicationState)
    }

    /**
     * Promotes legacy encrypted snapshots without weakening an in-flight replacement fence.
     * Inbox drafts predate sensitivity authoring and therefore migrate explicitly to false. A
     * pending canonical mutation derives its target from the exact journaled body; malformed or
     * incomplete uncertainty remains unreadable instead of being silently reclassified.
     */
    private fun decodeLegacySnapshot(
        payload: String,
        requireExistingSensitivity: Boolean = false,
        allowPreSensitivityJournal: Boolean = false,
    ): DayWeaveUiState {
        val legacyRoot = LEGACY_SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject
        if (requireExistingSensitivity) {
            requireExplicitSensitivity(legacyRoot, "schedule")
            requireExplicitSensitivity(legacyRoot, "canonicalItems")
        }
        val inbox = legacyRoot["inbox"]?.jsonArray?.map { entry ->
            val item = entry.jsonObject
            if (item["isSensitive"]?.jsonPrimitive?.booleanOrNull != null) item else {
                JsonObject(item + ("isSensitive" to JsonPrimitive(false)))
            }
        }
        val pending = legacyRoot["pendingCanonicalMutation"]
            ?.takeUnless { it is JsonNull }
            ?.let { pendingElement ->
                val item = pendingElement.jsonObject
                val journaledTarget = journaledPendingSensitivity(
                    pending = item,
                    allowMissingSensitivity = allowPreSensitivityJournal,
                )
                val existingTarget = item["targetIsSensitive"]
                    ?.jsonPrimitive
                    ?.booleanOrNull
                if (existingTarget != null && existingTarget != journaledTarget) {
                    throw SerializationException(
                        "Legacy pending sensitivity target does not match its exact replacement body",
                    )
                }
                if (existingTarget != null) item else {
                    JsonObject(item + ("targetIsSensitive" to JsonPrimitive(journaledTarget)))
                }
            }
        val migratedRoot = buildJsonObject {
            legacyRoot.forEach { (key, value) -> put(key, value) }
            if (inbox != null) put("inbox", kotlinx.serialization.json.JsonArray(inbox))
            if (pending != null) put("pendingCanonicalMutation", pending)
        }
        return LEGACY_SNAPSHOT_JSON.decodeFromJsonElement<DayWeaveUiState>(migratedRoot)
            .withPendingSensitivityHardened()
            .also(::validateSchedulePublicationState)
    }

    private fun validateSchedulePublicationState(state: DayWeaveUiState) {
        state.pendingSchedulePublication?.let { pending ->
            if (pending.schemaVersion != 1) {
                throw SerializationException("Unsupported schedule publication journal")
            }
            val key = runCatching { UUID.fromString(pending.idempotencyKey) }.getOrNull()
                ?: throw SerializationException("Invalid schedule publication idempotency key")
            if (key.toString() != pending.idempotencyKey || key == UUID(0L, 0L)) {
                throw SerializationException("Invalid schedule publication idempotency key")
            }
            runCatching { Instant.parse(pending.preparedAt) }.getOrElse {
                throw SerializationException("Invalid schedule publication timestamp")
            }
            val request = runCatching {
                validateSchedulePublishHttpRequest(pending.syncOrigin, pending.request)
            }.getOrElse {
                throw SerializationException("Invalid exact schedule publication request", it)
            }
            if (
                request.idempotencyKey != pending.idempotencyKey ||
                request.expectedInputDigest != pending.candidate.inputDigest ||
                request.schedule.asOf != pending.candidate.generatedAt ||
                request.schedule.timezoneName != pending.candidate.planningZoneId ||
                pending.candidate.syncOrigin != pending.syncOrigin ||
                pending.candidate.configurationId != pending.configurationId
            ) {
                throw SerializationException("Schedule publication journal fields disagree")
            }
        }
        state.publishedScheduleRevision?.let { published ->
            val id = runCatching { UUID.fromString(published.id) }.getOrNull()
                ?: throw SerializationException("Invalid published schedule id")
            if (
                id.toString() != published.id || id == UUID(0L, 0L) ||
                published.revisionNumber == 0uL ||
                published.revision != "${published.revisionNumber}:${published.id}"
            ) {
                throw SerializationException("Invalid published schedule revision")
            }
            runCatching { requireScheduleInputDigest(published.inputDigest) }.getOrElse {
                throw SerializationException("Invalid published schedule digest", it)
            }
            val start = runCatching { Instant.parse(published.horizonStart) }.getOrElse {
                throw SerializationException("Invalid published schedule horizon", it)
            }
            val end = runCatching { Instant.parse(published.horizonEnd) }.getOrElse {
                throw SerializationException("Invalid published schedule horizon", it)
            }
            if (end <= start || published.timezoneName.isBlank()) {
                throw SerializationException("Invalid published schedule horizon")
            }
            runCatching { Instant.parse(published.publishedAt) }.getOrElse {
                throw SerializationException("Invalid published schedule timestamp", it)
            }
            if (
                state.canonicalSyncOrigin == null ||
                state.scheduleInputDigest != published.inputDigest ||
                state.schedulePlanningZoneId != published.timezoneName
            ) {
                throw SerializationException("Published schedule receipt does not match the cache")
            }
        }
    }

    /** Validates the duplicated fence fields against the real snake_case wire journal. */
    private fun journaledPendingSensitivity(
        pending: JsonObject,
        allowMissingSensitivity: Boolean = false,
    ): Boolean {
        val expectedRevision = pending["expectedRevision"]?.jsonPrimitive?.longOrNull
        val targetStatus = pending["targetStatus"]?.jsonPrimitive?.content
        val requestJson = pending["replacementRequestJson"]?.jsonPrimitive?.content
        if (expectedRevision == null || targetStatus == null || requestJson == null) {
            throw SerializationException("Pending canonical mutation metadata is incomplete")
        }
        val request = runCatching {
            LEGACY_SNAPSHOT_JSON.parseToJsonElement(requestJson).jsonObject
        }.getOrElse {
            throw SerializationException("Pending canonical replacement body is malformed")
        }
        val requestRevision = request["expected_revision"]?.jsonPrimitive?.longOrNull
        val replacement = runCatching { request["item"]?.jsonObject }.getOrNull()
            ?: throw SerializationException("Pending canonical replacement item is missing")
        val requestStatus = replacement["status"]?.jsonPrimitive?.content
        val requestSensitivity = replacement["is_sensitive"]
            ?.jsonPrimitive
            ?.booleanOrNull
        if (
            requestRevision != expectedRevision || requestStatus != targetStatus ||
            requestSensitivity == null && !allowMissingSensitivity
        ) {
            throw SerializationException(
                "Pending canonical metadata does not match its exact replacement body",
            )
        }
        return requestSensitivity ?: false
    }

    private fun requireExplicitSensitivity(root: JsonObject, collection: String) {
        val entries = root[collection]?.jsonArray
            ?: throw SerializationException("$collection is required by the current snapshot contract")
        entries.forEachIndexed { index, entry ->
            val value = entry.jsonObject["isSensitive"]?.jsonPrimitive?.booleanOrNull
            if (value == null) {
                throw SerializationException(
                    "$collection[$index].isSensitive is required by the current snapshot contract",
                )
            }
        }
    }

    private companion object {
        val SNAPSHOT_JSON = Json {
            encodeDefaults = true
            ignoreUnknownKeys = false
        }
        val LEGACY_SNAPSHOT_JSON = Json {
            encodeDefaults = true
            ignoreUnknownKeys = true
        }
    }
}
