package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class OkHttpExecutionTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpExecutionTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpExecutionTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun snapshotUsesBoundOriginBearerAndStrictLeaseShape() = runBlocking {
        server.enqueue(jsonResponse("""{"execution":{"revision":4,"active_session":${sessionJson()}}}"""))

        val snapshot = transport.snapshot(configuration())

        assertEquals(4L, snapshot.revision)
        assertEquals(SESSION_ID, snapshot.activeSession?.id)
        assertEquals("paused", snapshot.activeSession?.status)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/execution", request.url.encodedPath)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun commandReplaysTheExactDurableBodyAndIdempotencyKey() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"mutation":{"revision":4,"active_session":${sessionJson()},"changed_session":${sessionJson()},"replayed":true}}""",
            ),
        )
        val body = """{"expected_revision":3,"command":{"type":"pause","session_id":"$SESSION_ID","duration_seconds":600}}"""

        val mutation = transport.command(configuration(), IDEMPOTENCY_KEY, body)

        assertTrueCompat(mutation.replayed)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("/tenant/v1/execution/commands", request.url.encodedPath)
        assertEquals(IDEMPOTENCY_KEY, request.headers["Idempotency-Key"])
        assertEquals(body, requireNotNull(request.body).utf8())
    }

    @Test
    fun invalidIdempotencyKeysFailBeforeNetworkIo() {
        listOf("short", "unsafe/key", "non-ascii-éé").forEach { key ->
            assertThrows(ExecutionApiException.InvalidResponse::class.java) {
                runBlocking { transport.command(configuration(), key, "{}") }
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun historyUsesExactOffsetAndRequiresContinuationField() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"sessions":[${sessionJson()}],"next_offset":300}""",
            ),
        )

        val page = transport.history(configuration(), limit = 100, offset = 200)

        assertEquals(1, page.sessions.size)
        assertEquals(300L, page.nextOffset)
        val request = server.takeRequest()
        assertEquals("100", request.url.queryParameter("limit"))
        assertEquals("200", request.url.queryParameter("offset"))

        server.enqueue(jsonResponse("""{"sessions":[]}"""))
        assertThrows(ExecutionApiException.InvalidResponse::class.java) {
            runBlocking { transport.history(configuration(), limit = 100, offset = 0) }
        }
        Unit
    }

    @Test
    fun executionErrorsAreTypedAndNeverExposeBearer() {
        server.enqueue(MockResponse.Builder().code(401).body("{}").build())
        val auth = assertThrows(ExecutionApiException.Authentication::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
        assertFalse(auth.toString().contains("unit-test-secret"))

        server.enqueue(MockResponse.Builder().code(404).body("{}").build())
        assertThrows(ExecutionApiException.NotFound::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
        server.enqueue(MockResponse.Builder().code(409).body("{}").build())
        assertThrows(ExecutionApiException.Conflict::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
        server.enqueue(MockResponse.Builder().code(422).body("{}").build())
        assertThrows(ExecutionApiException.Validation::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
    }

    @Test
    fun unknownOrOversizedExecutionResponseFailsClosed() {
        server.enqueue(
            jsonResponse("""{"execution":{"revision":0,"active_session":null,"future":true}}"""),
        )
        assertThrows(ExecutionApiException.InvalidResponse::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }

        server.enqueue(jsonResponse("{" + "x".repeat(300_000) + "}"))
        assertThrows(ExecutionApiException.InvalidResponse::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
    }

    @Test
    fun omittedRequiredNullableExecutionFieldsFailClosed() {
        server.enqueue(jsonResponse("""{"execution":{"revision":0}}"""))
        assertThrows(ExecutionApiException.InvalidResponse::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }

        val missingOccurrence = sessionJson().replace("\n  \"occurrence_id\":null,", "")
        server.enqueue(
            jsonResponse(
                """{"execution":{"revision":4,"active_session":$missingOccurrence}}""",
            ),
        )
        assertThrows(ExecutionApiException.InvalidResponse::class.java) {
            runBlocking { transport.snapshot(configuration()) }
        }
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

    private fun sessionJson(): String = """
        {
          "id":"$SESSION_ID",
          "item_id":"11111111-1111-4111-8111-111111111111",
          "item_revision":7,
          "occurrence_id":null,
          "session_index":0,
          "planned_block_id":"22222222-2222-4222-8222-222222222222",
          "source_device_id":"33333333-3333-4333-8333-333333333333",
          "status":"paused",
          "revision":2,
          "accumulated_seconds":120,
          "actual_seconds":null,
          "started_at":"2026-09-01T06:45:00Z",
          "running_since":null,
          "paused_at":"2026-09-01T06:50:00Z",
          "pause_until":"2026-09-01T07:10:00Z",
          "pause_reason":null,
          "ended_at":null,
          "created_at":"2026-09-01T06:45:00Z",
          "updated_at":"2026-09-01T06:50:00Z"
        }
    """.trimIndent()

    private fun assertTrueCompat(value: Boolean) = org.junit.Assert.assertTrue(value)

    private companion object {
        const val SESSION_ID = "44444444-4444-4444-8444-444444444444"
        const val IDEMPOTENCY_KEY = "55555555-5555-4555-8555-555555555555"
    }
}
