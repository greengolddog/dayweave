package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamEnd
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamException
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamTransport
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.RemoteItemDeltaPage
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.isCanonicalItemCursor
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

/** Only this exact encrypted Room generation may supply an item-stream resume cursor. */
internal data class DurableCanonicalItemInvalidationCursor(
    val syncOrigin: String?,
    val configurationId: String?,
    val cursor: String?,
)

/**
 * Foreground item invalidations are memory-only hints. The existing delta, preview, publication,
 * and encrypted commit sequence remains the sole authority for canonical state.
 */
internal class ForegroundCanonicalItemInvalidationManager(
    private val credentialStore: ApiCredentialStore,
    private val plannerTransport: CanonicalPlannerTransport,
    private val streamTransport: CanonicalItemInvalidationStreamTransport?,
    private val durableCursor: () -> DurableCanonicalItemInvalidationCursor,
    private val tryLaunchAuthoritativeRefresh: ((suspend () -> Unit) -> Boolean),
    private val authoritativeRefresh: suspend () -> Boolean,
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
            val generation = AtomicLong(0)
            val pending = AtomicReference<CanonicalItemInvalidationHint?>(null)
            val drainSignals = Channel<Unit>(Channel.CONFLATED)
            coroutineScope {
                val drainJob = launch {
                    drainHints(binding, pending, drainSignals)
                }
                val streamJob = streamTransport?.let { transport ->
                    launch {
                        try {
                            collectConnections(
                                binding = binding,
                                transport = transport,
                                generation = generation,
                                pending = pending,
                                drainSignals = drainSignals,
                            )
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            // The independent 30-second delta probe remains authoritative fallback.
                        }
                    }
                }
                try {
                    pollDeltaHints(binding, generation, pending, drainSignals)
                } finally {
                    drainSignals.close()
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
        transport: CanonicalItemInvalidationStreamTransport,
        generation: AtomicLong,
        pending: AtomicReference<CanonicalItemInvalidationHint?>,
        drainSignals: Channel<Unit>,
    ) {
        var reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            val configuration = configurationFor(binding) ?: return
            // Resume only from the last SQLCipher-confirmed cursor, never a received hint.
            val cursor = durableCursorFor(binding)
            val startedAt = monotonicNanos()
            val streamEnd = try {
                transport.collect(configuration, cursor) { hintedCursor ->
                    offerHint(generation, pending, drainSignals, hintedCursor)
                }
            } catch (_: CanonicalItemInvalidationStreamException.Authentication) {
                return
            } catch (_: CanonicalItemInvalidationStreamException.Protocol) {
                return
            } catch (error: CanonicalItemInvalidationStreamException.Http) {
                if (error.statusCode in CURSOR_REPAIR_HTTP_CODES) {
                    offerHint(generation, pending, drainSignals, cursor = null)
                    return
                }
                if (
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
                CanonicalItemInvalidationStreamEnd.UNSUPPORTED -> return
                CanonicalItemInvalidationStreamEnd.ENDED -> {
                    val livedNanos = monotonicNanos() - startedAt
                    if (livedNanos >= NORMAL_CONNECTION_LIFETIME_NANOS) {
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

    private suspend fun pollDeltaHints(
        binding: ActivationBinding,
        generation: AtomicLong,
        pending: AtomicReference<CanonicalItemInvalidationHint?>,
        drainSignals: Channel<Unit>,
    ) {
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            if (pending.get() == null) {
                probeDelta(binding)?.let { target ->
                    offerHint(generation, pending, drainSignals, target.cursor)
                }
            }
            delayMillis(FOREGROUND_DELTA_PROBE_INTERVAL_MILLIS)
        }
    }

    private suspend fun probeDelta(binding: ActivationBinding): ProbeHint? {
        val configuration = configurationFor(binding) ?: return null
        val cursor = durableCursorFor(binding)
        val page = try {
            plannerTransport.itemDeltaProbe(configuration, cursor)
        } catch (error: PlannerApiException.Validation) {
            return if (error.statusCode == INVALID_DELTA_CURSOR_STATUS) ProbeHint(null) else null
        } catch (_: PlannerApiException.Authentication) {
            return null
        } catch (_: ApiBindingChangedException) {
            return null
        } catch (_: InvalidApiConfigurationException) {
            return null
        } catch (_: SecureCredentialException) {
            return null
        } catch (_: IOException) {
            return null
        }
        if (!bindingIsCurrent(binding) || !page.isValidProbePage()) return null
        return if (
            page.changes.isEmpty() && !page.hasMore && page.nextCursor == cursor
        ) {
            null
        } else {
            ProbeHint(page.nextCursor)
        }
    }

    private suspend fun drainHints(
        binding: ActivationBinding,
        pending: AtomicReference<CanonicalItemInvalidationHint?>,
        signals: Channel<Unit>,
    ) {
        for (ignored in signals) {
            var catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
            while (currentCoroutineContext().isActive) {
                if (!bindingIsCurrent(binding)) return
                val captured = pending.get() ?: break
                val durableBefore = durableCursorFor(binding)
                if (captured.cursor != null && durableBefore == captured.cursor) {
                    pending.compareAndSet(captured, null)
                    catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                }

                val completed = CompletableDeferred<Boolean>()
                val admitted = runCatching {
                    tryLaunchAuthoritativeRefresh {
                        var succeeded = false
                        try {
                            succeeded = authoritativeRefresh()
                        } finally {
                            completed.complete(succeeded)
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
                    val latest = pending.get()
                    val durableAfter = durableCursorFor(binding)
                    when {
                        latest == captured -> pending.compareAndSet(captured, null)
                        latest?.cursor != null && latest.cursor == durableAfter ->
                            pending.compareAndSet(latest, null)
                    }
                    catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                }

                delayMillis(catchUpDelay)
                catchUpDelay = (catchUpDelay * 2).coerceAtMost(MAX_RECONNECT_DELAY_MILLIS)
            }
        }
    }

    private fun offerHint(
        generation: AtomicLong,
        pending: AtomicReference<CanonicalItemInvalidationHint?>,
        drainSignals: Channel<Unit>,
        cursor: String?,
    ) {
        if (cursor != null && !isCanonicalItemCursor(cursor)) return
        val nextGeneration = generation.incrementAndGet()
        if (nextGeneration <= 0) return
        if (pending.offerIfNewer(CanonicalItemInvalidationHint(nextGeneration, cursor))) {
            drainSignals.trySend(Unit)
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
        ) {
            return null
        }
        return ActivationBinding(baseUrl, configurationId)
    }

    private fun configurationFor(binding: ActivationBinding): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (!snapshot.matches(binding)) return null
        val configuration = configurationForSnapshot(snapshot) ?: return null
        return configuration.takeIf {
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

    private fun durableCursorFor(binding: ActivationBinding): String? {
        val cursor = durableCursor()
        return cursor.cursor?.takeIf {
            cursor.syncOrigin == binding.baseUrl &&
                cursor.configurationId == binding.configurationId &&
                isCanonicalItemCursor(it)
        }
    }

    private fun RemoteItemDeltaPage.isValidProbePage(): Boolean =
        changes.size <= maximumItemDeltaResponseChanges(FOREGROUND_DELTA_PROBE_PAGE_SIZE) &&
            isCanonicalItemCursor(nextCursor)

    private data class ProbeHint(val cursor: String?)
    private data class ActivationBinding(val baseUrl: String, val configurationId: String)

    private fun ApiConnectionSnapshot.matches(binding: ActivationBinding): Boolean =
        hasBearerToken && baseUrl == binding.baseUrl && configurationId == binding.configurationId

    private companion object {
        const val BUSY_GATE_RETRY_DELAY_MILLIS = 250L
        const val MIN_RECONNECT_DELAY_MILLIS = 1_000L
        const val MAX_RECONNECT_DELAY_MILLIS = 30_000L
        const val FOREGROUND_DELTA_PROBE_INTERVAL_MILLIS = 30_000L
        const val FOREGROUND_DELTA_PROBE_PAGE_SIZE = 1
        const val INVALID_DELTA_CURSOR_STATUS = 422
        const val NORMAL_CONNECTION_LIFETIME_NANOS = 4L * 60L * 1_000_000_000L
        val CURSOR_REPAIR_HTTP_CODES = setOf(400, 409)
        val RETRYABLE_CLIENT_HTTP_CODES = setOf(408, 425, 429)
    }
}

/** CAS ordering prevents a delayed older callback from overwriting a newer opaque observation. */
internal data class CanonicalItemInvalidationHint(val generation: Long, val cursor: String?) {
    init {
        require(generation > 0)
    }
}

internal fun AtomicReference<CanonicalItemInvalidationHint?>.offerIfNewer(
    candidate: CanonicalItemInvalidationHint,
): Boolean {
    while (true) {
        val current = get()
        if (current != null && current.generation >= candidate.generation) return false
        if (compareAndSet(current, candidate)) return true
    }
}
