package com.greengolddog.dayweave.data

import android.content.Context
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalTrashRetentionPolicy
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.canonicalTrashItemBytes
import com.greengolddog.dayweave.model.requireCanonicalAuthoringJournalBudget
import com.greengolddog.dayweave.model.withCanonicalTrashRetention
import com.greengolddog.dayweave.model.withPendingSensitivityHardened
import com.greengolddog.dayweave.model.withInvalidTimedBreakNotificationAttemptAbandoned
import com.greengolddog.dayweave.network.requireScheduleInputDigest
import com.greengolddog.dayweave.network.validateProposalApplyHttpRequest
import com.greengolddog.dayweave.network.validateProposalUndoHttpRequest
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
        val decoded = when (snapshot.payloadFormat) {
            PlannerSnapshotFormats.JSON_V9 -> {
                val decoded = decodeCurrentSnapshot(snapshot.payload)
                if (SNAPSHOT_JSON.encodeToString(decoded) != snapshot.payload) save(decoded)
                decoded
            }
            PlannerSnapshotFormats.JSON_V8 -> {
                val migrated = decodeCurrentSnapshot(
                    payload = snapshot.payload,
                    requireTimedBreakNotificationFields = false,
                ).withoutTimedBreakNotificationReceipts()
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V7 -> {
                val migrated = decodeCurrentSnapshot(
                    payload = snapshot.payload,
                    requirePublicationProofField = false,
                    requireTimedBreakNotificationFields = false,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V6 -> {
                val migrated = decodeCurrentSnapshot(
                    payload = snapshot.payload,
                    requireCanonicalAuthoringFields = false,
                    requirePublicationProofField = false,
                    requireTimedBreakNotificationFields = false,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V5 -> {
                val migrated = decodeCurrentSnapshot(
                    payload = snapshot.payload,
                    requireProposalApplicationFields = false,
                    requireCanonicalAuthoringFields = false,
                    requirePublicationProofField = false,
                    requireTimedBreakNotificationFields = false,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V4 -> {
                val migrated = decodeVersionFourSnapshot(snapshot.payload)
                    .withoutTimedBreakNotificationReceipts()
                    .copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V3 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    requireExistingSensitivity = true,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V2 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    allowPreSensitivityJournal = true,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            PlannerSnapshotFormats.JSON_V1 -> {
                val migrated = decodeLegacySnapshot(
                    payload = snapshot.payload,
                    allowPreSensitivityJournal = true,
                ).withoutTimedBreakNotificationReceipts().copy(publishedScheduleProof = null)
                save(migrated)
                migrated
            }
            else -> error("Unsupported planner snapshot format")
        }
        val hardened = decoded.withInvalidTimedBreakNotificationAttemptAbandoned()
        if (hardened != decoded) save(hardened)
        hardened
    }

    override suspend fun save(state: DayWeaveUiState) {
        val referenceEpochMillis = nowEpochMillis()
        val retainedState = state.withCanonicalTrashRetention(referenceEpochMillis)
            .withInvalidTimedBreakNotificationAttemptAbandoned()
        validateSchedulePublicationState(retainedState)
        validateProposalApplicationState(retainedState)
        validateCanonicalAuthoringState(retainedState, referenceEpochMillis)
        dao.save(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = SNAPSHOT_JSON.encodeToString(retainedState),
                updatedAtEpochMillis = referenceEpochMillis,
                payloadFormat = PlannerSnapshotFormats.JSON_V9,
            ),
        )
    }

    private fun decodeCurrentSnapshot(
        payload: String,
        requireProposalApplicationFields: Boolean = true,
        requireCanonicalAuthoringFields: Boolean = true,
        requirePublicationProofField: Boolean = true,
        requireTimedBreakNotificationFields: Boolean = true,
    ): DayWeaveUiState {
        val parsedRoot = SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject
        val publicationSafeRoot = if (requirePublicationProofField) {
            parsedRoot
        } else {
            // Older format labels can never gain authority from an injected newer field.
            JsonObject(parsedRoot - "publishedScheduleProof")
        }
        val root = if (requireTimedBreakNotificationFields) {
            publicationSafeRoot
        } else {
            // V8 and predecessors cannot acquire alert suppression or tap authority from fields
            // introduced only by V9, even if a payload is relabeled or fields were injected.
            JsonObject(publicationSafeRoot - TIMED_BREAK_NOTIFICATION_RECEIPT_FIELDS)
        }
        if (!root.containsKey("pendingSchedulePublication") ||
            !root.containsKey("publishedScheduleRevision")) {
            throw SerializationException("Current schedule publication fields are required")
        }
        if (requirePublicationProofField && !root.containsKey("publishedScheduleProof")) {
            throw SerializationException("Current exact schedule publication proof is required")
        }
        if (
            requireTimedBreakNotificationFields &&
            !root.keys.containsAll(TIMED_BREAK_NOTIFICATION_RECEIPT_FIELDS)
        ) {
            throw SerializationException("Current timed-break notification receipts are required")
        }
        if (
            requireProposalApplicationFields &&
            (!root.containsKey("pendingProposalApplicationMutation") ||
                !root.containsKey("proposalApplications"))
        ) {
            throw SerializationException("Current proposal application fields are required")
        }
        if (
            requireCanonicalAuthoringFields &&
            (!root.containsKey("pendingCanonicalAuthoringMutations") ||
                !root.containsKey("canonicalRecentlyDeleted"))
        ) {
            throw SerializationException("Current canonical authoring fields are required")
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
        val referenceEpochMillis = nowEpochMillis()
        return SNAPSHOT_JSON.decodeFromJsonElement<DayWeaveUiState>(root)
            .withCanonicalTrashRetention(referenceEpochMillis)
            .withPendingSensitivityHardened()
            .also {
                validateSchedulePublicationState(it)
                validateProposalApplicationState(it)
                validateCanonicalAuthoringState(it, referenceEpochMillis)
            }
    }

    private fun DayWeaveUiState.withoutTimedBreakNotificationReceipts(): DayWeaveUiState = copy(
        lastBreakEndNotificationAttemptDigest = null,
        lastConsumedBreakEndNotificationDigest = null,
        lastRejectedBreakEndNotificationDigest = null,
        acknowledgedBreakEndDigest = null,
    )

    /** V4 already required explicit sensitivity and an exact pending replacement target. */
    private fun decodeVersionFourSnapshot(payload: String): DayWeaveUiState {
        val root = JsonObject(
            SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject - "publishedScheduleProof",
        )
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
            .also {
                validateSchedulePublicationState(it)
                validateCanonicalAuthoringState(it)
            }
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
        val legacyRoot = JsonObject(
            LEGACY_SNAPSHOT_JSON.parseToJsonElement(payload).jsonObject -
                "publishedScheduleProof",
        )
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
            .also {
                validateSchedulePublicationState(it)
                validateCanonicalAuthoringState(it)
            }
    }

    private fun validateCanonicalAuthoringState(
        state: DayWeaveUiState,
        referenceEpochMillis: Long = nowEpochMillis(),
    ) {
        val mutations = state.pendingCanonicalAuthoringMutations
        if (mutations.size > MAX_CANONICAL_AUTHORING_MUTATIONS ||
            mutations.map { it.id }.distinct().size != mutations.size ||
            mutations.map { it.itemId }.distinct().size != mutations.size) {
            throw SerializationException("Canonical authoring queue is invalid")
        }
        runCatching { requireCanonicalAuthoringJournalBudget(mutations) }.getOrElse {
            throw SerializationException("Malformed or oversized canonical authoring journal", it)
        }
        val unresolvedSubmitted = mutations.filter {
            it.isSubmitted && it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        val hasPendingAuthoringOverlay = mutations.any {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        if (unresolvedSubmitted.size > 1) {
            throw SerializationException("More than one canonical authoring write is unresolved")
        }
        if (hasPendingAuthoringOverlay &&
            (state.pendingCanonicalMutation != null || state.pendingExecutionCommand != null ||
                state.pendingSchedulePublication != null ||
                state.pendingProposalApplicationMutation != null)) {
            throw SerializationException("Canonical authoring crosses another uncertainty fence")
        }
        if (hasPendingAuthoringOverlay &&
            (state.publishedScheduleRevision != null || state.publishedScheduleProof != null ||
                state.scheduleInputDigest != null)) {
            throw SerializationException("Pending canonical authoring retains a current-plan proof")
        }
        mutations.filter { it.syncOrigin != null }.forEach { mutation ->
            if (state.canonicalSyncOrigin != mutation.syncOrigin ||
                state.canonicalConfigurationId != mutation.configurationId) {
                throw SerializationException("Canonical authoring mutation crosses its API binding")
            }
        }

        val deleted = state.canonicalRecentlyDeleted
        if (deleted.size > MAX_RECENTLY_DELETED ||
            deleted.map { it.id }.distinct().size != deleted.size) {
            throw SerializationException("Recently-deleted canonical records are invalid")
        }
        deleted.forEach { record ->
            runCatching { record.requireValid() }.getOrElse {
                throw SerializationException("Malformed recently-deleted canonical record", it)
            }
            if (record.retentionAnchorAt == null) {
                throw SerializationException("Recently-deleted canonical retention anchor is missing")
            }
        }
        val retainedBodyBytes = deleted.sumOf { record ->
            (record.lastKnownItem?.let(::canonicalTrashItemBytes) ?: 0).toLong()
        }
        if (deleted.any { record ->
                record.lastKnownItem?.let(::canonicalTrashItemBytes)
                    ?.let { it > CanonicalTrashRetentionPolicy.MAX_ITEM_BYTES } == true
            } || retainedBodyBytes > CanonicalTrashRetentionPolicy.MAX_RETAINED_ITEM_BYTES.toLong()
        ) {
            throw SerializationException("Recently-deleted canonical bodies exceed retention limits")
        }
        if (state.canonicalItems.any { active -> deleted.any { it.id == active.id } }) {
            throw SerializationException("An active canonical item is also recently deleted")
        }
        mutations.filter { it.operation == CanonicalAuthoringOperation.RESTORE }.forEach { mutation ->
            val record = deleted.firstOrNull { it.id == mutation.itemId }
                ?: throw SerializationException("Restore journal has no deleted record")
            val expectedRevision = requireNotNull(mutation.expectedRevision)
            val revisionIsValid = when {
                !mutation.isSubmitted -> record.revision == expectedRevision
                record.revision == expectedRevision -> true
                record.revision > expectedRevision &&
                    mutation.disposition == CanonicalAuthoringDisposition.CONFLICTED -> true
                else -> false
            }
            if (!revisionIsValid) {
                throw SerializationException("Restore journal revision does not match deleted state")
            }
            val cutoff = Instant.ofEpochMilli(referenceEpochMillis)
                .minusSeconds(CanonicalTrashRetentionPolicy.RETENTION_SECONDS)
            if (Instant.parse(requireNotNull(record.retentionAnchorAt)) < cutoff &&
                mutation.baseItem != null) {
                throw SerializationException("Expired restore journal retains a full item body")
            }
        }
        val cutoff = Instant.ofEpochMilli(referenceEpochMillis)
            .minusSeconds(CanonicalTrashRetentionPolicy.RETENTION_SECONDS)
        mutations.filter { it.operation == CanonicalAuthoringOperation.TRASH }.forEach { mutation ->
            if (Instant.parse(mutation.createdAt) < cutoff && mutation.baseItem != null) {
                throw SerializationException("Expired trash journal retains a full item body")
            }
        }

        val parentById = state.canonicalItems.associate { it.id to it.parentId }.toMutableMap()
        mutations.filter {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }.forEach { mutation ->
            when (mutation.operation) {
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> parentById[mutation.itemId] = requireNotNull(mutation.draft).parentId
                CanonicalAuthoringOperation.TRASH -> parentById.remove(mutation.itemId)
                CanonicalAuthoringOperation.RESTORE -> {
                    val record = deleted.first { it.id == mutation.itemId }
                    parentById[mutation.itemId] = record.parentId
                }
            }
        }
        parentById.values.filterNotNull().forEach { parentId ->
            if (parentId !in parentById) {
                throw SerializationException("Canonical authoring hierarchy has a missing parent")
            }
        }
        parentById.keys.forEach { start ->
            val visited = mutableSetOf<String>()
            var current: String? = start
            while (current != null) {
                if (!visited.add(current)) {
                    throw SerializationException("Canonical authoring hierarchy contains a cycle")
                }
                current = parentById[current]
            }
        }
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
        state.publishedScheduleProof?.let { proof ->
            if (
                !proof.hasValidShape() || !proof.matchesStateBinding(state) ||
                !proof.matchesPublishedPlan(state.schedule)
            ) {
                throw SerializationException("Exact published schedule proof does not match cache")
            }
        }
    }

    private fun validateProposalApplicationState(state: DayWeaveUiState) {
        state.pendingProposalApplicationMutation?.let { pending ->
            if (pending.schemaVersion != PROPOSAL_APPLICATION_JOURNAL_VERSION) {
                throw SerializationException("Unsupported proposal application journal")
            }
            validateUuid(pending.idempotencyKey, "proposal application idempotency key")
            validateUuid(pending.proposalId, "proposal application proposal")
            if (pending.expectedProposalRevision <= 0 || pending.syncOrigin.isBlank()) {
                throw SerializationException("Invalid proposal application binding")
            }
            if (pending.expectedCommandIds.isEmpty() || pending.expectedCommandIds.size > 100 ||
                pending.expectedCommandIds.distinct().size != pending.expectedCommandIds.size) {
                throw SerializationException("Invalid proposal application command fence")
            }
            pending.expectedCommandIds.forEach {
                validateUuid(it, "proposal application command")
            }
            pending.configurationId?.takeIf(String::isBlank)?.let {
                throw SerializationException("Invalid proposal application configuration")
            }
            runCatching { Instant.parse(pending.preparedAt) }.getOrElse {
                throw SerializationException("Invalid proposal application timestamp", it)
            }
            when (pending.kind) {
                ProposalApplicationMutationKind.APPLY -> {
                    val previewId = pending.previewId
                        ?: throw SerializationException("Apply preview is required")
                    validateUuid(
                        previewId,
                        "proposal application preview",
                    )
                    val hash = pending.expectedReviewHash
                        ?: throw SerializationException("Apply review hash is required")
                    if (!hash.isSha256Digest() || pending.applicationId != null ||
                        pending.expectedApplicationRevision != null) {
                        throw SerializationException("Invalid apply recovery journal")
                    }
                    runCatching {
                        validateProposalApplyHttpRequest(
                            expectedBaseUrl = pending.syncOrigin,
                            request = pending.request,
                            previewId = previewId,
                            expectedReviewHash = hash,
                        )
                    }.getOrElse {
                        throw SerializationException("Invalid exact apply request", it)
                    }
                }
                ProposalApplicationMutationKind.UNDO -> {
                    val applicationId = pending.applicationId
                        ?: throw SerializationException("Undo application is required")
                    validateUuid(
                        applicationId,
                        "proposal application",
                    )
                    val expectedApplicationRevision = pending.expectedApplicationRevision
                    if (expectedApplicationRevision?.let { it > 0 } != true ||
                        pending.previewId != null || pending.expectedReviewHash != null) {
                        throw SerializationException("Invalid undo recovery journal")
                    }
                    runCatching {
                        validateProposalUndoHttpRequest(
                            expectedBaseUrl = pending.syncOrigin,
                            request = pending.request,
                            applicationId = applicationId,
                            expectedApplicationRevision = expectedApplicationRevision,
                        )
                    }.getOrElse {
                        throw SerializationException("Invalid exact undo request", it)
                    }
                }
            }
        }
        state.proposalApplications.forEach { (proposalId, receipt) ->
            validateUuid(proposalId, "proposal receipt key")
            validateUuid(receipt.proposalId, "proposal receipt proposal")
            validateUuid(receipt.applicationId, "proposal receipt application")
            if (
                proposalId != receipt.proposalId ||
                receipt.schemaVersion != PROPOSAL_APPLICATION_RECEIPT_VERSION ||
                receipt.syncOrigin.isBlank() || receipt.appliedProposalRevision <= 0 ||
                receipt.configurationId?.isBlank() == true ||
                receipt.commandIds.isEmpty() || receipt.commandIds.size > 100 ||
                receipt.affectedItemIds.isEmpty() ||
                receipt.commandIds.distinct().size != receipt.commandIds.size ||
                receipt.affectedItemIds.distinct().size != receipt.affectedItemIds.size
            ) {
                throw SerializationException("Invalid proposal application receipt")
            }
            receipt.commandIds.forEach { validateUuid(it, "proposal receipt command") }
            receipt.affectedItemIds.forEach { validateUuid(it, "proposal receipt item") }
            val appliedAt = runCatching { Instant.parse(receipt.appliedAt) }.getOrElse {
                throw SerializationException("Invalid proposal application time", it)
            }
            val undoExpiresAt = runCatching { Instant.parse(receipt.undoExpiresAt) }.getOrElse {
                throw SerializationException("Invalid proposal undo deadline", it)
            }
            if (undoExpiresAt <= appliedAt) {
                throw SerializationException("Invalid proposal undo window")
            }
            when (receipt.status) {
                ProposalApplicationStatusSnapshot.APPLIED -> if (
                    receipt.undoneAt != null || receipt.applicationRevision != 1L
                ) throw SerializationException("Applied proposal receipt is invalid")
                ProposalApplicationStatusSnapshot.UNDONE -> {
                    val undoneAt = receipt.undoneAt?.let { raw ->
                        runCatching { Instant.parse(raw) }.getOrElse {
                            throw SerializationException("Invalid proposal undo time", it)
                        }
                    } ?: throw SerializationException("Undone proposal receipt needs a timestamp")
                    if (
                        receipt.applicationRevision != 2L || undoneAt < appliedAt ||
                        undoneAt > undoExpiresAt
                    ) {
                        throw SerializationException("Invalid undone proposal receipt")
                    }
                }
            }
        }
        state.pendingProposalApplicationMutation?.let { pending ->
            val receipt = state.proposalApplications[pending.proposalId]
            if (pending.kind == ProposalApplicationMutationKind.APPLY && receipt != null) {
                throw SerializationException("Apply journal conflicts with an existing receipt")
            }
            if (pending.kind == ProposalApplicationMutationKind.UNDO &&
                (receipt == null || receipt.applicationId != pending.applicationId ||
                    receipt.applicationRevision != pending.expectedApplicationRevision ||
                    receipt.appliedProposalRevision != pending.expectedProposalRevision ||
                    receipt.commandIds != pending.expectedCommandIds ||
                    receipt.status != ProposalApplicationStatusSnapshot.APPLIED)) {
                throw SerializationException("Undo journal does not match its receipt")
            }
        }
        val applicationBindings = state.proposalApplications.values
            .map { it.syncOrigin to it.configurationId }
            .toSet()
        if (applicationBindings.size > 1) {
            throw SerializationException("Proposal receipts cross API bindings")
        }
        state.pendingProposalApplicationMutation?.let { pending ->
            if (applicationBindings.any {
                it != (pending.syncOrigin to pending.configurationId)
            }) {
                throw SerializationException("Proposal journal crosses its receipt binding")
            }
        }
        val proposalBinding = state.pendingProposalApplicationMutation
            ?.let { it.syncOrigin to it.configurationId }
            ?: applicationBindings.singleOrNull()
        proposalBinding?.let { (origin, configurationId) ->
            if (state.canonicalSyncOrigin != null &&
                (state.canonicalSyncOrigin != origin ||
                    state.canonicalConfigurationId != configurationId)) {
                throw SerializationException("Proposal state crosses the canonical binding")
            }
            if (state.canonicalExecutionSyncOrigin != null &&
                (state.canonicalExecutionSyncOrigin != origin ||
                    state.canonicalExecutionConfigurationId != configurationId)) {
                throw SerializationException("Proposal state crosses the execution binding")
            }
        }
    }

    private fun validateUuid(value: String, description: String) {
        val parsed = runCatching { UUID.fromString(value) }.getOrNull()
        if (parsed == null || parsed == UUID(0L, 0L) || parsed.toString() != value) {
            throw SerializationException("Invalid $description")
        }
    }

    private fun String.isSha256Digest(): Boolean =
        length == 71 && startsWith("sha256:") && drop(7).all { it in '0'..'9' || it in 'a'..'f' }

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
        const val PROPOSAL_APPLICATION_JOURNAL_VERSION = 1
        const val PROPOSAL_APPLICATION_RECEIPT_VERSION = 1
        const val MAX_CANONICAL_AUTHORING_MUTATIONS = 100
        const val MAX_RECENTLY_DELETED = CanonicalTrashRetentionPolicy.MAX_ENTRIES
        val TIMED_BREAK_NOTIFICATION_RECEIPT_FIELDS = setOf(
            "lastBreakEndNotificationAttemptDigest",
            "lastConsumedBreakEndNotificationDigest",
            "lastRejectedBreakEndNotificationDigest",
            "acknowledgedBreakEndDigest",
        )
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
