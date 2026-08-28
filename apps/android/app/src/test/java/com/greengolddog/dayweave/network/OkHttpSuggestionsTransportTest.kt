package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class OkHttpSuggestionsTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpSuggestionsTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpSuggestionsTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun listUsesBearerAuthPathPrefixAndTypedResponse() = runBlocking {
        server.enqueue(jsonResponse("""{"suggestions":[${remoteSuggestionJson()}]}"""))

        val suggestions = transport.list(configuration())

        assertEquals(1, suggestions.size)
        assertEquals("proposal-id", suggestions.single().id)
        assertEquals(7L, suggestions.single().revision)
        assertEquals("schedule_plan", suggestions.single().kind)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/suggestions", request.url.encodedPath)
        assertEquals("limit=200", request.url.encodedQuery)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun editAcceptAndRejectSendExpectedRevisionAndJsonContentType() = runBlocking {
        server.enqueue(jsonResponse("""{"suggestion":${remoteSuggestionJson(revision = 8)}}"""))
        server.enqueue(
            jsonResponse(
                """{"suggestion":${remoteSuggestionJson(revision = 9, status = "accepted")}}""",
            ),
        )
        server.enqueue(
            jsonResponse(
                """{"suggestion":${remoteSuggestionJson(revision = 10, status = "rejected")}}""",
            ),
        )

        transport.edit(configuration(), "proposal-id", 7, "Edited title", "Edited explanation")
        transport.accept(configuration(), "proposal-id", 8)
        transport.reject(configuration(), "proposal-id", 9)

        val edit = server.takeRequest()
        assertEquals("PATCH", edit.method)
        assertEquals("/tenant/v1/suggestions/proposal-id", edit.url.encodedPath)
        assertEquals("application/json; charset=utf-8", edit.headers["Content-Type"])
        assertEquals(
            Json.parseToJsonElement(
                """{"expected_revision":7,"title":"Edited title","explanation":"Edited explanation"}""",
            ),
            Json.parseToJsonElement(requireNotNull(edit.body).utf8()),
        )

        val accept = server.takeRequest()
        assertEquals("/tenant/v1/suggestions/proposal-id/accept", accept.url.encodedPath)
        assertEquals(
            Json.parseToJsonElement("""{"expected_revision":8}"""),
            Json.parseToJsonElement(requireNotNull(accept.body).utf8()),
        )

        val reject = server.takeRequest()
        assertEquals("/tenant/v1/suggestions/proposal-id/reject", reject.url.encodedPath)
        assertEquals(
            Json.parseToJsonElement("""{"expected_revision":9}"""),
            Json.parseToJsonElement(requireNotNull(reject.body).utf8()),
        )
    }

    @Test
    fun unauthorizedResponseIsTypedAndNeverIncludesTokenInDiagnostics() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(401)
                .addHeader("Content-Type", "application/json")
                .body("""{"error":{"code":"unauthorized","message":"A token is required"}}""")
                .build(),
        )

        val error = assertThrows(SuggestionApiException.Authentication::class.java) {
            runBlocking { transport.list(configuration()) }
        }

        assertFalse(error.toString().contains("unit-test-secret"))
    }

    @Test
    fun redirectIsRejectedWithoutReplayingTheBearerToken() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(302)
                .addHeader("Location", "https://redirected.example.test/v1/suggestions")
                .build(),
        )

        val error = assertThrows(SuggestionApiException.Http::class.java) {
            runBlocking { transport.list(configuration()) }
        }

        assertEquals(302, error.statusCode)
        assertEquals(1, server.requestCount)
        assertEquals("Bearer unit-test-secret", server.takeRequest().headers["Authorization"])
    }

    @Test
    fun productionConfigurationRejectsCleartextAndRedactsItsToken() {
        assertThrows(InvalidApiConfigurationException::class.java) {
            AuthenticatedApiConfiguration.create("http://example.com", "secret")
        }

        val configuration = AuthenticatedApiConfiguration.create(
            "https://api.example.com/dayweave",
            "secret",
        )
        assertEquals("https://api.example.com/dayweave/", configuration.baseUrl.toString())
        assertFalse(configuration.toString().contains("secret"))
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun jsonResponse(body: String): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "application/json")
        .body(body)
        .build()

    private fun remoteSuggestionJson(
        revision: Long = 7,
        status: String = "pending",
    ): String = """
        {
          "id":"proposal-id",
          "revision":$revision,
          "submitted_by":"token:fingerprint",
          "source":"codex",
          "source_reference":"conversation-42",
          "kind":"schedule_plan",
          "status":"$status",
          "title":"Protect recovery time",
          "explanation":"Keep a protected hour after deep work",
          "payload":{"start_minute":1020},
          "decision_note":null,
          "created_at":"2026-08-29T09:00:00Z",
          "updated_at":"2026-08-29T09:00:00Z",
          "expires_at":"2026-09-05T09:00:00Z",
          "decided_at":null
        }
    """.trimIndent()
}
