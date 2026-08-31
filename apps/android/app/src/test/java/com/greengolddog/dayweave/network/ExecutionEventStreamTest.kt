package com.greengolddog.dayweave.network

import java.io.ByteArrayInputStream
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.SocketEffect
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class ExecutionInvalidationSseParserTest {
    @Test
    fun acceptsExactLfAndCrLfFramesAndIgnoresHeartbeats() {
        val revisions = mutableListOf<Long>()
        val body = buildString {
            append(": heartbeat\n\n")
            append(event(7, "\r\n"))
            append(event(9, "\n"))
        }

        ExecutionInvalidationSseParser(initialRevision = 5).parse(
            ByteArrayInputStream(body.toByteArray(Charsets.UTF_8)),
            revisions::add,
        )

        assertEquals(listOf(7L, 9L), revisions)
    }

    @Test
    fun rejectsMalformedUnsafeAndNonCanonicalFrames() {
        val invalidBodies = listOf(
            "id: 1\revent: execution-invalidation\n",
            "id: 1\u0000\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "future: 1\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "id: 1\nid: 1\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "id: 1\ndata: {\"revision\":1}\n\n",
            "id: +1\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "id: 01\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "id: 9223372036854775808\nevent: execution-invalidation\n" +
                "data: {\"revision\":9223372036854775808}\n\n",
            "id: 1\nevent: execution-invalidation\ndata: {\"revision\":2}\n\n",
            "id: 1\nevent: execution-invalidation\ndata: {\"revision\":1}\n",
            event(4, "\n"),
        )

        invalidBodies.forEach { body ->
            assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
                parseAsTransportProtocol(body.toByteArray(Charsets.UTF_8), initialRevision = 5)
            }
        }

        assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
            parseAsTransportProtocol(byteArrayOf('i'.code.toByte(), 'd'.code.toByte(), ':'.code.toByte(),
                ' '.code.toByte(), 0xC3.toByte(), '\n'.code.toByte(), '\n'.code.toByte()))
        }
    }

    @Test
    fun rejectsOversizedLinesFramesAndEventCounts() {
        val oversizedLine = "id: ${"1".repeat(4_097)}\n"
        val oversizedFrame = buildString {
            append("id: ").append("1".repeat(4_090)).append('\n')
            append("event: execution-invalidation\n")
            append("data: ").append("x".repeat(4_090)).append("\n\n")
        }
        listOf(oversizedLine, oversizedFrame).forEach { body ->
            assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
                parseAsTransportProtocol(body.toByteArray(Charsets.UTF_8))
            }
        }

        val tooManyEvents = buildString {
            for (revision in 1L..10_001L) append(event(revision, "\n"))
        }
        assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
            parseAsTransportProtocol(tooManyEvents.toByteArray(Charsets.UTF_8))
        }
    }

    private fun parseAsTransportProtocol(bytes: ByteArray, initialRevision: Long = 0) {
        try {
            ExecutionInvalidationSseParser(initialRevision).parse(
                ByteArrayInputStream(bytes),
            ) {}
        } catch (error: Exception) {
            throw ExecutionInvalidationStreamException.Protocol(error)
        }
    }

    private fun event(revision: Long, newline: String): String =
        "id: $revision${newline}event: execution-invalidation${newline}" +
            "data: {\"revision\":$revision}${newline}${newline}"
}

class OkHttpExecutionInvalidationStreamTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpExecutionInvalidationStreamTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpExecutionInvalidationStreamTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun sendsExactBoundHeadersAndCollectsOnlyRevisionHints() = runBlocking {
        server.enqueue(
            sseResponse(
                ": heartbeat\n\nid: 42\nevent: execution-invalidation\n" +
                    "data: {\"revision\":42}\n\n",
            ),
        )
        val revisions = mutableListOf<Long>()

        val end = transport.collect(configuration(), 41, revisions::add)

        assertEquals(ExecutionInvalidationStreamEnd.ENDED, end)
        assertEquals(listOf(42L), revisions)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/execution/stream", request.url.encodedPath)
        assertEquals("text/event-stream", request.headers["Accept"])
        assertEquals("41", request.headers["Last-Event-ID"])
        assertEquals("no-store, no-cache", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        assertEquals("identity", request.headers["Accept-Encoding"])
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun treatsOnly404AsUnsupportedAndSanitizesOtherErrors() = runBlocking {
        server.enqueue(MockResponse.Builder().code(404).body("not installed").build())
        assertEquals(
            ExecutionInvalidationStreamEnd.UNSUPPORTED,
            transport.collect(configuration(), 0) {},
        )

        server.enqueue(
            MockResponse.Builder()
                .code(503)
                .body("remote-sensitive-text-${"x".repeat(20_000)}")
                .build(),
        )
        val error = assertThrows(ExecutionInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), 0) {} }
        }
        assertEquals(503, error.statusCode)
        assertFalse(error.toString().contains("remote-sensitive-text"))
        assertFalse(error.toString().contains("unit-test-secret"))
    }

    @Test
    fun unsupportedFallbackDoesNotWaitForOrParseTheResponseBody() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(404)
                .body("attacker-controlled-body")
                .onResponseBody(SocketEffect.Stall)
                .build(),
        )

        val end = withTimeout(2_000) {
            transport.collect(configuration(), 0) {}
        }

        assertEquals(ExecutionInvalidationStreamEnd.UNSUPPORTED, end)
    }

    @Test
    fun rejectsWrongDuplicateOrEncodedMimeAndDoesNotFollowRedirects() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/plain")
                .body(": heartbeat\n\n")
                .build(),
        )
        assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), 0) {} }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/event-stream")
                .addHeader("Content-Type", "text/event-stream")
                .body(": heartbeat\n\n")
                .build(),
        )
        assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), 0) {} }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/event-stream")
                .addHeader("Content-Encoding", "gzip")
                .body(": heartbeat\n\n")
                .build(),
        )
        assertThrows(ExecutionInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), 0) {} }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(302)
                .addHeader("Location", server.url("/redirected"))
                .build(),
        )
        assertThrows(ExecutionInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), 0) {} }
        }
        assertEquals(4, server.requestCount)
    }

    @Test
    fun cancellationClosesAStalledResponseBodyPromptly() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/event-stream")
                .body(": heartbeat\n\n")
                .onResponseBody(SocketEffect.Stall)
                .build(),
        )
        val collection = async(Dispatchers.IO) {
            transport.collect(configuration(), 0) {}
        }
        server.takeRequest(2, TimeUnit.SECONDS)

        withTimeout(2_000) { collection.cancelAndJoin() }

        assertTrue(collection.isCancelled)
    }

    @Test
    fun dedicatedClientHasBoundedStreamingTimeoutsAndRedirectsDisabled() {
        val client = OkHttpExecutionInvalidationStreamTransport
            .defaultExecutionInvalidationClient()

        assertFalse(client.followRedirects)
        assertFalse(client.followSslRedirects)
        assertEquals(TimeUnit.MINUTES.toMillis(6).toInt(), client.callTimeoutMillis)
        assertEquals(TimeUnit.SECONDS.toMillis(45).toInt(), client.readTimeoutMillis)
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun sseResponse(body: String): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "text/event-stream")
        .body(body)
        .build()
}
