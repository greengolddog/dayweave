package com.greengolddog.dayweave.network

import java.time.Duration
import java.time.Instant
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.RequestBody.Companion.toRequestBody
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class DeviceAuthRequestCoordinatorTest {
    private val now = Instant.parse("2026-08-29T12:00:00Z")
    private lateinit var server: MockWebServer

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun trusted401RefreshesOnceAndRetriesByteIdenticalRequest() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = successfulRefreshTransport(active)
        val coordinator = coordinator(store, transport)
        server.enqueue(trustedUnauthorized())
        server.enqueue(MockResponse.Builder().code(200).body("synthetic-success").build())
        val payload = """{"title":"SYNTHETIC-NON-SECRET","revision":7}"""
        val request = Request.Builder()
            .url(server.url("/tenant/v1/items/$SYNTHETIC_SESSION_ID"))
            .header("Idempotency-Key", "33333333-3333-4333-8333-333333333333")
            .header("Authorization", "Bearer stale-configuration-copy")
            .put(payload.toRequestBody("application/json".toMediaType()))
            .build()

        coordinator.executeAuthenticated(
            requireNotNull(coordinator.authenticatedConfiguration()),
            OkHttpClient(),
            request,
        ).use { response -> assertEquals(200, response.code) }

        assertEquals(1, transport.refreshCalls.size)
        val first = server.takeRequest()
        val retry = server.takeRequest()
        assertEquals(first.method, retry.method)
        assertEquals(first.url, retry.url)
        assertEquals(first.body, retry.body)
        assertEquals(first.headers["Idempotency-Key"], retry.headers["Idempotency-Key"])
        assertEquals("Bearer ${active.accessToken.value}", first.headers["Authorization"])
        assertNotEquals(first.headers["Authorization"], retry.headers["Authorization"])
    }

    @Test
    fun trusted401ReplaysExactSchedulePublicationBodyAndSecurityHeaders() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val refreshTransport = successfulRefreshTransport(active)
        val coordinator = coordinator(store, refreshTransport)
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())
        val digest = "sha256:${"a".repeat(64)}"
        val schedule = SchedulePreviewRequest(
            asOf = now.toString(),
            horizonStart = now.minus(Duration.ofHours(1)).toString(),
            horizonEnd = now.plus(Duration.ofDays(1)).toString(),
            timezoneName = "UTC",
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = now.toString(),
                    end = now.plus(Duration.ofHours(8)).toString(),
                ),
            ),
        )
        val exact = buildSchedulePublishHttpRequest(
            configuration,
            SchedulePublishRequest(
                idempotencyKey = "33333333-3333-4333-8333-333333333333",
                expectedInputDigest = digest,
                schedule = schedule,
            ),
        )
        val revisionId = "44444444-4444-4444-8444-444444444444"
        server.enqueue(trustedUnauthorized())
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body(
                    """{"revision":{"id":"$revisionId","revision":"1:$revisionId","revision_number":1,"input_digest":"$digest","horizon_start":"${schedule.horizonStart}","horizon_end":"${schedule.horizonEnd}","timezone_name":"UTC","published_at":"$now"},"replayed":false}""",
                )
                .build(),
        )

        val published = OkHttpCanonicalPlannerTransport().publish(configuration, exact)

        assertEquals(1uL, published.revision.revisionNumber)
        assertEquals(1, refreshTransport.refreshCalls.size)
        val first = server.takeRequest()
        val retry = server.takeRequest()
        assertEquals(first.method, retry.method)
        assertEquals(first.url, retry.url)
        assertEquals(first.body, retry.body)
        assertEquals(first.headers["Accept"], retry.headers["Accept"])
        assertEquals(first.headers["Content-Type"], retry.headers["Content-Type"])
        assertEquals(first.headers["Cache-Control"], retry.headers["Cache-Control"])
        assertEquals(first.headers["Pragma"], retry.headers["Pragma"])
        assertNotEquals(first.headers["Authorization"], retry.headers["Authorization"])
    }

    @Test
    fun concurrentProactiveRequestsShareOneRefresh() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(1)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = successfulRefreshTransport(active)
        val coordinator = coordinator(store, transport)
        server.enqueue(MockResponse.Builder().code(200).body("one").build())
        server.enqueue(MockResponse.Builder().code(200).body("two").build())
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())

        listOf("first", "second").map { suffix ->
            async {
                val request = Request.Builder()
                    .url(server.url("/tenant/v1/items/$suffix"))
                    .get()
                    .build()
                coordinator.executeAuthenticated(configuration, OkHttpClient(), request).use {
                    assertEquals(200, it.code)
                }
            }
        }.awaitAll()

        assertEquals(1, transport.refreshCalls.size)
        val newAccess = (store.envelope.state as StoredDeviceAuthState.Active).accessToken.value
        val authorizations = listOf(server.takeRequest(), server.takeRequest())
            .map { it.headers["Authorization"] }
        assertEquals(listOf("Bearer $newAccess", "Bearer $newAccess"), authorizations)
    }

    @Test
    fun stalledAuthenticatedRequestDoesNotSerializeUnrelatedRead() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val coordinator = coordinator(FakeDeviceAuthEnvelopeStore(active), RecordingDeviceAuthTransport())
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())
        val firstStarted = CountDownLatch(1)
        val releaseFirst = CountDownLatch(1)
        val secondFinished = CountDownLatch(1)
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            if (chain.request().url.encodedPath.endsWith("/first")) {
                firstStarted.countDown()
                check(releaseFirst.await(3, TimeUnit.SECONDS))
            } else {
                secondFinished.countDown()
            }
            Response.Builder()
                .request(chain.request())
                .protocol(Protocol.HTTP_1_1)
                .code(200)
                .message("OK")
                .body("synthetic".toResponseBody())
                .build()
        }.build()

        val first = async(Dispatchers.Default) {
            coordinator.executeAuthenticated(
                configuration,
                client,
                Request.Builder().url(server.url("/tenant/v1/items/first")).get().build(),
            ).close()
        }
        assertTrue(firstStarted.await(3, TimeUnit.SECONDS))
        val second = async(Dispatchers.Default) {
            coordinator.executeAuthenticated(
                configuration,
                client,
                Request.Builder().url(server.url("/tenant/v1/items/second")).get().build(),
            ).close()
        }

        assertTrue(secondFinished.await(3, TimeUnit.SECONDS))
        second.await()
        releaseFirst.countDown()
        first.await()
    }

    @Test
    fun untrusted401NeverRotatesOrQuarantines() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = successfulRefreshTransport(active)
        val coordinator = coordinator(store, transport)
        server.enqueue(
            MockResponse.Builder()
                .code(401)
                .addHeader("Content-Type", "application/json")
                .body(unauthorizedBody())
                .build(),
        )

        coordinator.executeAuthenticated(
            requireNotNull(coordinator.authenticatedConfiguration()),
            OkHttpClient(),
            Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
        ).use { response -> assertEquals(401, response.code) }

        assertTrue(transport.refreshCalls.isEmpty())
        assertEquals(active, store.envelope.state)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun secondTrusted401QuarantinesOnlyTheExactRotatedLease() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = successfulRefreshTransport(active)
        val coordinator = coordinator(store, transport)
        server.enqueue(trustedUnauthorized())
        server.enqueue(trustedUnauthorized())

        coordinator.executeAuthenticated(
            requireNotNull(coordinator.authenticatedConfiguration()),
            OkHttpClient(),
            Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
        ).use { response -> assertEquals(401, response.code) }

        assertEquals(1, transport.refreshCalls.size)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Reauth)
    }

    @Test
    fun secondTrusted401CannotQuarantineRewrappedSameRevisionLease() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        var rotatedActiveReads = 0
        val transport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                store.afterRead = { observed ->
                    if (observed.state is StoredDeviceAuthState.Active && observed.revision > 1) {
                        rotatedActiveReads += 1
                        if (rotatedActiveReads == 4) {
                            store.afterRead = null
                            store.forceExactIdentityChange()
                        }
                    }
                }
                DeviceSessionMutationResponse(nextSession(active.session), replayed = false)
            }
        }
        val coordinator = coordinator(store, transport)
        server.enqueue(trustedUnauthorized())
        server.enqueue(trustedUnauthorized())

        coordinator.executeAuthenticated(
            requireNotNull(coordinator.authenticatedConfiguration()),
            OkHttpClient(),
            Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
        ).use { response -> assertEquals(401, response.code) }

        assertTrue(store.envelope.state is StoredDeviceAuthState.Active)
        assertEquals(1, transport.refreshCalls.size)
    }

    @Test
    fun postResponseLeaseChangeClosesAndRejectsOldBindingResult() {
        val active = activeForRequests(
            SYNTHETIC_BASE_URL,
            accessExpiry = now.plus(Duration.ofMinutes(10)),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        val coordinator = coordinator(store, RecordingDeviceAuthTransport())
        val replacement = active.copy(
            session = active.session.copy(id = "44444444-4444-4444-8444-444444444444"),
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 70)),
        )
        val client = OkHttpClient.Builder()
            .addInterceptor { chain ->
                store.forceState(replacement)
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body("synthetic-old-binding".toResponseBody())
                    .build()
            }
            .build()

        assertThrows(DeviceAuthenticationChangedException::class.java) {
            runBlocking {
                coordinator.executeAuthenticated(
                    requireNotNull(coordinator.authenticatedConfiguration()).let {
                        // The replacement was not installed until the request interceptor runs.
                        AuthenticatedApiConfiguration.createCoordinated(
                            active.baseUrl,
                            active.accessToken.value,
                            active.session.id,
                            coordinator,
                            allowCleartextLoopback = false,
                        )
                    },
                    client,
                    Request.Builder().url("${SYNTHETIC_BASE_URL}v1/items").get().build(),
                )
            }
        }
        assertEquals(replacement, store.envelope.state)
    }

    @Test
    fun sameSessionSameRevisionReplacementAfter401IsNeverRotatedOrRetried() {
        val active = activeForRequests(
            SYNTHETIC_BASE_URL,
            accessExpiry = now.plus(Duration.ofMinutes(10)),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        val replacement = active.copy(
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 72)),
        )
        val transport = RecordingDeviceAuthTransport()
        val coordinator = coordinator(store, transport)
        var requestCount = 0
        val client = OkHttpClient.Builder()
            .addInterceptor { chain ->
                requestCount += 1
                store.forceState(replacement)
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(401)
                    .message("Unauthorized")
                    .header("Content-Type", "application/json")
                    .header("Cache-Control", "no-store, max-age=0")
                    .header("Pragma", "no-cache")
                    .header("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                    .body(unauthorizedBody().toResponseBody())
                    .build()
            }
            .build()

        assertThrows(DeviceAuthenticationChangedException::class.java) {
            runBlocking {
                coordinator.executeAuthenticated(
                    requireNotNull(coordinator.authenticatedConfiguration()),
                    client,
                    Request.Builder().url("${SYNTHETIC_BASE_URL}v1/items").get().build(),
                )
            }
        }

        assertEquals(1, requestCount)
        assertTrue(transport.refreshCalls.isEmpty())
        assertEquals(replacement, store.envelope.state)
    }

    @Test
    fun exactPreflightRejectsEnvelopeReplacementBeforeFirstDispatch() {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val coordinator = coordinator(store, RecordingDeviceAuthTransport())
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())
        store.afterRead = {
            store.afterRead = null
            store.forceExactIdentityChange()
        }

        assertThrows(DeviceAuthenticationChangedException::class.java) {
            runBlocking {
                coordinator.executeAuthenticated(
                    configuration,
                    OkHttpClient(),
                    Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
                )
            }
        }

        assertEquals(0, server.requestCount)
    }

    @Test
    fun replacementDuringRefreshPreventsAnyRetry() {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        val replacement = active.copy(
            session = active.session.copy(id = "55555555-5555-4555-8555-555555555555"),
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 71)),
        )
        val transport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                val pending = store.envelope.state as StoredDeviceAuthState.RefreshPending
                store.forceState(replacement)
                DeviceSessionMutationResponse(
                    nextSession(pending.session),
                    replayed = false,
                )
            }
        }
        val coordinator = coordinator(store, transport)
        server.enqueue(trustedUnauthorized())

        assertThrows(DeviceAuthenticationChangedException::class.java) {
            runBlocking {
                coordinator.executeAuthenticated(
                    requireNotNull(coordinator.authenticatedConfiguration()),
                    OkHttpClient(),
                    Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
                )
            }
        }
        assertEquals(1, server.requestCount)
        assertEquals(replacement, store.envelope.state)
    }

    @Test
    fun exactRetryPreflightRejectsRewrappedRotatedEnvelopeBeforeDispatch() {
        val baseUrl = server.url("/tenant/").toString()
        val active = activeForRequests(baseUrl, accessExpiry = now.plus(Duration.ofMinutes(10)))
        val store = FakeDeviceAuthEnvelopeStore(active)
        var rotatedActiveReads = 0
        val transport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                store.afterRead = { observed ->
                    if (observed.state is StoredDeviceAuthState.Active && observed.revision > 1) {
                        rotatedActiveReads += 1
                        if (rotatedActiveReads == 2) {
                            store.afterRead = null
                            store.forceExactIdentityChange()
                        }
                    }
                }
                DeviceSessionMutationResponse(nextSession(active.session), replayed = false)
            }
        }
        val coordinator = coordinator(store, transport)
        server.enqueue(trustedUnauthorized())

        assertThrows(DeviceAuthenticationChangedException::class.java) {
            runBlocking {
                coordinator.executeAuthenticated(
                    requireNotNull(coordinator.authenticatedConfiguration()),
                    OkHttpClient(),
                    Request.Builder().url(server.url("/tenant/v1/items")).get().build(),
                )
            }
        }

        assertEquals(1, server.requestCount)
    }

    @Test
    fun requestCannotEscapeConfiguredOriginOrPathPrefix() {
        val active = activeForRequests(
            SYNTHETIC_BASE_URL,
            accessExpiry = now.plus(Duration.ofMinutes(10)),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        val coordinator = coordinator(store, RecordingDeviceAuthTransport())
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())

        listOf(
            "https://other.example.test/tenant/v1/items",
            "https://api.example.test/other/v1/items",
        ).forEach { target ->
            assertThrows(DeviceAuthenticationChangedException::class.java) {
                runBlocking {
                    coordinator.executeAuthenticated(
                        configuration,
                        OkHttpClient(),
                        Request.Builder().url(target).get().build(),
                    )
                }
            }
        }
    }

    private fun activeForRequests(baseUrl: String, accessExpiry: Instant): StoredDeviceAuthState.Active {
        val issued = now.minus(Duration.ofMinutes(5))
        return syntheticActiveState(
            now = now,
            baseUrl = baseUrl,
            session = syntheticSession(
                now = now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = issued,
                lastSeenAt = issued,
                accessExpiresAt = accessExpiry,
                refreshIdleExpiresAt = issued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
    }

    private fun successfulRefreshTransport(
        active: StoredDeviceAuthState.Active,
    ) = RecordingDeviceAuthTransport().apply {
        refreshHandler = {
            DeviceSessionMutationResponse(nextSession(active.session), replayed = false)
        }
    }

    private fun nextSession(previous: DeviceSessionContract): DeviceSessionContract = syntheticSession(
        now = now,
        id = previous.id,
        clientInstanceId = previous.clientInstanceId,
        revision = previous.revision + 1,
        createdAt = Instant.parse(previous.createdAt),
        issuedAt = now,
        lastSeenAt = now,
        absoluteExpiresAt = Instant.parse(previous.absoluteExpiresAt),
    )

    private fun coordinator(
        store: FakeDeviceAuthEnvelopeStore,
        transport: RecordingDeviceAuthTransport,
    ) = DurableDeviceAuthCoordinator(
        store = store,
        transport = transport,
        clientVersion = SYNTHETIC_CLIENT_VERSION,
        deviceLabel = SYNTHETIC_DEVICE_LABEL,
        now = { now },
        generator = QueueDeviceCredentialGenerator(),
        allowCleartextLoopbackForTests = true,
    )

    private fun trustedUnauthorized(): MockResponse = MockResponse.Builder()
        .code(401)
        .addHeader("Content-Type", "application/json")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
        .body(unauthorizedBody())
        .build()

    private fun unauthorizedBody(): String =
        """{"error":{"code":"unauthorized","message":"A valid bearer token is required"}}"""
}
