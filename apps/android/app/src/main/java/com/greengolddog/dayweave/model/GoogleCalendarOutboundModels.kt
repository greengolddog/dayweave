package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleOutboundApproval
import com.greengolddog.dayweave.network.RemoteGoogleOutboundPreview
import com.greengolddog.dayweave.network.RemoteGoogleSyncCollection
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import java.nio.charset.StandardCharsets
import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import java.util.Base64
import java.util.Locale
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull

@Serializable
enum class GoogleCalendarOutboundStage {
    @SerialName("intent")
    INTENT,

    @SerialName("previewed")
    PREVIEWED,

    @SerialName("approval_attempted")
    APPROVAL_ATTEMPTED,

    @SerialName("approved")
    APPROVED,
}

/** One-time bearer authority retained only inside the encrypted planner snapshot. */
@Serializable
data class GoogleCalendarOutboundApprovalCapability(val value: String) {
    init {
        require(isValidGoogleCalendarOutboundCapability(value)) {
            "Google Calendar outbound approval capability is invalid"
        }
    }

    override fun toString(): String = "GoogleCalendarOutboundApprovalCapability(<redacted>)"
}

/**
 * Exact reviewed provider mutation returned by the API.
 *
 * This deliberately copies the network response into a versioned model-layer contract so future
 * transport changes cannot silently alter encrypted recovery authority.
 */
@Serializable
data class GoogleCalendarOutboundPreviewSnapshot(
    val id: String,
    val accountId: String,
    val collectionId: String,
    val collectionRevision: Long,
    val collectionDisplayName: String,
    val itemId: String,
    val itemRevision: Long,
    val entityKind: GoogleCalendarOutboundEntityKind,
    val operation: GoogleCalendarOutboundOperation,
    val providerResourceId: String? = null,
    val providerEtag: String? = null,
    val previewHash: String,
    val providerPayload: JsonObject,
    val expiresAt: String,
) {
    init {
        requireValidShape()
    }

    fun requireValidShape() {
        requireCanonicalOutboundUuid(id)
        requireCanonicalOutboundUuid(accountId)
        requireCanonicalOutboundUuid(collectionId)
        requireCanonicalOutboundUuid(itemId)
        require(collectionRevision > 0 && itemRevision > 0)
        requireValidOutboundText(collectionDisplayName, MAX_COLLECTION_DISPLAY_NAME_BYTES)
        require(
            when {
                operation == GoogleCalendarOutboundOperation.UPSERT &&
                    providerResourceId == null && providerEtag == null -> true
                providerResourceId != null && providerEtag != null -> {
                    isValidOutboundText(providerResourceId, MAX_PROVIDER_BINDING_BYTES) &&
                        isValidOutboundText(providerEtag, MAX_PROVIDER_BINDING_BYTES)
                }
                else -> false
            },
        ) { "Google Calendar outbound provider binding is incomplete" }
        require(isValidGoogleCalendarOutboundHash(previewHash))
        require(
            providerPayload.hasValidOutboundBudget(
                maximumValueStringBytes = if (
                    entityKind == GoogleCalendarOutboundEntityKind.TASK
                ) {
                    MAX_TASK_VALUE_STRING_BYTES
                } else {
                    MAX_PROVIDER_VALUE_STRING_BYTES
                },
            ) && providerPayload.isValidGoogleOutboundProjection(entityKind, operation),
        ) { "Google Calendar outbound provider payload is invalid" }
        requireCanonicalOutboundInstant(expiresAt)
    }

    /** Private provider payloads, hashes, and identifiers never enter diagnostics. */
    override fun toString(): String =
        "GoogleCalendarOutboundPreviewSnapshot(entityKind=$entityKind, " +
            "operation=$operation, providerPayload=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteGoogleOutboundPreview): GoogleCalendarOutboundPreviewSnapshot =
            GoogleCalendarOutboundPreviewSnapshot(
                id = remote.id,
                accountId = remote.accountId,
                collectionId = remote.collectionId,
                collectionRevision = remote.collectionRevision,
                collectionDisplayName = remote.collectionDisplayName,
                itemId = remote.itemId,
                itemRevision = remote.itemRevision,
                entityKind = remote.entityKind,
                operation = remote.operation,
                providerResourceId = remote.providerResourceId,
                providerEtag = remote.providerEtag,
                previewHash = remote.previewHash,
                providerPayload = remote.providerPayload,
                expiresAt = remote.expiresAt,
            )
    }
}

/**
 * Complete crash-recovery fence for one explicitly reviewed Google mutation.
 *
 * Approval is one-shot. The attempted marker must become durable before asking the server to mint
 * a capability, and the capability must become durable before enqueue. A validated authoritative
 * acceptance is the only ordinary path that clears an approved journal.
 */
@Serializable
data class GoogleCalendarOutboundJournal(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val recoveryId: String,
    val operationGeneration: Long,
    val configurationId: String,
    val apiBaseUrl: String,
    val accountId: String,
    val collectionId: String,
    val itemId: String,
    val expectedItemRevision: Long,
    val entityKind: GoogleCalendarOutboundEntityKind,
    val operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
    val intentExpiresAt: String,
    val preview: GoogleCalendarOutboundPreviewSnapshot? = null,
    val approvalAttempted: Boolean = false,
    val approvalCapability: GoogleCalendarOutboundApprovalCapability? = null,
    val approvalExpiresAt: String? = null,
    val createdAt: String,
) {
    init {
        requireValidShape()
    }

    val stage: GoogleCalendarOutboundStage
        get() = when {
            approvalCapability != null -> GoogleCalendarOutboundStage.APPROVED
            approvalAttempted -> GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED
            preview != null -> GoogleCalendarOutboundStage.PREVIEWED
            else -> GoogleCalendarOutboundStage.INTENT
        }

    fun requireValidShape() {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        requireCanonicalOutboundUuid(recoveryId)
        require(operationGeneration > 0)
        requireSafeOutboundConfigurationId(configurationId)
        requireCanonicalOutboundApiBaseUrl(apiBaseUrl)
        requireCanonicalOutboundUuid(accountId)
        requireCanonicalOutboundUuid(collectionId)
        requireCanonicalOutboundUuid(itemId)
        require(expectedItemRevision > 0)
        val created = requireCanonicalOutboundInstant(createdAt)
        val intentExpiry = requireCanonicalOutboundInstant(intentExpiresAt)
        require(intentExpiry > created)
        require(Duration.between(created, intentExpiry) <= MAXIMUM_INTENT_LIFETIME)

        preview?.let { exactPreview ->
            exactPreview.requireValidShape()
            val previewExpiry = requireCanonicalOutboundInstant(exactPreview.expiresAt)
            require(
                exactPreview.accountId == accountId &&
                    exactPreview.collectionId == collectionId &&
                    exactPreview.itemId == itemId &&
                    exactPreview.itemRevision == expectedItemRevision &&
                    exactPreview.entityKind == entityKind &&
                    exactPreview.operation == operation,
            ) { "Google Calendar outbound preview does not match its recovery intent" }
            require(previewExpiry >= created.minus(MAXIMUM_CLOCK_SKEW))
            require(previewExpiry <= intentExpiry.plus(MAXIMUM_CLOCK_SKEW))
        }
        require(preview != null || !approvalAttempted)
        require(preview != null || approvalCapability == null && approvalExpiresAt == null)
        require(approvalCapability == null || approvalAttempted)
        require((approvalCapability == null) == (approvalExpiresAt == null))
        approvalExpiresAt?.let { rawExpiry ->
            val approvalExpiry = requireCanonicalOutboundInstant(rawExpiry)
            val previewExpiry = requireCanonicalOutboundInstant(requireNotNull(preview).expiresAt)
            require(approvalExpiry >= created.minus(MAXIMUM_CLOCK_SKEW))
            require(approvalExpiry <= previewExpiry)
        }
    }

    fun isValidAt(now: Instant): Boolean = runCatching {
        requireValidShape()
        require(requireCanonicalOutboundInstant(createdAt) <= now.plus(MAXIMUM_CLOCK_SKEW))
    }.isSuccess

    fun recordingPreview(
        remote: RemoteGoogleOutboundPreview,
    ): GoogleCalendarOutboundJournal = recordingPreview(
        GoogleCalendarOutboundPreviewSnapshot.fromRemote(remote),
    )

    fun recordingPreview(
        exactPreview: GoogleCalendarOutboundPreviewSnapshot,
    ): GoogleCalendarOutboundJournal {
        require(stage == GoogleCalendarOutboundStage.INTENT)
        return copy(preview = exactPreview)
    }

    fun recordingApprovalAttempt(): GoogleCalendarOutboundJournal {
        require(stage == GoogleCalendarOutboundStage.PREVIEWED)
        return copy(approvalAttempted = true)
    }

    fun recordingApproval(
        remote: RemoteGoogleOutboundApproval,
    ): GoogleCalendarOutboundJournal {
        require(stage == GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED)
        require(remote.previewId == requireNotNull(preview).id)
        return copy(
            approvalCapability = GoogleCalendarOutboundApprovalCapability(
                remote.approvalCapability,
            ),
            approvalExpiresAt = remote.expiresAt,
        )
    }

    fun canTransitionTo(replacement: GoogleCalendarOutboundJournal): Boolean {
        if (this == replacement) return true
        if (!hasSameImmutableIntent(replacement)) return false
        return when (stage to replacement.stage) {
            GoogleCalendarOutboundStage.INTENT to GoogleCalendarOutboundStage.PREVIEWED ->
                replacement.preview?.let { runCatching { recordingPreview(it) }.getOrNull() } ==
                    replacement
            GoogleCalendarOutboundStage.PREVIEWED to
                GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED ->
                runCatching { recordingApprovalAttempt() }.getOrNull() == replacement
            GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED to
                GoogleCalendarOutboundStage.APPROVED -> {
                val capability = replacement.approvalCapability ?: return false
                val expiresAt = replacement.approvalExpiresAt ?: return false
                copy(
                    approvalCapability = capability,
                    approvalExpiresAt = expiresAt,
                ) == replacement
            }
            else -> false
        }
    }

    fun authorityExpiresAt(): Instant = when (stage) {
        GoogleCalendarOutboundStage.INTENT -> requireCanonicalOutboundInstant(intentExpiresAt)
        GoogleCalendarOutboundStage.PREVIEWED,
        GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED,
        -> requireCanonicalOutboundInstant(requireNotNull(preview).expiresAt)
        GoogleCalendarOutboundStage.APPROVED ->
            requireCanonicalOutboundInstant(requireNotNull(approvalExpiresAt))
    }

    fun safeDiscardAt(): Instant = authorityExpiresAt().plus(MAXIMUM_CLOCK_SKEW)

    fun canDiscardExpiredAt(now: Instant): Boolean = !now.isBefore(safeDiscardAt())

    private fun hasSameImmutableIntent(other: GoogleCalendarOutboundJournal): Boolean =
        schemaVersion == other.schemaVersion &&
            recoveryId == other.recoveryId &&
            operationGeneration == other.operationGeneration &&
            configurationId == other.configurationId &&
            apiBaseUrl == other.apiBaseUrl &&
            accountId == other.accountId &&
            collectionId == other.collectionId &&
            itemId == other.itemId &&
            expectedItemRevision == other.expectedItemRevision &&
            entityKind == other.entityKind &&
            operation == other.operation &&
            intentExpiresAt == other.intentExpiresAt &&
            createdAt == other.createdAt

    /** Capabilities, provider payloads, hashes, and all bound identifiers stay redacted. */
    override fun toString(): String =
        "GoogleCalendarOutboundJournal(stage=$stage, entityKind=$entityKind, " +
            "operation=$operation, binding=<redacted>, " +
            "target=<redacted>, capability=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 2
        val MAXIMUM_INTENT_LIFETIME: Duration = Duration.ofMinutes(35)
        val MAXIMUM_CLOCK_SKEW: Duration = Duration.ofMinutes(5)
    }
}

/** Current or retained canonical identity accepted by Android's outbound-only coordinator. */
data class GoogleCalendarOutboundCandidate(
    val itemId: String,
    val expectedItemRevision: Long,
    val entityKind: GoogleCalendarOutboundEntityKind =
        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
    val operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
) {
    init {
        requireCanonicalOutboundUuid(itemId)
        require(expectedItemRevision > 0)
    }

    override fun toString(): String =
        "GoogleCalendarOutboundCandidate(entityKind=$entityKind, operation=$operation, " +
            "item=<redacted>)"
}

/** Selected writable Google identity accepted by Android's outbound-only coordinator. */
data class GoogleCalendarOutboundTarget(
    val accountId: String,
    val collectionId: String,
    val collectionRevision: Long,
    val entityKind: GoogleCalendarOutboundEntityKind =
        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
    val operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
) {
    init {
        requireCanonicalOutboundUuid(accountId)
        requireCanonicalOutboundUuid(collectionId)
        require(collectionRevision > 0)
    }

    override fun toString(): String =
        "GoogleCalendarOutboundTarget(entityKind=$entityKind, operation=$operation, " +
            "account=<redacted>, collection=<redacted>)"
}

/**
 * Parses only a synced app-owned event/task or its retained recent-trash identity, with no write
 * uncertainty for that item. Provider-imported Tasks fail the canonical authoring projection gate.
 */
fun DayWeaveUiState.googleCalendarOutboundCandidate(
    itemId: String,
): GoogleCalendarOutboundCandidate? = runCatching {
    requireCanonicalOutboundUuid(itemId)
    require(!canonicalSyncOrigin.isNullOrBlank())
    require(!canonicalConfigurationId.isNullOrBlank())
    require(!canonicalDeltaCursor.isNullOrBlank())
    require(pendingCanonicalMutation == null)
    require(pendingCanonicalAuthoringMutations.none { it.itemId == itemId })
    val active = canonicalItems.singleOrNull { it.id == itemId && it.deletedAt == null }
    val retained = canonicalRecentlyDeleted.singleOrNull { it.id == itemId }
    require((active != null) xor (retained != null))
    if (active != null) {
        active.googleOutboundCandidate(
            expectedItemRevision = active.revision,
            operation = GoogleCalendarOutboundOperation.UPSERT,
        )
    } else {
        val record = requireNotNull(retained)
        record.requireValid()
        requireNotNull(record.lastKnownItem).googleOutboundCandidate(
            expectedItemRevision = record.revision,
            operation = GoogleCalendarOutboundOperation.DELETE,
        )
    }
}.getOrNull()

private fun CanonicalItemSnapshot.googleOutboundCandidate(
    expectedItemRevision: Long,
    operation: GoogleCalendarOutboundOperation,
): GoogleCalendarOutboundCandidate {
    require(revision > 0 && expectedItemRevision >= revision)
    return when (kind) {
        "event" -> {
            if (operation == GoogleCalendarOutboundOperation.UPSERT) require(status == "planned")
            val draft = copy(status = "planned", deletedAt = null).toCanonicalDraft()
            val timing = requireNotNull(draft.eventTiming)
            require(draft.kind == ItemKind.EVENT)
            if (operation == GoogleCalendarOutboundOperation.UPSERT) {
                require(!timing.allDay && !timing.tentative && timing.busy)
            }
            GoogleCalendarOutboundCandidate(
                itemId = id,
                expectedItemRevision = expectedItemRevision,
                entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
                operation = operation,
            )
        }
        "task" -> {
            val reviewable = if (operation == GoogleCalendarOutboundOperation.DELETE) {
                // Deletion sends no canonical body. Retain only enough parsing to prove this was
                // app-authored rather than a `google_sync` import, so newer task fields cannot
                // strand an already-owned provider mapping.
                copy(
                    status = "planned",
                    deletedAt = null,
                    recurrenceJson = null,
                    splitPolicyJson = "{\"type\":\"indivisible\"}",
                )
            } else {
                require(status in GOOGLE_TASK_PUBLISHABLE_STATUSES)
                require(recurrenceJson == null)
                copy(status = "planned", deletedAt = null)
            }
            val draft = reviewable.toCanonicalDraft()
            require(draft.kind == ItemKind.TASK)
            GoogleCalendarOutboundCandidate(
                itemId = id,
                expectedItemRevision = expectedItemRevision,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = operation,
            )
        }
        else -> throw IllegalArgumentException("Unsupported Google outbound entity")
    }
}

/**
 * Applies the complete Android target gate without retaining account labels or provider IDs.
 */
fun googleCalendarOutboundTarget(
    accountId: String,
    accountStatus: String,
    accountSyncEnabled: Boolean,
    accountHasCalendarWriteScope: Boolean,
    collection: RemoteGoogleSyncCollection,
    entityKind: GoogleCalendarOutboundEntityKind =
        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
    operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
    accountHasTasksWriteScope: Boolean = false,
): GoogleCalendarOutboundTarget? = runCatching {
    requireCanonicalOutboundUuid(accountId)
    require(accountStatus == "active" && accountSyncEnabled)
    require(
        when (entityKind) {
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> accountHasCalendarWriteScope
            GoogleCalendarOutboundEntityKind.TASK -> accountHasTasksWriteScope
        },
    )
    require(collection.accountId == accountId)
    requireCanonicalOutboundUuid(collection.id)
    require(
        collection.kind == when (entityKind) {
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> RemoteGoogleCollectionKind.CALENDAR
            GoogleCalendarOutboundEntityKind.TASK -> RemoteGoogleCollectionKind.TASK_LIST
        },
    )
    require(collection.selected && !collection.providerDeleted)
    require(collection.syncRole == RemoteGoogleSyncRole.WRITABLE)
    require(collection.revision > 0)
    if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
        require(
            collection.providerAccessRole?.lowercase(Locale.ROOT) in setOf("owner", "writer"),
        )
    }
    GoogleCalendarOutboundTarget(
        accountId = accountId,
        collectionId = collection.id,
        collectionRevision = collection.revision,
        entityKind = entityKind,
        operation = operation,
    )
}.getOrNull()

private fun requireCanonicalOutboundUuid(value: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed != ZERO_UUID && parsed.toString() == value) {
        "Google Calendar outbound identifier is invalid"
    }
}

private fun requireCanonicalOutboundInstant(value: String): Instant =
    Instant.parse(value).also { parsed ->
        require(parsed.toString() == value) { "Google Calendar outbound timestamp is not canonical" }
    }

private fun requireSafeOutboundConfigurationId(value: String) {
    require(
        value.length in 1..MAX_CONFIGURATION_ID_CHARS &&
            value.all { it.code in 0x21..0x7e },
    ) { "Google Calendar outbound configuration binding is invalid" }
}

private fun requireCanonicalOutboundApiBaseUrl(value: String) {
    require(
        value.length in 1..MAX_API_BASE_URL_CHARS && normalizedHttpsApiBaseUrl(value) == value,
    ) { "Google Calendar outbound API binding is invalid" }
}

private fun requireValidOutboundText(value: String, maximumUtf8Bytes: Int) {
    require(isValidOutboundText(value, maximumUtf8Bytes)) {
        "Google Calendar outbound text is invalid"
    }
}

private fun isValidOutboundText(value: String, maximumUtf8Bytes: Int): Boolean =
    value.isNotEmpty() && StandardCharsets.UTF_8.newEncoder().canEncode(value) &&
        value.toByteArray(StandardCharsets.UTF_8).size <= maximumUtf8Bytes &&
        value.none(Char::isISOControl)

private fun isValidGoogleCalendarOutboundHash(value: String): Boolean =
    value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }

private fun isValidGoogleCalendarOutboundCapability(value: String): Boolean {
    val prefix = "dw_ga1_"
    if (!value.startsWith(prefix)) return false
    val payload = value.substring(prefix.length)
    if (
        payload.length != 43 || payload.any { character ->
            character !in 'A'..'Z' && character !in 'a'..'z' &&
                character !in '0'..'9' && character != '-' && character != '_'
        }
    ) {
        return false
    }
    return runCatching {
        val decoded = Base64.getUrlDecoder().decode(payload)
        decoded.size == 32 && Base64.getUrlEncoder().withoutPadding()
            .encodeToString(decoded) == payload
    }.getOrDefault(false)
}

private fun JsonObject.hasValidOutboundBudget(
    maximumValueStringBytes: Int = MAX_PROVIDER_VALUE_STRING_BYTES,
): Boolean {
    var nodes = 0
    var stringBytes = 0L

    fun consumeString(value: String, maximumBytes: Int, forbidControls: Boolean): Boolean {
        if (!StandardCharsets.UTF_8.newEncoder().canEncode(value)) return false
        val bytes = value.toByteArray(StandardCharsets.UTF_8).size
        if (bytes > maximumBytes || stringBytes + bytes > MAX_PROVIDER_STRING_BYTES) return false
        if (forbidControls && value.any(Char::isISOControl)) return false
        stringBytes += bytes
        return true
    }

    fun validate(element: JsonElement, depth: Int): Boolean {
        if (depth > MAX_PROVIDER_DEPTH || nodes >= MAX_PROVIDER_NODES) return false
        nodes += 1
        return when (element) {
            is JsonObject -> element.size <= MAX_PROVIDER_CONTAINER_ENTRIES &&
                element.all { (key, value) ->
                    consumeString(key, MAX_PROVIDER_KEY_BYTES, forbidControls = true) &&
                        validate(value, depth + 1)
                }
            is JsonArray -> element.size <= MAX_PROVIDER_CONTAINER_ENTRIES &&
                element.all { validate(it, depth + 1) }
            is JsonPrimitive -> if (element.isString) {
                consumeString(
                    element.content,
                    maximumValueStringBytes,
                    forbidControls = false,
                )
            } else {
                element.content.length <= MAX_PROVIDER_NUMBER_BYTES
            }
            JsonNull -> true
        }
    }

    return size <= MAX_PROVIDER_CONTAINER_ENTRIES && validate(this, depth = 0)
}

private fun JsonObject.isValidGoogleOutboundProjection(
    entityKind: GoogleCalendarOutboundEntityKind,
    operation: GoogleCalendarOutboundOperation,
): Boolean = when (operation) {
    GoogleCalendarOutboundOperation.DELETE -> isEmpty()
    GoogleCalendarOutboundOperation.UPSERT -> when (entityKind) {
        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT ->
            isNotEmpty() && isValidPrivateFixedCalendarEvent()
        GoogleCalendarOutboundEntityKind.TASK ->
            isNotEmpty() && isValidGoogleTaskPreviewPayload()
    }
}

private fun JsonObject.isValidGoogleTaskPreviewPayload(): Boolean {
    if (keys != GOOGLE_TASK_ROOT_KEYS) return false
    val id = this["id"].outboundStringOrNull()
    val title = this["title"].outboundStringOrNull()
    val status = this["status"].outboundStringOrNull()
    val completed = this["completed"]
    return id != null && id.isEmpty() &&
        this["etag"] == JsonNull &&
        title != null && title.isValidGoogleTaskTitle() &&
        this["notes"].isValidGoogleTaskNotes() &&
        status in setOf("needsAction", "completed") &&
        this["due"].isValidGoogleTaskTimestamp(required = false) &&
        completed.isValidGoogleTaskTimestamp(required = status == "completed") &&
        (status == "completed") == (completed != JsonNull) &&
        this["updated"] == JsonNull &&
        this["parent"] == JsonNull &&
        this["position"] == JsonNull &&
        this["links"] == JsonNull &&
        this["deleted"].outboundBooleanOrNull() == false &&
        this["hidden"].outboundBooleanOrNull() == false
}

private fun String.isValidGoogleTaskTitle(): Boolean =
    isNotEmpty() && codePointCount(0, length) <= MAX_GOOGLE_TASK_TITLE_CODE_POINTS &&
        this == trim()

private fun JsonElement?.isValidGoogleTaskNotes(): Boolean = when (this) {
    JsonNull -> true
    is JsonPrimitive -> isString && content.let { notes ->
        notes.isNotEmpty() &&
            notes.codePointCount(0, notes.length) <= MAX_GOOGLE_TASK_NOTES_CODE_POINTS &&
            notes == notes.trim() &&
            notes.all { character -> character == '\n' || !character.isISOControl() } &&
            notes.split('\n').all { line -> line.isNotEmpty() && line == line.trim() } &&
            !notes.containsLegacyGoogleTaskMarker()
    }
    else -> false
}

private fun String.containsLegacyGoogleTaskMarker(): Boolean {
    val lower = lowercase()
    var searchFrom = 0
    while (searchFrom < lower.length) {
        val marker = lower.indexOf("[dayweave", startIndex = searchFrom)
        if (marker < 0) return false
        var suffix = marker + "[dayweave".length
        while (suffix < lower.length && lower[suffix].isWhitespace()) suffix += 1
        if (lower.startsWith("item:", startIndex = suffix)) return true
        searchFrom = marker + "[dayweave".length
    }
    return false
}

private fun JsonElement?.isValidGoogleTaskTimestamp(required: Boolean): Boolean = when (this) {
    JsonNull -> !required
    is JsonPrimitive -> isString && content.let { timestamp ->
        timestamp.isNotEmpty() && timestamp.toByteArray(StandardCharsets.UTF_8).size <= 64 &&
            offsetDateTimeOrNull(timestamp) != null
    }
    else -> false
}

/**
 * Revalidates the first outbound slice independently of the transport-layer validator. Persisted
 * bytes therefore cannot acquire approval authority merely because a future DTO becomes broader.
 */
private fun JsonObject.isValidPrivateFixedCalendarEvent(): Boolean {
    if (keys != CALENDAR_EVENT_ROOT_KEYS) return false
    val providerId = this["id"].outboundStringOrNull()
    val summary = this["summary"] as? JsonPrimitive
    val status = this["status"] as? JsonPrimitive
    val transparency = this["transparency"] as? JsonPrimitive
    val visibility = this["visibility"] as? JsonPrimitive
    val eventType = this["eventType"] as? JsonPrimitive
    val start = this["start"] as? JsonObject
    val end = this["end"] as? JsonObject
    val requiredShapeIsValid = providerId.isValidDayWeaveCalendarEventId() &&
        this["etag"] == JsonNull &&
        summary?.isString == true &&
        isValidOutboundText(summary.content, MAX_CALENDAR_SUMMARY_BYTES) &&
        summary.content.codePointCount(0, summary.content.length) <= MAX_CALENDAR_SUMMARY_CODE_POINTS &&
        this["description"].isValidCalendarDescription() &&
        this["location"] == JsonNull &&
        status?.isString == true && status.content == "confirmed" &&
        transparency?.isString == true && transparency.content == "opaque" &&
        visibility?.isString == true && visibility.content == "private" &&
        eventType?.isString == true && eventType.content == "default" &&
        start != null && end != null && haveValidCalendarBoundaries(start, end)
    val collaborationFieldsAreInert =
        (this["attendees"] as? JsonArray)?.isEmpty() == true &&
            (this["attachments"] as? JsonArray)?.isEmpty() == true &&
            (this["recurrence"] as? JsonArray)?.isEmpty() == true &&
            this["conferenceData"] == JsonNull &&
            this["recurringEventId"] == JsonNull &&
            this["originalStartTime"] == JsonNull &&
            this["updated"] == JsonNull &&
            this["sequence"] == JsonNull &&
            this["extendedProperties"].hasValidDayWeaveOwnershipProof()
    return requiredShapeIsValid && collaborationFieldsAreInert
}

private fun String?.isValidDayWeaveCalendarEventId(): Boolean =
    this != null && length in MIN_CALENDAR_EVENT_ID_CHARS..MAX_CALENDAR_EVENT_ID_CHARS &&
        first() == 'd' && drop(1).all { it in '0'..'9' || it in 'a'..'f' }

private fun JsonElement?.isValidCalendarDescription(): Boolean = when (this) {
    JsonNull -> true
    is JsonPrimitive -> isString &&
        StandardCharsets.UTF_8.newEncoder().canEncode(content) &&
        content.toByteArray(StandardCharsets.UTF_8).size <= MAX_CALENDAR_DESCRIPTION_BYTES &&
        content.codePointCount(0, content.length) <= MAX_CALENDAR_DESCRIPTION_CODE_POINTS &&
        content.all { it == '\n' || it == '\t' || !it.isISOControl() }
    else -> false
}

private fun JsonElement?.hasValidDayWeaveOwnershipProof(): Boolean {
    val properties = this as? JsonObject ?: return false
    if (properties.keys != CALENDAR_EXTENDED_PROPERTY_KEYS) return false
    val privateProperties = properties["private"] as? JsonObject ?: return false
    val sharedProperties = properties["shared"] as? JsonObject ?: return false
    if (privateProperties.keys != CALENDAR_PRIVATE_PROPERTY_KEYS || sharedProperties.isNotEmpty()) {
        return false
    }
    return privateProperties[DAYWEAVE_OWNERSHIP_PROOF_KEY].outboundStringOrNull() ==
        DAYWEAVE_REDACTED_OWNERSHIP_PROOF
}

private fun haveValidCalendarBoundaries(start: JsonObject, end: JsonObject): Boolean {
    if (start.keys != CALENDAR_BOUNDARY_KEYS || end.keys != CALENDAR_BOUNDARY_KEYS) return false
    val startDate = start["date"].outboundStringOrNull()
    val endDate = end["date"].outboundStringOrNull()
    val startTime = start["dateTime"].outboundStringOrNull()
    val endTime = end["dateTime"].outboundStringOrNull()
    val startZone = start["timeZone"].outboundStringOrNull()
    val endZone = end["timeZone"].outboundStringOrNull()
    if (startZone == null || endZone == null || startZone != endZone || !isValidZoneId(startZone)) {
        return false
    }
    return when {
        startDate == null && endDate == null && startTime != null && endTime != null -> {
            val parsedStart = offsetDateTimeOrNull(startTime)
            val parsedEnd = offsetDateTimeOrNull(endTime)
            parsedStart != null && parsedEnd != null && parsedStart.isBefore(parsedEnd)
        }
        else -> false
    }
}

private fun JsonElement?.outboundStringOrNull(): String? = when (this) {
    JsonNull, null -> null
    is JsonPrimitive -> content.takeIf { isString }
    else -> null
}

private fun JsonElement?.outboundBooleanOrNull(): Boolean? =
    (this as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull

private fun offsetDateTimeOrNull(value: String): OffsetDateTime? = try {
    OffsetDateTime.parse(value, DateTimeFormatter.ISO_OFFSET_DATE_TIME)
} catch (_: DateTimeParseException) {
    null
}

private fun isValidZoneId(value: String): Boolean = try {
    ZoneId.of(value)
    true
} catch (_: java.time.DateTimeException) {
    false
}

private const val MAX_CONFIGURATION_ID_CHARS = 256
private const val MAX_API_BASE_URL_CHARS = 2_048
private const val MAX_COLLECTION_DISPLAY_NAME_BYTES = 4_096
private const val MAX_PROVIDER_BINDING_BYTES = 2_048
private const val MAX_PROVIDER_DEPTH = 32
private const val MAX_PROVIDER_NODES = 20_000
private const val MAX_PROVIDER_STRING_BYTES = 1024L * 1024L
private const val MAX_PROVIDER_CONTAINER_ENTRIES = 10_000
private const val MAX_PROVIDER_KEY_BYTES = 1_024
private const val MAX_PROVIDER_VALUE_STRING_BYTES = 256 * 1_024
private const val MAX_TASK_VALUE_STRING_BYTES = 400_000
private const val MAX_PROVIDER_NUMBER_BYTES = 128
private const val MAX_CALENDAR_SUMMARY_BYTES = 8 * 1_024
private const val MAX_CALENDAR_SUMMARY_CODE_POINTS = 500
private const val MAX_CALENDAR_DESCRIPTION_BYTES = 256 * 1_024
private const val MAX_CALENDAR_DESCRIPTION_CODE_POINTS = 100_000
private const val MAX_GOOGLE_TASK_TITLE_CODE_POINTS = 500
private const val MAX_GOOGLE_TASK_NOTES_CODE_POINTS = 100_000
private const val MIN_CALENDAR_EVENT_ID_CHARS = 66
private const val MAX_CALENDAR_EVENT_ID_CHARS = 73
private const val DAYWEAVE_OWNERSHIP_PROOF_KEY = "dayweaveOwnershipProof"
private const val DAYWEAVE_REDACTED_OWNERSHIP_PROOF = "[server-managed]"
private val CALENDAR_BOUNDARY_KEYS = setOf("date", "dateTime", "timeZone")
private val CALENDAR_EVENT_ROOT_KEYS = setOf(
    "id",
    "etag",
    "status",
    "summary",
    "description",
    "location",
    "start",
    "end",
    "recurringEventId",
    "originalStartTime",
    "recurrence",
    "transparency",
    "visibility",
    "eventType",
    "attendees",
    "conferenceData",
    "attachments",
    "updated",
    "sequence",
    "extendedProperties",
)
private val CALENDAR_EXTENDED_PROPERTY_KEYS = setOf("private", "shared")
private val CALENDAR_PRIVATE_PROPERTY_KEYS = setOf(DAYWEAVE_OWNERSHIP_PROOF_KEY)
private val GOOGLE_TASK_ROOT_KEYS = setOf(
    "id",
    "etag",
    "title",
    "notes",
    "status",
    "due",
    "completed",
    "updated",
    "parent",
    "position",
    "links",
    "deleted",
    "hidden",
)
private val GOOGLE_TASK_PUBLISHABLE_STATUSES = setOf(
    "inbox",
    "planned",
    "scheduled",
    "in_progress",
    "paused",
    "completed",
)
private val ZERO_UUID = UUID(0L, 0L)
