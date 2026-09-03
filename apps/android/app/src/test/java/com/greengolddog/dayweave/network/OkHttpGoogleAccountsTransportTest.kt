package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class OkHttpGoogleAccountsTransportTest {
    @Test
    fun accountStatusIsStrictAuthenticatedAndContentBounded() = runBlocking {
        val server = MockWebServer()
        server.start()
        try {
            server.enqueue(jsonResponse(200, accountsJson()))
            val response = transport().accounts(configuration(server))

            assertEquals(1, response.accounts.size)
            assertEquals("Owner", response.accounts.single().displayLabel)
            val request = server.takeRequest()
            assertEquals("/v1/integrations/google/accounts", request.url.encodedPath)
            assertEquals("Bearer test-secret", request.headers["Authorization"])

            server.enqueue(jsonResponse(200, accountsJson(extra = ",\"unexpected\":true")))
            assertThrows(GoogleAccountsApiException.InvalidResponse::class.java) {
                runBlocking { transport().accounts(configuration(server)) }
            }

            listOf(
                ",\"token_expires_at\":\"2026-09-01T08:00:00Z\"",
                ",\"next_attempt_at\":null",
                ",\"last_failure_at\":null",
            ).forEach { requiredNullableMember ->
                server.enqueue(
                    jsonResponse(
                        200,
                        accountsJson().replace(requiredNullableMember, ""),
                    ),
                )
                assertThrows(GoogleAccountsApiException.InvalidResponse::class.java) {
                    runBlocking { transport().accounts(configuration(server)) }
                }
            }

            server.enqueue(jsonResponse(200, "x".repeat(2 * 1024 * 1024 + 1)))
            assertThrows(GoogleAccountsApiException.InvalidResponse::class.java) {
                runBlocking { transport().accounts(configuration(server)) }
            }
            Unit
        } finally {
            server.close()
        }
    }

    @Test
    fun authorizationStartUsesExplicitReadOnlySentinelAndDoesNotFollowRedirects() = runBlocking {
        val server = MockWebServer()
        server.start()
        try {
            server.enqueue(
                jsonResponse(
                    201,
                    """{"authorization_url":"https://accounts.google.com/o/oauth2/v2/auth?state=opaque&code_challenge=proof","expires_at":"2026-09-01T07:10:00Z"}""",
                ),
            )
            val started = transport().startAuthorization(
                configuration(server),
                IDEMPOTENCY_KEY,
                StartGoogleAuthorizationRequest(
                    makeDefault = true,
                ),
            )
            assertTrue(started.authorizationUrl.startsWith("https://accounts.google.com/"))
            val request = server.takeRequest()
            assertEquals("/v1/integrations/google/oauth/start", request.url.encodedPath)
            assertEquals(IDEMPOTENCY_KEY, request.headers["Idempotency-Key"])
            val body = Json.parseToJsonElement(requireNotNull(request.body).utf8()).jsonObject
            assertEquals(
                setOf(
                    "services",
                    "force_consent",
                    "login_hint",
                    "account_id",
                    "connect_new",
                    "make_default",
                ),
                body.keys,
            )
            assertTrue(body.getValue("services").jsonArray.isEmpty())
            assertEquals("false", body.getValue("connect_new").jsonPrimitive.content)
            assertEquals("true", body.getValue("make_default").jsonPrimitive.content)

            server.enqueue(
                MockResponse.Builder()
                    .code(302)
                    .addHeader("Location", "https://evil.example/")
                    .build(),
            )
            assertThrows(GoogleAccountsApiException.Http::class.java) {
                runBlocking { transport().accounts(configuration(server)) }
            }
            assertEquals(2, server.requestCount)
        } finally {
            server.close()
        }
    }

    @Test
    fun authorizationAllowsOnlyExactExistingAccountPublishingUpgrades() = runBlocking {
        val server = MockWebServer()
        server.start()
        try {
            val serviceSelections = listOf(
                listOf(GoogleService.CALENDAR),
                listOf(GoogleService.TASKS),
            )
            val transport = transport()
            val configuration = configuration(server)
            serviceSelections.forEach { services ->
                server.enqueue(
                    jsonResponse(
                        201,
                        """{"authorization_url":"https://accounts.google.com/o/oauth2/v2/auth?state=opaque","expires_at":"2026-09-01T07:10:00Z"}""",
                    ),
                )
                transport.startAuthorization(
                    configuration,
                    IDEMPOTENCY_KEY,
                    StartGoogleAuthorizationRequest(
                        services = services,
                        forceConsent = true,
                        accountId = ACCOUNT_ID,
                    ),
                )
                val body = Json.parseToJsonElement(
                    requireNotNull(server.takeRequest().body).utf8(),
                ).jsonObject
                assertEquals(
                    services.map { service -> service.serializedName },
                    body.getValue("services").jsonArray.map { it.jsonPrimitive.content },
                )
                assertEquals("true", body.getValue("force_consent").jsonPrimitive.content)
                assertEquals(ACCOUNT_ID, body.getValue("account_id").jsonPrimitive.content)
                assertEquals("false", body.getValue("connect_new").jsonPrimitive.content)
            }
            listOf(
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.CALENDAR_READ_ONLY),
                    forceConsent = true,
                    accountId = ACCOUNT_ID,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.TASKS_READ_ONLY),
                    forceConsent = true,
                    accountId = ACCOUNT_ID,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.CALENDAR, GoogleService.TASKS),
                    forceConsent = true,
                    accountId = ACCOUNT_ID,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.CALENDAR),
                    accountId = ACCOUNT_ID,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.TASKS),
                    forceConsent = true,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.CALENDAR),
                    forceConsent = true,
                    accountId = ACCOUNT_ID,
                    connectNew = true,
                ),
                StartGoogleAuthorizationRequest(
                    accountId = ACCOUNT_ID,
                    connectNew = true,
                ),
                StartGoogleAuthorizationRequest(
                    services = listOf(GoogleService.CALENDAR),
                    forceConsent = true,
                    accountId = "00000000-0000-0000-0000-000000000000",
                ),
                StartGoogleAuthorizationRequest(loginHint = ""),
                StartGoogleAuthorizationRequest(loginHint = "x".repeat(321)),
                StartGoogleAuthorizationRequest(loginHint = "owner\n@example.com"),
            ).forEach { invalidRequest ->
                assertThrows(IllegalArgumentException::class.java) {
                    runBlocking {
                        transport.startAuthorization(
                            configuration,
                            IDEMPOTENCY_KEY,
                            invalidRequest,
                        )
                    }
                }
            }
            assertEquals(serviceSelections.size, server.requestCount)
        } finally {
            server.close()
        }
    }

    @Test
    fun pauseResumeAndDisconnectAreRevisionedIdempotentRequests() = runBlocking {
        val server = MockWebServer()
        server.start()
        try {
            repeat(3) { server.enqueue(jsonResponse(200, accountJson())) }
            val transport = transport()
            val configuration = configuration(server)

            transport.setPaused(configuration, ACCOUNT_ID, 7, true, IDEMPOTENCY_KEY)
            transport.setPaused(configuration, ACCOUNT_ID, 7, false, IDEMPOTENCY_KEY)
            transport.disconnect(configuration, ACCOUNT_ID, 7, IDEMPOTENCY_KEY)

            val pause = server.takeRequest()
            assertEquals("/v1/integrations/google/accounts/$ACCOUNT_ID/pause", pause.url.encodedPath)
            assertEquals("POST", pause.method)
            assertEquals(IDEMPOTENCY_KEY, pause.headers["Idempotency-Key"])
            assertEquals("7", Json.parseToJsonElement(requireNotNull(pause.body).utf8()).jsonObject
                .getValue("expected_revision").jsonPrimitive.content)
            val resume = server.takeRequest()
            assertEquals("/v1/integrations/google/accounts/$ACCOUNT_ID/resume", resume.url.encodedPath)
            assertEquals(IDEMPOTENCY_KEY, resume.headers["Idempotency-Key"])
            val disconnect = server.takeRequest()
            assertEquals(
                "/v1/integrations/google/accounts/$ACCOUNT_ID?expected_revision=7",
                "${disconnect.url.encodedPath}?${disconnect.url.encodedQuery}",
            )
            assertEquals("DELETE", disconnect.method)
            assertEquals(IDEMPOTENCY_KEY, disconnect.headers["Idempotency-Key"])
        } finally {
            server.close()
        }
    }

    @Test
    fun malformedIdentifiersAndKeysFailBeforeNetworkIo() = runBlocking {
        val server = MockWebServer()
        server.start()
        try {
            val transport = transport()
            val configuration = configuration(server)
            assertThrows(IllegalArgumentException::class.java) {
                runBlocking {
                    transport.disconnect(configuration, "not-a-uuid", 1, IDEMPOTENCY_KEY)
                }
            }
            assertThrows(IllegalArgumentException::class.java) {
                runBlocking {
                    transport.startAuthorization(
                        configuration,
                        "invalid:key",
                        StartGoogleAuthorizationRequest(
                            connectNew = true,
                            makeDefault = true,
                        ),
                    )
                }
            }
            assertThrows(IllegalArgumentException::class.java) {
                runBlocking {
                    transport.setPaused(configuration, ACCOUNT_ID, 0, true, IDEMPOTENCY_KEY)
                }
            }
            assertThrows(IllegalArgumentException::class.java) {
                runBlocking {
                    transport.startAuthorization(
                        configuration,
                        "bad key",
                        StartGoogleAuthorizationRequest(
                            connectNew = true,
                            makeDefault = true,
                        ),
                    )
                }
            }
            assertEquals(0, server.requestCount)
        } finally {
            server.close()
        }
    }

    private fun transport() = OkHttpGoogleAccountsTransport()

    private val GoogleService.serializedName: String
        get() = when (this) {
            GoogleService.CALENDAR_READ_ONLY -> "calendar_read_only"
            GoogleService.CALENDAR -> "calendar"
            GoogleService.TASKS_READ_ONLY -> "tasks_read_only"
            GoogleService.TASKS -> "tasks"
        }

    private fun configuration(server: MockWebServer) =
        AuthenticatedApiConfiguration.createForLoopbackTest(server.url("/").toString(), "test-secret")

    private fun jsonResponse(code: Int, body: String) = MockResponse.Builder()
        .code(code)
        .addHeader("Content-Type", "application/json")
        .body(body)
        .build()

    private fun accountsJson(extra: String = "") =
        """{"accounts":[${accountJson()}],"cleanup":{"held":0,"pending":0,"retrying":0,"exhausted":0,"volatile_guardians":0,"durability_degraded":false,"revocation_fenced":false,"operator_recovery_required":false,"uncertain_authorizations":0,"legacy_recovery_required":0,"next_attempt_at":null,"last_failure_at":null}$extra}"""

    private fun accountJson() =
        """{"id":"$ACCOUNT_ID","external_account_id":"google-subject","display_label":"Owner","status":"active","sync_enabled":true,"is_default":true,"granted_scopes":["https://www.googleapis.com/auth/calendar","https://www.googleapis.com/auth/tasks"],"token_expires_at":"2026-09-01T08:00:00Z","revision":7,"created_at":"2026-09-01T06:00:00Z","updated_at":"2026-09-01T07:00:00Z"}"""

    private companion object {
        const val ACCOUNT_ID = "11111111-1111-4111-8111-111111111111"
        const val IDEMPOTENCY_KEY = "22222222-2222-4222-8222-222222222222"
    }
}
