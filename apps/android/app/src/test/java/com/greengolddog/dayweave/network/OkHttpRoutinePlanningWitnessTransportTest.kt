package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.*
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.SocketEffect
import okhttp3.Call
import okhttp3.EventListener
import okio.Buffer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

class OkHttpRoutinePlanningWitnessTransportTest {
    private lateinit var server: MockWebServer
    @Before fun start() { server = MockWebServer(); server.start() }
    @After fun close() { server.close() }

    @Test fun privatePostPreservesExactRequestAndNeverUsesIdempotencyOrQuerySelectors(): Unit = runBlocking {
        server.enqueue(response(ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse())))
        val request = planningTestRequest()
        val result = OkHttpRoutinePlanningWitnessTransport().capture(configuration(), request)
        assertEquals(planningTestResponse(), result)
        val sent = server.takeRequest()
        assertEquals("POST", sent.method)
        assertEquals("/synthetic/v1/routine-occurrences/planning-witness", sent.url.encodedPath)
        assertNull(sent.url.query)
        assertEquals("Bearer synthetic-planning-test", sent.headers["Authorization"])
        assertEquals("no-store, max-age=0", sent.headers["Cache-Control"])
        assertNull(sent.headers["Idempotency-Key"])
        assertArrayEquals(encodeRoutinePlanningWitnessRequest(request), requireNotNull(sent.body).toByteArray())
    }

    @Test fun typedRemoteRequiredResponseHasNoWitnessAndKnownErrorPairsStayReadOnly(): Unit = runBlocking {
        for (reason in RoutinePlanningRemoteReason.entries) {
            val expected = RoutinePlanningWitnessResponse(1, RoutinePlanningWitnessResult.RemoteRequired(reason))
            server.enqueue(response(ROUTINE_PLANNING_JSON.encodeToString(expected)))
            assertEquals(expected, OkHttpRoutinePlanningWitnessTransport().capture(configuration(), planningTestRequest()))
        }
        for (code in RoutinePlanningWitnessFailureCode.entries) {
            val body = """{"error":{"code":"${code.wire}","message":"Synthetic private reason"}}"""
            server.enqueue(response(body, code.status))
            val rejected = assertThrows(RoutinePlanningWitnessApiException.Rejected::class.java) {
                runBlocking { OkHttpRoutinePlanningWitnessTransport().capture(configuration(), planningTestRequest()) }
            }
            assertEquals(code, rejected.code); assertNull(rejected.cause)
            assertFalse(rejected.message.orEmpty().contains("Synthetic"))
            server.enqueue(response(body, 502))
            uncertain()
        }
    }

    @Test fun allReplayHeadersAreRejectedOnSuccessAndErrorEvenFalseOrEmpty() {
        for (status in listOf(200, 409)) for (value in listOf("true", "false", "", "false,false")) {
            val body = if (status == 200) ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse())
                else """{"error":{"code":"routine_planning_cursor_changed","message":"Synthetic"}}"""
            server.enqueue(responseBuilder(body, status).addHeader("Idempotency-Replayed", value).build())
            uncertain()
        }
    }

    @Test fun immutableInputSourceAndCursorMismatchCannotBecomeEvidence() {
        val witness = planningTestWitness()
        for (changed in listOf(witness.copy(terminalCursor = "other"), witness.copy(sourceItemRevisions = emptyMap()),
            witness.copy(schedule = witness.schedule.copy(asOf = "2026-09-10T12:00:00Z")))) {
            server.enqueue(response(ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse(changed))))
            uncertain()
        }
    }

    @Test fun responseRequiresPrivacyMediaUtf8AndIndependentSixteenMiBBodyBound() {
        val valid = ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse())
        server.enqueue(MockResponse.Builder().body(valid).addHeader("Content-Type", "application/json").build()); uncertain()
        server.enqueue(MockResponse.Builder().body(valid).addHeader("Cache-Control", "no-store, max-age=0").addHeader("Content-Type", "text/plain").build()); uncertain()
        server.enqueue(MockResponse.Builder().body(Buffer().write(byteArrayOf(0xc3.toByte(), 0x28))).addHeader("Cache-Control", "no-store, max-age=0").addHeader("Content-Type", "application/json").build()); uncertain()
        server.enqueue(response(" ".repeat(MAX_ROUTINE_PLANNING_WITNESS_BYTES + 1))); uncertain()
        server.enqueue(response(valid + " ".repeat(MAX_ROUTINE_OCCURRENCE_BYTES)))
        assertEquals(planningTestResponse(), runBlocking { OkHttpRoutinePlanningWitnessTransport().capture(configuration(), planningTestRequest()) })
        assertEquals(8 * 1024 * 1024, MAX_ROUTINE_OCCURRENCE_BYTES)
        assertEquals(16 * 1024 * 1024, MAX_ROUTINE_PLANNING_WITNESS_BYTES)
    }

    @Test fun cancellationClosesStalledBodyAndCannotAdmitSuccessOrNamedFailure(): Unit = runBlocking {
        for (status in listOf(200, 409)) {
            val bodyStarted = CompletableDeferred<Call>()
            val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder().eventListener(object : EventListener() {
                override fun responseBodyStart(call: Call) { bodyStarted.complete(call) }
            }).build()
            val body = if (status == 200) ROUTINE_PLANNING_JSON.encodeToString(planningTestResponse())
                else """{"error":{"code":"routine_planning_cursor_changed","message":"Synthetic private text"}}"""
            server.enqueue(responseBuilder(body, status).onResponseBody(SocketEffect.Stall).build())
            val admitted = AtomicBoolean(false)
            val attempt = async {
                try { OkHttpRoutinePlanningWitnessTransport(client).capture(configuration(), planningTestRequest()); admitted.set(true) }
                catch (error: RoutinePlanningWitnessApiException.Rejected) { admitted.set(true); throw error }
            }
            val call = withTimeout(2_000) { bodyStarted.await() }
            attempt.cancel(); withTimeout(2_000) { attempt.join() }
            assertTrue(call.isCanceled()); assertTrue(attempt.isCancelled); assertFalse(admitted.get())
        }
    }

    private fun uncertain() {
        val error = assertThrows(RoutinePlanningWitnessApiException.Uncertain::class.java) {
            runBlocking { OkHttpRoutinePlanningWitnessTransport().capture(configuration(), planningTestRequest()) }
        }
        // Coroutine stack recovery may chain another copy of the same fixed
        // public diagnostic. Never admit a parser/HTTP cause or private text.
        var current: Throwable? = error
        repeat(8) {
            current?.let { cause ->
                assertTrue(cause is RoutinePlanningWitnessApiException.Uncertain)
                assertEquals("Planning response could not be verified", cause.message)
                current = cause.cause
            }
        }
        assertNull(current)
    }
    private fun configuration() = AuthenticatedApiConfiguration.createForLoopbackTest(server.url("/synthetic/").toString(), "synthetic-planning-test")
    private fun responseBuilder(body: String, status: Int = 200) = MockResponse.Builder().code(status).body(body)
        .addHeader("Content-Type", "application/json").addHeader("Cache-Control", "no-store, max-age=0")
    private fun response(body: String, status: Int = 200) = responseBuilder(body, status).build()
}
