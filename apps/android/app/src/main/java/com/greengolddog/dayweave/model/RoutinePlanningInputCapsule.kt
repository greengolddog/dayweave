package com.greengolddog.dayweave.model

import java.io.OutputStream
import java.security.DigestOutputStream
import java.security.MessageDigest
import java.time.Instant
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.json.*

/** Admission-only ceilings, never an excuse to trim an existing outbox during ordinary saves. */
const val MAX_ROUTINE_PLANNING_CAPSULE_BYTES = 16 * 1024 * 1024
const val MAX_ROUTINE_PLANNING_CAPSULE_SNAPSHOT_ADMISSION_BYTES = 16 * 1024 * 1024
internal val ROUTINE_CAPSULE_JSON = Json { encodeDefaults = true; explicitNulls = true; ignoreUnknownKeys = false }

/** Encrypted fixed-input artifact. Construction and restart do not authenticate a live operation. */
@Serializable
data class RoutinePlanningInputCapsule(
    val schemaVersion: Int,
    val syncOrigin: String,
    val configurationId: String,
    val workspaceId: String,
    val userId: String,
    val originalRequestJson: String,
    val originalRequestDigest: String,
    val witness: RoutinePlanningWitness,
    val canonicalItems: List<CanonicalItemSnapshot>,
    val capturedAt: String,
    val stableInputFingerprint: String,
) {
    fun originalRequest(): RoutinePlanningWitnessRequest = decodeExactRoutinePlanningWitness(originalRequestJson)

    fun requireValid() {
        try {
            require(schemaVersion == 1)
            for (binding in listOf(syncOrigin, configurationId)) require(binding.isNotBlank() && binding.length <= 4096 && binding.none(Char::isISOControl))
            requirePlanningUuid(workspaceId); requirePlanningUuid(userId)
            require(capturedAt.endsWith('Z')); requireCompletionInstant(capturedAt)
            requirePlanningFingerprint(originalRequestDigest, REQUEST_DIGEST_PREFIX)
            requirePlanningFingerprint(stableInputFingerprint, STATE_DIGEST_PREFIX)
            require(originalRequestDigest == routinePlanningOriginalRequestDigest(originalRequestJson))
            canonicalItems.forEach { require(it.hasExplicitStructuralMetadata); it.requireValidStructuralMetadata() }
            val request = originalRequest()
            require(requireCompletionInstant(capturedAt) == requirePlanningInstant(request.schedule.asOf))
            witness.requireMatches(request, workspaceId, userId, canonicalItems)
            requireRoutinePlanningEncodedBudget(serializer(), this, MAX_ROUTINE_PLANNING_CAPSULE_BYTES)
        } catch (_: Exception) { throw RoutinePlanningWitnessProtocolException() }
    }

    /** Data eligibility only. A new process-local, privacy-owned operation is still mandatory. */
    fun isReusableInput(state: DayWeaveUiState, nowEpochMillis: Long, allowPrivateContent: Boolean): Boolean = runCatching {
        require(allowPrivateContent && state.hasRoutinePlanningInputReadiness())
        require(syncOrigin == state.canonicalSyncOrigin && configurationId == state.canonicalConfigurationId)
        require(state.routinePlanningInputCapsule === this || state.routinePlanningInputCapsule == this)
        require(stableInputFingerprint == state.routinePlanningStableInputFingerprint())
        require(witness.terminalCursor == state.routineOccurrenceLedger.deltaCursor)
        require(witness.executionSnapshotRevision == state.canonicalExecutionRevision)
        val now = Instant.ofEpochMilli(nowEpochMillis)
        require(now >= requireCompletionInstant(capturedAt))
        val zone = requireCanonicalTimezoneName(witness.schedule.timezoneName)
        require(now >= requirePlanningInstant(witness.schedule.horizonStart) && now < requirePlanningInstant(witness.schedule.horizonEnd))
        require(now.atZone(zone).toLocalDate() == requirePlanningInstant(witness.schedule.asOf).atZone(zone).toLocalDate())
        true
    }.getOrDefault(false)

    override fun toString() = "RoutinePlanningInputCapsule(<protected, inert>)"

    companion object {
        fun create(expectedState: DayWeaveUiState, originalRequestJson: String, witness: RoutinePlanningWitness, capturedAt: String): RoutinePlanningInputCapsule {
            val capsule = RoutinePlanningInputCapsule(1, requireNotNull(expectedState.canonicalSyncOrigin),
                requireNotNull(expectedState.canonicalConfigurationId), witness.workspaceId, witness.userId,
                originalRequestJson, routinePlanningOriginalRequestDigest(originalRequestJson), witness,
                expectedState.canonicalItems.sortedBy { it.id }, capturedAt, expectedState.routinePlanningStableInputFingerprint())
            capsule.requireValid()
            return capsule
        }
    }
}

internal fun DayWeaveUiState.hasRoutinePlanningInputReadiness(): Boolean =
    !canonicalSyncOrigin.isNullOrBlank() && !canonicalConfigurationId.isNullOrBlank() && !canonicalDeltaCursor.isNullOrBlank() &&
        routineOccurrenceLedger.syncOrigin == canonicalSyncOrigin && routineOccurrenceLedger.configurationId == canonicalConfigurationId &&
        routineOccurrenceLedger.deltaCursor != null && !routineOccurrenceLedger.hasRecoveryCustody &&
        pendingCanonicalMutation == null && pendingCanonicalAuthoringMutations.isEmpty() && pendingProposalApplicationMutation == null &&
        pendingSchedulePublication == null && pendingExecutionCommand == null && pendingExecutionDeferIntent == null &&
        canonicalExecutionSession == null && activeSession == null &&
        pendingGoogleCalendarOutbound == null && pendingGoogleSchedulePublication == null &&
        itemProgressLedger.pending.isEmpty() && itemCompletionLedger.pending.isEmpty() && !itemCompletionLedger.needsCanonicalCatchUp &&
        habitLedger.pendingMutations.isEmpty() && habitLedger.pendingMissedReconcile == null &&
        (canonicalItems.none { it.kind == "habit" && it.deletedAt == null } || habitLedger.syncOrigin == canonicalSyncOrigin &&
            habitLedger.configurationId == canonicalConfigurationId && habitLedger.deltaCaughtUp)

/** Runtime occurrence generations are excluded only from this persisted data fingerprint. */
internal fun DayWeaveUiState.routinePlanningStableInputFingerprint(): String {
    val input = RoutinePlanningStableInput(
        localInputFingerprint = copy(routineOccurrenceAuthorityGeneration = 0).localScheduleCompositionStateFingerprint(),
        syncOrigin = canonicalSyncOrigin, configurationId = canonicalConfigurationId, canonicalCursor = canonicalDeltaCursor,
        habitLedger = habitLedger, itemProgressLedger = itemProgressLedger, itemCompletionLedger = itemCompletionLedger,
        publication = publishedScheduleProof, publicationRevision = publishedScheduleRevision,
        hasPendingGoogleCalendar = pendingGoogleCalendarOutbound != null, hasPendingGoogleSchedule = pendingGoogleSchedulePublication != null,
    )
    return routinePlanningDigest(RoutinePlanningStableInput.serializer(), input, STATE_DIGEST_PREFIX)
}

@Serializable
private data class RoutinePlanningStableInput(
    val localInputFingerprint: String,
    val syncOrigin: String?,
    val configurationId: String?,
    val canonicalCursor: String?,
    val habitLedger: HabitLedgerSnapshot,
    val itemProgressLedger: ItemProgressLedger,
    val itemCompletionLedger: ItemCompletionLedger,
    val publication: PublishedScheduleProofSnapshot?,
    val publicationRevision: PublishedScheduleRevisionSnapshot?,
    val hasPendingGoogleCalendar: Boolean,
    val hasPendingGoogleSchedule: Boolean,
)

internal fun routinePlanningOriginalRequestDigest(body: String): String =
    REQUEST_DIGEST_PREFIX + MessageDigest.getInstance("SHA-256").digest(body.toByteArray(Charsets.UTF_8)).hex()

@OptIn(ExperimentalSerializationApi::class)
private fun <T> routinePlanningDigest(serializer: SerializationStrategy<T>, value: T, prefix: String): String {
    val digest = MessageDigest.getInstance("SHA-256")
    DigestOutputStream(object : OutputStream() { override fun write(value: Int) = Unit; override fun write(bytes: ByteArray, offset: Int, length: Int) = Unit }, digest).use {
        ROUTINE_CAPSULE_JSON.encodeToStream(serializer, value, it)
    }
    return prefix + digest.digest().hex()
}

/** Streams a size preflight without allocating or changing the prospective snapshot. */
@OptIn(ExperimentalSerializationApi::class)
internal fun <T> requireRoutinePlanningEncodedBudget(serializer: SerializationStrategy<T>, value: T, limit: Int) {
    val output = object : OutputStream() {
        var size = 0
        override fun write(value: Int) { require(size < limit); size++ }
        override fun write(bytes: ByteArray, offset: Int, length: Int) {
            require(offset >= 0 && length >= 0 && offset <= bytes.size - length && length <= limit - size)
            size += length
        }
    }
    ROUTINE_CAPSULE_JSON.encodeToStream(serializer, value, output)
}

private fun ByteArray.hex() = joinToString("") { "%02x".format(it.toInt() and 0xff) }
private const val REQUEST_DIGEST_PREFIX = "routine-input-request-sha256:"
private const val STATE_DIGEST_PREFIX = "routine-input-state-sha256:"
