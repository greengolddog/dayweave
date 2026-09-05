package com.greengolddog.dayweave.network

import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class OkHttpAccountRecoveryTransportTest {
    private val now = Instant.parse("2026-09-05T09:00:00Z")
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpAccountRecoveryTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpAccountRecoveryTransport(
            now = { now },
            allowCleartextLoopbackForTests = true,
        )
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun currentUsesCoordinatedAuthenticationAndAcceptsRequiredNull() = runBlocking {
        val executor = RecordingExecutor()
        server.enqueue(strictJson(200, "{\"recovery_code\":null}"))

        val result = transport.current(coordinatedConfiguration(executor))

        assertEquals(null, result.recoveryCode)
        assertEquals(1, executor.calls)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/auth/recovery-codes/current", request.url.encodedPath)
        assertEquals("Bearer refreshed-test-secret", request.headers["Authorization"])
        assertEquals("application/json", request.headers["Accept"])
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
    }

    @Test
    fun issue401ThenRefresh401RetainsJournalInExactReauthState() = runBlocking {
        val active = activeForRecoveryIssuance()
        val request = issueRequest()
        val journal = issuanceJournal(active, request)
        val store = FakeDeviceAuthEnvelopeStore(active, journal)
        val deviceTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = { throw DeviceAuthApiException.Authentication() }
        }
        server.enqueue(trustedUnauthorized())
        val manager = coordinatedManager(store, deviceTransport)

        assertEquals(DeviceAuthActionResult.AUTH_REQUIRED, manager.retryPending())

        val reauth = store.envelope.state as StoredDeviceAuthState.Reauth
        assertEquals(active.baseUrl, reauth.baseUrl)
        assertEquals(active.clientInstanceId, reauth.clientInstanceId)
        assertEquals(active.session.id, reauth.previousSessionId)
        assertEquals(journal, store.envelope.accountRecoveryJournal)
        assertTrue(manager.state.value.retryAvailable)
        assertTrue(manager.state.value.discardAvailable)
        assertEquals(1, deviceTransport.refreshCalls.size)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun issue401ThenRefreshSuccessAndSecond401RetainsJournalInExactReauthState() = runBlocking {
        val active = activeForRecoveryIssuance()
        val request = issueRequest()
        val journal = issuanceJournal(active, request)
        val store = FakeDeviceAuthEnvelopeStore(active, journal)
        val deviceTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                DeviceSessionMutationResponse(
                    session = syntheticSession(
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
        server.enqueue(trustedUnauthorized())
        server.enqueue(trustedUnauthorized())
        val manager = coordinatedManager(store, deviceTransport)

        assertEquals(DeviceAuthActionResult.AUTH_REQUIRED, manager.retryPending())

        val reauth = store.envelope.state as StoredDeviceAuthState.Reauth
        assertEquals(active.baseUrl, reauth.baseUrl)
        assertEquals(active.clientInstanceId, reauth.clientInstanceId)
        assertEquals(active.session.id, reauth.previousSessionId)
        assertEquals(journal, store.envelope.accountRecoveryJournal)
        assertTrue(manager.state.value.retryAvailable)
        assertTrue(manager.state.value.discardAvailable)
        assertEquals(1, deviceTransport.refreshCalls.size)
        assertEquals(2, server.requestCount)
    }

    @Test
    fun locallyExpiredRefreshAuthorityRetainsIssuanceJournalInExactReauthState() = runBlocking {
        val issuedAt = now.minus(Duration.ofMinutes(16))
        val active = syntheticActiveState(
            now = now,
            session = syntheticSession(
                now = now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = issuedAt,
                lastSeenAt = issuedAt,
                accessExpiresAt = now.minus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = now,
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val request = issueRequest()
        val journal = issuanceJournal(active, request)
        val store = FakeDeviceAuthEnvelopeStore(active, journal)
        val deviceTransport = RecordingDeviceAuthTransport()
        val manager = coordinatedManager(store, deviceTransport)

        assertEquals(DeviceAuthActionResult.AUTH_REQUIRED, manager.retryPending())

        val reauth = store.envelope.state as StoredDeviceAuthState.Reauth
        assertEquals(active.session.id, reauth.previousSessionId)
        assertEquals(journal, store.envelope.accountRecoveryJournal)
        assertTrue(manager.state.value.retryAvailable)
        assertTrue(deviceTransport.refreshCalls.isEmpty())
        assertEquals(0, server.requestCount)
    }

    @Test
    fun issueSendsExactTupleAndEnforcesStatusReplayCoupling() = runBlocking {
        val request = issueRequest()
        val contract = AccountRecoveryCodeContract(request.id, now.toString(), 1)
        server.enqueue(
            strictJson(
                201,
                DEVICE_AUTH_JSON.encodeToString(
                    AccountRecoveryCodeMutationResponse(contract, replayed = false),
                ),
            ),
        )

        assertEquals(
            AccountRecoveryCodeMutationResponse(contract, replayed = false),
            transport.issue(loopbackConfiguration(), request, now),
        )
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/auth/recovery-codes", recorded.url.encodedPath)
        assertEquals("application/json; charset=utf-8", recorded.headers["Content-Type"])
        assertEquals(DEVICE_AUTH_JSON.encodeToString(request), recorded.body?.utf8())
        assertFalse(request.toString().contains(request.recoveryCode))

        server.enqueue(
            strictJson(
                200,
                DEVICE_AUTH_JSON.encodeToString(
                    AccountRecoveryCodeMutationResponse(contract, replayed = false),
                ),
            ),
        )
        assertInvalid { transport.issue(loopbackConfiguration(), request, now) }
    }

    @Test
    fun consumeValidatesAndReturnsAtomicSessionAndSuccessor() = runBlocking {
        val recoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 31)
        val request = consumeRequest()
        val session = syntheticSession(
            now = now,
            id = request.sessionId,
            clientInstanceId = request.clientInstanceId,
        )
        val successor = AccountRecoveryCodeContract(
            request.successorRecoveryCodeId,
            now.toString(),
            1,
        )
        val response = AccountRecoveryConsumptionResponse(session, successor, replayed = false)
        server.enqueue(strictJson(201, DEVICE_AUTH_JSON.encodeToString(response)))

        assertEquals(
            response,
            transport.consume(loopbackBaseUrl(), recoveryCode, request, now),
        )
        val recorded = server.takeRequest()
        assertEquals("/tenant/v1/auth/recovery-codes/consume", recorded.url.encodedPath)
        assertEquals("Bearer $recoveryCode", recorded.headers["Authorization"])
        assertEquals(DEVICE_AUTH_JSON.encodeToString(request), recorded.body?.utf8())
        assertFalse(request.toString().contains(request.accessToken))
        assertFalse(request.toString().contains(request.refreshToken))
        assertFalse(request.toString().contains(request.successorRecoveryCode))
    }

    @Test
    fun consumeRejectsMismatchedSessionOrSuccessorBeforeReturningCommitEvidence() {
        val recoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 35)
        val request = consumeRequest()
        val validSession = syntheticSession(
            now = now,
            id = request.sessionId,
            clientInstanceId = request.clientInstanceId,
        )
        val wrongSession = AccountRecoveryConsumptionResponse(
            session = validSession.copy(id = "77777777-7777-4777-8777-777777777777"),
            successorRecoveryCode = AccountRecoveryCodeContract(
                request.successorRecoveryCodeId,
                now.toString(),
                1,
            ),
            replayed = false,
        )
        server.enqueue(strictJson(201, DEVICE_AUTH_JSON.encodeToString(wrongSession)))
        assertInvalid {
            transport.consume(loopbackBaseUrl(), recoveryCode, request, now)
        }

        val wrongSuccessor = AccountRecoveryConsumptionResponse(
            session = validSession,
            successorRecoveryCode = AccountRecoveryCodeContract(
                "88888888-8888-4888-8888-888888888888",
                now.toString(),
                1,
            ),
            replayed = false,
        )
        server.enqueue(strictJson(201, DEVICE_AUTH_JSON.encodeToString(wrongSuccessor)))
        assertInvalid {
            transport.consume(loopbackBaseUrl(), recoveryCode, request, now)
        }
    }

    @Test
    fun consumeRejectsNonfutureRefreshAndAbsoluteExpiriesForNewAndExactReplay() {
        val recoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 36)
        val request = consumeRequest()
        val issuedAt = now.minusSeconds(3)
        listOf(false, true).forEach { replayed ->
            listOf(ExpiryField.REFRESH_IDLE, ExpiryField.ABSOLUTE).forEach { field ->
                listOf(now, now.minusSeconds(1)).forEach { invalidExpiry ->
                    // When absolute expiry is nonfuture, the session contract necessarily makes
                    // access and idle expiry nonfuture too because both must be <= absolute.
                    // The scalar verifier test below isolates each receipt-time condition.
                    val supportingExpiry = invalidExpiry.minusSeconds(1)
                    val session = syntheticSession(
                        now = now,
                        id = request.sessionId,
                        clientInstanceId = request.clientInstanceId,
                        createdAt = issuedAt,
                        issuedAt = issuedAt,
                        lastSeenAt = issuedAt,
                        accessExpiresAt = if (field == ExpiryField.ABSOLUTE) {
                            supportingExpiry
                        } else {
                            now.plusSeconds(60)
                        },
                        refreshIdleExpiresAt = if (field == ExpiryField.REFRESH_IDLE) {
                            invalidExpiry
                        } else {
                            supportingExpiry
                        },
                        absoluteExpiresAt = if (field == ExpiryField.ABSOLUTE) {
                            invalidExpiry
                        } else {
                            now.plusSeconds(3_600)
                        },
                    )
                    val response = AccountRecoveryConsumptionResponse(
                        session = session,
                        successorRecoveryCode = AccountRecoveryCodeContract(
                            request.successorRecoveryCodeId,
                            session.createdAt,
                            1,
                        ),
                        replayed = replayed,
                    )
                    server.enqueue(
                        strictJson(
                            if (replayed) 200 else 201,
                            DEVICE_AUTH_JSON.encodeToString(response),
                        ),
                    )

                    assertInvalid {
                        transport.consume(loopbackBaseUrl(), recoveryCode, request, issuedAt)
                    }
                }
            }
        }
    }

    @Test
    fun receiptExpiryVerifierRejectsEachBoundaryIndependently() {
        listOf(now, now.minusSeconds(1)).forEach { nonfuture ->
            assertThrows(IllegalArgumentException::class.java) {
                requireRecoverySessionExpiriesAfterReceipt(
                    refreshIdleExpiry = now.plusSeconds(1),
                    absoluteExpiry = nonfuture,
                    receivedAt = now,
                )
            }
            assertThrows(IllegalArgumentException::class.java) {
                requireRecoverySessionExpiriesAfterReceipt(
                    refreshIdleExpiry = nonfuture,
                    absoluteExpiry = now.plusSeconds(1),
                    receivedAt = now,
                )
            }
        }
    }

    @Test
    fun rejectsDuplicateUnknownAndNoncanonicalJsonBeforeDecoding() {
        val code = AccountRecoveryCodeContract(RECOVERY_ID, now.toString(), 1)
        val valid = DEVICE_AUTH_JSON.encodeToString(
            CurrentAccountRecoveryCodeResponse(code),
        )
        val bodies = listOf(
            "{\"recovery_code\":null,\"recovery\\u005fcode\":null}",
            valid.replace("\"revision\":1", "\"revision\":1,\"revi\\u0073ion\":1"),
            valid.dropLast(1) + ",\"future\":true}",
            valid.replace("\"revision\":1", "\"revision\":1e0"),
        )
        bodies.forEach { body ->
            server.enqueue(strictJson(200, body))
            assertInvalid { transport.current(loopbackConfiguration()) }
        }
    }

    @Test
    fun rejectsMissingNoStoreWrongMediaFutureAndStaleMutationTimes() {
        val current = DEVICE_AUTH_JSON.encodeToString(
            CurrentAccountRecoveryCodeResponse(
                AccountRecoveryCodeContract(RECOVERY_ID, now.toString(), 1),
            ),
        )
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body(current)
                .build(),
        )
        assertInvalid { transport.current(loopbackConfiguration()) }

        server.enqueue(strictJson(200, current, "application/problem+json"))
        assertInvalid { transport.current(loopbackConfiguration()) }

        val future = current.replace(now.toString(), now.plusSeconds(301).toString())
        server.enqueue(strictJson(200, future))
        assertInvalid { transport.current(loopbackConfiguration()) }

        val issue = issueRequest()
        val stale = AccountRecoveryCodeMutationResponse(
            AccountRecoveryCodeContract(issue.id, now.minusSeconds(301).toString(), 1),
            replayed = false,
        )
        server.enqueue(strictJson(201, DEVICE_AUTH_JSON.encodeToString(stale)))
        assertInvalid { transport.issue(loopbackConfiguration(), issue, now) }
    }

    @Test
    fun responseBodyBoundAcceptsExactLimitAndRejectsOneByteMore() = runBlocking {
        val body = "{\"recovery_code\":null}"
        val padding = 64 * 1024 - body.toByteArray().size
        server.enqueue(strictJson(200, body + " ".repeat(padding)))
        assertEquals(null, transport.current(loopbackConfiguration()).recoveryCode)

        server.enqueue(strictJson(200, body + " ".repeat(padding + 1)))
        assertInvalid { transport.current(loopbackConfiguration()) }
    }

    private fun issueRequest() = CreateAccountRecoveryCodeRequest(
        id = RECOVERY_ID,
        recoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 30),
        replacesRecoveryCodeId = null,
        replacesRecoveryCodeRevision = null,
    )

    private fun activeForRecoveryIssuance() = syntheticActiveState(
        now = now,
    )

    private fun issuanceJournal(
        active: StoredDeviceAuthState.Active,
        request: CreateAccountRecoveryCodeRequest,
    ) = StoredAccountRecoveryJournal.IssuancePending(
        baseUrl = active.baseUrl,
        configurationId = active.session.id,
        clientInstanceId = active.clientInstanceId,
        candidateId = request.id,
        candidateCode = DeviceAuthSecret(request.recoveryCode),
        replacesId = request.replacesRecoveryCodeId,
        replacesRevision = request.replacesRecoveryCodeRevision,
        preparedAt = now.toString(),
    )

    private fun coordinatedManager(
        store: FakeDeviceAuthEnvelopeStore,
        deviceTransport: RecordingDeviceAuthTransport,
    ): AccountRecoveryManager {
        val gate = ApiBindingOperationGate()
        val coordinator = DurableDeviceAuthCoordinator(
            store = store,
            transport = deviceTransport,
            deviceSessionsTransport = RecordingDeviceSessionsTransport(),
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            bindingOperationGate = gate,
            now = { now },
            generator = QueueDeviceCredentialGenerator(),
            allowCleartextLoopbackForTests = true,
        )
        return AccountRecoveryManager(
            store = store,
            credentialStore = coordinator,
            transport = routedCoordinatedTransport(),
            bindingOperationGate = gate,
            bindingFence = AllowDeviceAuthBindingChange,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            now = { now },
            allowCleartextLoopbackForTests = true,
        )
    }

    private fun routedCoordinatedTransport() = OkHttpAccountRecoveryTransport(
        client = OkHttpClient.Builder()
            .addInterceptor { chain ->
                val original = chain.request()
                val routed = server.url(original.url.encodedPath).newBuilder()
                    .encodedQuery(original.url.encodedQuery)
                    .build()
                chain.proceed(original.newBuilder().url(routed).build())
            }
            .build(),
        now = { now },
        allowCleartextLoopbackForTests = true,
    )

    private fun consumeRequest() = ConsumeAccountRecoveryCodeRequest(
        sessionId = SYNTHETIC_SESSION_ID,
        accessToken = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 32),
        refreshToken = syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 33),
        clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
        deviceLabel = SYNTHETIC_DEVICE_LABEL,
        clientVersion = SYNTHETIC_CLIENT_VERSION,
        successorRecoveryCodeId = SUCCESSOR_ID,
        successorRecoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 34),
    )

    private fun assertInvalid(block: suspend () -> Unit) {
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { block() }
        }
    }

    private fun loopbackBaseUrl(): String = server.url("/tenant/").toString()

    private fun loopbackConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            loopbackBaseUrl(),
            "unit-test-secret",
        )

    private fun coordinatedConfiguration(
        executor: DeviceAuthRequestExecutor,
    ): AuthenticatedApiConfiguration = AuthenticatedApiConfiguration.createCoordinated(
        baseUrl = loopbackBaseUrl(),
        bearerToken = "captured-test-secret",
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

    private fun trustedUnauthorized(): MockResponse = MockResponse.Builder()
        .code(401)
        .addHeader("Content-Type", "application/json")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
        .body(
            """{"error":{"code":"unauthorized","message":"A valid bearer token is required"}}""",
        )
        .build()

    private inner class RecordingExecutor : DeviceAuthRequestExecutor {
        var calls = 0

        override suspend fun executeAuthenticated(
            configuration: AuthenticatedApiConfiguration,
            client: OkHttpClient,
            request: Request,
        ): Response {
            calls += 1
            return client.newCall(
                request.newBuilder()
                    .header("Authorization", "Bearer refreshed-test-secret")
                    .build(),
            ).execute()
        }
    }

    private enum class ExpiryField {
        REFRESH_IDLE,
        ABSOLUTE,
    }

    private companion object {
        const val RECOVERY_ID = "55555555-5555-4555-8555-555555555555"
        const val SUCCESSOR_ID = "66666666-6666-4666-8666-666666666666"
    }
}
