package com.greengolddog.dayweave.network

import java.io.ByteArrayInputStream
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class ScheduleInvalidationSseParserTest {
    @Test
    fun acceptsCanonicalUnsignedRevisionsAndHeartbeats() {
        val revisions = mutableListOf<ULong>()
        val body = buildString {
            append(": heartbeat\n\n")
            append(event(7uL, "\r\n"))
            append(event(ULong.MAX_VALUE, "\n"))
        }

        ScheduleInvalidationSseParser(5uL).parse(
            ByteArrayInputStream(body.toByteArray()),
            revisions::add,
        )

        assertEquals(listOf(7uL, ULong.MAX_VALUE), revisions)
    }

    @Test
    fun rejectsWrongEventDataOrderingAndNonCanonicalUnsignedIds() {
        listOf(
            "id: 6\nevent: execution-invalidation\ndata: {\"revision\":6}\n\n",
            "id: 6\nevent: schedule-invalidation\ndata: {\"revision\":7}\n\n",
            "id: 06\nevent: schedule-invalidation\ndata: {\"revision\":6}\n\n",
            "id: +6\nevent: schedule-invalidation\ndata: {\"revision\":6}\n\n",
            "id: 18446744073709551616\nevent: schedule-invalidation\n" +
                "data: {\"revision\":18446744073709551616}\n\n",
            event(5uL, "\n"),
            "id: 6\nevent: schedule-invalidation\ndata: {\"revision\":6}\n",
        ).forEach { body ->
            assertThrows(Exception::class.java) {
                ScheduleInvalidationSseParser(5uL).parse(
                    ByteArrayInputStream(body.toByteArray()),
                ) {}
            }
        }
    }

    private fun event(revision: ULong, newline: String): String =
        "id: $revision${newline}event: schedule-invalidation${newline}" +
            "data: {\"revision\":$revision}${newline}${newline}"
}

class OkHttpScheduleInvalidationStreamTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpScheduleInvalidationStreamTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpScheduleInvalidationStreamTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun sendsExactResumeRequestAndCollectsContentFreeHints() = runBlocking {
        server.enqueue(
            sseResponse(
                ": heartbeat\n\nid: 42\nevent: schedule-invalidation\n" +
                    "data: {\"revision\":42}\n\n",
            ),
        )
        val revisions = mutableListOf<ULong>()

        val end = transport.collect(configuration(), 41uL, revisions::add)

        assertEquals(ScheduleInvalidationStreamEnd.ENDED, end)
        assertEquals(listOf(42uL), revisions)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/schedule/stream", request.url.encodedPath)
        assertEquals("text/event-stream", request.headers["Accept"])
        assertEquals("41", request.headers["Last-Event-ID"])
        assertEquals("identity", request.headers["Accept-Encoding"])
        assertEquals("no-store, no-cache", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun exposesCursorAheadForAuthoritativeRepairAndOnly404AsUnsupported() = runBlocking {
        server.enqueue(MockResponse.Builder().code(409).body("private-details").build())
        val ahead = assertThrows(ScheduleInvalidationStreamException.Http::class.java) {
            runBlocking { transport.collect(configuration(), 99uL) {} }
        }
        assertEquals(409, ahead.statusCode)
        assertFalse(ahead.toString().contains("private-details"))

        server.enqueue(MockResponse.Builder().code(404).body("not installed").build())
        assertEquals(
            ScheduleInvalidationStreamEnd.UNSUPPORTED,
            transport.collect(configuration(), 0uL) {},
        )
    }

    @Test
    fun rejectsMissingPrivacyHeadersWrongMimeAndEncodedBodies() {
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/event-stream")
                .body(": heartbeat\n\n")
                .build(),
        )
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/plain")
                .addHeader("Cache-Control", "no-store, no-cache")
                .addHeader("Pragma", "no-cache")
                .addHeader("X-Accel-Buffering", "no")
                .body(": heartbeat\n\n")
                .build(),
        )
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/event-stream")
                .addHeader("Cache-Control", "no-store, no-cache")
                .addHeader("Pragma", "no-cache")
                .addHeader("X-Accel-Buffering", "no")
                .addHeader("Content-Encoding", "gzip")
                .body(": heartbeat\n\n")
                .build(),
        )
        repeat(3) {
            assertThrows(ScheduleInvalidationStreamException.Protocol::class.java) {
                runBlocking { transport.collect(configuration(), 0uL) {} }
            }
        }
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun sseResponse(body: String): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "text/event-stream; charset=utf-8")
        .addHeader("Cache-Control", "no-store, no-cache")
        .addHeader("Pragma", "no-cache")
        .addHeader("X-Accel-Buffering", "no")
        .body(body)
        .build()
}
