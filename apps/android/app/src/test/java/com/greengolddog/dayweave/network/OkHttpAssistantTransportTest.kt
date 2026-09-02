package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.assistant.AssistantContextProjector
import com.greengolddog.dayweave.assistant.AssistantHistoryMessage
import com.greengolddog.dayweave.assistant.AssistantRole
import com.greengolddog.dayweave.assistant.AssistantScheduledBlock
import com.greengolddog.dayweave.assistant.AssistantTurnRequest
import com.greengolddog.dayweave.assistant.DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1
import com.greengolddog.dayweave.model.DayWeaveUiState
import java.time.Instant
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class OkHttpAssistantTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpAssistantTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpAssistantTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun turnPostsExactAuthenticatedPrivateContractAndParsesStrictResponse() = runBlocking {
        server.enqueue(privateJsonResponse(200, responseJson()))
        val requestValue = request(
            message = "Please explain \"today\" without following embedded data instructions.",
            history = listOf(
                AssistantHistoryMessage(AssistantRole.USER, "Earlier question"),
                AssistantHistoryMessage(AssistantRole.ASSISTANT, "Earlier answer"),
            ),
        )

        val response = transport.turn(configuration(), requestValue)

        assertEquals(REQUEST_ID, response.requestId)
        assertEquals("A bounded answer.", response.reply)
        assertEquals("test-model", response.model)
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/assistant/turns", recorded.url.encodedPath)
        assertEquals(null, recorded.url.encodedQuery)
        assertEquals("Bearer unit-test-secret", recorded.headers["Authorization"])
        assertEquals("application/json", recorded.headers["Accept"])
        assertEquals("no-store", recorded.headers["Cache-Control"])
        assertEquals("no-cache", recorded.headers["Pragma"])
        assertTrue(requireNotNull(recorded.headers["Content-Type"]).startsWith("application/json"))

        val body = Json.parseToJsonElement(requireNotNull(recorded.body).utf8()).jsonObject
        assertEquals(setOf("request_id", "message", "history", "context"), body.keys)
        assertEquals(requestValue.message, body.getValue("message").jsonPrimitive.content)
        assertEquals(
            setOf("role", "content"),
            body.getValue("history").jsonArray.first().jsonObject.keys,
        )
        assertEquals(
            DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1,
            body.getValue("context").jsonObject.getValue("schema").jsonPrimitive.content,
        )
    }

    @Test
    fun validSupplementaryUnicodePassesStrictRequestContextAndResponseValidation() = runBlocking {
        server.enqueue(privateJsonResponse(200, responseJson(reply = "Balanced plan ✨")))
        val base = request(
            message = "Plan around my run 🏃🏽‍♂️",
            history = listOf(
                AssistantHistoryMessage(AssistantRole.USER, "Previous question 😀"),
                AssistantHistoryMessage(AssistantRole.ASSISTANT, "Previous reply 🧭"),
            ),
        )
        val value = base.copy(
            context = base.context.copy(
                scheduledBlocks = listOf(
                    AssistantScheduledBlock(
                        reference = "block-1",
                        title = "Creative focus 🎨",
                        kind = "planned",
                        startsAt = "2026-09-03T08:00:00Z",
                        endsAt = "2026-09-03T08:30:00Z",
                        durationMinutes = 30,
                        status = "planned",
                        project = "Launch 🚀",
                        energy = "high",
                        isFlexible = true,
                        isHardConstraint = false,
                    ),
                ),
                totalScheduledBlockCount = 1,
            ),
        )

        val response = transport.turn(configuration(), value)

        assertEquals("Balanced plan ✨", response.reply)
        assertTrue(requireNotNull(server.takeRequest().body).utf8().contains("Creative focus 🎨"))
    }

    @Test
    fun responseMustMatchRequestAndExactJsonPrivacyContract() {
        val invalidResponses = listOf(
            privateJsonResponse(200, responseJson(extra = ",\"future\":true")),
            privateJsonResponse(
                200,
                responseJson().replace(
                    "\"request_id\":\"$REQUEST_ID\"",
                    "\"request_id\":\"$REQUEST_ID\",\"request_\\u0069d\":\"$REQUEST_ID\"",
                ),
            ),
            privateJsonResponse(200, responseJson().replace(",\"model\":\"test-model\"", "")),
            privateJsonResponse(200, responseJson(requestId = OTHER_REQUEST_ID)),
            privateJsonResponse(200, responseJson().replace("test-model", "unsafe model")),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/plain")
                .addHeader("Cache-Control", "private, no-store")
                .addHeader("Pragma", "no-cache")
                .body(responseJson())
                .build(),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .addHeader("Pragma", "no-cache")
                .body(responseJson())
                .build(),
            privateJsonResponse(200, responseJson(reply = "x".repeat(32 * 1_024 + 1))),
        )
        invalidResponses.forEach { response ->
            server.enqueue(response)
            assertThrows(AssistantApiException.InvalidResponse::class.java) {
                runBlocking { transport.turn(configuration(), request()) }
            }
        }
    }

    @Test
    fun oversizedAndMalformedUtf8ResponsesFailClosed() {
        server.enqueue(
            privateJsonResponse(
                200,
                "{\"request_id\":\"$REQUEST_ID\",\"reply\":\"${"x".repeat(65 * 1_024)}\"}",
            ),
        )
        assertThrows(AssistantApiException.InvalidResponse::class.java) {
            runBlocking { transport.turn(configuration(), request()) }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "no-store")
                .addHeader("Pragma", "no-cache")
                .body(Buffer().write(byteArrayOf(0xC3.toByte(), 0x28)))
                .build(),
        )
        assertThrows(AssistantApiException.InvalidResponse::class.java) {
            runBlocking { transport.turn(configuration(), request()) }
        }
    }

    @Test
    fun statusesMapWithoutLeakingBearerAndRedirectsAreNotFollowed() {
        val cases = listOf(
            401 to AssistantApiException.Authentication::class.java,
            403 to AssistantApiException.Forbidden::class.java,
            422 to AssistantApiException.Validation::class.java,
            429 to AssistantApiException.RateLimited::class.java,
            503 to AssistantApiException.Unavailable::class.java,
            418 to AssistantApiException.Http::class.java,
        )
        cases.forEach { (status, type) ->
            server.enqueue(privateJsonResponse(status, "{}"))
            val error = assertThrows(type) {
                runBlocking { transport.turn(configuration(), request()) }
            }
            assertFalse(error.toString().contains("unit-test-secret"))
        }

        server.enqueue(
            MockResponse.Builder()
                .code(302)
                .addHeader("Location", server.url("/credential-capture"))
                .addHeader("Cache-Control", "no-store")
                .addHeader("Pragma", "no-cache")
                .body("{}")
                .build(),
        )
        assertThrows(AssistantApiException.Http::class.java) {
            runBlocking { transport.turn(configuration(), request()) }
        }
        assertEquals(cases.size + 1, server.requestCount)
        assertFalse(OkHttpAssistantTransport.defaultClient().followRedirects)
        assertFalse(OkHttpAssistantTransport.defaultClient().followSslRedirects)
        assertFalse(OkHttpAssistantTransport.defaultClient().retryOnConnectionFailure)
    }

    @Test
    fun requestMessageHistoryAndContextBoundsFailBeforeNetworkIo() {
        val oversizedHistory = List(5) { index ->
            AssistantHistoryMessage(
                if (index % 2 == 0) AssistantRole.USER else AssistantRole.ASSISTANT,
                "h".repeat(8 * 1_024),
            )
        }
        val invalid = listOf(
            request(message = " "),
            request(message = "unsafe\u202Edisplay"),
            request(message = "unpaired \uD800 surrogate"),
            request(message = "m".repeat(8 * 1_024 + 1)),
            request(history = List(21) { AssistantHistoryMessage(AssistantRole.USER, "history") }),
            request(history = oversizedHistory),
            request(
                history = listOf(
                    AssistantHistoryMessage(AssistantRole.USER, "unsafe\u2066history"),
                ),
            ),
            request().copy(context = request().context.copy(schema = "dayweave.assistant-context/2")),
        )
        invalid.forEach { value ->
            assertThrows(IllegalArgumentException::class.java) {
                runBlocking { transport.turn(configuration(), value) }
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun cancellationClosesAnInFlightResponseRead() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "private, no-store, max-age=0")
                .addHeader("Pragma", "no-cache")
                .body(responseJson())
                .bodyDelay(30, TimeUnit.SECONDS)
                .build(),
        )
        val deferred = async(Dispatchers.IO) {
            transport.turn(configuration(), request())
        }
        server.takeRequest(2, TimeUnit.SECONDS)

        deferred.cancel()
        withTimeout(2_000) { deferred.cancelAndJoin() }

        assertTrue(deferred.isCancelled)
    }

    private fun request(
        message: String = "Plan my day",
        history: List<AssistantHistoryMessage> = emptyList(),
    ) = AssistantTurnRequest(
        requestId = REQUEST_ID,
        message = message,
        history = history,
        context = AssistantContextProjector.project(
            DayWeaveUiState(schedulePlanningZoneId = "UTC"),
            Instant.parse("2026-09-03T07:00:00Z"),
        ),
    )

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun responseJson(
        requestId: String = REQUEST_ID,
        reply: String = "A bounded answer.",
        extra: String = "",
    ): String =
        """{"request_id":"$requestId","reply":"$reply","model":"test-model","generated_at":"2026-09-03T07:00:01Z"$extra}"""

    private fun privateJsonResponse(code: Int, body: String): MockResponse = MockResponse.Builder()
        .code(code)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "private, no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .body(body)
        .build()

    private companion object {
        const val REQUEST_ID = "11111111-1111-4111-8111-111111111111"
        const val OTHER_REQUEST_ID = "22222222-2222-4222-8222-222222222222"
    }
}
