package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamEnd
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamException
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamTransport
import com.greengolddog.dayweave.network.SecureCredentialException
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch

/** Only an exact SQLCipher-confirmed binding may supply the schedule SSE resume revision. */
internal data class DurableScheduleInvalidationCursor(
    val syncOrigin: String?,
    val configurationId: String?,
    /** Last schedule head installed by an authoritative current-schedule response. */
    val revision: ULong,
    /** Newest durably observed authenticated hint, used only as the SSE resume cursor. */
    val latestObservedRevision: ULong = revision,
)

/** One-shot authority to replace a rejected SSE cursor with a lower server-epoch head. */
internal data class ScheduleRevisionEpochResetFence(
    val syncOrigin: String,
    val configurationId: String,
    val rejectedRevision: ULong,
) {
    init {
        require(syncOrigin.isNotBlank() && configurationId.isNotBlank())
        require(rejectedRevision in 1uL..Long.MAX_VALUE.toULong())
    }
}

/**
 * Runs only in the unlocked foreground. Authenticated revision hints are durably fenced before
 * they can invalidate membership; every schedule mutation still comes from a current-schedule GET.
 */
internal class ForegroundScheduleInvalidationManager(
    private val credentialStore: ApiCredentialStore,
    private val streamTransport: ScheduleInvalidationStreamTransport?,
    private val durableCursor: () -> DurableScheduleInvalidationCursor,
    private val recordRevisionHint: suspend (String, String, ULong) -> Boolean = { _, _, _ -> true },
    private val tryLaunchAuthoritativeRefresh: ((suspend () -> Unit) -> Boolean),
    private val authoritativeRefresh: suspend (ScheduleRevisionEpochResetFence?) -> Boolean,
    private val delayMillis: suspend (Long) -> Unit = { delay(it) },
    private val monotonicNanos: () -> Long = System::nanoTime,
) {
    private val activeForegroundJob = AtomicReference<Job?>(null)

    fun cancelActiveSession() {
        activeForegroundJob.get()?.cancel()
    }

    suspend fun cancelAndDrainActiveSession() {
        activeForegroundJob.get()?.cancelAndJoin()
    }

    suspend fun runForegroundActivation() {
        val sessionJob = currentCoroutineContext().job
        if (!registerSession(sessionJob)) return
        try {
            val binding = captureBinding() ?: return
            val highWater = AtomicReference<ScheduleRevisionHint?>(null)
            val epochResetFromRevision = AtomicLong(0L)
            val forceRefresh = AtomicLong(0)
            val signals = Channel<Unit>(Channel.CONFLATED)
            coroutineScope {
                val drainJob = launch {
                    drainHints(
                        binding,
                        highWater,
                        epochResetFromRevision,
                        forceRefresh,
                        signals,
                    )
                }
                val streamJob = streamTransport?.let { transport ->
                    launch {
                        try {
                            collectConnections(
                                binding,
                                transport,
                                highWater,
                                epochResetFromRevision,
                                forceRefresh,
                                signals,
                            )
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            // The independent 30-second authoritative GET remains active.
                        }
                    }
                }
                try {
                    pollCurrentSchedule(binding, forceRefresh, signals)
                } finally {
                    signals.close()
                    streamJob?.cancelAndJoin()
                    drainJob.cancelAndJoin()
                }
            }
        } finally {
            activeForegroundJob.compareAndSet(sessionJob, null)
        }
    }

    private suspend fun registerSession(sessionJob: Job): Boolean {
        while (currentCoroutineContext().isActive) {
            val existing = activeForegroundJob.get()
            if (existing == null) {
                if (activeForegroundJob.compareAndSet(null, sessionJob)) return true
            } else {
                if (existing.isActive) return false
                existing.join()
                activeForegroundJob.compareAndSet(existing, null)
            }
        }
        return false
    }

    private suspend fun collectConnections(
        binding: ActivationBinding,
        transport: ScheduleInvalidationStreamTransport,
        highWater: AtomicReference<ScheduleRevisionHint?>,
        epochResetFromRevision: AtomicLong,
        forceRefresh: AtomicLong,
        signals: Channel<Unit>,
    ) {
        var reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            val configuration = configurationFor(binding) ?: return
            val cursor = durableResumeRevisionFor(binding)
            val startedAt = monotonicNanos()
            val streamEnd = try {
                transport.collect(configuration, cursor) { revision ->
                    if (!recordRevisionHint(binding.baseUrl, binding.configurationId, revision)) {
                        throw IOException("Could not durably fence a newer schedule revision")
                    }
                    offerHighWater(highWater, revision)
                    signals.trySend(Unit)
                }
            } catch (_: ScheduleInvalidationStreamException.Authentication) {
                return
            } catch (_: ScheduleInvalidationStreamException.Protocol) {
                return
            } catch (error: ScheduleInvalidationStreamException.Http) {
                if (error.statusCode == CURSOR_AHEAD_STATUS) {
                    // The durable cursor belongs to an invalidated server epoch. Any memory-only
                    // target derived from that epoch is invalid too; retaining it would make a
                    // successful GET that restores a lower head look perpetually behind.
                    highWater.set(null)
                    if (cursor > 0uL) epochResetFromRevision.set(cursor.toLong())
                    offerForcedRefresh(forceRefresh)
                    signals.trySend(Unit)
                    // Do not hammer the same invalid cursor. Wait under bounded backoff until the
                    // authoritative GET repairs (or clears) the durable revision, then resume SSE
                    // from that newly durable server epoch during this same foreground session.
                    var repairDelay = MIN_RECONNECT_DELAY_MILLIS
                    while (
                        currentCoroutineContext().isActive && bindingIsCurrent(binding) &&
                        durableResumeRevisionFor(binding) == cursor
                    ) {
                        delayMillis(repairDelay)
                        repairDelay = (repairDelay * 2)
                            .coerceAtMost(MAX_RECONNECT_DELAY_MILLIS)
                    }
                    if (!currentCoroutineContext().isActive || !bindingIsCurrent(binding)) return
                    reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                } else if (
                    error.statusCode in 400..499 &&
                    error.statusCode !in RETRYABLE_CLIENT_HTTP_CODES
                ) {
                    return
                }
                null
            } catch (_: ApiBindingChangedException) {
                return
            } catch (_: InvalidApiConfigurationException) {
                return
            } catch (_: SecureCredentialException) {
                return
            } catch (_: IOException) {
                null
            }
            if (!bindingIsCurrent(binding)) return
            when (streamEnd) {
                ScheduleInvalidationStreamEnd.UNSUPPORTED -> return
                ScheduleInvalidationStreamEnd.ENDED -> {
                    if (monotonicNanos() - startedAt >= NORMAL_CONNECTION_LIFETIME_NANOS) {
                        reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
                        continue
                    }
                }
                null -> Unit
            }
            delayMillis(reconnectDelay)
            reconnectDelay = (reconnectDelay * 2).coerceAtMost(MAX_RECONNECT_DELAY_MILLIS)
        }
    }

    private suspend fun pollCurrentSchedule(
        binding: ActivationBinding,
        forceRefresh: AtomicLong,
        signals: Channel<Unit>,
    ) {
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            offerForcedRefresh(forceRefresh)
            signals.trySend(Unit)
            delayMillis(FOREGROUND_SCHEDULE_POLL_INTERVAL_MILLIS)
        }
    }

    private suspend fun drainHints(
        binding: ActivationBinding,
        highWater: AtomicReference<ScheduleRevisionHint?>,
        epochResetFromRevision: AtomicLong,
        forceRefresh: AtomicLong,
        signals: Channel<Unit>,
    ) {
        for (ignored in signals) {
            var catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
            while (currentCoroutineContext().isActive) {
                if (!bindingIsCurrent(binding)) return
                val target = highWater.get()
                val epochReset = epochResetFromRevision.get()
                    .takeIf { it > 0L }
                    ?.toULong()
                val forcedGeneration = forceRefresh.get()
                if (
                    epochReset == null &&
                    forcedGeneration == 0L &&
                    (target == null || durableRevisionFor(binding) >= target.revision)
                ) {
                    if (target != null) highWater.compareAndSet(target, null)
                    break
                }
                val completed = CompletableDeferred<Boolean>()
                val admitted = runCatching {
                    tryLaunchAuthoritativeRefresh {
                        var success = false
                        try {
                            success = authoritativeRefresh(
                                epochReset?.let { rejectedRevision ->
                                    ScheduleRevisionEpochResetFence(
                                        syncOrigin = binding.baseUrl,
                                        configurationId = binding.configurationId,
                                        rejectedRevision = rejectedRevision,
                                    )
                                },
                            )
                        } finally {
                            completed.complete(success)
                        }
                    }
                }.getOrDefault(false)
                if (!admitted) {
                    delayMillis(BUSY_GATE_RETRY_DELAY_MILLIS)
                    continue
                }
                val succeeded = completed.await()
                if (!bindingIsCurrent(binding)) return
                if (succeeded) {
                    if (epochReset != null) {
                        epochResetFromRevision.compareAndSet(epochReset.toLong(), 0L)
                    }
                    if (forcedGeneration > 0) {
                        forceRefresh.compareAndSet(forcedGeneration, 0)
                    }
                    val latest = highWater.get()
                    if (latest != null && durableRevisionFor(binding) >= latest.revision) {
                        highWater.compareAndSet(latest, null)
                    }
                    if (forceRefresh.get() == 0L && highWater.get() == null) break
                    catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                }
                delayMillis(catchUpDelay)
                catchUpDelay = (catchUpDelay * 2).coerceAtMost(MAX_RECONNECT_DELAY_MILLIS)
            }
        }
    }

    private fun offerHighWater(
        highWater: AtomicReference<ScheduleRevisionHint?>,
        revision: ULong,
    ) {
        if (revision == 0uL) return
        while (true) {
            val current = highWater.get()
            if (current != null && revision <= current.revision) return
            if (highWater.compareAndSet(current, ScheduleRevisionHint(revision))) return
        }
    }

    private fun offerForcedRefresh(generation: AtomicLong) {
        while (true) {
            val current = generation.get()
            val next = if (current == Long.MAX_VALUE) 1L else current + 1L
            if (generation.compareAndSet(current, next)) return
        }
    }

    private fun captureBinding(): ActivationBinding? {
        val snapshot = credentialStore.snapshot()
        val baseUrl = snapshot.baseUrl ?: return null
        val configurationId = snapshot.configurationId ?: return null
        if (!snapshot.hasBearerToken) return null
        val configuration = configurationForSnapshot(snapshot) ?: return null
        if (
            configuration.baseUrl.toString() != baseUrl ||
            configuration.configurationId != configurationId
        ) return null
        return ActivationBinding(baseUrl, configurationId)
    }

    private fun configurationFor(binding: ActivationBinding): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (!snapshot.matches(binding)) return null
        return configurationForSnapshot(snapshot)?.takeIf {
            it.baseUrl.toString() == binding.baseUrl &&
                it.configurationId == binding.configurationId
        }
    }

    private fun configurationForSnapshot(
        snapshot: ApiConnectionSnapshot,
    ): AuthenticatedApiConfiguration? {
        if (!snapshot.hasBearerToken) return null
        return try {
            credentialStore.authenticatedConfiguration()
        } catch (_: InvalidApiConfigurationException) {
            null
        } catch (_: SecureCredentialException) {
            null
        }
    }

    private fun bindingIsCurrent(binding: ActivationBinding): Boolean =
        credentialStore.snapshot().matches(binding)

    private fun durableRevisionFor(binding: ActivationBinding): ULong {
        val cursor = durableCursor()
        return cursor.revision.takeIf {
            cursor.syncOrigin == binding.baseUrl &&
                cursor.configurationId == binding.configurationId
        } ?: 0uL
    }

    private fun durableResumeRevisionFor(binding: ActivationBinding): ULong {
        val cursor = durableCursor()
        return cursor.latestObservedRevision.takeIf {
            cursor.syncOrigin == binding.baseUrl &&
                cursor.configurationId == binding.configurationId
        } ?: 0uL
    }

    private data class ActivationBinding(val baseUrl: String, val configurationId: String)
    private data class ScheduleRevisionHint(val revision: ULong)

    private fun ApiConnectionSnapshot.matches(binding: ActivationBinding): Boolean =
        hasBearerToken && baseUrl == binding.baseUrl && configurationId == binding.configurationId

    private companion object {
        const val BUSY_GATE_RETRY_DELAY_MILLIS = 250L
        const val MIN_RECONNECT_DELAY_MILLIS = 1_000L
        const val MAX_RECONNECT_DELAY_MILLIS = 30_000L
        const val FOREGROUND_SCHEDULE_POLL_INTERVAL_MILLIS = 30_000L
        const val NORMAL_CONNECTION_LIFETIME_NANOS = 4L * 60L * 1_000_000_000L
        const val CURSOR_AHEAD_STATUS = 409
        val RETRYABLE_CLIENT_HTTP_CODES = setOf(408, 425, 429)
    }
}
