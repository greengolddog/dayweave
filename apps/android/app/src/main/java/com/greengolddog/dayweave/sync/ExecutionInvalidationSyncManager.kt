package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ExecutionInvalidationStreamEnd
import com.greengolddog.dayweave.network.ExecutionInvalidationStreamException
import com.greengolddog.dayweave.network.ExecutionInvalidationStreamTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.SecureCredentialException
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
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

/** Only this exact durable binding may become an SSE resume cursor. */
internal data class DurableExecutionInvalidationCursor(
    val syncOrigin: String?,
    val configurationId: String?,
    val revision: Long,
)

/**
 * Runs beside polling while the unlocked UI is foregrounded. Stream revisions are untrusted
 * memory-only hints; the normal stable snapshot/history reconciliation remains authoritative.
 */
internal class ForegroundExecutionInvalidationManager(
    private val credentialStore: ApiCredentialStore,
    private val streamTransport: ExecutionInvalidationStreamTransport?,
    private val durableCursor: () -> DurableExecutionInvalidationCursor,
    private val tryLaunchAuthoritativeRefresh: ((suspend () -> Unit) -> Boolean),
    private val authoritativeRefresh: suspend () -> Unit,
    private val delayMillis: suspend (Long) -> Unit = { delay(it) },
    private val monotonicNanos: () -> Long = System::nanoTime,
) {
    private val activeForegroundJob = AtomicReference<Job?>(null)

    /** A lock, background transition, or composition disposal can cancel without blocking. */
    fun cancelActiveSession() {
        activeForegroundJob.get()?.cancel()
    }

    /** Credential replacement waits until the old response body and parser are fully drained. */
    suspend fun cancelAndDrainActiveSession() {
        activeForegroundJob.get()?.cancelAndJoin()
    }

    suspend fun runForegroundActivation() {
        val transport = streamTransport ?: return
        val sessionJob = currentCoroutineContext().job
        if (!registerSession(sessionJob)) return
        try {
            val binding = captureBinding() ?: return
            val highWater = AtomicLong(0)
            val drainSignals = Channel<Unit>(Channel.CONFLATED)
            coroutineScope {
                val drainJob = launch {
                    drainHighWater(binding, highWater, drainSignals)
                }
                try {
                    collectConnections(
                        binding = binding,
                        transport = transport,
                        highWater = highWater,
                        drainSignals = drainSignals,
                    )
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
        transport: ExecutionInvalidationStreamTransport,
        highWater: AtomicLong,
        drainSignals: Channel<Unit>,
    ) {
        var reconnectDelay = MIN_RECONNECT_DELAY_MILLIS
        while (currentCoroutineContext().isActive && bindingIsCurrent(binding)) {
            val configuration = configurationFor(binding) ?: return
            // Never resume from a hint. Only an encrypted, durably applied snapshot can advance
            // this cursor, including after a reconnect within the same foreground activation.
            val cursor = durableRevisionFor(binding)
            val startedAt = monotonicNanos()
            val streamEnd = try {
                transport.collect(configuration, cursor) { revision ->
                    offerHighWater(highWater, revision)
                    drainSignals.trySend(Unit)
                }
            } catch (_: ExecutionInvalidationStreamException.Authentication) {
                return
            } catch (_: ExecutionInvalidationStreamException.Protocol) {
                return
            } catch (error: ExecutionInvalidationStreamException.Http) {
                if (
                    error.statusCode in 400..499 &&
                    error.statusCode !in RETRYABLE_CLIENT_HTTP_CODES
                ) {
                    return
                }
                null
            } catch (_: InvalidApiConfigurationException) {
                return
            } catch (_: SecureCredentialException) {
                return
            } catch (_: IOException) {
                null
            }
            if (!bindingIsCurrent(binding)) return
            when (streamEnd) {
                ExecutionInvalidationStreamEnd.UNSUPPORTED -> return
                ExecutionInvalidationStreamEnd.ENDED -> {
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

    private suspend fun drainHighWater(
        binding: ActivationBinding,
        highWater: AtomicLong,
        signals: Channel<Unit>,
    ) {
        for (ignored in signals) {
            var catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
            while (currentCoroutineContext().isActive) {
                if (!bindingIsCurrent(binding)) return
                val target = highWater.get()
                if (target <= 0) break
                if (durableRevisionFor(binding) >= target) {
                    highWater.compareAndSet(target, 0)
                    continue
                }

                val completed = CompletableDeferred<Unit>()
                val admitted = runCatching {
                    tryLaunchAuthoritativeRefresh {
                        try {
                            authoritativeRefresh()
                        } finally {
                            completed.complete(Unit)
                        }
                    }
                }.getOrDefault(false)
                if (!admitted) {
                    // Unlike UI taps, invalidations retain their high-water mark until the busy
                    // action leaves. This is bounded waiting, not a dropped best-effort refresh.
                    delayMillis(BUSY_GATE_RETRY_DELAY_MILLIS)
                    continue
                }
                completed.await()
                if (!bindingIsCurrent(binding)) return
                if (durableRevisionFor(binding) >= target) {
                    highWater.compareAndSet(target, 0)
                    catchUpDelay = MIN_RECONNECT_DELAY_MILLIS
                    continue
                }

                // An ahead or temporarily unreachable hint cannot create a tight refresh loop.
                delayMillis(catchUpDelay)
                catchUpDelay = (catchUpDelay * 2).coerceAtMost(MAX_RECONNECT_DELAY_MILLIS)
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

    private fun durableRevisionFor(binding: ActivationBinding): Long {
        val cursor = durableCursor()
        return if (
            cursor.revision >= 0 &&
            cursor.syncOrigin == binding.baseUrl &&
            cursor.configurationId == binding.configurationId
        ) {
            cursor.revision
        } else {
            0
        }
    }

    private fun offerHighWater(highWater: AtomicLong, revision: Long) {
        if (revision <= 0) return
        while (true) {
            val current = highWater.get()
            if (revision <= current || highWater.compareAndSet(current, revision)) return
        }
    }

    private data class ActivationBinding(
        val baseUrl: String,
        val configurationId: String,
    )

    private fun ApiConnectionSnapshot.matches(binding: ActivationBinding): Boolean =
        hasBearerToken && baseUrl == binding.baseUrl && configurationId == binding.configurationId

    private companion object {
        const val BUSY_GATE_RETRY_DELAY_MILLIS = 250L
        const val MIN_RECONNECT_DELAY_MILLIS = 1_000L
        const val MAX_RECONNECT_DELAY_MILLIS = 30_000L
        const val NORMAL_CONNECTION_LIFETIME_NANOS = 4L * 60L * 1_000_000_000L
        val RETRYABLE_CLIENT_HTTP_CODES = setOf(408, 425, 429)
    }
}
