package com.greengolddog.dayweave.network

import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class OkHttpDeviceSessionsTransportTest {
    private val now = Instant.parse("2026-09-05T09:00:00Z")
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpDeviceSessionsTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpDeviceSessionsTransport(now = { now })
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun listUsesCoordinatedAuthenticationAndStrictPrivateRequest() = runBlocking {
        val executor = RecordingRequestExecutor()
        val android = syntheticSession(now)
        val mac = syntheticSession(
            now = now,
            id = OTHER_SESSION_ID,
            clientInstanceId = OTHER_INSTANCE_ID,
        ).copy(
            clientKind = "macos",
            deviceLabel = "Home Mac",
            scopes = ANDROID_DEVICE_AUTH_SCOPES.filterNot { it == "schedule_publish" },
            clientContractVersion = 1,
            clientCapabilities = emptyList(),
        )
        server.enqueue(strictJson(200, body(android, mac)))

        val result = transport.listSessions(coordinatedConfiguration(executor))

        assertEquals(listOf(android, mac), result.sessions)
        assertEquals(1, executor.calls)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/auth/sessions", request.url.encodedPath)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
        assertEquals("application/json", request.headers["Accept"])
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
    }

    @Test
    fun revokeUsesCoordinatedAuthenticationAndRequiresEmptyStrict204() = runBlocking {
        val executor = RecordingRequestExecutor()
        server.enqueue(strictNoContent())
        transport.revokeSession(coordinatedConfiguration(executor), OTHER_SESSION_ID)

        val request = server.takeRequest()
        assertEquals(1, executor.calls)
        assertEquals("DELETE", request.method)
        assertEquals("/tenant/v1/auth/sessions/$OTHER_SESSION_ID", request.url.encodedPath)

        assertThrows(DeviceSessionDeleteOutcomeAmbiguousException::class.java) {
            runBlocking {
                transport.revokeSession(
                    coordinatedConfiguration(FixedNoContentBodyExecutor()),
                    OTHER_SESSION_ID,
                )
            }
        }
        server.enqueue(MockResponse.Builder().code(204).build())
        assertThrows(DeviceSessionDeleteOutcomeAmbiguousException::class.java) {
            runBlocking {
                transport.revokeSession(loopbackConfiguration(), OTHER_SESSION_ID)
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.revokeSession(loopbackConfiguration(), "not-a-uuid") }
        }
        Unit
    }

    @Test
    fun listRejectsMissingOrDuplicatedNoStoreHeadersAndWrongMediaTypes() {
        val valid = body(syntheticSession(now))
        val responses = listOf(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body(valid)
                .build(),
            strictJson(200, valid).newBuilder()
                .addHeader("Cache-Control", "no-store")
                .build(),
            strictJson(200, valid).newBuilder()
                .addHeader("Pragma", "no-cache")
                .build(),
            strictJson(200, valid, "application/json; charset=utf-8; profile=test"),
            strictJson(200, valid, "application/problem+json"),
        )

        responses.forEach { response ->
            server.enqueue(response)
            assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
                runBlocking { transport.listSessions(loopbackConfiguration()) }
            }
        }
    }

    @Test
    fun listAcceptsExactlyOneMiBAndRejectsOneByteMoreWithoutWideningAuthErrors() = runBlocking {
        val valid = body(syntheticSession(now))
        val padding = MAX_DEVICE_SESSIONS_RESPONSE_BYTES - valid.toByteArray().size
        server.enqueue(strictJson(200, valid + " ".repeat(padding)))
        assertEquals(
            listOf(syntheticSession(now)),
            transport.listSessions(loopbackConfiguration()).sessions,
        )

        server.enqueue(strictJson(200, valid + " ".repeat(padding + 1)))
        assertInvalidList()

        val error = unauthorizedBody()
        server.enqueue(
            strictJson(
                401,
                error + " ".repeat(MAX_DEVICE_AUTH_RESPONSE_BYTES - error.toByteArray().size + 1),
            ),
        )
        assertThrows(DeviceAuthApiException.Unavailable::class.java) {
            runBlocking { transport.listSessions(loopbackConfiguration()) }
        }
        Unit
    }

    @Test
    fun listRejectsMalformedUtf8() {

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "no-store, max-age=0")
                .addHeader("Pragma", "no-cache")
                .body(Buffer().write(byteArrayOf(0xC3.toByte(), 0x28)))
                .build(),
        )
        assertInvalidList()
    }

    @Test
    fun listRejectsUnknownOuterOrSessionKeysAndAcceptsOnlySixteenRows() = runBlocking {
        val validSession = DEVICE_AUTH_JSON.encodeToString(syntheticSession(now))
        server.enqueue(strictJson(200, "{\"sessions\":[$validSession],\"future\":true}"))
        assertInvalidList()

        server.enqueue(
            strictJson(
                200,
                "{\"sessions\":[${validSession.dropLast(1)},\"future\":true}]}",
            ),
        )
        assertInvalidList()

        val maximumRows = canonicalSessions(MAX_ACTIVE_DEVICE_SESSIONS)
        server.enqueue(strictJson(200, body(*maximumRows.toTypedArray())))
        assertEquals(maximumRows, transport.listSessions(loopbackConfiguration()).sessions)

        server.enqueue(
            strictJson(200, body(*canonicalSessions(MAX_ACTIVE_DEVICE_SESSIONS + 1).toTypedArray())),
        )
        assertInvalidList()
        Unit
    }

    @Test
    fun listRejectsDuplicateRootAndNestedKeysBeforeTreeDecoding() {
        val validSession = DEVICE_AUTH_JSON.encodeToString(syntheticSession(now))
        server.enqueue(
            strictJson(200, "{\"sessions\":[],\"sess\\u0069ons\":[]}"),
        )
        assertInvalidList()

        val duplicateRevision = validSession.replace(
            "\"revision\":1",
            "\"revision\":1,\"revi\\u0073ion\":1",
        )
        server.enqueue(strictJson(200, "{\"sessions\":[$duplicateRevision]}"))
        assertInvalidList()
    }

    @Test
    fun listRejectsNoncanonicalJsonIntegerTokens() {
        val validSession = DEVICE_AUTH_JSON.encodeToString(syntheticSession(now))
        val exponentRevision = validSession.replace("\"revision\":1", "\"revision\":1e0")
        server.enqueue(strictJson(200, "{\"sessions\":[$exponentRevision]}"))

        assertInvalidList()
    }

    @Test
    fun listRejectsDuplicateIdsAndMalformedIdentityMetadataOrScopeContracts() {
        val valid = syntheticSession(now)
        val invalid = listOf(
            valid.copy(id = "00000000-0000-0000-0000-000000000000"),
            valid.copy(clientInstanceId = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
            valid.copy(clientKind = "ios"),
            valid.copy(scopes = valid.scopes + valid.scopes.first()),
            valid.copy(scopes = listOf("unknown_scope")),
            valid.copy(clientContractVersion = 1),
            valid.copy(clientContractVersion = DEVICE_AUTH_CONTRACT_VERSION + 1),
        )
        server.enqueue(strictJson(200, body(valid, valid.copy(deviceLabel = "Duplicate"))))
        assertInvalidList()

        invalid.forEach { session ->
            server.enqueue(strictJson(200, body(session)))
            assertInvalidList()
        }
    }

    @Test
    fun listRejectsImpossibleOrOverlongTimestamps() {
        val valid = syntheticSession(now)
        val invalid = listOf(
            valid.copy(createdAt = "not-an-instant"),
            valid.copy(lastSeenAt = now.minusSeconds(1).toString()),
            valid.copy(accessExpiresAt = now.toString()),
            valid.copy(accessExpiresAt = now.plus(DEVICE_AUTH_ACCESS_TTL).plusSeconds(2).toString()),
            valid.copy(refreshIdleExpiresAt = now.plus(DEVICE_AUTH_REFRESH_IDLE_TTL).plusSeconds(2).toString()),
            valid.copy(absoluteExpiresAt = now.plus(DEVICE_AUTH_ABSOLUTE_TTL).plusSeconds(2).toString()),
        )

        invalid.forEach { session ->
            server.enqueue(strictJson(200, body(session)))
            assertInvalidList()
        }
    }

    @Test
    fun listRejectsFutureStaleOrAlreadyEndedInventoryRows() {
        val staleIssuedAt = now.minus(DEVICE_AUTH_REFRESH_IDLE_TTL)
        val invalid = listOf(
            syntheticSession(now.plusSeconds(301)),
            syntheticSession(now).copy(lastSeenAt = now.plusSeconds(301).toString()),
            syntheticSession(
                now = now,
                createdAt = staleIssuedAt,
                issuedAt = staleIssuedAt,
                lastSeenAt = now.minusSeconds(400),
                refreshIdleExpiresAt = now.minusSeconds(300),
                absoluteExpiresAt = staleIssuedAt.plus(DEVICE_AUTH_ABSOLUTE_TTL),
            ),
            syntheticSession(
                now = now,
                createdAt = now.minusSeconds(86_400),
                issuedAt = now.minusSeconds(86_400),
                lastSeenAt = now,
                refreshIdleExpiresAt = now,
                absoluteExpiresAt = now,
            ),
        )

        invalid.forEach { session ->
            server.enqueue(strictJson(200, body(session)))
            assertInvalidList()
        }
    }

    @Test
    fun listAcceptsClockSkewFreshnessAndTtlToleranceBoundaries() = runBlocking {
        val latestAcceptedServerTime = now.plusSeconds(300)
        val ttlBoundary = syntheticSession(
            now = latestAcceptedServerTime,
            accessExpiresAt = latestAcceptedServerTime.plus(DEVICE_AUTH_ACCESS_TTL)
                .plusSeconds(1),
            refreshIdleExpiresAt = latestAcceptedServerTime.plus(DEVICE_AUTH_REFRESH_IDLE_TTL)
                .plusSeconds(1),
            absoluteExpiresAt = latestAcceptedServerTime.plus(DEVICE_AUTH_ABSOLUTE_TTL)
                .plusSeconds(1),
        )
        server.enqueue(strictJson(200, body(ttlBoundary)))
        assertEquals(
            listOf(ttlBoundary),
            transport.listSessions(loopbackConfiguration()).sessions,
        )

        val issuedAt = now.minus(DEVICE_AUTH_REFRESH_IDLE_TTL)
        val recentlyExpiredIdle = syntheticSession(
            now = now,
            createdAt = issuedAt,
            issuedAt = issuedAt,
            lastSeenAt = now.minusSeconds(400),
            refreshIdleExpiresAt = now.minusSeconds(299),
            absoluteExpiresAt = now.plusSeconds(86_400),
        )
        server.enqueue(strictJson(200, body(recentlyExpiredIdle)))
        assertEquals(
            listOf(recentlyExpiredIdle),
            transport.listSessions(loopbackConfiguration()).sessions,
        )
    }

    @Test
    fun listAcceptsBoundaryTimestampsAndBothSupportedContractVersions() = runBlocking {
        val v2 = syntheticSession(now)
        val v1 = syntheticSession(
            now = now,
            id = OTHER_SESSION_ID,
            clientInstanceId = OTHER_INSTANCE_ID,
        ).copy(
            clientKind = "macos",
            scopes = ANDROID_DEVICE_AUTH_SCOPES.filterNot { it == "schedule_publish" },
            clientContractVersion = 1,
            clientCapabilities = listOf("calendar_read"),
        )
        server.enqueue(strictJson(200, body(v2, v1)))

        assertEquals(listOf(v2, v1), transport.listSessions(loopbackConfiguration()).sessions)
    }

    @Test
    fun listRejectsNonCanonicalServerOrdering() {
        val olderLowerId = syntheticSession(now)
        val newerHigherId = syntheticSession(
            now = now,
            id = OTHER_SESSION_ID,
            clientInstanceId = OTHER_INSTANCE_ID,
            lastSeenAt = now.plusSeconds(1),
        )
        server.enqueue(strictJson(200, body(olderLowerId, newerHigherId)))
        assertInvalidList()

        server.enqueue(strictJson(200, body(newerHigherId, olderLowerId)))
        assertEquals(
            listOf(newerHigherId, olderLowerId),
            runBlocking { transport.listSessions(loopbackConfiguration()).sessions },
        )

        server.enqueue(strictJson(200, body(olderLowerId.copy(id = OTHER_SESSION_ID), olderLowerId)))
        assertInvalidList()
    }

    @Test
    fun coordinatedTrusted401RotatesAndRetriesTheSessionInventory() = runBlocking {
        val baseUrl = server.url("/tenant/").toString()
        val issuedAt = now.minusSeconds(300)
        val active = syntheticActiveState(
            now = now,
            baseUrl = baseUrl,
            session = syntheticSession(
                now = now,
                createdAt = now.minusSeconds(86_400),
                issuedAt = issuedAt,
                lastSeenAt = issuedAt,
                accessExpiresAt = now.plusSeconds(600),
                refreshIdleExpiresAt = issuedAt.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(DEVICE_AUTH_ABSOLUTE_TTL).minusSeconds(86_400),
            ),
        )
        val authTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now = now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        issuedAt = now,
                        lastSeenAt = now,
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }
        val coordinator = DurableDeviceAuthCoordinator(
            store = FakeDeviceAuthEnvelopeStore(active),
            transport = authTransport,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            now = { now },
            generator = QueueDeviceCredentialGenerator(),
            allowCleartextLoopbackForTests = true,
        )
        assertEquals(active.clientInstanceId, coordinator.snapshot().clientInstanceId)
        server.enqueue(trustedUnauthorized())
        server.enqueue(strictJson(200, body(syntheticSession(now))))

        assertEquals(
            listOf(syntheticSession(now)),
            transport.listSessions(requireNotNull(coordinator.authenticatedConfiguration())).sessions,
        )

        assertEquals(1, authTransport.refreshCalls.size)
        assertEquals(active.clientInstanceId, coordinator.snapshot().clientInstanceId)
        val first = server.takeRequest()
        val retry = server.takeRequest()
        assertEquals(first.method, retry.method)
        assertEquals(first.url, retry.url)
        assertNotEquals(first.headers["Authorization"], retry.headers["Authorization"])
    }

    private fun assertInvalidList() {
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { transport.listSessions(loopbackConfiguration()) }
        }
    }

    private fun body(vararg sessions: DeviceSessionContract): String =
        DEVICE_AUTH_JSON.encodeToString(DeviceSessionListResponse(sessions.toList()))

    private fun canonicalSessions(count: Int): List<DeviceSessionContract> = List(count) { index ->
        val ordinal = index + 1
        val timestamp = now.minusSeconds(index.toLong())
        syntheticSession(
            now = timestamp,
            id = "00000000-0000-4000-8000-${ordinal.toString().padStart(12, '0')}",
            clientInstanceId =
                "10000000-0000-4000-8000-${ordinal.toString().padStart(12, '0')}",
        )
    }

    private fun loopbackConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun coordinatedConfiguration(
        executor: DeviceAuthRequestExecutor,
    ): AuthenticatedApiConfiguration = AuthenticatedApiConfiguration.createCoordinated(
        baseUrl = server.url("/tenant/").toString(),
        bearerToken = "captured-secret",
        configurationId = SYNTHETIC_SESSION_ID,
        executor = executor,
        allowCleartextLoopback = true,
    )

    private fun strictJson(
        code: Int,
        body: String,
        contentType: String = "application/json; charset=utf-8",
    ): MockResponse = MockResponse.Builder()
        .code(code)
        .addHeader("Content-Type", contentType)
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .body(body)
        .build()

    private fun strictNoContent(): MockResponse = MockResponse.Builder()
        .code(204)
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .build()

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

    private class RecordingRequestExecutor : DeviceAuthRequestExecutor {
        var calls = 0
        override suspend fun executeAuthenticated(
            configuration: AuthenticatedApiConfiguration,
            client: OkHttpClient,
            request: Request,
        ): Response {
            calls += 1
            assertTrue(configuration.configurationId != null)
            return client.newCall(
                request.newBuilder()
                    .header("Authorization", "Bearer unit-test-secret")
                    .build(),
            ).execute()
        }
    }

    private class FixedNoContentBodyExecutor : DeviceAuthRequestExecutor {
        override suspend fun executeAuthenticated(
            configuration: AuthenticatedApiConfiguration,
            client: OkHttpClient,
            request: Request,
        ): Response = Response.Builder()
            .request(request)
            .protocol(Protocol.HTTP_1_1)
            .code(204)
            .message("No Content")
            .header("Cache-Control", "no-store, max-age=0")
            .header("Pragma", "no-cache")
            .body("unexpected".toResponseBody())
            .build()
    }

    private companion object {
        const val OTHER_SESSION_ID = "33333333-3333-4333-8333-333333333333"
        const val OTHER_INSTANCE_ID = "44444444-4444-4444-8444-444444444444"
    }
}
