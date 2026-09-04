package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.HabitInvalidationStreamEnd
import com.greengolddog.dayweave.network.HabitInvalidationStreamException
import com.greengolddog.dayweave.network.HabitInvalidationStreamTransport
import java.io.IOException
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitInvalidationSyncManagerTest {
    @Test
    fun contentFreeHintTriggersOneAuthoritativeDeltaRefresh() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_1"))
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            stream = FakeHabitInvalidationStream { cursor, onInvalidation ->
                assertEquals("cursor_1", cursor)
                onInvalidation("cursor_2")
                awaitCancellation()
            },
            refresh = {
                refreshes += 1
                durable.set(boundCursor("cursor_2"))
                true
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        repeat(20) { yield() }
        activation.cancelAndJoin()

        assertEquals(1, refreshes)
        assertEquals("cursor_2", durable.get().cursor)
    }

    @Test
    fun busyGateRetainsNewestHintUntilRefreshIsAdmitted() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_1"))
        var admissions = 0
        var refreshes = 0
        val delays = mutableListOf<Long>()
        val manager = ForegroundHabitInvalidationManager(
            credentialStore = credentials,
            streamTransport = FakeHabitInvalidationStream { _, onInvalidation ->
                onInvalidation("cursor_2")
                onInvalidation("cursor_3")
                awaitCancellation()
            },
            durableCursor = durable::get,
            tryLaunchAuthoritativeRefresh = { action ->
                admissions += 1
                if (admissions < 3) null else async { action() }
            },
            authoritativeRefresh = {
                refreshes += 1
                durable.set(boundCursor("cursor_3"))
                true
            },
            delayMillis = { millis ->
                delays += millis
                yield()
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        activation.cancelAndJoin()

        assertTrue(admissions >= 3)
        assertTrue(delays.count { it == 250L } >= 2)
        assertEquals("cursor_3", durable.get().cursor)
    }

    @Test
    fun admittedLaunchSuppressedByALaterRecoveryFenceCompletesAndRetries() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_1"))
        var admissions = 0
        var refreshes = 0
        val manager = ForegroundHabitInvalidationManager(
            credentialStore = credentials,
            streamTransport = FakeHabitInvalidationStream { _, onInvalidation ->
                onInvalidation("cursor_2")
                awaitCancellation()
            },
            durableCursor = durable::get,
            tryLaunchAuthoritativeRefresh = { action ->
                admissions += 1
                async {
                    if (admissions == 1) {
                        // Mirrors launchCanonicalResultAction when Google recovery appears after
                        // gate admission but before the authoritative callback starts.
                        false
                    } else {
                        action()
                    }
                }
            },
            authoritativeRefresh = {
                refreshes += 1
                durable.set(boundCursor("cursor_2"))
                true
            },
            delayMillis = { yield() },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        activation.cancelAndJoin()

        assertTrue(admissions >= 2)
        assertEquals("cursor_2", durable.get().cursor)
    }

    @Test
    fun cursorAheadResponseQueuesRepairBeforeTheStreamWorkerEnds() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_ahead"))
        var streamCalls = 0
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            stream = FakeHabitInvalidationStream { _, _ ->
                streamCalls += 1
                throw HabitInvalidationStreamException.Http(409)
            },
            refresh = {
                refreshes += 1
                durable.set(boundCursor("cursor_repaired"))
                true
            },
            delayMillis = { yield() },
        )

        manager.runForegroundActivation()

        assertEquals(1, streamCalls)
        assertEquals(1, refreshes)
        assertEquals("cursor_repaired", durable.get().cursor)
    }

    @Test
    fun transientDisconnectBacksOffAndReconnectsOnlyFromDurableState() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_1"))
        val resumes = mutableListOf<String?>()
        val delays = mutableListOf<Long>()
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            stream = FakeHabitInvalidationStream { cursor, _ ->
                resumes += cursor
                if (resumes.size == 1) throw IOException("synthetic disconnect")
                awaitCancellation()
            },
            refresh = { true },
            delayMillis = { millis ->
                delays += millis
                yield()
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { resumes.size == 2 }
        activation.cancelAndJoin()

        assertEquals(listOf("cursor_1", "cursor_1"), resumes)
        assertTrue(1_000L in delays)
    }

    @Test
    fun cursorFromAnotherCredentialGenerationIsNeverSent() = runBlocking {
        val credentials = FakeHabitInvalidationCredentialStore()
        val durable = AtomicReference(boundCursor("cursor_old", configurationId = "old-binding"))
        val observed = CompletableDeferred<String?>()
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            stream = FakeHabitInvalidationStream { cursor, _ ->
                observed.complete(cursor)
                awaitCancellation()
            },
            refresh = { true },
        )
        val activation = async { manager.runForegroundActivation() }

        assertNull(withTimeout(2_000) { observed.await() })
        activation.cancelAndJoin()
    }

    private fun manager(
        scope: CoroutineScope,
        credentials: FakeHabitInvalidationCredentialStore,
        durable: AtomicReference<DurableHabitInvalidationCursor>,
        stream: HabitInvalidationStreamTransport,
        refresh: suspend () -> Boolean,
        delayMillis: suspend (Long) -> Unit = { awaitCancellation() },
    ) = ForegroundHabitInvalidationManager(
        credentialStore = credentials,
        streamTransport = stream,
        durableCursor = durable::get,
        tryLaunchAuthoritativeRefresh = { action -> scope.async { action() } },
        authoritativeRefresh = refresh,
        delayMillis = delayMillis,
    )

    private fun boundCursor(
        cursor: String?,
        configurationId: String = "binding-1",
    ) = DurableHabitInvalidationCursor(
        syncOrigin = TEST_BASE_URL,
        configurationId = configurationId,
        cursor = cursor,
    )

    private suspend fun waitUntil(predicate: () -> Boolean) {
        withTimeout(2_000) {
            while (!predicate()) yield()
        }
    }

    private companion object {
        const val TEST_BASE_URL = "http://127.0.0.1:9/"
    }
}

private class FakeHabitInvalidationCredentialStore : ApiCredentialStore {
    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "http://127.0.0.1:9/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = "binding-1",
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            "http://127.0.0.1:9/",
            "unit-test-secret",
        ).let { unbound ->
            AuthenticatedApiConfiguration.createCoordinated(
                baseUrl = unbound.baseUrl.toString(),
                bearerToken = "unit-test-secret",
                configurationId = "binding-1",
                executor = object : com.greengolddog.dayweave.network.DeviceAuthRequestExecutor {
                    override suspend fun executeAuthenticated(
                        configuration: AuthenticatedApiConfiguration,
                        client: okhttp3.OkHttpClient,
                        request: okhttp3.Request,
                    ): okhttp3.Response = error("No HTTP expected")
                },
                allowCleartextLoopback = true,
            )
        }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeHabitInvalidationStream(
    private val handler: suspend (String?, (String) -> Unit) -> HabitInvalidationStreamEnd,
) : HabitInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): HabitInvalidationStreamEnd = handler(lastDurableCursor, onInvalidation)
}
