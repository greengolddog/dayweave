package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamEnd
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamException
import com.greengolddog.dayweave.network.ScheduleInvalidationStreamTransport
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleInvalidationSyncManagerTest {
    @Test
    fun coalescesSseHighWaterAndResumesOnlyFromDurableRevision() = runBlocking {
        val durable = AtomicReference(5uL)
        val emitted = CompletableDeferred<Unit>()
        val transport = FakeScheduleStream().apply {
            handler = { _, _, invalidation ->
                invalidation(7uL)
                invalidation(9uL)
                invalidation(8uL)
                emitted.complete(Unit)
                awaitCancellation()
            }
        }
        val refreshes = AtomicInteger()
        val manager = manager(
            transport = transport,
            durableRevision = durable,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = {
                emitted.await()
                refreshes.incrementAndGet()
                durable.set(9uL)
                true
            },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) { emitted.await() }
        withTimeout(2_000) {
            while (durable.get() != 9uL) yield()
        }

        manager.cancelAndDrainActiveSession()
        assertTrue(collection.isCancelled)
        assertEquals(listOf(5uL), transport.cursors)
        // The immediate poll and coalesced hint may share one refresh; neither may install a hint.
        assertTrue(refreshes.get() in 1..2)
    }

    @Test
    fun cursorAheadStopsThatStreamAndForcesAuthoritativeGetInsteadOfRetryingCursor() = runBlocking {
        val refreshStarted = CompletableDeferred<Unit>()
        val transport = FakeScheduleStream().apply {
            handler = { _, _, _ -> throw ScheduleInvalidationStreamException.Http(409) }
        }
        lateinit var manager: ForegroundScheduleInvalidationManager
        manager = manager(
            transport = transport,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = {
                refreshStarted.complete(Unit)
                manager.cancelActiveSession()
                true
            },
            delayMillis = { delay(1) },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) { refreshStarted.await() }
        withTimeout(2_000) { collection.join() }

        assertTrue(collection.isCancelled)
        assertEquals(1, transport.calls.get())
    }

    @Test
    fun cursorAheadDropsOldEpochHighWaterAndReconnectsFromRepairedRevision() = runBlocking {
        val durable = AtomicReference(12uL)
        val firstPollCompleted = CompletableDeferred<Unit>()
        val cursorRepairCompleted = CompletableDeferred<Unit>()
        val reconnected = CompletableDeferred<Unit>()
        val runawayRefresh = CompletableDeferred<Unit>()
        val transport = FakeScheduleStream().apply {
            handler = { _, _, invalidation ->
                if (calls.get() == 1) {
                    firstPollCompleted.await()
                    invalidation(12uL)
                    throw ScheduleInvalidationStreamException.Http(409)
                }
                reconnected.complete(Unit)
                awaitCancellation()
            }
        }
        val refreshes = AtomicInteger()
        val manager = manager(
            transport = transport,
            durableRevision = durable,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = {
                when (refreshes.incrementAndGet()) {
                    1 -> firstPollCompleted.complete(Unit)
                    2 -> {
                        durable.set(2uL)
                        cursorRepairCompleted.complete(Unit)
                    }
                    else -> runawayRefresh.complete(Unit)
                }
                true
            },
            delayMillis = { delayMillis ->
                if (delayMillis == 30_000L) awaitCancellation() else yield()
            },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) { cursorRepairCompleted.await() }
        withTimeout(2_000) { reconnected.await() }
        assertEquals(null, withTimeoutOrNull(100) { runawayRefresh.await() })

        manager.cancelAndDrainActiveSession()
        assertTrue(collection.isCancelled)
        assertEquals(listOf(12uL, 2uL), transport.cursors)
        assertEquals(2, refreshes.get())
        assertEquals(2uL, durable.get())
    }

    @Test
    fun stream404FallsBackToBoundedPollingAndPollDoesNotUseHintAsAuthority() = runBlocking {
        val refreshes = AtomicInteger()
        val transport = FakeScheduleStream().apply {
            handler = { _, _, _ -> ScheduleInvalidationStreamEnd.UNSUPPORTED }
        }
        lateinit var manager: ForegroundScheduleInvalidationManager
        manager = manager(
            transport = transport,
            tryLaunch = { action ->
                launch { action() }
                true
            },
            refresh = {
                if (refreshes.incrementAndGet() == 2) manager.cancelActiveSession()
                true
            },
            delayMillis = { delay(1) },
        )
        val collection = async { manager.runForegroundActivation() }

        withTimeout(2_000) { collection.join() }

        assertTrue(collection.isCancelled)
        assertEquals(1, transport.calls.get())
        assertEquals(2, refreshes.get())
    }

    private fun manager(
        transport: ScheduleInvalidationStreamTransport?,
        durableRevision: AtomicReference<ULong> = AtomicReference(5uL),
        tryLaunch: ((suspend () -> Unit) -> Boolean),
        refresh: suspend () -> Boolean,
        delayMillis: suspend (Long) -> Unit = { delay(it) },
    ) = ForegroundScheduleInvalidationManager(
        credentialStore = ScheduleStreamCredentials(),
        streamTransport = transport,
        durableCursor = {
            DurableScheduleInvalidationCursor(
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                revision = durableRevision.get(),
            )
        },
        tryLaunchAuthoritativeRefresh = tryLaunch,
        authoritativeRefresh = refresh,
        delayMillis = delayMillis,
    )

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "schedule-configuration"
    }
}

private class ScheduleStreamCredentials : ApiCredentialStore {
    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = "schedule-configuration",
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createBound(
            baseUrl = "https://api.example.test/",
            bearerToken = "synthetic-schedule-token",
            configurationId = "schedule-configuration",
        )

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeScheduleStream : ScheduleInvalidationStreamTransport {
    val calls = AtomicInteger()
    val cursors = mutableListOf<ULong>()
    var handler: suspend (
        AuthenticatedApiConfiguration,
        ULong,
        (ULong) -> Unit,
    ) -> ScheduleInvalidationStreamEnd = { _, _, _ -> error("No response configured") }

    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: ULong,
        onInvalidation: (ULong) -> Unit,
    ): ScheduleInvalidationStreamEnd {
        calls.incrementAndGet()
        cursors += lastDurableRevision
        return handler(configuration, lastDurableRevision, onInvalidation)
    }
}
