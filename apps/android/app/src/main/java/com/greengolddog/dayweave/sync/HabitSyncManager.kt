package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitPauseResumeCommandSnapshot
import com.greengolddog.dayweave.model.HabitPauseSnapshot
import com.greengolddog.dayweave.model.HabitPauseStartCommandSnapshot
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.HabitApiException
import com.greengolddog.dayweave.network.HabitTransport
import com.greengolddog.dayweave.network.MAX_HABIT_RESPONSE_PAGE_LIMIT
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.RemoteHabitAnalyticsBucket
import com.greengolddog.dayweave.network.RemoteHabitDeltaChange
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerPersistenceReceipt
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.temporal.ChronoUnit
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class HabitSyncOutcome {
    SUCCESS,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    CONFLICT,
    NOT_FOUND,
    VALIDATION_FAILURE,
    TRANSIENT_NETWORK_FAILURE,
    RETRYABLE_SERVER_FAILURE,
    PROTOCOL_FAILURE,
    LOCAL_STORAGE_FAILURE,
    CONFIGURATION_CHANGED,
    INVALID_LOCAL_STATE,
    UNEXPECTED_FAILURE,
}

data class HabitSyncState(
    val phase: CanonicalSyncPhase,
    val message: String,
) {
    val isBusy: Boolean get() = phase == CanonicalSyncPhase.SYNCING
}

/** Owns Android's origin-bound habit cache, exact offline outbox, and cross-device reconciliation. */
class HabitSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: HabitTransport,
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
) {
    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(initialState())
    val state: StateFlow<HabitSyncState> = mutableState.asStateFlow()

    /** Called under the process-wide binding writer before credential mutation. */
    internal fun quarantineBindingState() {
        mutableState.value = initialState()
    }

    suspend fun refresh(): HabitSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Synchronizing habit history…")
            try {
                configuration.withBindingOperation {
                    ensureBound(configuration)
                    replayPending(configuration)
                    pullDelta(configuration)
                }
                completeSuccessfulSync("Habit history is synchronized across devices")
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    suspend fun loadHabit(
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
    ): HabitSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Loading habit occurrences…")
            try {
                requireValidHabitDateRange(startDate, endDate)
                configuration.withBindingOperation {
                    ensureBound(configuration)
                    replayPending(configuration)
                    var cursor: String? = null
                    var pages = 0
                    val seenCursors = mutableSetOf<String>()
                    do {
                        if (++pages > MAX_OCCURRENCE_PAGE_CHAIN) {
                            throw InvalidHabitProtocolException()
                        }
                        if (cursor != null && !seenCursors.add(cursor)) {
                            throw InvalidHabitProtocolException()
                        }
                        val page = transport.listOccurrences(
                            configuration,
                            habitId,
                            startDate,
                            endDate,
                            cursor,
                            MAX_HABIT_RESPONSE_PAGE_LIMIT,
                        )
                        if (page.hasMore != (page.nextCursor != null)) {
                            throw InvalidHabitProtocolException()
                        }
                        if (page.nextCursor?.let(seenCursors::contains) == true) {
                            throw InvalidHabitProtocolException()
                        }
                        ensureConfigurationCurrent(configuration)
                        awaitDurable(
                            plannerStore.mergeHabitOccurrencePage(
                                configuration.baseUrl.toString(),
                                requireConfigurationId(configuration),
                                habitId,
                                page.occurrences.map(HabitOccurrenceSnapshot::fromRemote),
                            ),
                        )
                        cursor = page.nextCursor
                    } while (page.hasMore)
                }
                completeSuccessfulSync("Habit occurrences are available offline")
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    suspend fun refreshAnalytics(
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        bucket: HabitAnalyticsBucketSnapshot,
    ): HabitSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Calculating private habit statistics…")
            try {
                requireValidHabitDateRange(startDate, endDate)
                configuration.withBindingOperation {
                    ensureBound(configuration)
                    replayPending(configuration)
                    val requestedBucket = when (bucket) {
                        HabitAnalyticsBucketSnapshot.DAY -> RemoteHabitAnalyticsBucket.DAY
                        HabitAnalyticsBucketSnapshot.WEEK -> RemoteHabitAnalyticsBucket.WEEK
                        HabitAnalyticsBucketSnapshot.MONTH -> RemoteHabitAnalyticsBucket.MONTH
                    }
                    val remote = transport.analytics(
                        configuration,
                        habitId,
                        startDate,
                        endDate,
                        requestedBucket,
                    )
                    if (
                        remote.habitId != habitId ||
                        remote.startDate != startDate.toString() ||
                        remote.endDate != endDate.toString() ||
                        remote.bucket != requestedBucket
                    ) {
                        throw InvalidHabitProtocolException()
                    }
                    ensureConfigurationCurrent(configuration)
                    awaitDurable(
                        plannerStore.cacheHabitAnalytics(
                            configuration.baseUrl.toString(),
                            requireConfigurationId(configuration),
                            HabitAnalyticsSnapshot.fromRemote(remote),
                        ),
                    )
                }
                completeSuccessfulSync("Habit statistics are up to date")
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    /**
     * Saves the exact outcome command locally without waiting for a network round trip.
     *
     * [observedOutcomeRevision] is the revision the user actually reviewed. Both the early check
     * here and PlannerStore's atomic staging fence reject a background refresh that changed the
     * occurrence while an editor was open. SUCCESS therefore means the exact encrypted command is
     * durable and safe for a transient screen to dismiss; a later [refresh] owns reconciliation.
     */
    suspend fun stageOutcome(
        habitId: String,
        occurrenceId: String,
        observedOutcomeRevision: Long,
        outcome: HabitOutcomeInputSnapshot,
    ): HabitSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Saving habit change securely…")
            try {
                configuration.withBindingOperation {
                    ensureBound(configuration)
                    val pending = createOutcomeMutation(
                        configuration = configuration,
                        habitId = habitId,
                        occurrenceId = occurrenceId,
                        observedOutcomeRevision = observedOutcomeRevision,
                        outcome = outcome,
                    )
                    awaitDurable(plannerStore.stageHabitMutation(pending))
                }
                mutableState.value = HabitSyncState(
                    CanonicalSyncPhase.READY,
                    "Habit change saved securely · synchronizing when possible",
                )
                HabitSyncOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    suspend fun recordOutcome(
        habitId: String,
        occurrenceId: String,
        observedOutcomeRevision: Long,
        outcome: HabitOutcomeInputSnapshot,
    ): HabitSyncOutcome = mutate { configuration ->
        createOutcomeMutation(
            configuration = configuration,
            habitId = habitId,
            occurrenceId = occurrenceId,
            observedOutcomeRevision = observedOutcomeRevision,
            outcome = outcome,
        )
    }

    suspend fun startPause(habitId: String): HabitSyncOutcome = mutate { configuration ->
        val operationId = canonicalNewUuid()
        val pauseId = canonicalNewUuid()
        val startedAt = canonicalNow()
        val command = HabitPauseStartCommandSnapshot(
            operationId = operationId,
            pauseId = pauseId,
            expectedRevision = 0,
            startedAt = startedAt,
        )
        PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.START_PAUSE,
            habitId = habitId,
            targetId = pauseId,
            expectedRevision = 0,
            idempotencyKey = operationId,
            requestJson = command.encoded(),
            createdAt = startedAt,
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = requireConfigurationId(configuration),
        )
    }

    suspend fun resumePause(
        habitId: String,
        pauseId: String,
    ): HabitSyncOutcome = mutate { configuration ->
        val pause = plannerStore.state.value.habitLedger.pauses[pauseId]
            ?: throw InvalidLocalHabitStateException("The habit pause is not cached")
        if (pause.habitId != habitId || pause.endedAt != null) {
            throw InvalidLocalHabitStateException("The habit pause is not open")
        }
        val operationId = canonicalNewUuid()
        val endedAt = canonicalNow()
        val command = HabitPauseResumeCommandSnapshot(
            operationId = operationId,
            expectedRevision = pause.revision,
            endedAt = endedAt,
        )
        PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.RESUME_PAUSE,
            habitId = habitId,
            targetId = pauseId,
            expectedRevision = pause.revision,
            idempotencyKey = operationId,
            requestJson = command.encoded(),
            createdAt = endedAt,
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = requireConfigurationId(configuration),
        )
    }

    suspend fun discardReviewedMutation(idempotencyKey: String): HabitSyncOutcome =
        withReadyStore {
            operationMutex.withLock {
                try {
                    awaitDurable(plannerStore.discardReviewedHabitMutation(idempotencyKey))
                    updateConnected("Saved habit update discarded")
                    HabitSyncOutcome.SUCCESS
                } catch (error: Throwable) {
                    handleFailure(error)
                }
            }
        }

    private suspend fun mutate(
        create: (AuthenticatedApiConfiguration) -> PendingHabitMutation,
    ): HabitSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Saving habit change…")
            try {
                configuration.withBindingOperation {
                    ensureBound(configuration)
                    val pending = create(configuration).also(PendingHabitMutation::requireValid)
                    awaitDurable(plannerStore.stageHabitMutation(pending))
                    // Persist the newly requested action before touching the network. In
                    // particular, one ambiguous older write must not make a later offline action
                    // disappear merely because replay stops at the first unavailable response.
                    replayPending(configuration)
                    pullDelta(configuration)
                }
                completeSuccessfulSync("Habit change synchronized across devices")
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private fun createOutcomeMutation(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        observedOutcomeRevision: Long,
        outcome: HabitOutcomeInputSnapshot,
    ): PendingHabitMutation {
        outcome.requireValid()
        requireValidMutationTime(outcome.occurredAt)
        val occurrence = plannerStore.state.value.habitLedger.occurrences[occurrenceId]
            ?: throw InvalidLocalHabitStateException("The habit occurrence is not cached")
        if (occurrence.evidence.habitId != habitId) {
            throw InvalidLocalHabitStateException("The occurrence belongs to another habit")
        }
        if ((occurrence.outcome?.revision ?: 0) != observedOutcomeRevision) {
            throw InvalidLocalHabitStateException(
                "Habit history changed since this outcome was opened · review it and try again",
            )
        }
        val operationId = canonicalNewUuid()
        val command = HabitOutcomeCommandSnapshot(
            operationId = operationId,
            expectedRevision = observedOutcomeRevision,
            outcome = outcome,
        )
        return PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.OUTCOME,
            habitId = habitId,
            targetId = occurrenceId,
            expectedRevision = observedOutcomeRevision,
            idempotencyKey = operationId,
            requestJson = command.encoded(),
            createdAt = canonicalNow(),
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = requireConfigurationId(configuration),
        )
    }

    private suspend fun ensureBound(configuration: AuthenticatedApiConfiguration) {
        val origin = configuration.baseUrl.toString()
        val configurationId = requireConfigurationId(configuration)
        val ledger = plannerStore.state.value.habitLedger
        if (ledger.isBound &&
            (ledger.syncOrigin != origin || ledger.configurationId != configurationId)
        ) {
            if (ledger.pendingMutations.isNotEmpty()) throw HabitConfigurationChangedException()
            awaitDurable(plannerStore.quarantineHabitLedger())
        }
        awaitDurable(plannerStore.bindHabitLedger(origin, configurationId))
    }

    private suspend fun replayPending(configuration: AuthenticatedApiConfiguration) {
        val pending = plannerStore.state.value.habitLedger.pendingMutations.toList()
        pending.filter { it.disposition == PendingHabitMutationDisposition.PENDING }
            .forEach { replayOne(configuration, it) }
    }

    private suspend fun replayOne(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingHabitMutation,
    ) {
        pending.requireValid()
        try {
            when (pending.kind) {
                PendingHabitMutationKind.OUTCOME -> {
                    val mutation = transport.putOutcome(
                        configuration,
                        pending.habitId,
                        pending.targetId,
                        pending.idempotencyKey,
                        pending.requestJson,
                    )
                    ensureConfigurationCurrent(configuration)
                    awaitDurable(
                        plannerStore.reconcileHabitOccurrence(
                            pending.idempotencyKey,
                            HabitOccurrenceSnapshot.fromRemote(mutation.value),
                        ),
                    )
                }
                PendingHabitMutationKind.START_PAUSE -> {
                    val mutation = transport.startPause(
                        configuration,
                        pending.habitId,
                        pending.idempotencyKey,
                        pending.requestJson,
                    )
                    ensureConfigurationCurrent(configuration)
                    awaitDurable(
                        plannerStore.reconcileHabitPause(
                            pending.idempotencyKey,
                            HabitPauseSnapshot.fromRemote(mutation.value),
                        ),
                    )
                }
                PendingHabitMutationKind.RESUME_PAUSE -> {
                    val mutation = transport.resumePause(
                        configuration,
                        pending.habitId,
                        pending.targetId,
                        pending.idempotencyKey,
                        pending.requestJson,
                    )
                    ensureConfigurationCurrent(configuration)
                    awaitDurable(
                        plannerStore.reconcileHabitPause(
                            pending.idempotencyKey,
                            HabitPauseSnapshot.fromRemote(mutation.value),
                        ),
                    )
                }
            }
        } catch (error: HabitApiException.Conflict) {
            markForReview(pending.idempotencyKey, PendingHabitMutationDisposition.CONFLICT)
            throw ReviewedHabitMutationException(
                HabitSyncOutcome.CONFLICT,
                "Habit changed elsewhere · review your saved update",
                error,
            )
        } catch (error: HabitApiException.NotFound) {
            markForReview(pending.idempotencyKey, PendingHabitMutationDisposition.NOT_FOUND)
            throw ReviewedHabitMutationException(
                HabitSyncOutcome.NOT_FOUND,
                "Habit occurrence is no longer available · review your saved update",
                error,
            )
        } catch (error: HabitApiException.Validation) {
            markForReview(pending.idempotencyKey, PendingHabitMutationDisposition.REJECTED)
            throw ReviewedHabitMutationException(
                HabitSyncOutcome.VALIDATION_FAILURE,
                "Habit update needs correction before it can synchronize",
                error,
            )
        }
    }

    private suspend fun markForReview(
        idempotencyKey: String,
        disposition: PendingHabitMutationDisposition,
    ) {
        awaitDurable(plannerStore.markHabitMutationForReview(idempotencyKey, disposition))
    }

    private suspend fun pullDelta(configuration: AuthenticatedApiConfiguration) {
        var pages = 0
        var repairedRejectedCursor = false
        val seenCursors = mutableSetOf<String>()
        while (true) {
            if (++pages > MAX_DELTA_PAGE_CHAIN) throw InvalidHabitProtocolException()
            val cursor = plannerStore.state.value.habitLedger.deltaCursor
            if (cursor != null && !seenCursors.add(cursor)) {
                throw InvalidHabitProtocolException()
            }
            val page = try {
                transport.delta(configuration, cursor, MAX_HABIT_RESPONSE_PAGE_LIMIT)
            } catch (error: HabitApiException.Validation) {
                if (
                    error.statusCode != INVALID_DELTA_CURSOR_STATUS ||
                    cursor == null ||
                    repairedRejectedCursor
                ) {
                    throw error
                }
                ensureConfigurationCurrent(configuration)
                awaitDurable(
                    plannerStore.resetHabitDeltaCursor(
                        configuration.baseUrl.toString(),
                        requireConfigurationId(configuration),
                    ),
                )
                repairedRejectedCursor = true
                pages = 0
                seenCursors.clear()
                continue
            }
            if (
                page.nextCursor in seenCursors &&
                (page.hasMore || page.nextCursor != cursor)
            ) {
                throw InvalidHabitProtocolException()
            }
            ensureConfigurationCurrent(configuration)
            val occurrences = mutableListOf<HabitOccurrenceSnapshot>()
            val pauses = mutableListOf<HabitPauseSnapshot>()
            page.changes.forEach { change ->
                when (change) {
                    is RemoteHabitDeltaChange.OccurrenceUpsert ->
                        occurrences += HabitOccurrenceSnapshot.fromRemote(change.occurrence)
                    is RemoteHabitDeltaChange.PauseUpsert ->
                        pauses += HabitPauseSnapshot.fromRemote(change.pause)
                }
            }
            awaitDurable(
                plannerStore.applyHabitDeltaPage(
                    configuration.baseUrl.toString(),
                    requireConfigurationId(configuration),
                    occurrences,
                    pauses,
                    nextCursor = page.nextCursor,
                    hasMore = page.hasMore,
                ),
            )
            if (!page.hasMore) return
        }
    }

    private suspend fun awaitDurable(receipt: PlannerPersistenceReceipt?) {
        if (receipt == null || !receipt.awaitDurable()) throw LocalHabitStorageException()
    }

    private fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = HabitSyncState(
                CanonicalSyncPhase.NOT_CONFIGURED,
                "Configure the DayWeave API to synchronize habits",
            )
            return null
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = HabitSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Sign in again to synchronize habits",
            )
            return null
        }
        return try {
            credentialStore.authenticatedConfiguration().also { configuration ->
                if (configuration == null) {
                    mutableState.value = HabitSyncState(
                        CanonicalSyncPhase.AUTH_REQUIRED,
                        "Sign in again to synchronize habits",
                    )
                }
            }
        } catch (_: SecureCredentialException) {
            mutableState.value = HabitSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "The encrypted sign-in token is unavailable",
            )
            null
        } catch (_: InvalidApiConfigurationException) {
            updateError("The stored API URL is invalid")
            null
        }
    }

    private fun requireConfigurationId(
        configuration: AuthenticatedApiConfiguration,
    ): String = configuration.configurationId
        ?: throw HabitConfigurationChangedException()

    private fun ensureConfigurationCurrent(configuration: AuthenticatedApiConfiguration) {
        val current = credentialStore.snapshot()
        if (
            current.baseUrl != configuration.baseUrl.toString() ||
            current.configurationId != configuration.configurationId ||
            !current.hasBearerToken
        ) {
            throw HabitConfigurationChangedException()
        }
    }

    private suspend fun withReadyStore(
        block: suspend () -> HabitSyncOutcome,
    ): HabitSyncOutcome {
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) {
            return handleFailure(LocalHabitStorageException())
        }
        return block()
    }

    private fun canonicalNow(): String = now().truncatedTo(ChronoUnit.MICROS).toString()

    private fun requireValidMutationTime(value: String) {
        val instant = Instant.parse(value)
        val current = now()
        if (
            instant.nano % 1_000 != 0 ||
            instant < current.minus(MUTATION_PAST_LIMIT) ||
            instant > current.plus(MUTATION_FUTURE_LIMIT)
        ) {
            throw InvalidLocalHabitStateException("Habit time is outside the supported range")
        }
    }

    private fun requireValidHabitDateRange(startDate: LocalDate, endDate: LocalDate) {
        if (
            startDate.year < MIN_HABIT_DATE_YEAR ||
            endDate.year > MAX_HABIT_DATE_YEAR ||
            endDate < startDate ||
            endDate.toEpochDay() - startDate.toEpochDay() >= MAX_HABIT_RANGE_DAYS
        ) {
            throw InvalidLocalHabitStateException("Habit date range is outside the supported range")
        }
    }

    private fun canonicalNewUuid(): String {
        val value = newUuid()
        if (value == UUID(0L, 0L)) throw InvalidLocalHabitStateException("UUID source returned nil")
        return value.toString()
    }

    private fun handleFailure(error: Throwable): HabitSyncOutcome {
        if (error is CancellationException) {
            mutableState.value = initialState()
            throw error
        }
        if (error is ApiBindingChangedException || error is HabitConfigurationChangedException) {
            mutableState.value = initialState()
            return HabitSyncOutcome.CONFIGURATION_CHANGED
        }
        val (phase, message, outcome) = when (error) {
            is ReviewedHabitMutationException -> Triple(
                CanonicalSyncPhase.ERROR,
                error.safeMessage,
                error.outcome,
            )
            is HabitApiException.Authentication -> Triple(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Sign in again to synchronize habits",
                HabitSyncOutcome.AUTH_REQUIRED,
            )
            is HabitApiException.NotFound -> Triple(
                CanonicalSyncPhase.ERROR,
                "The requested habit is no longer available",
                HabitSyncOutcome.NOT_FOUND,
            )
            is HabitApiException.Conflict -> Triple(
                CanonicalSyncPhase.ERROR,
                "Habit history changed on another device · refresh and review",
                HabitSyncOutcome.CONFLICT,
            )
            is HabitApiException.Validation -> Triple(
                CanonicalSyncPhase.ERROR,
                "The habit request needs correction",
                HabitSyncOutcome.VALIDATION_FAILURE,
            )
            is HabitApiException.InvalidResponse,
            is InvalidHabitProtocolException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                "Habit synchronization returned an invalid response",
                HabitSyncOutcome.PROTOCOL_FAILURE,
            )
            is HabitApiException.Http -> if (error.statusCode >= 500) {
                Triple(
                    CanonicalSyncPhase.ERROR,
                    "Habit service is temporarily unavailable · your saved changes remain queued",
                    HabitSyncOutcome.RETRYABLE_SERVER_FAILURE,
                )
            } else {
                Triple(
                    CanonicalSyncPhase.ERROR,
                    "Habit service rejected the request",
                    HabitSyncOutcome.PROTOCOL_FAILURE,
                )
            }
            is LocalHabitStorageException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Encrypted habit storage is unavailable",
                HabitSyncOutcome.LOCAL_STORAGE_FAILURE,
            )
            is InvalidLocalHabitStateException,
            is IllegalArgumentException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                error.message ?: "Habit state needs review",
                HabitSyncOutcome.INVALID_LOCAL_STATE,
            )
            is IOException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Offline · your encrypted habit changes remain queued",
                HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            )
            else -> Triple(
                CanonicalSyncPhase.ERROR,
                "Habit synchronization could not finish",
                HabitSyncOutcome.UNEXPECTED_FAILURE,
            )
        }
        mutableState.value = HabitSyncState(phase, message)
        return outcome
    }

    private fun updateBusy(message: String) {
        mutableState.value = HabitSyncState(CanonicalSyncPhase.SYNCING, message)
    }

    private fun updateConnected(message: String) {
        mutableState.value = HabitSyncState(CanonicalSyncPhase.CONNECTED, message)
    }

    private fun updateError(message: String) {
        mutableState.value = HabitSyncState(CanonicalSyncPhase.ERROR, message)
    }

    private fun completeSuccessfulSync(message: String): HabitSyncOutcome {
        reviewedMutationResult()?.let { (outcome, reviewMessage) ->
            updateError(reviewMessage)
            return outcome
        }
        credentialStore.recordSuccessfulSync(now().toEpochMilli())
        updateConnected(message)
        return HabitSyncOutcome.SUCCESS
    }

    private fun reviewedMutationResult(): Pair<HabitSyncOutcome, String>? {
        val dispositions = plannerStore.state.value.habitLedger.pendingMutations
            .mapTo(mutableSetOf()) { it.disposition }
        return when {
            PendingHabitMutationDisposition.CONFLICT in dispositions ->
                HabitSyncOutcome.CONFLICT to
                    "Habit changed elsewhere · review your saved update"
            PendingHabitMutationDisposition.REJECTED in dispositions ->
                HabitSyncOutcome.VALIDATION_FAILURE to
                    "Habit update needs correction before it can synchronize"
            PendingHabitMutationDisposition.NOT_FOUND in dispositions ->
                HabitSyncOutcome.NOT_FOUND to
                    "Habit occurrence is no longer available · review your saved update"
            else -> null
        }
    }

    private fun stateOutcome(): HabitSyncOutcome = when (mutableState.value.phase) {
        CanonicalSyncPhase.NOT_CONFIGURED -> HabitSyncOutcome.NOT_CONFIGURED
        CanonicalSyncPhase.AUTH_REQUIRED -> HabitSyncOutcome.AUTH_REQUIRED
        else -> HabitSyncOutcome.PROTOCOL_FAILURE
    }

    private companion object {
        // Preserve the former 200 x 100 catch-up budget while each response stays under 2 MiB.
        const val MAX_HABIT_RECORDS_PER_OPERATION = 20_000
        const val MAX_OCCURRENCE_PAGE_CHAIN =
            MAX_HABIT_RECORDS_PER_OPERATION / MAX_HABIT_RESPONSE_PAGE_LIMIT
        const val MAX_DELTA_PAGE_CHAIN =
            MAX_HABIT_RECORDS_PER_OPERATION / MAX_HABIT_RESPONSE_PAGE_LIMIT
        const val INVALID_DELTA_CURSOR_STATUS = 400
        const val MIN_HABIT_DATE_YEAR = 1900
        const val MAX_HABIT_DATE_YEAR = 2200
        const val MAX_HABIT_RANGE_DAYS = 366
        val MUTATION_PAST_LIMIT: Duration = Duration.ofDays(366L * 20)
        val MUTATION_FUTURE_LIMIT: Duration = Duration.ofMinutes(5)

        fun initialState() = HabitSyncState(
            CanonicalSyncPhase.NOT_CONFIGURED,
            "Configure the DayWeave API to synchronize habits",
        )
    }
}

private class LocalHabitStorageException : IOException("Encrypted habit storage failed")

private class HabitConfigurationChangedException : IOException("Habit API binding changed")

private class InvalidHabitProtocolException(
    cause: Throwable? = null,
) : IOException("Habit synchronization protocol failed", cause)

private class InvalidLocalHabitStateException(message: String) : IllegalArgumentException(message)

private class ReviewedHabitMutationException(
    val outcome: HabitSyncOutcome,
    val safeMessage: String,
    cause: Throwable,
) : IOException(safeMessage, cause)
