package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationAccepted
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationApproval
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationChange
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationPreview
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationStatus
import com.greengolddog.dayweave.network.ScheduleGooglePublicationOperation
import com.greengolddog.dayweave.network.ScheduleGooglePublicationState
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import java.nio.charset.StandardCharsets
import java.time.Duration
import java.time.Instant
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
enum class GoogleSchedulePublicationStage {
    @SerialName("intent")
    INTENT,

    @SerialName("previewed")
    PREVIEWED,

    @SerialName("approval_attempted")
    APPROVAL_ATTEMPTED,

    @SerialName("approved")
    APPROVED,

    @SerialName("accepted")
    ACCEPTED,
}

/** One-use bearer authority. It is persisted only inside the SQLCipher planner snapshot. */
@Serializable
data class GoogleSchedulePublicationApprovalCapability(val value: String) {
    init {
        require(isValidSchedulePublicationCapability(value))
    }

    override fun toString(): String =
        "GoogleSchedulePublicationApprovalCapability(<redacted>)"
}

@Serializable
data class GoogleSchedulePublicationChangeSnapshot(
    val ordinal: Int,
    val slotId: String,
    val sourceBlockId: String? = null,
    val operation: ScheduleGooglePublicationOperation,
    val providerResourceId: String? = null,
    val providerEtag: String? = null,
    val summary: String,
    val startsAt: String,
    val endsAt: String,
) {
    init {
        requireValidShape()
    }

    fun requireValidShape() {
        require(ordinal in 0 until MAX_SCHEDULE_PUBLICATION_CHANGES)
        requireSchedulePublicationUuid(slotId)
        sourceBlockId?.let(::requireSchedulePublicationUuid)
        require(
            when (operation) {
                ScheduleGooglePublicationOperation.CREATE,
                ScheduleGooglePublicationOperation.UPDATE,
                ScheduleGooglePublicationOperation.NOOP,
                -> sourceBlockId != null
                ScheduleGooglePublicationOperation.DELETE -> sourceBlockId == null
            },
        )
        require(
            when (operation) {
                ScheduleGooglePublicationOperation.CREATE ->
                    providerResourceId == null && providerEtag == null
                ScheduleGooglePublicationOperation.UPDATE,
                ScheduleGooglePublicationOperation.DELETE,
                ScheduleGooglePublicationOperation.NOOP,
                -> providerResourceId.isSafeProviderBinding() && providerEtag.isSafeProviderBinding()
            },
        )
        require(summary.isSafeScheduleSummary())
        val start = requireSchedulePublicationInstant(startsAt)
        val end = requireSchedulePublicationInstant(endsAt)
        require(start < end)
    }

    override fun toString(): String =
        "GoogleSchedulePublicationChangeSnapshot(ordinal=$ordinal, operation=$operation, " +
            "content=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteScheduleGooglePublicationChange) =
            GoogleSchedulePublicationChangeSnapshot(
                ordinal = remote.ordinal,
                slotId = remote.slotId,
                sourceBlockId = remote.sourceBlockId,
                operation = remote.operation,
                providerResourceId = remote.providerResourceId,
                providerEtag = remote.providerEtag,
                summary = remote.summary,
                startsAt = remote.startsAt,
                endsAt = remote.endsAt,
            )
    }
}

/** Immutable, review-safe projection. Private provider payloads never reach Android. */
@Serializable
data class GoogleSchedulePublicationPreviewSnapshot(
    val id: String,
    val accountId: String,
    val collectionId: String,
    val collectionRevision: Long,
    val collectionDisplayName: String,
    val scheduleRevisionId: String,
    val scheduleRevisionNumber: Long,
    val previewHash: String,
    val createCount: Int,
    val updateCount: Int,
    val deleteCount: Int,
    val noopCount: Int,
    val changes: List<GoogleSchedulePublicationChangeSnapshot>,
    val expiresAt: String,
) {
    init {
        requireValidShape()
    }

    fun requireValidShape() {
        requireSchedulePublicationUuid(id)
        requireSchedulePublicationUuid(accountId)
        requireSchedulePublicationUuid(collectionId)
        requireSchedulePublicationUuid(scheduleRevisionId)
        require(collectionRevision > 0 && scheduleRevisionNumber > 0)
        require(collectionDisplayName.isSafeScheduleLabel())
        require(isSchedulePublicationHash(previewHash))
        require(changes.size <= MAX_SCHEDULE_PUBLICATION_CHANGES)
        require(listOf(createCount, updateCount, deleteCount, noopCount).all { it >= 0 })
        require(createCount + updateCount + deleteCount + noopCount == changes.size)
        changes.forEachIndexed { index, change ->
            change.requireValidShape()
            require(change.ordinal == index)
        }
        require(changes.map { it.slotId }.distinct().size == changes.size)
        require(createCount == changes.count { it.operation == ScheduleGooglePublicationOperation.CREATE })
        require(updateCount == changes.count { it.operation == ScheduleGooglePublicationOperation.UPDATE })
        require(deleteCount == changes.count { it.operation == ScheduleGooglePublicationOperation.DELETE })
        require(noopCount == changes.count { it.operation == ScheduleGooglePublicationOperation.NOOP })
        requireSchedulePublicationInstant(expiresAt)
    }

    override fun toString(): String =
        "GoogleSchedulePublicationPreviewSnapshot(changeCount=${changes.size}, " +
            "content=<redacted>, binding=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteScheduleGooglePublicationPreview) =
            GoogleSchedulePublicationPreviewSnapshot(
                id = remote.id,
                accountId = remote.accountId,
                collectionId = remote.collectionId,
                collectionRevision = remote.collectionRevision,
                collectionDisplayName = remote.collectionDisplayName,
                scheduleRevisionId = remote.scheduleRevisionId,
                scheduleRevisionNumber = remote.scheduleRevisionNumber,
                previewHash = remote.previewHash,
                createCount = remote.createCount,
                updateCount = remote.updateCount,
                deleteCount = remote.deleteCount,
                noopCount = remote.noopCount,
                changes = remote.changes.map(GoogleSchedulePublicationChangeSnapshot::fromRemote),
                expiresAt = remote.expiresAt,
            )
    }
}

@Serializable
data class GoogleSchedulePublicationAcceptedSnapshot(
    val publicationId: String,
    val replayed: Boolean,
) {
    init {
        requireSchedulePublicationUuid(publicationId)
    }

    override fun toString(): String =
        "GoogleSchedulePublicationAcceptedSnapshot(replayed=$replayed, id=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteScheduleGooglePublicationAccepted) =
            GoogleSchedulePublicationAcceptedSnapshot(remote.publicationId, remote.replayed)
    }
}

@Serializable
data class GoogleSchedulePublicationStatusSnapshot(
    val publicationId: String,
    val accountId: String,
    val collectionId: String,
    val scheduleRevisionId: String,
    val state: ScheduleGooglePublicationState,
    val totalCount: Int,
    val pendingCount: Int,
    val deliveringCount: Int,
    val publishedCount: Int,
    val conflictedCount: Int,
    val failedCount: Int,
    val supersededCount: Int,
    val createdAt: String,
    val completedAt: String? = null,
    val lastErrorCode: String? = null,
) {
    init {
        requireValidShape()
    }

    val isTerminal: Boolean
        get() = state in TERMINAL_SCHEDULE_PUBLICATION_STATES

    fun requireValidShape() {
        requireSchedulePublicationUuid(publicationId)
        requireSchedulePublicationUuid(accountId)
        requireSchedulePublicationUuid(collectionId)
        requireSchedulePublicationUuid(scheduleRevisionId)
        val counts = listOf(
            pendingCount,
            deliveringCount,
            publishedCount,
            conflictedCount,
            failedCount,
            supersededCount,
        )
        require(totalCount in 0..MAX_SCHEDULE_PUBLICATION_CHANGES)
        require(counts.all { it in 0..MAX_SCHEDULE_PUBLICATION_CHANGES })
        require(counts.sum() == totalCount)
        val created = requireSchedulePublicationInstant(createdAt)
        val completed = completedAt?.let(::requireSchedulePublicationInstant)
        require((completedAt != null) == isTerminal)
        require(completed == null || completed >= created)
        lastErrorCode?.let { require(it.isSafeScheduleErrorCode()) }
        require(
            when (state) {
                ScheduleGooglePublicationState.DELIVERING -> deliveringCount > 0
                ScheduleGooglePublicationState.PENDING,
                ScheduleGooglePublicationState.BACKOFF,
                -> deliveringCount == 0 && pendingCount > 0
                ScheduleGooglePublicationState.PUBLISHED -> publishedCount == totalCount
                ScheduleGooglePublicationState.PARTIALLY_PUBLISHED ->
                    pendingCount == 0 && deliveringCount == 0 &&
                        publishedCount in 1 until totalCount
                ScheduleGooglePublicationState.CONFLICT ->
                    pendingCount == 0 && deliveringCount == 0 && publishedCount == 0 &&
                        conflictedCount > 0
                ScheduleGooglePublicationState.FAILED ->
                    pendingCount == 0 && deliveringCount == 0 && publishedCount == 0 &&
                        conflictedCount == 0 && failedCount > 0
                ScheduleGooglePublicationState.SUPERSEDED ->
                    totalCount > 0 && supersededCount == totalCount &&
                        pendingCount + deliveringCount + publishedCount + conflictedCount +
                        failedCount == 0
            },
        )
    }

    override fun toString(): String =
        "GoogleSchedulePublicationStatusSnapshot(state=$state, totalCount=$totalCount, " +
            "pendingCount=$pendingCount, deliveringCount=$deliveringCount, " +
            "publishedCount=$publishedCount, conflictedCount=$conflictedCount, " +
            "failedCount=$failedCount, supersededCount=$supersededCount, binding=<redacted>)"

    companion object {
        fun fromRemote(remote: RemoteScheduleGooglePublicationStatus) =
            GoogleSchedulePublicationStatusSnapshot(
                publicationId = remote.publicationId,
                accountId = remote.accountId,
                collectionId = remote.collectionId,
                scheduleRevisionId = remote.scheduleRevisionId,
                state = remote.state,
                totalCount = remote.totalCount,
                pendingCount = remote.pendingCount,
                deliveringCount = remote.deliveringCount,
                publishedCount = remote.publishedCount,
                conflictedCount = remote.conflictedCount,
                failedCount = remote.failedCount,
                supersededCount = remote.supersededCount,
                createdAt = remote.createdAt,
                completedAt = remote.completedAt,
                lastErrorCode = remote.lastErrorCode,
            )
    }
}

/** Crash-safe preview, one-shot approval, enqueue, and status-recovery journal. */
@Serializable
data class GoogleSchedulePublicationJournal(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val recoveryId: String,
    val operationGeneration: Long,
    val configurationId: String,
    val apiBaseUrl: String,
    val accountId: String,
    val collectionId: String,
    val expectedScheduleRevisionId: String,
    val intentExpiresAt: String,
    val preview: GoogleSchedulePublicationPreviewSnapshot? = null,
    val approvalAttempted: Boolean = false,
    val approvalCapability: GoogleSchedulePublicationApprovalCapability? = null,
    val approvalExpiresAt: String? = null,
    val accepted: GoogleSchedulePublicationAcceptedSnapshot? = null,
    val status: GoogleSchedulePublicationStatusSnapshot? = null,
    val createdAt: String,
) {
    init {
        requireValidShape()
    }

    val stage: GoogleSchedulePublicationStage
        get() = when {
            accepted != null -> GoogleSchedulePublicationStage.ACCEPTED
            approvalCapability != null -> GoogleSchedulePublicationStage.APPROVED
            approvalAttempted -> GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED
            preview != null -> GoogleSchedulePublicationStage.PREVIEWED
            else -> GoogleSchedulePublicationStage.INTENT
        }

    fun requireValidShape() {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        requireSchedulePublicationUuid(recoveryId)
        require(operationGeneration > 0)
        requireSafeScheduleConfigurationId(configurationId)
        require(apiBaseUrl.length <= MAX_SCHEDULE_API_URL_CHARS)
        require(normalizedHttpsApiBaseUrl(apiBaseUrl) == apiBaseUrl)
        requireSchedulePublicationUuid(accountId)
        requireSchedulePublicationUuid(collectionId)
        requireSchedulePublicationUuid(expectedScheduleRevisionId)
        val created = requireSchedulePublicationInstant(createdAt)
        val intentExpiry = requireSchedulePublicationInstant(intentExpiresAt)
        require(intentExpiry > created)
        require(Duration.between(created, intentExpiry) <= MAXIMUM_INTENT_LIFETIME)
        preview?.let {
            it.requireValidShape()
            require(
                it.accountId == accountId && it.collectionId == collectionId &&
                    it.scheduleRevisionId == expectedScheduleRevisionId,
            )
            require(requireSchedulePublicationInstant(it.expiresAt) <= intentExpiry + MAXIMUM_CLOCK_SKEW)
        }
        require(preview != null || !approvalAttempted)
        require(preview != null || approvalCapability == null && approvalExpiresAt == null)
        require(approvalCapability == null || approvalAttempted)
        require((approvalCapability == null) == (approvalExpiresAt == null))
        approvalExpiresAt?.let {
            val expiry = requireSchedulePublicationInstant(it)
            require(expiry <= requireSchedulePublicationInstant(requireNotNull(preview).expiresAt))
        }
        accepted?.let {
            require(approvalAttempted && preview != null)
            require(approvalCapability == null && approvalExpiresAt == null)
        }
        require(status == null || accepted != null)
        status?.let {
            it.requireValidShape()
            require(
                it.publicationId == accepted?.publicationId && it.accountId == accountId &&
                    it.collectionId == collectionId &&
                    it.scheduleRevisionId == expectedScheduleRevisionId,
            )
        }
    }

    fun recordingPreview(remote: RemoteScheduleGooglePublicationPreview) =
        copy(preview = GoogleSchedulePublicationPreviewSnapshot.fromRemote(remote))
            .also { require(stage == GoogleSchedulePublicationStage.INTENT) }

    fun recordingApprovalAttempt(): GoogleSchedulePublicationJournal {
        require(stage == GoogleSchedulePublicationStage.PREVIEWED)
        return copy(approvalAttempted = true)
    }

    fun recordingApproval(remote: RemoteScheduleGooglePublicationApproval): GoogleSchedulePublicationJournal {
        require(stage == GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED)
        require(remote.previewId == requireNotNull(preview).id)
        return copy(
            approvalCapability = GoogleSchedulePublicationApprovalCapability(
                remote.approvalCapability,
            ),
            approvalExpiresAt = remote.expiresAt,
        )
    }

    fun recordingAcceptance(
        remote: RemoteScheduleGooglePublicationAccepted,
    ): GoogleSchedulePublicationJournal {
        require(stage == GoogleSchedulePublicationStage.APPROVED)
        return copy(
            approvalCapability = null,
            approvalExpiresAt = null,
            accepted = GoogleSchedulePublicationAcceptedSnapshot.fromRemote(remote),
        )
    }

    fun recordingStatus(
        remote: RemoteScheduleGooglePublicationStatus,
    ): GoogleSchedulePublicationJournal {
        require(stage == GoogleSchedulePublicationStage.ACCEPTED)
        val snapshot = GoogleSchedulePublicationStatusSnapshot.fromRemote(remote)
        require(snapshot.publicationId == requireNotNull(accepted).publicationId)
        return copy(status = snapshot)
    }

    fun canTransitionTo(replacement: GoogleSchedulePublicationJournal): Boolean {
        if (this == replacement) return true
        if (!hasSameIntent(replacement)) return false
        return when (stage to replacement.stage) {
            GoogleSchedulePublicationStage.INTENT to GoogleSchedulePublicationStage.PREVIEWED ->
                replacement.preview != null
            GoogleSchedulePublicationStage.PREVIEWED to
                GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED ->
                replacement == copy(approvalAttempted = true)
            GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED to
                GoogleSchedulePublicationStage.APPROVED ->
                replacement.preview == preview && replacement.approvalCapability != null
            GoogleSchedulePublicationStage.APPROVED to GoogleSchedulePublicationStage.ACCEPTED ->
                replacement.preview == preview && replacement.accepted != null &&
                    replacement.status == null
            GoogleSchedulePublicationStage.ACCEPTED to GoogleSchedulePublicationStage.ACCEPTED ->
                replacement.preview == preview && replacement.accepted == accepted &&
                    replacement.status != null
            else -> false
        }
    }

    fun isValidAt(now: Instant): Boolean = runCatching {
        requireValidShape()
        require(requireSchedulePublicationInstant(createdAt) <= now.plus(MAXIMUM_CLOCK_SKEW))
    }.isSuccess

    fun authorityExpiresAt(): Instant = when (stage) {
        GoogleSchedulePublicationStage.INTENT -> requireSchedulePublicationInstant(intentExpiresAt)
        GoogleSchedulePublicationStage.PREVIEWED,
        GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED,
        -> requireSchedulePublicationInstant(requireNotNull(preview).expiresAt)
        GoogleSchedulePublicationStage.APPROVED ->
            requireSchedulePublicationInstant(requireNotNull(approvalExpiresAt))
        GoogleSchedulePublicationStage.ACCEPTED -> Instant.MAX
    }

    fun canDiscardExpiredAt(now: Instant): Boolean =
        stage != GoogleSchedulePublicationStage.ACCEPTED &&
            !now.isBefore(authorityExpiresAt().plus(MAXIMUM_CLOCK_SKEW))

    private fun hasSameIntent(other: GoogleSchedulePublicationJournal): Boolean =
        schemaVersion == other.schemaVersion && recoveryId == other.recoveryId &&
            operationGeneration == other.operationGeneration &&
            configurationId == other.configurationId && apiBaseUrl == other.apiBaseUrl &&
            accountId == other.accountId && collectionId == other.collectionId &&
            expectedScheduleRevisionId == other.expectedScheduleRevisionId &&
            intentExpiresAt == other.intentExpiresAt && createdAt == other.createdAt

    override fun toString(): String =
        "GoogleSchedulePublicationJournal(stage=$stage, preview=<redacted>, " +
            "binding=<redacted>, capability=<redacted>)"

    companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        val MAXIMUM_INTENT_LIFETIME: Duration = Duration.ofMinutes(35)
        val MAXIMUM_CLOCK_SKEW: Duration = Duration.ofMinutes(5)
    }
}

@Serializable
data class GoogleSchedulePublicationTarget(
    val accountId: String,
    val collectionId: String,
    val collectionRevision: Long,
) {
    init {
        requireSchedulePublicationUuid(accountId)
        requireSchedulePublicationUuid(collectionId)
        require(collectionRevision > 0)
    }

    override fun toString(): String = "GoogleSchedulePublicationTarget(<redacted>)"
}

private fun requireSchedulePublicationUuid(value: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed != ZERO_SCHEDULE_PUBLICATION_UUID && parsed.toString() == value)
}

private fun requireSchedulePublicationInstant(value: String): Instant =
    Instant.parse(value).also { require(it.toString() == value) }

private fun String?.isSafeProviderBinding(): Boolean =
    this != null && isNotEmpty() && length <= MAX_PROVIDER_BINDING_CHARS &&
        StandardCharsets.UTF_8.newEncoder().canEncode(this) && none(Char::isISOControl)

private fun String.isSafeScheduleSummary(): Boolean =
    isNotEmpty() && codePointCount(0, length) <= MAX_SCHEDULE_SUMMARY_CODE_POINTS &&
        toByteArray(StandardCharsets.UTF_8).size <= MAX_SCHEDULE_SUMMARY_BYTES &&
        none(Char::isISOControl)

private fun String.isSafeScheduleLabel(): Boolean =
    isNotEmpty() && toByteArray(StandardCharsets.UTF_8).size <= MAX_COLLECTION_LABEL_BYTES &&
        none(Char::isISOControl)

private fun String.isSafeScheduleErrorCode(): Boolean =
    length in 1..MAX_ERROR_CODE_CHARS && all { it in 'a'..'z' || it in '0'..'9' || it == '_' }

private fun isSchedulePublicationHash(value: String): Boolean =
    value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }

private fun isValidSchedulePublicationCapability(value: String): Boolean {
    if (!value.startsWith(SCHEDULE_PUBLICATION_CAPABILITY_PREFIX)) return false
    val payload = value.removePrefix(SCHEDULE_PUBLICATION_CAPABILITY_PREFIX)
    if (payload.length != 43 || payload.any { it !in BASE64_URL_CHARACTERS }) return false
    return runCatching {
        val decoded = Base64.getUrlDecoder().decode(payload)
        decoded.size == 32 &&
            Base64.getUrlEncoder().withoutPadding().encodeToString(decoded) == payload
    }.getOrDefault(false)
}

private fun requireSafeScheduleConfigurationId(value: String) {
    require(value.length in 1..MAX_CONFIGURATION_ID_CHARS && value.all { it.code in 0x21..0x7e })
}

private val TERMINAL_SCHEDULE_PUBLICATION_STATES = setOf(
    ScheduleGooglePublicationState.PARTIALLY_PUBLISHED,
    ScheduleGooglePublicationState.PUBLISHED,
    ScheduleGooglePublicationState.CONFLICT,
    ScheduleGooglePublicationState.FAILED,
    ScheduleGooglePublicationState.SUPERSEDED,
)
private const val MAX_SCHEDULE_PUBLICATION_CHANGES = 10_000
private const val MAX_SCHEDULE_SUMMARY_CODE_POINTS = 500
private const val MAX_SCHEDULE_SUMMARY_BYTES = 8 * 1024
private const val MAX_COLLECTION_LABEL_BYTES = 4 * 1024
private const val MAX_PROVIDER_BINDING_CHARS = 2_048
private const val MAX_ERROR_CODE_CHARS = 128
private const val MAX_CONFIGURATION_ID_CHARS = 256
private const val MAX_SCHEDULE_API_URL_CHARS = 2_048
private const val SCHEDULE_PUBLICATION_CAPABILITY_PREFIX = "dw_gsa1_"
private val BASE64_URL_CHARACTERS =
    ('A'..'Z') + ('a'..'z') + ('0'..'9') + setOf('-', '_')
private val ZERO_SCHEDULE_PUBLICATION_UUID = UUID(0, 0)
