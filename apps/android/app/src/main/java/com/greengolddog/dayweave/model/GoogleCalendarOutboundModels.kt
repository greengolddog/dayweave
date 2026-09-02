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
        require(entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT)
        require(operation == GoogleCalendarOutboundOperation.UPSERT)
        requireValidOutboundText(collectionDisplayName, MAX_COLLECTION_DISPLAY_NAME_BYTES)
        require(
            when {
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
            providerPayload.isNotEmpty() &&
                providerPayload.hasValidOutboundBudget() &&
                providerPayload.isValidPrivateFixedCalendarEvent(),
        ) { "Google Calendar outbound provider payload is invalid" }
        requireCanonicalOutboundInstant(expiresAt)
    }

    /** Private provider payloads, hashes, and identifiers never enter diagnostics. */
    override fun toString(): String =
        "GoogleCalendarOutboundPreviewSnapshot(entityKind=CALENDAR_EVENT, " +
            "operation=UPSERT, providerPayload=<redacted>)"

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
 * Complete crash-recovery fence for one explicitly reviewed Calendar event upsert.
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
        require(operation == GoogleCalendarOutboundOperation.UPSERT)
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
                    exactPreview.entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT &&
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
            operation == other.operation &&
            intentExpiresAt == other.intentExpiresAt &&
            createdAt == other.createdAt

    /** Capabilities, provider payloads, hashes, and all bound identifiers stay redacted. */
    override fun toString(): String =
        "GoogleCalendarOutboundJournal(stage=$stage, binding=<redacted>, " +
            "target=<redacted>, capability=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        val MAXIMUM_INTENT_LIFETIME: Duration = Duration.ofMinutes(35)
        val MAXIMUM_CLOCK_SKEW: Duration = Duration.ofMinutes(5)
    }
}

/** Current canonical event identity accepted by Android's outbound-only coordinator. */
data class GoogleCalendarOutboundCandidate(
    val itemId: String,
    val expectedItemRevision: Long,
    val operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
) {
    init {
        requireCanonicalOutboundUuid(itemId)
        require(expectedItemRevision > 0)
        require(operation == GoogleCalendarOutboundOperation.UPSERT)
    }

    override fun toString(): String =
        "GoogleCalendarOutboundCandidate(operation=UPSERT, item=<redacted>)"
}

/** Selected writable Calendar identity accepted by Android's outbound-only coordinator. */
data class GoogleCalendarOutboundTarget(
    val accountId: String,
    val collectionId: String,
    val collectionRevision: Long,
    val operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
) {
    init {
        requireCanonicalOutboundUuid(accountId)
        requireCanonicalOutboundUuid(collectionId)
        require(collectionRevision > 0)
        require(operation == GoogleCalendarOutboundOperation.UPSERT)
    }

    override fun toString(): String =
        "GoogleCalendarOutboundTarget(account=<redacted>, collection=<redacted>)"
}

/**
 * Parses only a synced, current, app-owned timed event with no write uncertainty for that item.
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
    val item = canonicalItems.single { it.id == itemId }
    require(item.deletedAt == null && item.revision > 0)
    val draft = item.toCanonicalDraft()
    val timing = requireNotNull(draft.eventTiming)
    require(draft.kind == ItemKind.EVENT)
    require(draft.placement == CanonicalDraftPlacement.PLANNED)
    require(!timing.allDay && !timing.tentative && timing.busy)
    GoogleCalendarOutboundCandidate(
        itemId = item.id,
        expectedItemRevision = item.revision,
    )
}.getOrNull()

/**
 * Applies the complete Android target gate without retaining account labels or provider IDs.
 */
fun googleCalendarOutboundTarget(
    accountId: String,
    accountStatus: String,
    accountSyncEnabled: Boolean,
    accountHasCalendarWriteScope: Boolean,
    collection: RemoteGoogleSyncCollection,
): GoogleCalendarOutboundTarget? = runCatching {
    requireCanonicalOutboundUuid(accountId)
    require(accountStatus == "active" && accountSyncEnabled && accountHasCalendarWriteScope)
    require(collection.accountId == accountId)
    requireCanonicalOutboundUuid(collection.id)
    require(collection.kind == RemoteGoogleCollectionKind.CALENDAR)
    require(collection.selected && !collection.providerDeleted)
    require(collection.syncRole == RemoteGoogleSyncRole.WRITABLE)
    require(collection.revision > 0)
    require(
        collection.providerAccessRole?.lowercase(Locale.ROOT) in setOf("owner", "writer"),
    )
    GoogleCalendarOutboundTarget(
        accountId = accountId,
        collectionId = collection.id,
        collectionRevision = collection.revision,
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

private fun JsonObject.hasValidOutboundBudget(): Boolean {
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
                    MAX_PROVIDER_VALUE_STRING_BYTES,
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
private const val MAX_PROVIDER_NUMBER_BYTES = 128
private const val MAX_CALENDAR_SUMMARY_BYTES = 8 * 1_024
private const val MAX_CALENDAR_SUMMARY_CODE_POINTS = 500
private const val MAX_CALENDAR_DESCRIPTION_BYTES = 256 * 1_024
private const val MAX_CALENDAR_DESCRIPTION_CODE_POINTS = 100_000
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
private val ZERO_UUID = UUID(0L, 0L)
