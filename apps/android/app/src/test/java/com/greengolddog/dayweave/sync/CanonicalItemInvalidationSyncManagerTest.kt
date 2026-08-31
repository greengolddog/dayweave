package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamEnd
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamException
import com.greengolddog.dayweave.network.CanonicalItemInvalidationStreamTransport
import com.greengolddog.dayweave.network.CanonicalItemRevisionRequest
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.CreateCanonicalItemRequest
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteItemDeltaPage
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.RemoteSchedulePublishResponse
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishHttpRequest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalItemInvalidationSyncManagerTest {
    @Test
    fun delayedOlderCallbackCannotOverwriteNewerOpaqueGeneration() {
        val nextGeneration = AtomicLong(0)
        val pending = AtomicReference<CanonicalItemInvalidationHint?>(null)
        val olderAssigned = CountDownLatch(1)
        val releaseOlder = CountDownLatch(1)
        val older = thread {
            val generation = nextGeneration.incrementAndGet()
            olderAssigned.countDown()
            releaseOlder.await()
            pending.offerIfNewer(CanonicalItemInvalidationHint(generation, "opaque_old"))
        }
        olderAssigned.await()

        val newerGeneration = nextGeneration.incrementAndGet()
        pending.offerIfNewer(CanonicalItemInvalidationHint(newerGeneration, "opaque_new"))
        releaseOlder.countDown()
        older.join()

        assertEquals(newerGeneration, pending.get()?.generation)
        assertEquals("opaque_new", pending.get()?.cursor)
    }

    @Test
    fun bootstrapOmitsResumeCursorAndEstablishesFirstDurableCursorAuthoritatively() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor(null))
        val observedResume = CompletableDeferred<String?>()
        val stream = FakeItemStream { cursor, onInvalidation ->
            observedResume.complete(cursor)
            onInvalidation("opaque_first")
            awaitCancellation()
        }
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = stream,
            refresh = {
                refreshes += 1
                durable.set(boundCursor("opaque_first"))
                true
            },
        )
        val activation = async { manager.runForegroundActivation() }

        assertNull(withTimeout(2_000) { observedResume.await() })
        waitUntil { refreshes == 1 }

        activation.cancelAndJoin()
        assertEquals("opaque_first", durable.get().cursor)
    }

    @Test
    fun busyGateRetainsLatestOpaqueHintUntilOneRefreshIsAdmitted() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        var admissions = 0
        var refreshes = 0
        val delays = mutableListOf<Long>()
        val manager = ForegroundCanonicalItemInvalidationManager(
            credentialStore = credentials,
            plannerTransport = blockingPlanner(),
            streamTransport = FakeItemStream { _, onInvalidation ->
                onInvalidation("opaque_2")
                onInvalidation("opaque_3")
                awaitCancellation()
            },
            durableCursor = durable::get,
            tryLaunchAuthoritativeRefresh = { action ->
                admissions += 1
                if (admissions < 3) false else {
                    launch { action() }
                    true
                }
            },
            authoritativeRefresh = {
                refreshes += 1
                durable.set(boundCursor("opaque_3"))
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
        assertEquals("opaque_3", durable.get().cursor)
    }

    @Test
    fun newerHintDuringRefreshGetsASecondPassWhenFinalCursorDoesNotCoverIt() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        val firstRefreshStarted = CompletableDeferred<Unit>()
        val releaseFirstRefresh = CompletableDeferred<Unit>()
        var refreshes = 0
        val stream = FakeItemStream { _, onInvalidation ->
            onInvalidation("opaque_2")
            firstRefreshStarted.await()
            onInvalidation("opaque_3")
            releaseFirstRefresh.complete(Unit)
            awaitCancellation()
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = stream,
            refresh = {
                refreshes += 1
                if (refreshes == 1) {
                    firstRefreshStarted.complete(Unit)
                    releaseFirstRefresh.await()
                    durable.set(boundCursor("opaque_2"))
                } else {
                    durable.set(boundCursor("opaque_3"))
                }
                true
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 2 }
        activation.cancelAndJoin()

        assertEquals("opaque_3", durable.get().cursor)
    }

    @Test
    fun ownWriteHintCoveredByFinalDurableCursorDoesNotCreateASecondRefresh() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        val refreshStarted = CompletableDeferred<Unit>()
        val ownHintSent = CompletableDeferred<Unit>()
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = FakeItemStream { _, onInvalidation ->
                onInvalidation("opaque_2")
                refreshStarted.await()
                onInvalidation("opaque_own")
                ownHintSent.complete(Unit)
                awaitCancellation()
            },
            refresh = {
                refreshes += 1
                refreshStarted.complete(Unit)
                ownHintSent.await()
                durable.set(boundCursor("opaque_own"))
                true
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { durable.get().cursor == "opaque_own" }
        repeat(20) { yield() }
        activation.cancelAndJoin()

        assertEquals(1, refreshes)
    }

    @Test
    fun failedCatchUpUsesBoundedExponentialDelayInsteadOfTightLoop() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        val delays = mutableListOf<Long>()
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = FakeItemStream { _, onInvalidation ->
                onInvalidation("opaque_unreachable")
                awaitCancellation()
            },
            refresh = {
                refreshes += 1
                false
            },
            delayMillis = { millis ->
                delays += millis
                if (refreshes >= 3) awaitCancellation() else yield()
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes >= 3 }
        activation.cancelAndJoin()

        assertTrue(delays.containsAll(listOf(1_000L, 2_000L)))
    }

    @Test
    fun thirtySecondProbeIgnoresCleanPageThenQueuesChangedCursor() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        var probes = 0
        var refreshes = 0
        val planner = FakePlannerTransport { cursor ->
            probes += 1
            if (probes == 1) emptyPage(requireNotNull(cursor)) else emptyPage("opaque_2")
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = planner,
            stream = null,
            refresh = {
                refreshes += 1
                durable.set(boundCursor("opaque_2"))
                true
            },
            delayMillis = { millis ->
                assertEquals(30_000L, millis)
                if (probes >= 2) awaitCancellation() else yield()
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        activation.cancelAndJoin()

        assertEquals(2, probes)
        assertEquals("opaque_2", durable.get().cursor)
    }

    @Test
    fun invalidProbeCursorForcesOneAuthoritativeRebuild() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_old"))
        var probes = 0
        var refreshes = 0
        val planner = FakePlannerTransport {
            probes += 1
            throw PlannerApiException.Validation(422)
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = planner,
            stream = null,
            refresh = {
                refreshes += 1
                durable.set(boundCursor("opaque_rebuilt"))
                true
            },
            delayMillis = { awaitCancellation() },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        activation.cancelAndJoin()

        assertEquals(1, probes)
    }

    @Test
    fun cursorAheadStreamResponseQueuesAuthoritativeRepairWithoutReconnectLoop() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_ahead"))
        var streamCalls = 0
        var refreshes = 0
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = FakeItemStream { _, _ ->
                streamCalls += 1
                throw CanonicalItemInvalidationStreamException.Http(409)
            },
            refresh = {
                refreshes += 1
                durable.set(boundCursor("opaque_repaired"))
                true
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { refreshes == 1 }
        activation.cancelAndJoin()

        assertEquals(1, streamCalls)
    }

    @Test
    fun unsupportedStreamIsActivationLocalAndProbeFallbackRemainsAlive() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        var streamCalls = 0
        var probeCalls = 0
        val stream = FakeItemStream { _, _ ->
            streamCalls += 1
            CanonicalItemInvalidationStreamEnd.UNSUPPORTED
        }
        val planner = FakePlannerTransport { cursor ->
            probeCalls += 1
            emptyPage(requireNotNull(cursor))
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = planner,
            stream = stream,
            refresh = { true },
            delayMillis = { awaitCancellation() },
        )

        val first = async { manager.runForegroundActivation() }
        waitUntil { streamCalls == 1 && probeCalls == 1 }
        first.cancelAndJoin()
        val second = async { manager.runForegroundActivation() }
        waitUntil { streamCalls == 2 && probeCalls == 2 }
        second.cancelAndJoin()
    }

    @Test
    fun transientDisconnectBacksOffAndReconnectsFromDurableCursor() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        val resumes = mutableListOf<String?>()
        val delays = mutableListOf<Long>()
        val stream = FakeItemStream { cursor, _ ->
            resumes += cursor
            if (resumes.size == 1) throw java.io.IOException("synthetic disconnect")
            awaitCancellation()
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = stream,
            refresh = { true },
            delayMillis = { millis ->
                delays += millis
                yield()
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { resumes.size == 2 }
        activation.cancelAndJoin()

        assertEquals(listOf("opaque_1", "opaque_1"), resumes)
        assertTrue(delays.contains(1_000L))
    }

    @Test
    fun normalFiveMinuteExpiryReconnectsImmediatelyFromLatestDurableCursor() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_1"))
        val resumes = mutableListOf<String?>()
        val monotonicReads = AtomicLong(0)
        val stream = FakeItemStream { cursor, _ ->
            resumes += cursor
            if (resumes.size == 1) {
                durable.set(boundCursor("opaque_2"))
                CanonicalItemInvalidationStreamEnd.ENDED
            } else {
                awaitCancellation()
            }
        }
        val manager = ForegroundCanonicalItemInvalidationManager(
            credentialStore = credentials,
            plannerTransport = blockingPlanner(),
            streamTransport = stream,
            durableCursor = durable::get,
            tryLaunchAuthoritativeRefresh = { false },
            authoritativeRefresh = { true },
            delayMillis = { error("Normal expiry must not back off") },
            monotonicNanos = {
                if (monotonicReads.getAndIncrement() == 0L) 0L else 5L * 60L * 1_000_000_000L
            },
        )
        val activation = async { manager.runForegroundActivation() }

        waitUntil { resumes.size == 2 }
        activation.cancelAndJoin()

        assertEquals(listOf("opaque_1", "opaque_2"), resumes)
    }

    @Test
    fun durableCursorFromAnotherBindingIsNeverUsedAsResumeAuthority() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(
            DurableCanonicalItemInvalidationCursor(
                syncOrigin = TEST_BASE_URL,
                configurationId = "old-binding",
                cursor = "opaque_old",
            ),
        )
        val observedResume = CompletableDeferred<String?>()
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = FakeItemStream { cursor, _ ->
                observedResume.complete(cursor)
                awaitCancellation()
            },
            refresh = { true },
        )
        val activation = async { manager.runForegroundActivation() }

        assertNull(withTimeout(2_000) { observedResume.await() })
        activation.cancelAndJoin()
    }

    @Test
    fun bindingCancellationDrainsOldStreamAndNeverReusesItsCursor() = runBlocking {
        val credentials = FakeItemCredentialStore()
        val durable = AtomicReference(boundCursor("opaque_old"))
        val oldStarted = CompletableDeferred<Unit>()
        val oldClosed = CompletableDeferred<Unit>()
        val resumes = mutableListOf<String?>()
        var staleCallback: ((String) -> Unit)? = null
        var refreshes = 0
        val stream = FakeItemStream { cursor, onInvalidation ->
            resumes += cursor
            if (resumes.size == 1) {
                staleCallback = onInvalidation
                oldStarted.complete(Unit)
                try {
                    awaitCancellation()
                } finally {
                    oldClosed.complete(Unit)
                }
            }
            awaitCancellation()
        }
        val manager = manager(
            scope = this,
            credentials = credentials,
            durable = durable,
            planner = blockingPlanner(),
            stream = stream,
            refresh = {
                refreshes += 1
                true
            },
        )

        val first = async { manager.runForegroundActivation() }
        withTimeout(2_000) { oldStarted.await() }
        manager.cancelAndDrainActiveSession()
        withTimeout(2_000) { oldClosed.await() }
        first.join()

        credentials.replaceBinding("binding-2")
        durable.set(boundCursor("opaque_new", configurationId = "binding-2"))
        requireNotNull(staleCallback)("opaque_stale")
        repeat(10) { yield() }
        val second = async { manager.runForegroundActivation() }
        waitUntil { resumes.size == 2 }
        second.cancelAndJoin()

        assertEquals(listOf("opaque_old", "opaque_new"), resumes)
        assertEquals(0, refreshes)
    }

    private fun manager(
        scope: CoroutineScope,
        credentials: FakeItemCredentialStore,
        durable: AtomicReference<DurableCanonicalItemInvalidationCursor>,
        planner: CanonicalPlannerTransport,
        stream: CanonicalItemInvalidationStreamTransport?,
        refresh: suspend () -> Boolean,
        delayMillis: suspend (Long) -> Unit = { awaitCancellation() },
    ) = ForegroundCanonicalItemInvalidationManager(
        credentialStore = credentials,
        plannerTransport = planner,
        streamTransport = stream,
        durableCursor = durable::get,
        tryLaunchAuthoritativeRefresh = { action ->
            scope.launch { action() }
            true
        },
        authoritativeRefresh = refresh,
        delayMillis = delayMillis,
    )

    private fun blockingPlanner() = FakePlannerTransport { awaitCancellation() }

    private fun boundCursor(
        cursor: String?,
        configurationId: String = "binding-1",
    ) = DurableCanonicalItemInvalidationCursor(
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

private class FakeItemCredentialStore(
    private var configurationId: String = "binding-1",
) : ApiCredentialStore {
    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "http://127.0.0.1:9/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            "http://127.0.0.1:9/",
            "unit-test-secret",
        ).let { unbound ->
            AuthenticatedApiConfiguration.createCoordinated(
                baseUrl = unbound.baseUrl.toString(),
                bearerToken = "unit-test-secret",
                configurationId = configurationId,
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

    fun replaceBinding(value: String) {
        configurationId = value
    }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeItemStream(
    private val handler: suspend (
        String?,
        (String) -> Unit,
    ) -> CanonicalItemInvalidationStreamEnd,
) : CanonicalItemInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): CanonicalItemInvalidationStreamEnd = handler(lastDurableCursor, onInvalidation)
}

private class FakePlannerTransport(
    private val probe: suspend (String?) -> RemoteItemDeltaPage,
) : CanonicalPlannerTransport {
    override suspend fun itemDelta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
    ): RemoteItemDeltaPage = probe(cursor)

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePreviewRequest,
    ): RemoteSchedulePreview = error("Not used")

    override suspend fun publish(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePublishHttpRequest,
    ): RemoteSchedulePublishResponse = error("Not used")

    override suspend fun createItem(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: CreateCanonicalItemRequest,
    ): RemoteCanonicalItem = error("Not used")

    override suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem = error("Not used")

    override suspend fun trashItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        expectedRevision: Long,
    ): RemoteCanonicalItem = error("Not used")

    override suspend fun restoreItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: CanonicalItemRevisionRequest,
    ): RemoteCanonicalItem = error("Not used")
}

private fun emptyPage(cursor: String) = RemoteItemDeltaPage(
    changes = emptyList(),
    nextCursor = cursor,
    hasMore = false,
)
