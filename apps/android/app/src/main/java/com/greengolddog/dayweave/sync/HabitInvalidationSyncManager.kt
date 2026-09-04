package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.HabitInvalidationStreamEnd
import com.greengolddog.dayweave.network.HabitInvalidationStreamException
import com.greengolddog.dayweave.network.HabitInvalidationStreamTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.isHabitDeltaCursor
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch

/** Only the exact encrypted ledger generation may supply a habit-stream resume cursor. */
internal data class DurableHabitInvalidationCursor(
    val syncOrigin: String?,
    val configurationId: String?,
    val cursor: String?,
)

/**
 * Foreground SSE is only a content-free hint. Habit delta plus an encrypted atomic commit remains
 * authoritative, and the app's independent foreground poll remains the rollout fallback.
 */
internal class ForegroundHabitInvalidationManager(
    private val credentialStore: ApiCredentialStore,
    private val streamTransport: HabitInvalidationStreamTransport,
    private val durableCursor: () -> DurableHabitInvalidationCursor,
    private val tryLaunchAuthoritativeRefresh: ((suspend () -> Boolean) -> Deferred<Boolean>?),
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
            val pending = AtomicReference<HabitInvalidationHint?>(null)
            val drainSignals = Channel<Unit>(Channel.CONFLATED)
            coroutineScope {
                val drainJob = launch { drainHints(binding, pending, drainSignals) }
                try {
                    collectConnections(binding, generation, pending, drainSignals)
                } finally {
                    drainSignals.close()
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
        generation: AtomicLong,
        pending: AtomicReference<HabitInvalidationHint?>,
        drainSignals: Channel<Unit>,
    ) {
        var reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            val configuration = configurationFor(binding) ?: return
            // Never resume from a merely observed event: only SQLCipher-confirmed delta state.
            val cursor = durableCursorFor(binding)
            val startedAt = monotonicNanos()
            val streamEnd = try {
                streamTransport.collect(configuration, cursor) { hintedCursor ->
                    offerHint(generation, pending, drainSignals, hintedCursor)
                }
            } catch (_: HabitInvalidationStreamException.Authentication) {
                return
            } catch (_: HabitInvalidationStreamException.Protocol) {
                return
            } catch (error: HabitInvalidationStreamException.Http) {
                if (error.statusCode in CURSOR_REPAIR_HTTP_CODES) {
                    offerHint(generation, pending, drainSignals, cursor = null)
                    awaitPendingDrain(binding, pending)
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
                HabitInvalidationStreamEnd.UNSUPPORTED -> return
                HabitInvalidationStreamEnd.ENDED -> {
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

    private suspend fun drainHints(
        binding: ActivationBinding,
        pending: AtomicReference<HabitInvalidationHint?>,
        signals: Channel<Unit>,
    ) {
        for (ignored in signals) {
            var catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
            while (currentCoroutineContext().isActive) {
                if (!bindingIsCurrent(binding)) return
                val captured = pending.get() ?: break
                val durableBefore = durableCursorFor(binding)
                if (captured.cursor != null && captured.cursor == durableBefore) {
                    pending.compareAndSet(captured, null)
                    catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                }

                val completion = runCatching {
                    tryLaunchAuthoritativeRefresh(authoritativeRefresh)
                }.getOrNull()
                if (completion == null) {
                    delayMillis(BUSY_GATE_RETRY_DELAY_MILLIS)
                    continue
                }

                val succeeded = try {
                    completion.await()
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    false
                }
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

    private suspend fun awaitPendingDrain(
        binding: ActivationBinding,
        pending: AtomicReference<HabitInvalidationHint?>,
    ) {
        while (
            currentCoroutineContext().isActive &&
            bindingIsCurrent(binding) &&
            pending.get() != null
        ) {
            delayMillis(BUSY_GATE_RETRY_DELAY_MILLIS)
        }
    }

    private fun offerHint(
        generation: AtomicLong,
        pending: AtomicReference<HabitInvalidationHint?>,
        drainSignals: Channel<Unit>,
        cursor: String?,
    ) {
        if (cursor != null && !isHabitDeltaCursor(cursor)) return
        val nextGeneration = generation.incrementAndGet()
        if (nextGeneration <= 0) return
        val candidate = HabitInvalidationHint(nextGeneration, cursor)
        while (true) {
            val current = pending.get()
            if (current != null && current.generation >= candidate.generation) return
            if (pending.compareAndSet(current, candidate)) {
                drainSignals.trySend(Unit)
                return
            }
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
        val durable = durableCursor()
        return durable.cursor?.takeIf {
            durable.syncOrigin == binding.baseUrl &&
                durable.configurationId == binding.configurationId &&
                isHabitDeltaCursor(it)
        }
    }

    private data class ActivationBinding(val baseUrl: String, val configurationId: String)

    private fun ApiConnectionSnapshot.matches(binding: ActivationBinding): Boolean =
        hasBearerToken && baseUrl == binding.baseUrl && configurationId == binding.configurationId

    private companion object {
        const val BUSY_GATE_RETRY_DELAY_MILLIS = 250L
        const val MIN_RECONNECT_DELAY_MILLIS = 1_000L
        const val MAX_RECONNECT_DELAY_MILLIS = 30_000L
        const val NORMAL_CONNECTION_LIFETIME_NANOS = 4L * 60L * 1_000_000_000L
        val CURSOR_REPAIR_HTTP_CODES = setOf(400, 409)
        val RETRYABLE_CLIENT_HTTP_CODES = setOf(408, 425, 429)
    }
}

private data class HabitInvalidationHint(val generation: Long, val cursor: String?) {
    init {
        require(generation > 0)
    }
}
