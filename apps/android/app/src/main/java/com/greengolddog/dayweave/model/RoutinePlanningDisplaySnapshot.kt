package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.scheduler.RoutineOccurrenceLocalComposition
import com.greengolddog.dayweave.scheduler.RustScheduleComposer
import java.security.MessageDigest
import java.time.Instant
import java.time.ZoneId
import kotlinx.serialization.Serializable

/** Separate encrypted display custody. It is never a canonical schedule or publication proof. */
@Serializable
data class RoutinePlanningDisplaySnapshot(
    val schemaVersion: Int,
    val originalRequestDigest: String,
    val stableInputFingerprint: String,
    val witnessFingerprint: String,
    val localInputFingerprint: String,
    val occurrenceSnapshotRevision: Long,
    val helperRequestFingerprint: String,
    val helperResponseJson: String,
    val capturedAt: String,
    val computedAt: String,
) {
    /** Pure strict codec validation: the explicit inert bridge can never load or invoke JNI. */
    fun validateAndDecode(capsule: RoutinePlanningInputCapsule): RoutineOccurrenceLocalComposition {
        try {
            capsule.requireValid()
            require(schemaVersion == 1 && originalRequestDigest == capsule.originalRequestDigest &&
                stableInputFingerprint == capsule.stableInputFingerprint && witnessFingerprint == capsule.witness.witnessFingerprint &&
                localInputFingerprint == capsule.witness.localInputFingerprint &&
                occurrenceSnapshotRevision == capsule.witness.occurrenceLifecycle.snapshotRevision && capturedAt == capsule.capturedAt)
            require(computedAt.endsWith('Z'))
            require(requireCompletionInstant(computedAt) >= requireCompletionInstant(capturedAt))
            requirePlanningFingerprint(helperRequestFingerprint, "sha256:")
            requireRoutinePlanningEncodedBudget(serializer(), this, MAX_ROUTINE_PLANNING_CAPSULE_BYTES)
            val codec = RustScheduleComposer(bridge = { error("Inert codec cannot invoke JNI") })
            val input = codec.encodeWitnessRequest(capsule.canonicalItems, capsule.witness)
            require(helperRequestFingerprint == "sha256:" + MessageDigest.getInstance("SHA-256").digest(input)
                .joinToString("") { "%02x".format(it.toInt() and 255) })
            return codec.decodeV2Response(helperResponseJson.toByteArray(Charsets.UTF_8), capsule.witness).let {
                it.copy(composition = it.composition.copy(scheduleRequestFingerprint = helperRequestFingerprint))
            }
        } catch (_: Exception) { throw RoutinePlanningWitnessProtocolException() }
    }

    override fun toString() = "RoutinePlanningDisplaySnapshot(<protected, display-only>)"

    companion object {
        fun create(capsule: RoutinePlanningInputCapsule, composed: RoutineOccurrenceLocalComposition, computedAt: String): RoutinePlanningDisplaySnapshot {
            try {
                val result = RoutinePlanningDisplaySnapshot(1, capsule.originalRequestDigest, capsule.stableInputFingerprint,
                    capsule.witness.witnessFingerprint, composed.composition.localInputFingerprint, composed.occurrenceSnapshotRevision,
                    composed.composition.scheduleRequestFingerprint, requireNotNull(composed.helperResponseJson), capsule.capturedAt, computedAt)
                require(result.validateAndDecode(capsule) == composed)
                return result
            } catch (_: Exception) { throw RoutinePlanningWitnessProtocolException() }
        }
    }
}

/** Process-only private presentation permission, never restored or passed to execution consumers. */
class RoutinePlanningDisplayAdmission internal constructor(
    val capsule: RoutinePlanningInputCapsule,
    val display: RoutinePlanningDisplaySnapshot,
    val composed: RoutineOccurrenceLocalComposition,
    private val occurrenceGeneration: Long,
    private val completionGeneration: Long,
) {
    fun matchesState(state: DayWeaveUiState): Boolean =
        state.routinePlanningInputCapsule === capsule && state.routinePlanningDisplaySnapshot === display &&
            state.routineOccurrenceAuthorityGeneration == occurrenceGeneration && state.itemCompletionEvidenceGeneration == completionGeneration &&
            state.hasRoutinePlanningInputReadiness() && capsule.stableInputFingerprint == state.routinePlanningStableInputFingerprint()

    fun permitsClock(reference: Instant, currentZone: ZoneId): Boolean = runCatching {
        val zone = requireCanonicalTimezoneName(capsule.witness.schedule.timezoneName)
        require(zone == currentZone && reference >= requireCompletionInstant(display.computedAt))
        require(reference >= requirePlanningInstant(capsule.witness.schedule.horizonStart) && reference < requirePlanningInstant(capsule.witness.schedule.horizonEnd))
        require(reference.atZone(zone).toLocalDate() == requireCompletionInstant(capsule.capturedAt).atZone(zone).toLocalDate())
        true
    }.getOrDefault(false)

    override fun toString() = "RoutinePlanningDisplayAdmission(<protected, runtime-only>)"
}

/** Only memory-only admissions are excluded; no durable source, journal or proof is normalized. */
internal fun DayWeaveUiState.withoutRoutinePlanningRuntime() = copy(
    routineOccurrenceAuthorityGeneration = 0, routineOccurrenceDeferAdmission = null,
    itemCompletionGetProofs = emptyMap(), itemCompletionEvidenceGeneration = 0, routinePlanningDisplayAdmission = null)
