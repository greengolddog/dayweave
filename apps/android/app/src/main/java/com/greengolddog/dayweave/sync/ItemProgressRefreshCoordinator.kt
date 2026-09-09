package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiCredentialStore
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.selects.select

internal enum class ItemProgressRefreshResult { SUCCESS, FAILED, DEFERRED }

/** A path hint or failed catch-up cannot mint a progress baseline; re-read after canonical repair. */
internal suspend fun refreshItemProgressDetailEvidence(
    itemId: String,
    manager: ItemProgressSyncManager,
    isCurrent: () -> Boolean,
    canonicalCatchUp: suspend (() -> Boolean) -> Boolean,
): Boolean {
    if (!isCurrent()) return false
    if (manager.load(itemId, isCurrent)) return isCurrent()
    if (!isCurrent() || itemId !in manager.state.value.canonicalCatchUpItemIds) return false
    if (!canonicalCatchUp(isCurrent) || !isCurrent()) return false
    return manager.load(itemId, isCurrent) && isCurrent()
}

/** Content-free lifecycle hints wake retries; only a verified durable HTTP result is read proof. */
internal class ItemProgressRefreshCoordinator(
    private val credentials: ApiCredentialStore,
    private val workAllowed: () -> Boolean,
    private val refreshSelected: suspend (String, () -> Boolean) -> ItemProgressRefreshResult,
    private val replayOutbox: suspend (() -> Boolean) -> ItemProgressRefreshResult,
    private val delayMillis: suspend (Long) -> Unit = { delay(it) },
) {
    private val generation = AtomicLong()
    private val selected = AtomicReference<Session?>(null)
    private val foreground = AtomicReference<Session?>(null)
    private val reconnectHints = MutableSharedFlow<Unit>(extraBufferCapacity = 1,
        onBufferOverflow = BufferOverflow.DROP_OLDEST)

    /** A newly saved or explicitly retried outbox should not wait for the ordinary fallback. */
    fun requestOutboxReplay() { reconnectHints.tryEmit(Unit) }

    fun cancelActiveSessions() {
        generation.incrementAndGet() // Fence a non-cooperative response before requesting cancellation.
        selected.get()?.job?.cancel()
        foreground.get()?.job?.cancel()
    }

    suspend fun cancelAndDrainActiveSessions() {
        val jobs = listOfNotNull(selected.get()?.job, foreground.get()?.job)
        cancelActiveSessions()
        jobs.forEach { it.join() }
    }

    suspend fun runSelectedDetail(itemId: String) = runSession(selected) { alive, current ->
        poll(alive, current, 5_000) { refreshSelected(itemId, current) }
    }

    /** Independent of selected UI: ambiguous writes keep their exact outbox custody on any failure. */
    suspend fun runForegroundActivation(networkReconnects: Flow<Unit>) = runSession(foreground) { alive, current ->
        coroutineScope {
            val network = launch(start = CoroutineStart.UNDISPATCHED) {
                networkReconnects.catch { /* Timed retry remains available without network hints. */ }
                    .collect { if (current()) reconnectHints.tryEmit(Unit) }
            }
            try { poll(alive, current, 30_000) { replayOutbox(current) } }
            finally { network.cancel() }
        }
    }

    private suspend fun runSession(owner: AtomicReference<Session?>,
        action: suspend (() -> Boolean, () -> Boolean) -> Unit,
    ) {
        val binding = binding() ?: return
        val session = Session(currentCoroutineContext().job, generation.get(), binding)
        val previous = owner.getAndSet(session)
        previous?.job?.cancel()
        fun alive() = owner.get() === session && session.job.isActive &&
            generation.get() == session.generation && binding() == session.binding
        fun current() = alive() && workAllowed()
        try { if (alive()) action(::alive, ::current) }
        finally { owner.compareAndSet(session, null) }
    }

    private suspend fun poll(alive: () -> Boolean, current: () -> Boolean, successDelay: Long,
        refresh: suspend () -> ItemProgressRefreshResult,
    ) = coroutineScope {
        val signals = Channel<Unit>(Channel.CONFLATED)
        val hints = launch(start = CoroutineStart.UNDISPATCHED) {
            reconnectHints.collect { signals.trySend(Unit) }
        }
        var failures = 0
        try {
            while (alive()) {
                var result = try { if (current()) refresh() else ItemProgressRefreshResult.DEFERRED }
                catch (error: CancellationException) { throw error }
                catch (_: Exception) { ItemProgressRefreshResult.FAILED }
                if (!alive()) return@coroutineScope
                if (!current()) result = ItemProgressRefreshResult.DEFERRED
                failures = when (result) {
                    ItemProgressRefreshResult.SUCCESS -> 0
                    ItemProgressRefreshResult.FAILED -> (failures + 1).coerceAtMost(4)
                    ItemProgressRefreshResult.DEFERRED -> failures
                }
                val wait = when (result) {
                    ItemProgressRefreshResult.SUCCESS -> successDelay
                    ItemProgressRefreshResult.FAILED -> (5_000L * (1L shl failures)).coerceAtMost(60_000)
                    ItemProgressRefreshResult.DEFERRED -> 1_000L
                }
                // A reconnect never cancels an in-flight PUT; it only wakes the next wait.
                if (waitForHint(signals, wait)) failures = 0
            }
        } finally { hints.cancel(); signals.close() }
    }

    private suspend fun waitForHint(signals: Channel<Unit>, milliseconds: Long): Boolean = coroutineScope {
        val timer = async { delayMillis(milliseconds); false }
        try { select { signals.onReceive { true }; timer.onAwait { it } } }
        finally { timer.cancel() }
    }

    private fun binding(): Binding? = credentials.snapshot().let {
        if (!it.hasBearerToken || it.baseUrl.isNullOrBlank() || it.configurationId.isNullOrBlank()) null
        else Binding(requireNotNull(it.baseUrl), requireNotNull(it.configurationId))
    }
    private data class Binding(val origin: String, val configurationId: String)
    private data class Session(val job: Job, val generation: Long, val binding: Binding)
}
