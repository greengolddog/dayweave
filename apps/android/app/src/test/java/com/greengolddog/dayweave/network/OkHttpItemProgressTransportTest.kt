package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

class OkHttpItemProgressTransportTest {
    private lateinit var server: MockWebServer
    private val transport = OkHttpItemProgressTransport()

    @Before fun start() { server = MockWebServer(); server.start() }
    @After fun close() { server.close() }

    @Test fun admittedGetRetainsExplicitEmptyBaselineAndBoundRequestHeaders() = runBlocking {
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(progressTestSnapshot())))
        assertEquals(progressTestSnapshot(), transport.get(configuration(), PROGRESS_ITEM))
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/items/$PROGRESS_ITEM/progress", request.url.encodedPath)
        assertEquals("Bearer synthetic-progress-test", request.headers["Authorization"])
        assertEquals("no-store, max-age=0", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
    }

    @Test fun putSendsExactReviewedWhitespaceAndAcceptsBothReplayOutcomes() = runBlocking {
        val exact = " \n" + progressTestMutation().requestJson + "\n "
        for (replayed in listOf(false, true)) {
            server.enqueue(response(resultJson(replayed), listOf(replayed.toString())))
            assertEquals(replayed, transport.put(configuration(), PROGRESS_ITEM, exact).replayed)
            val request = server.takeRequest()
            assertEquals("PUT", request.method)
            assertEquals("/tenant/v1/items/$PROGRESS_ITEM/progress", request.url.encodedPath)
            assertEquals(exact, requireNotNull(request.body).utf8())
        }
    }

    @Test fun putRequiresExactlyOneMatchingLiteralReplayHeader() {
        for (headers in listOf(emptyList(), listOf("false", "false"), listOf("true"), listOf("False"), listOf("false,true"))) {
            server.enqueue(response(resultJson(false), headers))
            assertThrows(ItemProgressApiException.Uncertain::class.java) {
                runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
            }
        }
    }

    @Test fun getRejectsReplayHeaderMissingNoStoreAndWrongItemIdentity() {
        val snapshot = ITEM_PROGRESS_JSON.encodeToString(progressTestSnapshot())
        val cases = listOf(response(snapshot, listOf("false")),
            response(snapshot, cache = "no-cache"), response(snapshot, pragma = ""),
            response(snapshot.replace(PROGRESS_ITEM, PROGRESS_COMPONENT)))
        for (case in cases) {
            server.enqueue(case)
            assertThrows(ItemProgressApiException.Uncertain::class.java) {
                runBlocking { transport.get(configuration(), PROGRESS_ITEM) }
            }
        }
    }

    @Test fun rawDuplicateKeysFractionalIntegersAndOmittedFieldsNeverBecomeProof() {
        val snapshot = ITEM_PROGRESS_JSON.encodeToString(progressTestSnapshot())
        val cases = listOf(snapshot.replace("\"revision\":0", "\"revision\":0,\"rev\\u0069sion\":0"),
            snapshot.replace("\"revision\":0", "\"revision\":0.0"),
            snapshot.replace(",\"updated_at\":null", ""), snapshot.replace("\"schema_version\":1", "\"schema_version\":2"))
        for (body in cases) {
            server.enqueue(response(body))
            assertThrows(ItemProgressApiException.Uncertain::class.java) {
                runBlocking { transport.get(configuration(), PROGRESS_ITEM) }
            }
        }
    }

    @Test fun putRejectsMismatchedOperationItemRevisionsAndComponents() {
        val correct = resultJson(false)
        val cases = listOf(correct.replace(PROGRESS_OPERATION, PROGRESS_COMPONENT),
            correct.replace(PROGRESS_ITEM, PROGRESS_COMPONENT),
            correct.replace("\"item_revision\":7", "\"item_revision\":8"),
            correct.replace("\"revision\":1", "\"revision\":2"),
            correct.replace("\"basis_points\":4250", "\"basis_points\":4251"),
            correct.replace("\"replayed\":false", "\"replayed\":true,\"replayed\":false"))
        for (body in cases) {
            server.enqueue(response(body, listOf("false")))
            assertThrows(ItemProgressApiException.Uncertain::class.java) {
                runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
            }
        }
    }

    @Test fun onlyExactErrorCodeAndStatusPermitDefinitiveDisposition() {
        for (code in ItemProgressFailureCode.entries) {
            server.enqueue(response(errorJson(code.wire), status = code.status))
            val failure = assertThrows(ItemProgressApiException.Definitive::class.java) {
                runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
            }
            assertEquals(code, failure.code)
            for ((status, body) in listOf(code.status to errorJson("gateway_failure"),
                    502 to errorJson(code.wire), code.status to errorJson(code.wire).replace("\"code\":", "\"code\":\"gateway\",\"code\":"))) {
                server.enqueue(response(body, status = status))
                assertThrows(ItemProgressApiException.Uncertain::class.java) {
                    runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
                }
            }
        }
    }

    @Test fun authenticationAndUntrustedErrorHeadersRetainUncertainCustody() {
        for (status in listOf(401, 403)) {
            server.enqueue(response(errorJson("unauthenticated"), status = status))
            assertThrows(ItemProgressApiException.Authentication::class.java) {
                runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
            }
        }
        server.enqueue(response(errorJson("item_progress_item_missing"), status = 404, cache = "public"))
        assertThrows(ItemProgressApiException.Uncertain::class.java) {
            runBlocking { transport.put(configuration(), PROGRESS_ITEM, progressTestMutation().requestJson) }
        }
    }

    private fun configuration() = AuthenticatedApiConfiguration.createForLoopbackTest(
        server.url("/tenant/").toString(), "synthetic-progress-test")
    private fun resultJson(replayed: Boolean) = ITEM_PROGRESS_JSON.encodeToString(
        ItemProgressMutationResult(PROGRESS_OPERATION, replayed, progressTestSnapshot(1)))
    private fun errorJson(code: String) = """{"error":{"code":"$code","message":"Synthetic error"}}"""
    private fun response(body: String, replayHeaders: List<String> = emptyList(), status: Int = 200,
        cache: String = "no-store, max-age=0", pragma: String = "no-cache",
    ): MockResponse = MockResponse.Builder().code(status).addHeader("Content-Type", "application/json")
        .addHeader("Cache-Control", cache).addHeader("Pragma", pragma)
        .apply { replayHeaders.forEach { addHeader("Idempotency-Replayed", it) } }.body(body).build()
}
