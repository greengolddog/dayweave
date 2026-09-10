package com.greengolddog.dayweave.model

import java.io.OutputStream
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.json.encodeToStream

const val MAX_ROUTINE_OCCURRENCE_OBSERVATIONS = 256
const val MAX_ROUTINE_OCCURRENCE_PENDING = 64
const val MAX_ROUTINE_OCCURRENCE_PENDING_BYTES = 1_048_576
const val MAX_ROUTINE_OCCURRENCE_STATE_BYTES = 8 * 1024 * 1024
/** Membership across the entire retained cache, not a per-instance allowance. */
const val MAX_ROUTINE_OCCURRENCE_CACHED_MEMBERS = 20_000
const val MAX_ROUTINE_OCCURRENCE_TERMINAL_PAGES = 128
const val MAX_ROUTINE_OCCURRENCE_TERMINAL_BYTES = 32 * 1024 * 1024
const val MAX_ROUTINE_OCCURRENCE_TERMINAL_MEMBER_VISITS = 40_000

@Serializable
enum class RoutineOccurrenceDisposition { PENDING, REVIEW_REQUIRED, INSTANCE_MISSING, REJECTED }

/** The request string is frozen wire custody; the typed copy must agree exactly with it. */
@Serializable
data class PendingRoutineOccurrenceMutation(
    val schemaVersion: Int = 1,
    val operationId: String,
    val instanceId: String,
    val memberId: String,
    val syncOrigin: String,
    val configurationId: String,
    val requestJson: String,
    val request: RoutineOccurrenceRequest,
    val createdAt: String,
    val submittedAt: String? = null,
    val disposition: RoutineOccurrenceDisposition = RoutineOccurrenceDisposition.PENDING,
) {
    fun requireValid() {
        require(schemaVersion == 1)
        requireRoutineBinding(syncOrigin, configurationId)
        requireCanonicalUuid(instanceId, "occurrence instance")
        require(requestJson.toByteArray(Charsets.UTF_8).size <= MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES)
        request.requireValid(memberId)
        require(request.operationId == operationId && request.expectedMemberRevision <= request.expectedInstanceRevision)
        require(decodeExactRoutineOccurrence<RoutineOccurrenceRequest>(requestJson) == request)
        val created = requireCompletionInstant(createdAt)
        submittedAt?.let { require(requireCompletionInstant(it) >= created) }
    }
}

/** Display/history observation only. Neither this nor a receipt grants a fresh GET review. */
@Serializable
data class RoutineOccurrenceObservation(val snapshot: RoutineOccurrenceSnapshot, val observedAt: String)

/**
 * SQLCipher snapshot sidecar, bound independently from canonical templates. This is a bounded
 * observation cache, not a complete lifecycle replica. The opaque terminal cursor, first-source
 * manifest and evidence hash are never current-source local planning witnesses.
 */
@Serializable
data class RoutineOccurrenceLedger(
    val schemaVersion: Int = 1,
    val syncOrigin: String? = null,
    val configurationId: String? = null,
    val observations: Map<String, RoutineOccurrenceObservation> = emptyMap(),
    /** Only the final page of a completed current/delta read may advance this checkpoint. */
    val deltaCursor: String? = null,
    val pending: List<PendingRoutineOccurrenceMutation> = emptyList(),
    /** A terminal read must actually observe each receipt target at or above this revision. */
    val minimumCatchUpRevisions: Map<String, Long> = emptyMap(),
    val needsRemoteScheduleCatchUp: Boolean = false,
) {
    val hasRecoveryCustody: Boolean
        get() = pending.isNotEmpty() || minimumCatchUpRevisions.isNotEmpty() || needsRemoteScheduleCatchUp

    fun requireValid() {
        require(schemaVersion == 1 && (syncOrigin == null) == (configurationId == null))
        if (syncOrigin == null) {
            require(observations.isEmpty() && deltaCursor == null && !hasRecoveryCustody)
        } else requireRoutineBinding(syncOrigin, requireNotNull(configurationId))
        deltaCursor?.let(::requireRoutineCursor)
        require(observations.size <= MAX_ROUTINE_OCCURRENCE_OBSERVATIONS)
        require(observations.values.sumOf { it.snapshot.aggregate.manifest.members.size.toLong() } <= MAX_ROUTINE_OCCURRENCE_CACHED_MEMBERS)
        require(pending.size <= MAX_ROUTINE_OCCURRENCE_PENDING)
        require(pending.sumOf { it.requestJson.toByteArray(Charsets.UTF_8).size.toLong() } <= MAX_ROUTINE_OCCURRENCE_PENDING_BYTES)
        require(pending.map { it.operationId }.distinct().size == pending.size)
        // Instance revision serializes every member write, including writes to distinct members.
        require(pending.map { it.instanceId }.distinct().size == pending.size)
        observations.forEach { (id, observation) ->
            observation.snapshot.requireValid()
            require(id == observation.snapshot.aggregate.manifest.id)
            requireCompletionInstant(observation.observedAt)
        }
        pending.forEach { mutation ->
            mutation.requireValid()
            require(mutation.syncOrigin == syncOrigin && mutation.configurationId == configurationId)
            val aggregate = requireNotNull(observations[mutation.instanceId]).snapshot.aggregate
            require(aggregate.members.any { it.itemId == mutation.memberId })
            require(aggregate.revision >= mutation.request.expectedInstanceRevision)
            require(aggregate.members.single { it.itemId == mutation.memberId }.revision >= mutation.request.expectedMemberRevision)
        }
        require(minimumCatchUpRevisions.size <= MAX_ROUTINE_OCCURRENCE_OBSERVATIONS)
        if (minimumCatchUpRevisions.isNotEmpty()) require(needsRemoteScheduleCatchUp)
        minimumCatchUpRevisions.forEach { (id, revision) ->
            requireCanonicalUuid(id, "occurrence receipt target")
            require(revision >= 2 && requireNotNull(observations[id]).snapshot.aggregate.revision >= revision)
        }
        routineStateEncodedBytes(serializer(), this, MAX_ROUTINE_OCCURRENCE_STATE_BYTES)
    }
}

internal fun RoutineOccurrenceLedger.bindRoutineOccurrences(origin: String, configuration: String): RoutineOccurrenceLedger {
    requireValid()
    requireRoutineBinding(origin, configuration)
    if (syncOrigin == origin && configurationId == configuration) return this
    require(!hasRecoveryCustody && observations.isEmpty() && deltaCursor == null)
    return RoutineOccurrenceLedger(syncOrigin = origin, configurationId = configuration).also { it.requireValid() }
}

internal fun RoutineOccurrenceLedger.quarantineRoutineOccurrences(): RoutineOccurrenceLedger {
    requireValid()
    require(!hasRecoveryCustody)
    return RoutineOccurrenceLedger()
}

/** Comparison ignores ephemeral evidence/eligibility/evaluation fields at equal revision. */
internal fun RoutineOccurrenceLedger.observeRoutineOccurrence(observation: RoutineOccurrenceObservation): RoutineOccurrenceLedger {
    requireValid()
    require(syncOrigin != null)
    observation.snapshot.requireValid()
    requireCompletionInstant(observation.observedAt)
    val incoming = observation.snapshot.aggregate
    val existing = observations[incoming.manifest.id]?.snapshot?.aggregate
    if (existing != null) {
        require(incoming.manifest == existing.manifest)
        if (incoming.revision < existing.revision) return this
        if (incoming.revision == existing.revision) require(incoming == existing)
    }
    return copy(observations = observations + (incoming.manifest.id to observation)).boundedRoutineOccurrences()
}

/** Caller must separately admit a live protected GET; cached eligibility cannot authorize staging. */
internal fun RoutineOccurrenceLedger.enqueueRoutineOccurrence(mutation: PendingRoutineOccurrenceMutation): RoutineOccurrenceLedger {
    requireValid(); mutation.requireValid()
    require(mutation.syncOrigin == syncOrigin && mutation.configurationId == configurationId)
    require(mutation.submittedAt == null && mutation.disposition == RoutineOccurrenceDisposition.PENDING)
    require(pending.none { it.instanceId == mutation.instanceId || it.operationId == mutation.operationId })
    require(minimumCatchUpRevisions.isEmpty() && !needsRemoteScheduleCatchUp)
    val snapshot = requireNotNull(observations[mutation.instanceId]).snapshot
    val member = snapshot.aggregate.members.single { it.itemId == mutation.memberId }
    require(snapshot.freshEditEligible && snapshot.aggregate.revision == mutation.request.expectedInstanceRevision &&
        member.revision == mutation.request.expectedMemberRevision && snapshot.evidenceHash == mutation.request.expectedEvidenceHash)
    return copy(pending = pending + mutation).boundedRoutineOccurrences()
}

internal fun RoutineOccurrenceLedger.markRoutineOccurrenceSubmitted(
    expected: PendingRoutineOccurrenceMutation,
    submittedAt: String,
): RoutineOccurrenceLedger {
    requireExactRoutineMutation(expected)
    require(expected.disposition == RoutineOccurrenceDisposition.PENDING && expected.submittedAt == null)
    return copy(pending = pending.map { if (it == expected) it.copy(submittedAt = submittedAt) else it })
        .also { it.requireValid() }
}

/** Immutable historical receipts settle only matching submitted custody, never replace newer state. */
internal fun RoutineOccurrenceLedger.settleRoutineOccurrence(
    expected: PendingRoutineOccurrenceMutation,
    result: RoutineOccurrenceMutationResult,
    observedAt: String,
): RoutineOccurrenceLedger {
    requireExactRoutineMutation(expected)
    require(expected.submittedAt != null && expected.disposition == RoutineOccurrenceDisposition.PENDING)
    result.requireMatches(expected.instanceId, expected.memberId, expected.request)
    requireCompletionInstant(observedAt)
    val receiptAggregate = result.occurrence.aggregate
    val existingAggregate = observations[expected.instanceId]?.snapshot?.aggregate
    if (existingAggregate != null) {
        require(existingAggregate.manifest == receiptAggregate.manifest)
        if (existingAggregate.revision == receiptAggregate.revision) require(existingAggregate == receiptAggregate)
    }
    // A historical receipt is never a fresh GET observation, even at the same revision. Retain
    // every field of a later GET, including source-drift eligibility, evidence and observed time.
    val observed = if (existingAggregate != null && existingAggregate.revision >= receiptAggregate.revision) {
        this
    } else {
        // Preserve the pending pin until the receipt snapshot has been admitted.
        observeRoutineOccurrence(RoutineOccurrenceObservation(result.occurrence, observedAt))
    }
    return observed.copy(
        pending = pending.filterNot { it == expected },
        minimumCatchUpRevisions = minimumCatchUpRevisions + (expected.instanceId to maxOf(
            minimumCatchUpRevisions[expected.instanceId] ?: 0, result.occurrence.aggregate.revision)),
        needsRemoteScheduleCatchUp = true,
    ).also { it.requireValid() }
}

/** Only an admitted definitive rejection may call this transition; ambiguous replies retain PENDING. */
internal fun RoutineOccurrenceLedger.resolveRoutineOccurrence(
    expected: PendingRoutineOccurrenceMutation,
    disposition: RoutineOccurrenceDisposition,
): RoutineOccurrenceLedger {
    requireExactRoutineMutation(expected)
    require(expected.disposition == RoutineOccurrenceDisposition.PENDING && disposition != RoutineOccurrenceDisposition.PENDING)
    return copy(pending = pending.map { if (it == expected) it.copy(disposition = disposition) else it })
        .also { it.requireValid() }
}

internal fun RoutineOccurrenceLedger.discardRoutineOccurrence(expected: PendingRoutineOccurrenceMutation): RoutineOccurrenceLedger {
    requireExactRoutineMutation(expected)
    require(expected.submittedAt == null || expected.disposition != RoutineOccurrenceDisposition.PENDING)
    return copy(pending = pending.filterNot { it == expected }).also { it.requireValid() }
}

/** A cursor reset retains all observations, intents and receipt targets until the new terminal read. */
internal fun RoutineOccurrenceLedger.resetRoutineOccurrenceDeltaCursor(expected: RoutineOccurrenceLedger): RoutineOccurrenceLedger {
    requireValid(); require(this == expected)
    return copy(deltaCursor = null).also { it.requireValid() }
}

/**
 * Install only a full terminal chain begun at [expected]. Exact capture includes the starting
 * cursor, journal and receipt targets. Only snapshots in this read can discharge receipt targets;
 * prior cache observations (including receipt snapshots) never count as catch-up evidence.
 * Pages are supplied by authenticated current/delta transport in request order.
 */
internal fun RoutineOccurrenceLedger.installRoutineOccurrenceTerminal(
    expected: RoutineOccurrenceLedger,
    pages: List<RoutineOccurrencePage>,
    observedAt: String,
    isCurrentList: Boolean = expected.deltaCursor == null,
    startingCursor: String? = expected.deltaCursor,
): RoutineOccurrenceLedger {
    requireValid(); require(this == expected && syncOrigin != null)
    require(startingCursor == expected.deltaCursor && isCurrentList == (startingCursor == null))
    require(pages.size in 1..MAX_ROUTINE_OCCURRENCE_TERMINAL_PAGES)
    // Bound the entire supplied wire chain before semantic merge or the cross-page instance map.
    var memberVisits = 0L
    var chainBytes = 0L
    val returnedCursors = HashSet<String>()
    pages.forEachIndexed { index, page ->
        require(page.schemaVersion == 1 && page.changes.size in 0..100)
        require(!page.hasMore || page.changes.isNotEmpty())
        require(page.hasMore == (index < pages.lastIndex))
        requireRoutineCursor(page.cursor)
        require(returnedCursors.add(page.cursor))
        require(page.cursor != startingCursor || pages.size == 1 && page.changes.isEmpty() && !page.hasMore)
        page.changes.forEach { change ->
            val occurrence = change.occurrence
            val memberCount = occurrence.aggregate.manifest.members.size
            require(occurrence.schemaVersion == 1 && occurrence.aggregate.manifest.schemaVersion == 1)
            require(memberCount in 1..MAX_ROUTINE_OCCURRENCE_MEMBERS)
            require(occurrence.aggregate.members.size == memberCount && occurrence.members.size == memberCount)
            memberVisits += memberCount.toLong()
            require(memberVisits <= MAX_ROUTINE_OCCURRENCE_TERMINAL_MEMBER_VISITS)
        }
        chainBytes += routineStateEncodedBytes(RoutineOccurrencePage.serializer(), page,
            MAX_ROUTINE_OCCURRENCE_TERMINAL_BYTES - chainBytes.toInt())
    }
    requireCompletionInstant(observedAt)
    var candidate = this
    var lastSequence = 0L
    val observedTargets = HashSet<String>()
    val seen = HashMap<String, RoutineOccurrenceAggregate>()
    pages.forEach { page ->
        page.requireValid(isCurrentList)
        page.changes.forEach { change ->
            require(change.sequence > lastSequence); lastSequence = change.sequence
            val aggregate = change.occurrence.aggregate
            val prior = seen.put(aggregate.manifest.id, aggregate)
            require(prior == null || !isCurrentList && aggregate.revision > prior.revision && aggregate.manifest == prior.manifest)
            candidate = candidate.observeRoutineOccurrence(RoutineOccurrenceObservation(change.occurrence, observedAt))
            minimumCatchUpRevisions[aggregate.manifest.id]?.let { revision ->
                if (aggregate.revision >= revision) observedTargets.add(aggregate.manifest.id)
            }
        }
    }
    // A read that did not reach its receipt revision cannot advance past that unresolved gap.
    require(observedTargets.containsAll(minimumCatchUpRevisions.keys))
    return candidate.copy(deltaCursor = pages.last().cursor, minimumCatchUpRevisions = emptyMap(),
        // An authenticated head advance can change policy or out-of-horizon work even when
        // retained members are unchanged. The durable schedule fence must survive restart.
        needsRemoteScheduleCatchUp = needsRemoteScheduleCatchUp || pages.last().cursor != expected.deltaCursor)
        .also { it.requireValid() }
}

/** Caller supplies an authoritative remote schedule result begun after the captured terminal read. */
internal fun RoutineOccurrenceLedger.acknowledgeRoutineOccurrenceRemoteScheduleCatchUp(
    expected: RoutineOccurrenceLedger,
): RoutineOccurrenceLedger {
    requireValid(); require(this == expected)
    require(deltaCursor != null && minimumCatchUpRevisions.isEmpty() && pending.isEmpty())
    return copy(needsRemoteScheduleCatchUp = false).also { it.requireValid() }
}

private fun RoutineOccurrenceLedger.requireExactRoutineMutation(expected: PendingRoutineOccurrenceMutation) {
    requireValid()
    require(pending.singleOrNull { it.operationId == expected.operationId } == expected)
}

/** Eviction touches only unpinned observations. A command or receipt is never dropped to fit. */
private fun RoutineOccurrenceLedger.boundedRoutineOccurrences(): RoutineOccurrenceLedger {
    val pins = pending.mapTo(hashSetOf()) { it.instanceId } + minimumCatchUpRevisions.keys
    val retained = observations.toMutableMap()
    var members = retained.values.sumOf { it.snapshot.aggregate.manifest.members.size.toLong() }
    val sizes = retained.mapValues { (id, observation) ->
        // Canonical UUID keys need no JSON escaping; quotes and colon add three bytes.
        id.toByteArray(Charsets.UTF_8).size.toLong() + 3 + routineStateEncodedBytes(
            RoutineOccurrenceObservation.serializer(), observation, MAX_ROUTINE_OCCURRENCE_STATE_BYTES + 16_384)
    }
    val base = routineStateEncodedBytes(RoutineOccurrenceLedger.serializer(), copy(observations = emptyMap()), MAX_ROUTINE_OCCURRENCE_STATE_BYTES)
    var bytes = base + sizes.values.sum() + (retained.size - 1).coerceAtLeast(0)
    val evictable = retained.entries.filter { it.key !in pins }
        .sortedWith(compareBy<Map.Entry<String, RoutineOccurrenceObservation>> { requireCompletionInstant(it.value.observedAt) }.thenBy { it.key })
        .iterator()
    while (retained.size > MAX_ROUTINE_OCCURRENCE_OBSERVATIONS || members > MAX_ROUTINE_OCCURRENCE_CACHED_MEMBERS || bytes > MAX_ROUTINE_OCCURRENCE_STATE_BYTES) {
        require(evictable.hasNext()) { "Pinned occurrence recovery exceeds the encrypted cache budget" }
        val entry = evictable.next()
        bytes -= sizes.getValue(entry.key) + if (retained.size > 1) 1 else 0
        members -= entry.value.snapshot.aggregate.manifest.members.size
        retained.remove(entry.key)
    }
    return copy(observations = retained).also { it.requireValid() }
}

private fun requireRoutineBinding(origin: String, configuration: String) {
    listOf(origin, configuration).forEach { require(it.isNotBlank() && it.length <= 4_096 && it.none(Char::isISOControl)) }
}

/** Counts serialized UTF-8 without allocating a whole extra copy of the protected ledger. */
@OptIn(ExperimentalSerializationApi::class)
private fun <T> routineStateEncodedBytes(serializer: SerializationStrategy<T>, value: T, maximum: Int): Long {
    var size = 0L
    val output = object : OutputStream() {
        override fun write(value: Int) { add(1) }
        override fun write(bytes: ByteArray, offset: Int, length: Int) { add(length) }
        private fun add(count: Int) {
            require(count >= 0 && size <= maximum - count.toLong())
            size += count
        }
    }
    ITEM_PROGRESS_JSON.encodeToStream(serializer, value, output)
    return size
}
