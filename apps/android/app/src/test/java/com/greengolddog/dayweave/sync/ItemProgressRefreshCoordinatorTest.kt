package com.greengolddog.dayweave.sync

import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableSharedFlow
import org.junit.Assert.*
import org.junit.Test

class ItemProgressRefreshCoordinatorTest {
    @Test fun temporaryRecoveryBlockerWaitsWithoutNetworkAndResumesWithoutReselection() = runBlocking {
        val clock = ManualDelay()
        var allowed = false
        var calls = 0
        val coordinator = ItemProgressRefreshCoordinator(GenerationBoundCredentialStore(), { allowed }, { _, _ ->
            calls++
            ItemProgressRefreshResult.SUCCESS
        }, { error("No outbox expected") }, clock::sleep)
        val selected = launch { coordinator.runSelectedDetail("synthetic-item") }
        val blocked = clock.next()
        assertEquals(1_000L, blocked.milliseconds)
        assertEquals(0, calls)
        allowed = true
        blocked.release.complete(Unit)
        val regular = clock.next()
        assertEquals(5_000L, regular.milliseconds)
        assertEquals(1, calls)
        allowed = false
        regular.release.complete(Unit)
        assertEquals(1_000L, clock.next().milliseconds)
        assertEquals(1, calls)
        selected.cancelAndJoin()
    }

    @Test fun visibleDetailRefreshesImmediatelyThenEveryFiveSecondsAndRestartsImmediately() = runBlocking {
        val clock = ManualDelay()
        val calls = Channel<String>(Channel.UNLIMITED)
        val coordinator = coordinator(clock, load = { id, _ -> calls.send(id); ItemProgressRefreshResult.SUCCESS })
        val first = launch { coordinator.runSelectedDetail("synthetic-item") }
        assertEquals("synthetic-item", calls.next())
        val tick = clock.next()
        assertEquals(5_000L, tick.milliseconds)
        tick.release.complete(Unit)
        assertEquals("synthetic-item", calls.next())
        clock.next()
        first.cancelAndJoin()
        val resumed = launch { coordinator.runSelectedDetail("synthetic-item") }
        assertEquals("synthetic-item", calls.next())
        resumed.cancelAndJoin()
        assertTrue(calls.tryReceive().isFailure)
    }

    @Test fun failuresBackOffToSixtySecondsAndSuccessResetsInterval() = runBlocking {
        val clock = ManualDelay()
        var calls = 0
        val coordinator = coordinator(clock, load = { _, _ ->
            if (++calls <= 5) ItemProgressRefreshResult.FAILED else ItemProgressRefreshResult.SUCCESS
        })
        val selected = launch { coordinator.runSelectedDetail("synthetic-item") }
        for (expected in listOf(10_000L, 20_000L, 40_000L, 60_000L, 60_000L)) {
            val wait = clock.next()
            assertEquals(expected, wait.milliseconds)
            wait.release.complete(Unit)
        }
        assertEquals(5_000L, clock.next().milliseconds)
        selected.cancelAndJoin()
    }

    @Test fun localGateContentionRetriesSoonWithoutIncreasingNetworkBackoff() = runBlocking {
        val clock = ManualDelay()
        var calls = 0
        val coordinator = coordinator(clock, load = { _, _ ->
            when (++calls) { 1 -> ItemProgressRefreshResult.DEFERRED; else -> ItemProgressRefreshResult.FAILED }
        })
        val selected = launch { coordinator.runSelectedDetail("synthetic-item") }
        val busy = clock.next()
        assertEquals(1_000L, busy.milliseconds)
        busy.release.complete(Unit)
        assertEquals(10_000L, clock.next().milliseconds)
        selected.cancelAndJoin()
    }

    @Test fun reconnectWakesBackoffAndOutboxRunsWithoutAnyDetail() = runBlocking {
        val clock = ManualDelay()
        val reconnects = MutableSharedFlow<Unit>()
        val calls = Channel<Unit>(Channel.UNLIMITED)
        val coordinator = coordinator(clock, replay = { calls.send(Unit); ItemProgressRefreshResult.FAILED })
        val foreground = launch { coordinator.runForegroundActivation(reconnects) }
        calls.next()
        assertEquals(10_000L, clock.next().milliseconds)
        reconnects.emit(Unit)
        calls.next()
        assertEquals(10_000L, clock.next().milliseconds)
        foreground.cancelAndJoin()
        assertEquals(0, reconnects.subscriptionCount.value)
    }

    @Test fun reconnectCannotCancelAnInFlightWriteAndItsWakeIsRetained() = runBlocking {
        val clock = ManualDelay()
        val reconnects = MutableSharedFlow<Unit>()
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val second = CompletableDeferred<Unit>()
        var calls = 0
        val coordinator = coordinator(clock, replay = { current ->
            if (++calls == 1) { started.complete(Unit); release.await(); assertTrue(current()) }
            else second.complete(Unit)
            ItemProgressRefreshResult.SUCCESS
        })
        val foreground = launch { coordinator.runForegroundActivation(reconnects) }
        withTimeout(2_000) { started.await() }
        reconnects.emit(Unit)
        yield()
        assertEquals(1, calls)
        assertTrue(foreground.isActive)
        release.complete(Unit)
        withTimeout(2_000) { second.await() }
        foreground.cancelAndJoin()
    }

    @Test fun lockFencesCancellationInsensitiveResponseUntilItActuallyReturns() = runBlocking {
        val clock = ManualDelay()
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        var currentAfterReturn = true
        val coordinator = coordinator(clock, load = { _, current ->
            withContext(NonCancellable) { started.complete(Unit); release.await(); currentAfterReturn = current() }
            ItemProgressRefreshResult.SUCCESS
        })
        val selected = launch { coordinator.runSelectedDetail("synthetic-item") }
        withTimeout(2_000) { started.await() }
        coordinator.cancelActiveSessions()
        assertFalse(selected.isCompleted)
        release.complete(Unit)
        selected.join()
        assertFalse(currentAfterReturn)
        assertTrue(clock.waits.tryReceive().isFailure)
    }

    @Test fun changingCredentialBindingRejectsOldDetailResponse() = runBlocking {
        val clock = ManualDelay()
        val credentials = GenerationBoundCredentialStore()
        val seen = CompletableDeferred<Boolean>()
        val coordinator = ItemProgressRefreshCoordinator(credentials, { true }, { _, current ->
            credentials.configurationId = "synthetic-new-account"
            seen.complete(current())
            ItemProgressRefreshResult.SUCCESS
        }, { ItemProgressRefreshResult.SUCCESS }, clock::sleep)
        val selected = launch { coordinator.runSelectedDetail("synthetic-item") }
        assertFalse(withTimeout(2_000) { seen.await() })
        selected.join()
        assertTrue(clock.waits.tryReceive().isFailure)
    }

    private fun coordinator(clock: ManualDelay,
        load: suspend (String, () -> Boolean) -> ItemProgressRefreshResult = { _, _ -> error("No detail expected") },
        replay: suspend (() -> Boolean) -> ItemProgressRefreshResult = { error("No outbox expected") },
    ) = ItemProgressRefreshCoordinator(GenerationBoundCredentialStore(), { true }, load, replay, clock::sleep)

    private class ManualDelay {
        val waits = Channel<Wait>(Channel.UNLIMITED)
        suspend fun sleep(milliseconds: Long) {
            val wait = Wait(milliseconds)
            waits.send(wait)
            wait.release.await()
        }
        suspend fun next(): Wait = waits.next()
    }
    private data class Wait(val milliseconds: Long, val release: CompletableDeferred<Unit> = CompletableDeferred())
    private companion object {
        suspend fun <T> Channel<T>.next(): T = withTimeout(2_000) { receive() }
    }
}
