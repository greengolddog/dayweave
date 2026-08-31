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
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class CanonicalItemInvalidationSseParserTest {
    @Test
    fun acceptsExactLfAndCrLfFramesWithoutOrderingOpaqueCursors() {
        val cursors = mutableListOf<String>()
        val body = buildString {
            append(": heartbeat\n\n")
            append(event("opaque.Z:9~", "\r\n"))
            append(event("opaque_A", "\n"))
            append(event("opaque_A", "\n"))
        }

        CanonicalItemInvalidationSseParser().parse(
            ByteArrayInputStream(body.toByteArray(Charsets.UTF_8)),
            cursors::add,
        )

        assertEquals(listOf("opaque.Z:9~", "opaque_A", "opaque_A"), cursors)
    }

    @Test
    fun rejectsMalformedUnsafeMismatchedAndNonCanonicalFrames() {
        val invalidBodies = listOf(
            "id: abc\revent: item-invalidation\n",
            "id: abc\u0000\nevent: item-invalidation\ndata: {\"cursor\":\"abc\"}\n\n",
            "future: abc\nevent: item-invalidation\ndata: {\"cursor\":\"abc\"}\n\n",
            "id: abc\nid: abc\nevent: item-invalidation\ndata: {\"cursor\":\"abc\"}\n\n",
            "id: abc\ndata: {\"cursor\":\"abc\"}\n\n",
            "id: \nevent: item-invalidation\ndata: {\"cursor\":\"\"}\n\n",
            "id: unsafe cursor\nevent: item-invalidation\n" +
                "data: {\"cursor\":\"unsafe cursor\"}\n\n",
            "id: opaqueé\nevent: item-invalidation\ndata: {\"cursor\":\"opaqueé\"}\n\n",
            "id: opaque\\cursor\nevent: item-invalidation\n" +
                "data: {\"cursor\":\"opaque\\\\cursor\"}\n\n",
            "id: opaque\"cursor\nevent: item-invalidation\n" +
                "data: {\"cursor\":\"opaque\\\"cursor\"}\n\n",
            "id: opaque_A\nevent: future-event\ndata: {\"cursor\":\"opaque_A\"}\n\n",
            "id: opaque_A\nevent: item-invalidation\ndata: {\"cursor\":\"opaque_B\"}\n\n",
            "id: opaque_A\nevent: item-invalidation\n" +
                "data: {\"cursor\":\"opaque_A\",\"extra\":true}\n\n",
            "id: opaque_A\nevent: item-invalidation\ndata: { \"cursor\":\"opaque_A\"}\n\n",
            "id: ${"a".repeat(MAX_CANONICAL_ITEM_CURSOR_BYTES + 1)}\n" +
                "event: item-invalidation\ndata: {\"cursor\":\"x\"}\n\n",
            event("opaque_A", "\n").dropLast(1),
        )

        invalidBodies.forEach { body ->
            assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
                parseAsTransportProtocol(body.toByteArray(Charsets.UTF_8))
            }
        }

        assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
            parseAsTransportProtocol(
                byteArrayOf(
                    'i'.code.toByte(), 'd'.code.toByte(), ':'.code.toByte(),
                    ' '.code.toByte(), 0xC3.toByte(), '\n'.code.toByte(), '\n'.code.toByte(),
                ),
            )
        }
    }

    @Test
    fun rejectsOversizedLinesFramesAndEventCounts() {
        val oversizedLine = "future: ${"a".repeat(12 * 1024)}\n"
        val oversizedFrame = buildString {
            append("id: ").append("a".repeat(250)).append('\n')
            append("event: item-invalidation\n")
            append("data: ").append("x".repeat(12 * 1024 - 5)).append("\n\n")
        }
        listOf(oversizedLine, oversizedFrame).forEach { body ->
            assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
                parseAsTransportProtocol(body.toByteArray(Charsets.UTF_8))
            }
        }

        val tooManyEvents = buildString {
            repeat(10_001) { append(event("opaque_A", "\n")) }
        }
        assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
            parseAsTransportProtocol(tooManyEvents.toByteArray(Charsets.UTF_8))
        }
    }

    private fun parseAsTransportProtocol(bytes: ByteArray) {
        try {
            CanonicalItemInvalidationSseParser().parse(ByteArrayInputStream(bytes)) {}
        } catch (error: Exception) {
            throw CanonicalItemInvalidationStreamException.Protocol(error)
        }
    }

    private fun event(cursor: String, newline: String): String =
        "id: $cursor${newline}event: item-invalidation${newline}" +
            "data: {\"cursor\":\"$cursor\"}${newline}${newline}"
}

class OkHttpCanonicalItemInvalidationStreamTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpCanonicalItemInvalidationStreamTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpCanonicalItemInvalidationStreamTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun sendsExactBoundHeadersAndOmitsAbsentDurableCursor() = runBlocking {
        server.enqueue(sseResponse(event("opaque_2")))
        val cursors = mutableListOf<String>()

        assertEquals(
            CanonicalItemInvalidationStreamEnd.ENDED,
            transport.collect(configuration(), "opaque_1", cursors::add),
        )
        assertEquals(listOf("opaque_2"), cursors)
        val resumed = server.takeRequest()
        assertEquals("GET", resumed.method)
        assertEquals("/tenant/v1/items/stream", resumed.url.encodedPath)
        assertEquals("text/event-stream", resumed.headers["Accept"])
        assertEquals("opaque_1", resumed.headers["Last-Event-ID"])
        assertEquals("no-store, no-cache", resumed.headers["Cache-Control"])
        assertEquals("no-cache", resumed.headers["Pragma"])
        assertEquals("identity", resumed.headers["Accept-Encoding"])
        assertEquals("Bearer unit-test-secret", resumed.headers["Authorization"])

        server.enqueue(sseResponse(": heartbeat\n\n"))
        transport.collect(configuration(), null) {}
        assertNull(server.takeRequest().headers["Last-Event-ID"])
    }

    @Test
    fun treatsOnly404AsUnsupportedAndSanitizesOtherErrors() = runBlocking {
        server.enqueue(MockResponse.Builder().code(404).body("not installed").build())
        assertEquals(
            CanonicalItemInvalidationStreamEnd.UNSUPPORTED,
            transport.collect(configuration(), null) {},
        )

        server.enqueue(
            MockResponse.Builder().code(503)
                .body("remote-sensitive-text-${"x".repeat(20_000)}").build(),
        )
        val error = assertThrows(CanonicalItemInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }
        assertEquals(503, error.statusCode)
        assertFalse(error.toString().contains("remote-sensitive-text"))
        assertFalse(error.toString().contains("unit-test-secret"))
    }

    @Test
    fun rejectsUnsafeOrOversizedDurableCursorBeforeSendingARequest() {
        listOf("unsafe cursor", "opaqueé", "opaque\\cursor", "a".repeat(257)).forEach { cursor ->
            assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
                runBlocking { transport.collect(configuration(), cursor) {} }
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun unsupportedFallbackNeverConsumesAStalledBody() = runBlocking {
        server.enqueue(
            MockResponse.Builder().code(404).body("attacker-controlled-body")
                .onResponseBody(SocketEffect.Stall).build(),
        )

        val end = withTimeout(2_000) { transport.collect(configuration(), null) {} }

        assertEquals(CanonicalItemInvalidationStreamEnd.UNSUPPORTED, end)
    }

    @Test
    fun rejectsWrongDuplicateOrEncodedMimeAndDoesNotFollowRedirects() = runBlocking {
        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "text/plain")
                .body(": heartbeat\n\n").build(),
        )
        assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }

        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "text/event-stream")
                .addHeader("Content-Type", "text/event-stream")
                .body(": heartbeat\n\n").build(),
        )
        assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }

        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "text/event-stream")
                .addHeader("Content-Encoding", "gzip").body(": heartbeat\n\n").build(),
        )
        assertThrows(CanonicalItemInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }

        server.enqueue(
            MockResponse.Builder().code(302).addHeader("Location", server.url("/redirected"))
                .build(),
        )
        assertThrows(CanonicalItemInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }
        assertEquals(4, server.requestCount)
    }

    @Test
    fun cancellationInterruptsResponseEstablishmentAndBodyCollection() = runBlocking {
        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "text/event-stream")
                .onResponseStart(SocketEffect.Stall).build(),
        )
        val establishing = async(Dispatchers.IO) { transport.collect(configuration(), null) {} }
        server.takeRequest(2, TimeUnit.SECONDS)
        withTimeout(2_000) { establishing.cancelAndJoin() }
        assertTrue(establishing.isCancelled)

        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "text/event-stream")
                .body(": heartbeat\n\n").onResponseBody(SocketEffect.Stall).build(),
        )
        val collecting = async(Dispatchers.IO) { transport.collect(configuration(), null) {} }
        server.takeRequest(2, TimeUnit.SECONDS)
        withTimeout(2_000) { collecting.cancelAndJoin() }
        assertTrue(collecting.isCancelled)
    }

    @Test
    fun dedicatedClientHasBoundedTimeoutsAndRedirectsDisabled() {
        val client = OkHttpCanonicalItemInvalidationStreamTransport
            .defaultCanonicalItemInvalidationClient()

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

    private fun event(cursor: String): String =
        "id: $cursor\nevent: item-invalidation\ndata: {\"cursor\":\"$cursor\"}\n\n"

    private fun sseResponse(body: String): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "text/event-stream")
        .body(body)
        .build()
}
