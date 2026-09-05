package com.greengolddog.dayweave.network

import java.security.MessageDigest
import java.time.DateTimeException
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient

internal const val DEVICE_AUTH_CONTRACT_VERSION = 2
internal const val DEVICE_AUTH_ENVELOPE_VERSION = 3
internal const val DEVICE_ACCESS_TOKEN_PREFIX = "dw_da1_"
internal const val DEVICE_REFRESH_TOKEN_PREFIX = "dw_dr1_"
internal const val DEVICE_ENROLLMENT_TOKEN_PREFIX = "dw_en1_"
internal const val ACCOUNT_RECOVERY_TOKEN_PREFIX = "dw_rc1_"
internal const val DEVICE_AUTH_TOKEN_PAYLOAD_LENGTH = 43
internal val DEVICE_AUTH_ACCESS_TTL: Duration = Duration.ofMinutes(15)
internal val DEVICE_AUTH_REFRESH_IDLE_TTL: Duration = Duration.ofDays(30)
internal val DEVICE_AUTH_ABSOLUTE_TTL: Duration = Duration.ofDays(180)

internal val ANDROID_DEVICE_AUTH_SCOPES = listOf(
    "suggestions_read",
    "suggestions_write",
    "schedule_read",
    "schedule_simulate",
    "schedule_publish",
    "items_read",
    "items_write",
    "execution_read",
    "execution_write",
    "google_read",
    "google_write",
    "auth_sessions_read",
    "auth_sessions_write",
    "auth_mcp_clients_read",
    "auth_mcp_clients_write",
)

internal val ANDROID_DEVICE_AUTH_CAPABILITIES = listOf(
    "atomic-refresh",
    "exact-replay",
    "session-binding",
)

/** A serializable secret whose diagnostic representation never contains plaintext. */
@Serializable
internal class DeviceAuthSecret(val value: String) {
    override fun toString(): String = "<redacted>"

    override fun equals(other: Any?): Boolean =
        other is DeviceAuthSecret && value == other.value

    override fun hashCode(): Int = value.hashCode()
}

@Serializable
internal data class DeviceSessionContract(
    val id: String,
    @SerialName("client_instance_id") val clientInstanceId: String,
    @SerialName("client_kind") val clientKind: String,
    @SerialName("device_label") val deviceLabel: String,
    val scopes: List<String>,
    @SerialName("client_contract_version") val clientContractVersion: Int,
    @SerialName("client_version") val clientVersion: String,
    @SerialName("client_capabilities") val clientCapabilities: List<String>,
    @SerialName("created_at") val createdAt: String,
    @SerialName("last_seen_at") val lastSeenAt: String,
    @SerialName("credential_issued_at") val credentialIssuedAt: String,
    @SerialName("access_expires_at") val accessExpiresAt: String,
    @SerialName("refresh_idle_expires_at") val refreshIdleExpiresAt: String,
    @SerialName("absolute_expires_at") val absoluteExpiresAt: String,
    val revision: Long,
) {
    val accessExpiry: Instant get() = Instant.parse(accessExpiresAt)
    val refreshIdleExpiry: Instant get() = Instant.parse(refreshIdleExpiresAt)
    val absoluteExpiry: Instant get() = Instant.parse(absoluteExpiresAt)
}

/** Exact bootstrap-enrollment request bytes and security headers, encrypted with the envelope. */
@Serializable
internal data class DeviceEnrollmentCreationHttpRequest(
    val url: String,
    val method: String,
    @SerialName("accept_header") val acceptHeader: String,
    @SerialName("content_type_header") val contentTypeHeader: String,
    @SerialName("cache_control_header") val cacheControlHeader: String,
    @SerialName("pragma_header") val pragmaHeader: String,
    @SerialName("authorization_header") val authorizationHeader: DeviceAuthSecret,
    @SerialName("body_base64url") val bodyBase64Url: DeviceAuthSecret,
)

@Serializable
internal sealed class StoredDeviceAuthState {
    abstract val baseUrl: String?
    abstract val clientInstanceId: String?

    @Serializable
    @SerialName("unconfigured")
    data class Unconfigured(
        override val baseUrl: String? = null,
        override val clientInstanceId: String,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("legacy")
    data class Legacy(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val bindingId: String,
        val bootstrapToken: DeviceAuthSecret,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("enrollment_creation_pending")
    data class EnrollmentCreationPending(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val previousBaseUrl: String? = null,
        val previousBindingId: String? = null,
        val enrollmentId: String,
        val deviceLabel: String,
        val clientVersion: String,
        val preparedAt: String,
        val scopes: List<String>,
        val capabilities: List<String>,
        val enrollmentToken: DeviceAuthSecret,
        val request: DeviceEnrollmentCreationHttpRequest,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("enrollment_pending")
    data class EnrollmentPending(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val previousBaseUrl: String? = null,
        val previousBindingId: String? = null,
        val sessionId: String,
        val deviceLabel: String,
        val clientVersion: String,
        val preparedAt: String,
        val scopes: List<String>,
        val capabilities: List<String>,
        val enrollmentToken: DeviceAuthSecret,
        val accessToken: DeviceAuthSecret,
        val refreshToken: DeviceAuthSecret,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("active")
    data class Active(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val session: DeviceSessionContract,
        val accessToken: DeviceAuthSecret,
        val refreshToken: DeviceAuthSecret,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("refresh_pending")
    data class RefreshPending(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val session: DeviceSessionContract,
        val preparedAt: String,
        val currentAccessToken: DeviceAuthSecret,
        val currentRefreshToken: DeviceAuthSecret,
        val nextAccessToken: DeviceAuthSecret,
        val nextRefreshToken: DeviceAuthSecret,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("reauth")
    data class Reauth(
        override val baseUrl: String,
        override val clientInstanceId: String,
        val previousSessionId: String? = null,
        val reason: String,
    ) : StoredDeviceAuthState()

    @Serializable
    @SerialName("incompatible")
    data class Incompatible(
        val reason: String,
    ) : StoredDeviceAuthState() {
        override val baseUrl: String? = null
        override val clientInstanceId: String? = null
    }
}

@Serializable
internal data class StoredDeviceAuthEnvelope(
    @SerialName("schema_version") val schemaVersion: Int = DEVICE_AUTH_ENVELOPE_VERSION,
    val revision: Long,
    val state: StoredDeviceAuthState,
    @SerialName("account_recovery_journal")
    val accountRecoveryJournal: StoredAccountRecoveryJournal? = null,
    @Transient val storageIdentity: DeviceAuthStorageIdentity? = null,
)

/**
 * Crash-safe recovery mutation/disclosure state stored inside the same Keystore envelope as the
 * device credential. Keeping this at envelope level lets refreshes preserve an in-flight recovery
 * tuple and lets a successful consume install its session and successor code in one durable CAS.
 */
@Serializable
internal sealed class StoredAccountRecoveryJournal {
    abstract val baseUrl: String?

    /**
     * Memory projection for a journal payload whose encrypted envelope remains authentic but
     * cannot be safely interpreted by this build. It is never created silently as a replacement;
     * the original ciphertext stays intact until an owner confirms recovery-only removal.
     */
    @Serializable
    @SerialName("repair_required")
    data class RepairRequired(
        override val baseUrl: String? = null,
        val reason: String,
    ) : StoredAccountRecoveryJournal()

    @Serializable
    @SerialName("issuance_pending")
    data class IssuancePending(
        override val baseUrl: String,
        @SerialName("configuration_id") val configurationId: String,
        @SerialName("client_instance_id") val clientInstanceId: String,
        @SerialName("candidate_id") val candidateId: String,
        @SerialName("candidate_code") val candidateCode: DeviceAuthSecret,
        @SerialName("replaces_id") val replacesId: String?,
        @SerialName("replaces_revision") val replacesRevision: Long?,
        @SerialName("prepared_at") val preparedAt: String,
    ) : StoredAccountRecoveryJournal()

    @Serializable
    @SerialName("consumption_pending")
    data class ConsumptionPending(
        override val baseUrl: String,
        @SerialName("previous_base_url") val previousBaseUrl: String?,
        @SerialName("previous_binding_id") val previousBindingId: String?,
        @SerialName("client_instance_id") val clientInstanceId: String,
        @SerialName("session_id") val sessionId: String,
        @SerialName("device_label") val deviceLabel: String,
        @SerialName("client_version") val clientVersion: String,
        @SerialName("prepared_at") val preparedAt: String,
        @SerialName("recovery_code") val recoveryCode: DeviceAuthSecret,
        @SerialName("access_token") val accessToken: DeviceAuthSecret,
        @SerialName("refresh_token") val refreshToken: DeviceAuthSecret,
        @SerialName("successor_id") val successorId: String,
        @SerialName("successor_code") val successorCode: DeviceAuthSecret,
    ) : StoredAccountRecoveryJournal()

    /** Validated server commit captured before any fallible cache quarantine or local install. */
    @Serializable
    @SerialName("consumption_committed_awaiting_installation")
    data class ConsumptionCommittedAwaitingInstallation(
        override val baseUrl: String,
        @SerialName("previous_base_url") val previousBaseUrl: String?,
        @SerialName("previous_binding_id") val previousBindingId: String?,
        @SerialName("client_instance_id") val clientInstanceId: String,
        val session: DeviceSessionContract,
        @SerialName("access_token") val accessToken: DeviceAuthSecret,
        @SerialName("refresh_token") val refreshToken: DeviceAuthSecret,
        @SerialName("successor_id") val successorId: String,
        @SerialName("successor_code") val successorCode: DeviceAuthSecret,
        @SerialName("successor_created_at") val successorCreatedAt: String,
        @SerialName("successor_revision") val successorRevision: Long,
    ) : StoredAccountRecoveryJournal()

    @Serializable
    @SerialName("disclosure_pending")
    data class DisclosurePending(
        override val baseUrl: String,
        val id: String,
        val code: DeviceAuthSecret,
        @SerialName("created_at") val createdAt: String,
        val revision: Long,
        val source: String,
    ) : StoredAccountRecoveryJournal()
}

/** Nonserialized exact durable-record identity; diagnostics never reveal its digest. */
internal class DeviceAuthStorageIdentity(private val digest: ByteArray) {
    override fun equals(other: Any?): Boolean =
        other is DeviceAuthStorageIdentity && MessageDigest.isEqual(digest, other.digest)

    override fun hashCode(): Int = digest.contentHashCode()

    override fun toString(): String = "<storage-identity>"
}

enum class DeviceAuthPhase {
    UNCONFIGURED,
    LEGACY,
    ENROLLMENT_CREATION_PENDING,
    ENROLLMENT_PENDING,
    ACTIVE,
    REFRESH_PENDING,
    REAUTH,
    INCOMPATIBLE,
}

data class DeviceAuthUiState(
    val phase: DeviceAuthPhase,
    val baseUrl: String?,
    val clientInstanceId: String?,
    val sessionId: String?,
    val deviceLabel: String?,
    val accessExpiresAt: String?,
    val message: String,
    val isBusy: Boolean = false,
) {
    val isConfigured: Boolean
        get() = phase in setOf(
            DeviceAuthPhase.ACTIVE,
            DeviceAuthPhase.REFRESH_PENDING,
        )
}

internal fun StoredDeviceAuthState.bindingId(): String? = when (this) {
    is StoredDeviceAuthState.Legacy -> bindingId
    is StoredDeviceAuthState.Active -> session.id
    is StoredDeviceAuthState.RefreshPending -> session.id
    else -> null
}

internal fun StoredAccountRecoveryJournal?.blocksApiBoundWork(): Boolean =
    this is StoredAccountRecoveryJournal.ConsumptionPending ||
        this is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation ||
        this is StoredAccountRecoveryJournal.RepairRequired

internal fun StoredDeviceAuthState.toUiState(
    isBusy: Boolean = false,
    overrideMessage: String? = null,
): DeviceAuthUiState {
    val phase = when (this) {
        is StoredDeviceAuthState.Unconfigured -> DeviceAuthPhase.UNCONFIGURED
        is StoredDeviceAuthState.Legacy -> DeviceAuthPhase.LEGACY
        is StoredDeviceAuthState.EnrollmentCreationPending ->
            DeviceAuthPhase.ENROLLMENT_CREATION_PENDING
        is StoredDeviceAuthState.EnrollmentPending -> DeviceAuthPhase.ENROLLMENT_PENDING
        is StoredDeviceAuthState.Active -> DeviceAuthPhase.ACTIVE
        is StoredDeviceAuthState.RefreshPending -> DeviceAuthPhase.REFRESH_PENDING
        is StoredDeviceAuthState.Reauth -> DeviceAuthPhase.REAUTH
        is StoredDeviceAuthState.Incompatible -> DeviceAuthPhase.INCOMPATIBLE
    }
    val session = when (this) {
        is StoredDeviceAuthState.Active -> session
        is StoredDeviceAuthState.RefreshPending -> session
        else -> null
    }
    val defaultMessage = when (phase) {
        DeviceAuthPhase.UNCONFIGURED ->
            "Add an HTTPS endpoint, then upgrade with a bootstrap credential or consume a one-time enrollment code."
        DeviceAuthPhase.LEGACY ->
            "Legacy bootstrap is enrollment-only. Retry the reviewed upgrade; ordinary API work stays disabled."
        DeviceAuthPhase.ENROLLMENT_CREATION_PENDING ->
            "Enrollment creation is journaled. Retry sends the exact URL, headers, and body bytes."
        DeviceAuthPhase.ENROLLMENT_PENDING ->
            "Enrollment is journaled. Retry uses the exact same session and credential tuple."
        DeviceAuthPhase.ACTIVE ->
            "Durable device session active. Access credentials rotate automatically."
        DeviceAuthPhase.REFRESH_PENDING ->
            "Credential rotation is journaled and will retry the exact tuple."
        DeviceAuthPhase.REAUTH ->
            "This device session needs a new one-time enrollment code or reviewed hybrid bootstrap."
        DeviceAuthPhase.INCOMPATIBLE ->
            "Stored authentication state is incompatible. Update DayWeave or explicitly remove local authentication state."
    }
    return DeviceAuthUiState(
        phase = phase,
        baseUrl = baseUrl,
        clientInstanceId = clientInstanceId,
        sessionId = session?.id ?: (this as? StoredDeviceAuthState.EnrollmentPending)?.sessionId,
        deviceLabel = session?.deviceLabel ?: when (this) {
            is StoredDeviceAuthState.EnrollmentCreationPending -> deviceLabel
            is StoredDeviceAuthState.EnrollmentPending -> deviceLabel
            else -> null
        },
        accessExpiresAt = session?.accessExpiresAt,
        message = overrideMessage ?: defaultMessage,
        isBusy = isBusy,
    )
}

internal fun validateStoredDeviceAuthState(state: StoredDeviceAuthState) {
    state.baseUrl?.let(::requireCanonicalBaseUrl)
    state.clientInstanceId?.let(::requireUuid)
    when (state) {
        is StoredDeviceAuthState.Unconfigured -> Unit
        is StoredDeviceAuthState.Legacy -> {
            requireUuid(state.bindingId)
            validateLegacyBootstrapToken(state.bootstrapToken.value)
        }
        is StoredDeviceAuthState.EnrollmentCreationPending -> {
            state.previousBaseUrl?.let(::requireCanonicalBaseUrl)
            state.previousBindingId?.let(::requireUuid)
            requireUuid(state.enrollmentId)
            requireValidDeviceIdentity(state.deviceLabel, state.clientVersion)
            parseInstant(state.preparedAt)
            require(state.scopes == ANDROID_DEVICE_AUTH_SCOPES)
            require(state.capabilities == ANDROID_DEVICE_AUTH_CAPABILITIES)
            validateExactDeviceToken(state.enrollmentToken.value, DEVICE_ENROLLMENT_TOKEN_PREFIX)
            validateEnrollmentCreationRequest(state)
        }
        is StoredDeviceAuthState.EnrollmentPending -> {
            state.previousBaseUrl?.let(::requireCanonicalBaseUrl)
            state.previousBindingId?.let(::requireUuid)
            requireUuid(state.sessionId)
            requireValidDeviceIdentity(state.deviceLabel, state.clientVersion)
            parseInstant(state.preparedAt)
            require(state.scopes == ANDROID_DEVICE_AUTH_SCOPES)
            require(state.capabilities == ANDROID_DEVICE_AUTH_CAPABILITIES)
            validateExactDeviceToken(state.enrollmentToken.value, DEVICE_ENROLLMENT_TOKEN_PREFIX)
            validateExactDeviceToken(state.accessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(state.refreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            requireDistinctTokens(
                state.enrollmentToken.value,
                state.accessToken.value,
                state.refreshToken.value,
            )
        }
        is StoredDeviceAuthState.Active -> {
            validateDeviceSessionContract(
                session = state.session,
                expectedSessionId = state.session.id,
                expectedClientInstanceId = state.clientInstanceId,
                expectedDeviceLabel = state.session.deviceLabel,
                expectedClientVersion = state.session.clientVersion,
                expectedMinimumRevision = 1,
            )
            validateExactDeviceToken(state.accessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(state.refreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            requireDistinctTokens(state.accessToken.value, state.refreshToken.value)
        }
        is StoredDeviceAuthState.RefreshPending -> {
            validateDeviceSessionContract(
                session = state.session,
                expectedSessionId = state.session.id,
                expectedClientInstanceId = state.clientInstanceId,
                expectedDeviceLabel = state.session.deviceLabel,
                expectedClientVersion = state.session.clientVersion,
                expectedMinimumRevision = 1,
            )
            parseInstant(state.preparedAt)
            validateExactDeviceToken(state.currentAccessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(state.currentRefreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            validateExactDeviceToken(state.nextAccessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(state.nextRefreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            requireDistinctTokens(
                state.currentAccessToken.value,
                state.currentRefreshToken.value,
                state.nextAccessToken.value,
                state.nextRefreshToken.value,
            )
        }
        is StoredDeviceAuthState.Reauth -> {
            state.previousSessionId?.let(::requireUuid)
            require(state.reason in REAUTH_REASONS)
        }
        is StoredDeviceAuthState.Incompatible -> require(state.reason.isNotBlank())
    }
}

internal fun validateStoredAccountRecoveryJournal(
    journal: StoredAccountRecoveryJournal?,
) {
    journal ?: return
    journal.baseUrl?.let(::requireCanonicalBaseUrl)
    when (journal) {
        is StoredAccountRecoveryJournal.RepairRequired -> {
            require(journal.reason in ACCOUNT_RECOVERY_REPAIR_REASONS)
        }
        is StoredAccountRecoveryJournal.IssuancePending -> {
            requireRecoveryNonNilUuid(journal.configurationId)
            requireRecoveryNonNilUuid(journal.clientInstanceId)
            requireRecoveryNonNilUuid(journal.candidateId)
            validateExactDeviceToken(journal.candidateCode.value, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            require((journal.replacesId == null) == (journal.replacesRevision == null))
            journal.replacesId?.let {
                requireRecoveryNonNilUuid(it)
                require(it != journal.candidateId)
            }
            journal.replacesRevision?.let { require(it > 0 && it < Long.MAX_VALUE) }
            parseInstant(journal.preparedAt)
        }
        is StoredAccountRecoveryJournal.ConsumptionPending -> {
            journal.previousBaseUrl?.let(::requireCanonicalBaseUrl)
            journal.previousBindingId?.let(::requireRecoveryNonNilUuid)
            requireRecoveryNonNilUuid(journal.clientInstanceId)
            requireRecoveryNonNilUuid(journal.sessionId)
            requireRecoveryNonNilUuid(journal.successorId)
            require(journal.sessionId != journal.successorId)
            requireValidDeviceIdentity(journal.deviceLabel, journal.clientVersion)
            parseInstant(journal.preparedAt)
            validateExactDeviceToken(journal.recoveryCode.value, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            validateExactDeviceToken(journal.accessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(journal.refreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            validateExactDeviceToken(journal.successorCode.value, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            requireDistinctCredentialMaterials(
                journal.recoveryCode.value,
                journal.accessToken.value,
                journal.refreshToken.value,
                journal.successorCode.value,
            )
        }
        is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation -> {
            journal.previousBaseUrl?.let(::requireCanonicalBaseUrl)
            journal.previousBindingId?.let(::requireRecoveryNonNilUuid)
            requireRecoveryNonNilUuid(journal.clientInstanceId)
            requireRecoveryNonNilUuid(journal.successorId)
            validateDeviceSessionContract(
                session = journal.session,
                expectedSessionId = journal.session.id,
                expectedClientInstanceId = journal.clientInstanceId,
                expectedDeviceLabel = journal.session.deviceLabel,
                expectedClientVersion = journal.session.clientVersion,
                expectedMinimumRevision = 1,
            )
            require(journal.session.revision == 1L)
            require(journal.successorId != journal.session.id)
            validateExactDeviceToken(journal.accessToken.value, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(journal.refreshToken.value, DEVICE_REFRESH_TOKEN_PREFIX)
            validateExactDeviceToken(journal.successorCode.value, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            requireDistinctCredentialMaterials(
                journal.accessToken.value,
                journal.refreshToken.value,
                journal.successorCode.value,
            )
            require(
                parseInstant(journal.successorCreatedAt) ==
                    parseInstant(journal.session.createdAt),
            )
            require(journal.successorRevision == 1L)
        }
        is StoredAccountRecoveryJournal.DisclosurePending -> {
            requireRecoveryNonNilUuid(journal.id)
            validateExactDeviceToken(journal.code.value, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            parseInstant(journal.createdAt)
            require(journal.revision > 0 && journal.revision < Long.MAX_VALUE)
            require(journal.source in setOf("issued", "successor"))
        }
    }
}

internal fun validateStoredDeviceAuthEnvelopeContents(
    state: StoredDeviceAuthState,
    journal: StoredAccountRecoveryJournal?,
) {
    validateStoredDeviceAuthState(state)
    validateStoredAccountRecoveryJournal(journal)
    when (journal) {
        null, is StoredAccountRecoveryJournal.RepairRequired -> Unit
        is StoredAccountRecoveryJournal.IssuancePending -> {
            require(journal.baseUrl == state.baseUrl)
            require(journal.clientInstanceId == state.clientInstanceId)
            when (state) {
                is StoredDeviceAuthState.Active -> {
                    require(journal.configurationId == state.session.id)
                    require(journal.clientInstanceId == state.session.clientInstanceId)
                }
                is StoredDeviceAuthState.RefreshPending -> {
                    require(journal.configurationId == state.session.id)
                    require(journal.clientInstanceId == state.session.clientInstanceId)
                }
                is StoredDeviceAuthState.Reauth -> {
                    // A coordinated issuance can discover definitive session expiry/rejection
                    // only after the exact request was persisted. Retain that request, but bind
                    // it to the retired session so unrelated Reauth state cannot adopt it.
                    require(journal.configurationId == state.previousSessionId)
                }
                else -> throw IllegalArgumentException("Recovery issuance has no exact binding")
            }
        }
        is StoredAccountRecoveryJournal.ConsumptionPending -> {
            require(state !is StoredDeviceAuthState.Incompatible)
            require(journal.previousBaseUrl == state.baseUrl)
            require(journal.previousBindingId == state.recoveryReplacementBindingId())
            state.clientInstanceId?.let { require(journal.clientInstanceId == it) }
            require(journal.sessionId != journal.previousBindingId)
            require(journal.successorId != journal.previousBindingId)
        }
        is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation -> {
            require(state !is StoredDeviceAuthState.Incompatible)
            require(journal.previousBaseUrl == state.baseUrl)
            require(journal.previousBindingId == state.recoveryReplacementBindingId())
            state.clientInstanceId?.let { require(journal.clientInstanceId == it) }
            require(journal.session.id != journal.previousBindingId)
            require(journal.successorId != journal.previousBindingId)
        }
        is StoredAccountRecoveryJournal.DisclosurePending -> {
            require(journal.baseUrl == state.baseUrl)
            when (journal.source) {
                "issued" -> require(
                    state is StoredDeviceAuthState.Active ||
                        state is StoredDeviceAuthState.RefreshPending,
                )
                "successor" -> {
                    require(state is StoredDeviceAuthState.Active)
                    require(journal.createdAt == state.session.createdAt)
                    require(journal.id != state.session.id)
                }
            }
        }
    }
}

private fun StoredDeviceAuthState.recoveryReplacementBindingId(): String? = when (this) {
    is StoredDeviceAuthState.Reauth -> previousSessionId
    is StoredDeviceAuthState.EnrollmentCreationPending -> previousBindingId ?: enrollmentId
    is StoredDeviceAuthState.EnrollmentPending -> previousBindingId ?: sessionId
    else -> bindingId()
}

internal const val RECOVERY_JOURNAL_MALFORMED = "malformed_recovery_journal"
internal const val RECOVERY_JOURNAL_UNSUPPORTED = "unsupported_recovery_journal"
private val ACCOUNT_RECOVERY_REPAIR_REASONS = setOf(
    RECOVERY_JOURNAL_MALFORMED,
    RECOVERY_JOURNAL_UNSUPPORTED,
)

internal fun validateDeviceSessionContract(
    session: DeviceSessionContract,
    expectedSessionId: String,
    expectedClientInstanceId: String,
    expectedDeviceLabel: String,
    expectedClientVersion: String,
    expectedMinimumRevision: Long,
) {
    require(expectedMinimumRevision >= 1)
    requireUuid(session.id)
    require(session.id == expectedSessionId)
    requireUuid(session.clientInstanceId)
    require(session.clientInstanceId == expectedClientInstanceId)
    require(session.clientKind == "android")
    require(session.deviceLabel == expectedDeviceLabel)
    require(session.scopes == ANDROID_DEVICE_AUTH_SCOPES)
    require(session.scopes.distinct().size == session.scopes.size)
    require(session.clientContractVersion == DEVICE_AUTH_CONTRACT_VERSION)
    require(session.clientVersion == expectedClientVersion)
    require(session.clientCapabilities == ANDROID_DEVICE_AUTH_CAPABILITIES)
    require(session.revision >= expectedMinimumRevision && session.revision < Long.MAX_VALUE)
    val created = parseInstant(session.createdAt)
    val lastSeen = parseInstant(session.lastSeenAt)
    val issued = parseInstant(session.credentialIssuedAt)
    val accessExpiry = parseInstant(session.accessExpiresAt)
    val idleExpiry = parseInstant(session.refreshIdleExpiresAt)
    val absoluteExpiry = parseInstant(session.absoluteExpiresAt)
    require(!lastSeen.isBefore(created))
    require(!issued.isBefore(created) && issued.isBefore(absoluteExpiry))
    require(!lastSeen.isBefore(issued))
    require(absoluteExpiry.isAfter(created))
    require(accessExpiry.isAfter(issued))
    require(accessExpiry <= issued.checkedPlus(DEVICE_AUTH_ACCESS_TTL))
    require(accessExpiry <= absoluteExpiry)
    require(idleExpiry.isAfter(issued))
    require(idleExpiry <= issued.checkedPlus(DEVICE_AUTH_REFRESH_IDLE_TTL))
    require(idleExpiry <= absoluteExpiry)
    require(absoluteExpiry <= created.checkedPlus(DEVICE_AUTH_ABSOLUTE_TTL))
}

private fun requireCanonicalBaseUrl(value: String) {
    require(normalizedHttpsApiBaseUrl(value) == value) { "API endpoint must be canonical" }
}

private fun Instant.checkedPlus(duration: Duration): Instant = try {
    plus(duration)
} catch (_: DateTimeException) {
    throw IllegalArgumentException("Invalid credential timestamp")
} catch (_: ArithmeticException) {
    throw IllegalArgumentException("Invalid credential timestamp")
}

internal fun requireValidDeviceIdentity(deviceLabel: String, clientVersion: String) {
    require(deviceLabel.isNotBlank() && deviceLabel.length <= 200)
    require(clientVersion.isNotBlank() && clientVersion.length <= 100)
    require((deviceLabel + clientVersion).none { it.isISOControl() })
}

internal fun validateLegacyBootstrapToken(token: String): String {
    validateBearerToken(token)
    require(!token.startsWith("dw_")) { "A bootstrap credential cannot use a reserved prefix" }
    return token
}

internal fun validateExactDeviceToken(token: String, prefix: String): String {
    require(token.startsWith(prefix)) { "Invalid device credential" }
    val payload = token.removePrefix(prefix)
    require(payload.length == DEVICE_AUTH_TOKEN_PAYLOAD_LENGTH && payload.all(::isBase64Url)) {
        "Invalid device credential"
    }
    val decoded = try {
        java.util.Base64.getUrlDecoder().decode(payload)
    } catch (_: IllegalArgumentException) {
        throw IllegalArgumentException("Invalid device credential")
    }
    try {
        require(decoded.size == 32) { "Invalid device credential" }
        require(Base64.getUrlEncoder().withoutPadding().encodeToString(decoded) == payload) {
            "Invalid device credential"
        }
    } finally {
        decoded.fill(0)
    }
    return token
}

private fun requireDistinctTokens(vararg tokens: String) {
    val payloads = tokens.map(::credentialTokenMaterial)
    require(payloads.distinct().size == payloads.size) { "Credential material must be distinct" }
}

internal fun requireDistinctCredentialMaterials(vararg tokens: String) {
    val payloads = tokens.map(::credentialTokenMaterial)
    require(payloads.distinct().size == payloads.size) { "Credential material must be distinct" }
}

private fun credentialTokenMaterial(token: String): String = when {
    token.startsWith(DEVICE_ACCESS_TOKEN_PREFIX) -> token.removePrefix(DEVICE_ACCESS_TOKEN_PREFIX)
    token.startsWith(DEVICE_REFRESH_TOKEN_PREFIX) -> token.removePrefix(DEVICE_REFRESH_TOKEN_PREFIX)
    token.startsWith(DEVICE_ENROLLMENT_TOKEN_PREFIX) -> token.removePrefix(DEVICE_ENROLLMENT_TOKEN_PREFIX)
    token.startsWith(ACCOUNT_RECOVERY_TOKEN_PREFIX) -> token.removePrefix(ACCOUNT_RECOVERY_TOKEN_PREFIX)
    else -> throw IllegalArgumentException("Invalid device credential")
}

private fun requireUuid(value: String) {
    require(runCatching { UUID.fromString(value).toString() == value.lowercase() }.getOrDefault(false)) {
        "Invalid device identifier"
    }
}

internal fun requireRecoveryNonNilUuid(value: String) {
    requireUuid(value)
    require(UUID.fromString(value) != UUID(0L, 0L)) { "Invalid device identifier" }
}

private fun parseInstant(value: String): Instant = try {
    Instant.parse(value)
} catch (_: DateTimeParseException) {
    throw IllegalArgumentException("Invalid credential timestamp")
}

private fun isBase64Url(character: Char): Boolean =
    character in 'A'..'Z' || character in 'a'..'z' || character in '0'..'9' ||
        character == '-' || character == '_'

internal const val REAUTH_REFRESH_REJECTED = "refresh_rejected"
internal const val REAUTH_REFRESH_EXPIRED = "refresh_expired"
internal const val REAUTH_SESSION_REVOKED = "session_revoked"
internal const val REAUTH_LOCAL_RECOVERY = "local_recovery_required"
internal const val REAUTH_CONTRACT_REJECTED = "server_contract_rejected"
private val REAUTH_REASONS = setOf(
    REAUTH_REFRESH_REJECTED,
    REAUTH_REFRESH_EXPIRED,
    REAUTH_SESSION_REVOKED,
    REAUTH_LOCAL_RECOVERY,
    REAUTH_CONTRACT_REJECTED,
)
