package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class OkHttpHabitInvalidationStreamTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpHabitInvalidationStreamTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpHabitInvalidationStreamTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun acceptsOnlyContentFreeHabitFramesAndSendsExactBoundHeaders() = runBlocking {
        server.enqueue(sseResponse(": heartbeat\n\n${event("cursor_B")}"))
        val cursors = mutableListOf<String>()

        assertEquals(
            HabitInvalidationStreamEnd.ENDED,
            transport.collect(configuration(), "cursor_A", cursors::add),
        )
        assertEquals(listOf("cursor_B"), cursors)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/habits/stream", request.url.encodedPath)
        assertEquals("text/event-stream", request.headers["Accept"])
        assertEquals("cursor_A", request.headers["Last-Event-ID"])
        assertEquals("no-store, no-cache", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        assertEquals("identity", request.headers["Accept-Encoding"])
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])

        server.enqueue(sseResponse(": heartbeat\n\n"))
        transport.collect(configuration(), null) {}
        assertNull(server.takeRequest().headers["Last-Event-ID"])
    }

    @Test
    fun rejectsWrongEventUnsafeCursorAndNonEventStreamResponses() {
        assertThrows(HabitInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), "unsafe.cursor") {} }
        }
        assertEquals(0, server.requestCount)

        server.enqueue(
            sseResponse(
                "id: cursor_B\nevent: item-invalidation\n" +
                    "data: {\"cursor\":\"cursor_B\"}\n\n",
            ),
        )
        assertThrows(HabitInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }

        server.enqueue(
            MockResponse.Builder().code(200)
                .addHeader("Content-Type", "text/event-stream; charset=utf-8")
                .body(": heartbeat\n\n")
                .build(),
        )
        assertThrows(HabitInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }

        server.enqueue(
            MockResponse.Builder().code(200).addHeader("Content-Type", "application/json")
                .body("{}").build(),
        )
        assertThrows(HabitInvalidationStreamException.Protocol::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }
    }

    @Test
    fun only404IsUnsupportedAndRemoteErrorTextNeverEscapes() = runBlocking {
        server.enqueue(MockResponse.Builder().code(404).body("not installed").build())
        assertEquals(
            HabitInvalidationStreamEnd.UNSUPPORTED,
            transport.collect(configuration(), null) {},
        )

        server.enqueue(
            MockResponse.Builder().code(503)
                .body("remote-private-text-${"x".repeat(20_000)}").build(),
        )
        val error = assertThrows(HabitInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), null) {} }
        }
        assertEquals(503, error.statusCode)
        assertFalse(error.toString().contains("remote-private-text"))
        assertFalse(error.toString().contains("unit-test-secret"))
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun event(cursor: String): String =
        "id: $cursor\nevent: habit-invalidation\ndata: {\"cursor\":\"$cursor\"}\n\n"

    private fun sseResponse(body: String): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "text/event-stream; charset=utf-8")
        .addHeader("Cache-Control", "no-store, no-cache")
        .addHeader("Pragma", "no-cache")
        .addHeader("X-Accel-Buffering", "no")
        .body(body)
        .build()
}
