package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ExecutionInvalidationStreamEnd
import com.greengolddog.dayweave.network.ExecutionInvalidationStreamTransport
import java.io.IOException
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withContext
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ExecutionInvalidationSyncManagerTest {
    @Test
    fun coalescesHighWaterAndResumesOnlyFromDurableRevision() = runBlocking {
        val credentials = MutableStreamCredentials()
        val durableRevision = AtomicLong(5)
        val emitted = CompletableDeferred<Unit>()
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, onInvalidation ->
                onInvalidation(7)
                onInvalidation(9)
                onInvalidation(8)
                emitted.complete(Unit)
                awaitCancellation()
            }
        }
        var gateOpen = false
        val admissionAttempts = AtomicInteger()
        val refreshCalls = AtomicInteger()
        val manager = manager(
            credentials = credentials,
            transport = transport,
            durableRevision = durableRevision,
            tryLaunch = { action ->
                admissionAttempts.incrementAndGet()
                if (!gateOpen) {
                    false
                } else {
                    launch { action() }
                    true
                }
            },
            refresh = {
                refreshCalls.incrementAndGet()
                durableRevision.set(9)
            },
            delayMillis = { delay(5) },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) { emitted.await() }
        gateOpen = true
        withTimeout(2_000) {
            while (durableRevision.get() != 9L || refreshCalls.get() == 0) yield()
        }

        manager.cancelAndDrainActiveSession()
        assertTrue(collection.isCancelled)
        assertEquals(listOf(5L), transport.cursors)
        assertEquals(1, refreshCalls.get())
        assertTrue(admissionAttempts.get() >= 2)
    }

    @Test
    fun higherHintArrivingDuringRefreshRemainsQueuedForTheNextCatchUp() = runBlocking {
        val durableRevision = AtomicLong(5)
        val firstRefreshStarted = CompletableDeferred<Unit>()
        val releaseFirstRefresh = CompletableDeferred<Unit>()
        lateinit var emitInvalidation: (Long) -> Unit
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, onInvalidation ->
                emitInvalidation = onInvalidation
                onInvalidation(7)
                awaitCancellation()
            }
        }
        val refreshes = AtomicInteger()
        val manager = manager(
            transport = transport,
            durableRevision = durableRevision,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = {
                if (refreshes.incrementAndGet() == 1) {
                    firstRefreshStarted.complete(Unit)
                    releaseFirstRefresh.await()
                    durableRevision.set(7)
                } else {
                    durableRevision.set(9)
                }
            },
            delayMillis = { delay(1) },
        )
        val collection = async { manager.runForegroundActivation() }
        withTimeout(2_000) { firstRefreshStarted.await() }

        emitInvalidation(9)
        releaseFirstRefresh.complete(Unit)
        withTimeout(2_000) {
            while (durableRevision.get() != 9L) yield()
        }

        manager.cancelAndDrainActiveSession()
        assertTrue(collection.isCancelled)
        assertEquals(2, refreshes.get())
    }

    @Test
    fun busyActionGateRetainsHintUntilAnAuthoritativeRefreshIsAdmitted() = runBlocking {
        val durableRevision = AtomicLong(4)
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, onInvalidation ->
                onInvalidation(6)
                awaitCancellation()
            }
        }
        val attempts = AtomicInteger()
        val refreshes = AtomicInteger()
        val manager = manager(
            transport = transport,
            durableRevision = durableRevision,
            tryLaunch = { action ->
                if (attempts.incrementAndGet() < 3) {
                    false
                } else {
                    launch { action() }
                    true
                }
            },
            refresh = {
                refreshes.incrementAndGet()
                durableRevision.set(6)
            },
            delayMillis = { delay(1) },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) {
            while (durableRevision.get() != 6L) yield()
        }

        manager.cancelAndDrainActiveSession()
        assertTrue(collection.isCancelled)
        assertEquals(3, attempts.get())
        assertEquals(1, refreshes.get())
    }

    @Test
    fun unreachableHintUsesBoundedExponentialCatchUpInsteadOfATightLoop() = runBlocking {
        val durableRevision = AtomicLong(3)
        val delays = mutableListOf<Long>()
        val refreshes = AtomicInteger()
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, onInvalidation ->
                onInvalidation(100)
                awaitCancellation()
            }
        }
        lateinit var manager: ForegroundExecutionInvalidationManager
        manager = manager(
            transport = transport,
            durableRevision = durableRevision,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = { refreshes.incrementAndGet() },
            delayMillis = { millis ->
                delays += millis
                if (millis >= 4_000) manager.cancelActiveSession()
                yield()
            },
        )

        val collection = async { manager.runForegroundActivation() }
        withTimeout(2_000) { collection.join() }

        assertTrue(collection.isCancelled)
        assertEquals(listOf(1_000L, 2_000L, 4_000L), delays.take(3))
        assertEquals(3, refreshes.get())
        assertEquals(3L, durableRevision.get())
    }

    @Test
    fun unsupportedIsScopedToOneActivationAndPollingCanStartANewProbe() = runBlocking {
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, _ -> ExecutionInvalidationStreamEnd.UNSUPPORTED }
        }
        val manager = manager(transport = transport)

        manager.runForegroundActivation()
        manager.runForegroundActivation()

        assertEquals(2, transport.calls.get())
    }

    @Test
    fun transientConnectionsBackOffExponentiallyAndCapableServerCanRecover() = runBlocking {
        val delays = mutableListOf<Long>()
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, _ ->
                if (calls.get() <= 8) throw IOException("synthetic disconnect")
                ExecutionInvalidationStreamEnd.UNSUPPORTED
            }
        }
        val manager = manager(
            transport = transport,
            delayMillis = { delays += it },
        )

        manager.runForegroundActivation()

        assertEquals(9, transport.calls.get())
        assertEquals(
            listOf(1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L, 30_000L),
            delays,
        )
    }

    @Test
    fun normalServerExpiryReconnectsImmediatelyWithTheLatestDurableCursor() = runBlocking {
        val durableRevision = AtomicLong(5)
        val clock = listOf(
            0L,
            4L * 60L * 1_000_000_000L,
            4L * 60L * 1_000_000_000L + 1,
        ).iterator()
        val delays = mutableListOf<Long>()
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, _ ->
                if (calls.get() == 1) {
                    durableRevision.set(7)
                    ExecutionInvalidationStreamEnd.ENDED
                } else {
                    ExecutionInvalidationStreamEnd.UNSUPPORTED
                }
            }
        }
        val manager = manager(
            transport = transport,
            durableRevision = durableRevision,
            delayMillis = { delays += it },
            monotonicNanos = { clock.next() },
        )

        manager.runForegroundActivation()

        assertEquals(listOf(5L, 7L), transport.cursors)
        assertTrue(delays.isEmpty())
    }

    @Test
    fun bindingCancellationDrainsOldStreamAndNewBindingCannotReuseOldCursor() = runBlocking {
        val credentials = MutableStreamCredentials()
        val durableRevision = AtomicLong(12)
        val oldStarted = CompletableDeferred<Unit>()
        val oldDrained = CompletableDeferred<Unit>()
        lateinit var staleOldBindingCallback: (Long) -> Unit
        var refreshAttempted = false
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, onInvalidation ->
                if (calls.get() == 1) {
                    staleOldBindingCallback = onInvalidation
                    oldStarted.complete(Unit)
                    try {
                        awaitCancellation()
                    } finally {
                        oldDrained.complete(Unit)
                    }
                }
                ExecutionInvalidationStreamEnd.UNSUPPORTED
            }
        }
        val manager = manager(
            credentials = credentials,
            transport = transport,
            durableRevision = durableRevision,
            tryLaunch = {
                refreshAttempted = true
                false
            },
        )
        val oldCollection = async { manager.runForegroundActivation() }
        withTimeout(2_000) { oldStarted.await() }

        manager.cancelAndDrainActiveSession()
        withTimeout(2_000) { oldDrained.await() }
        assertTrue(oldCollection.isCancelled)
        credentials.configurationId = "configuration-b"
        staleOldBindingCallback(99)

        manager.runForegroundActivation()

        assertEquals(listOf(12L, 0L), transport.cursors)
        assertEquals("configuration-b", transport.configurationIds.last())
        assertFalse(refreshAttempted)
    }

    @Test
    fun immediateForegroundRestartWaitsForCancelledParserCleanupThenReconnects() = runBlocking {
        val firstStarted = CompletableDeferred<Unit>()
        val cleanupStarted = CompletableDeferred<Unit>()
        val releaseCleanup = CompletableDeferred<Unit>()
        val transport = FakeInvalidationTransport().apply {
            handler = { _, _, _ ->
                if (calls.get() == 1) {
                    firstStarted.complete(Unit)
                    try {
                        awaitCancellation()
                    } finally {
                        withContext(NonCancellable) {
                            cleanupStarted.complete(Unit)
                            releaseCleanup.await()
                        }
                    }
                }
                ExecutionInvalidationStreamEnd.UNSUPPORTED
            }
        }
        val manager = manager(transport = transport)
        val first = async { manager.runForegroundActivation() }
        withTimeout(2_000) { firstStarted.await() }
        manager.cancelActiveSession()
        withTimeout(2_000) { cleanupStarted.await() }

        val replacement = async { manager.runForegroundActivation() }
        yield()
        assertEquals(1, transport.calls.get())
        releaseCleanup.complete(Unit)

        withTimeout(2_000) { replacement.await() }
        assertTrue(first.isCancelled)
        assertEquals(2, transport.calls.get())
    }

    @Test
    fun absentOptionalStreamTransportLeavesPollingOnly() = runBlocking {
        var refreshAttempted = false
        val manager = manager(
            transport = null,
            tryLaunch = {
                refreshAttempted = true
                false
            },
        )

        manager.runForegroundActivation()

        assertFalse(refreshAttempted)
    }

    private fun manager(
        credentials: MutableStreamCredentials = MutableStreamCredentials(),
        transport: ExecutionInvalidationStreamTransport? = FakeInvalidationTransport().apply {
            handler = { _, _, _ -> ExecutionInvalidationStreamEnd.UNSUPPORTED }
        },
        durableRevision: AtomicLong = AtomicLong(5),
        tryLaunch: ((suspend () -> Unit) -> Boolean) = { false },
        refresh: suspend () -> Unit = {},
        delayMillis: suspend (Long) -> Unit = { delay(it) },
        monotonicNanos: () -> Long = System::nanoTime,
    ) = ForegroundExecutionInvalidationManager(
        credentialStore = credentials,
        streamTransport = transport,
        durableCursor = {
            DurableExecutionInvalidationCursor(
                syncOrigin = ORIGIN,
                configurationId = "configuration-a",
                revision = durableRevision.get(),
            )
        },
        tryLaunchAuthoritativeRefresh = tryLaunch,
        authoritativeRefresh = refresh,
        delayMillis = delayMillis,
        monotonicNanos = monotonicNanos,
    )

    private companion object {
        const val ORIGIN = "https://api.example.test/"
    }
}

private class MutableStreamCredentials : ApiCredentialStore {
    @Volatile
    var configurationId: String? = "configuration-a"

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = ORIGIN.takeIf { configurationId != null },
        hasBearerToken = configurationId != null,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        configurationId?.let { binding ->
            AuthenticatedApiConfiguration.createBound(
                baseUrl = ORIGIN,
                bearerToken = "synthetic-stream-token",
                configurationId = binding,
            )
        }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() {
        configurationId = null
    }

    override fun recordSuccessfulSync(epochMillis: Long) = Unit

    private companion object {
        const val ORIGIN = "https://api.example.test/"
    }
}

private class FakeInvalidationTransport : ExecutionInvalidationStreamTransport {
    val calls = AtomicInteger()
    val cursors = mutableListOf<Long>()
    val configurationIds = mutableListOf<String?>()
    var handler: suspend (
        AuthenticatedApiConfiguration,
        Long,
        (Long) -> Unit,
    ) -> ExecutionInvalidationStreamEnd = { _, _, _ -> error("No stream response configured") }

    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: Long,
        onInvalidation: (Long) -> Unit,
    ): ExecutionInvalidationStreamEnd {
        calls.incrementAndGet()
        cursors += lastDurableRevision
        configurationIds += configuration.configurationId
        return handler(configuration, lastDurableRevision, onInvalidation)
    }
}
