package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

class OkHttpItemCompletionTransportTest {
    private lateinit var server: MockWebServer
    private val transport = OkHttpItemCompletionTransport()
    @Before fun start() { server = MockWebServer(); server.start() }
    @After fun close() { server.close() }

    @Test fun realGetAndExactPutUseBoundRoutePrivacyHeadersAndBothReplayOutcomes() = runBlocking {
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(completionTestSnapshot())))
        assertEquals(completionTestSnapshot(), transport.get(configuration(), PROGRESS_ITEM))
        val get = server.takeRequest()
        assertEquals("/tenant/v1/items/$PROGRESS_ITEM/completion", get.url.encodedPath)
        assertEquals("Bearer synthetic-completion-test", get.headers["Authorization"])
        assertEquals("no-store, max-age=0", get.headers["Cache-Control"])
        assertEquals("no-cache", get.headers["Pragma"])
        for (replayed in listOf(false, true)) {
            server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(completionTestResult(replayed)), listOf(replayed.toString())))
            assertEquals(replayed, transport.put(configuration(), PROGRESS_ITEM, completionTestMutation().requestJson).replayed)
            assertEquals(completionTestMutation().requestJson, requireNotNull(server.takeRequest().body).utf8())
        }
    }

    @Test fun readProofRejectsDuplicateFractionMissingNullableIdentityAndPrivacyHeaderFailures() {
        val body = ITEM_PROGRESS_JSON.encodeToString(completionTestSnapshot())
        val cases = listOf(response(body, listOf("false")), response(body, cache = "public"),
            response(body.replace("\"revision\":0", "\"revision\":0,\"rev\\u0069sion\":0")),
            response(body.replace("\"revision\":0", "\"revision\":0e0")),
            response(body.replace(",\"updated_at\":null", "")), response(body.replace(PROGRESS_ITEM, PROGRESS_COMPONENT)))
        cases.forEach { server.enqueue(it); assertThrows(ItemCompletionApiException.Uncertain::class.java) {
            runBlocking { transport.get(configuration(), PROGRESS_ITEM) }
        } }
    }

    @Test fun receiptMustMatchBothCasRevisionsPolicyAndSingleReplayHeader() {
        val body = ITEM_PROGRESS_JSON.encodeToString(completionTestResult())
        val cases = listOf(response(body), response(body, listOf("false", "false")), response(body, listOf("true")),
            response(body.replace(PROGRESS_OPERATION, PROGRESS_COMPONENT), listOf("false")),
            response(body.replace("\"item_revision\":8", "\"item_revision\":9"), listOf("false")),
            response(body.replace("\"revision\":1", "\"revision\":2"), listOf("false")),
            response(body.replace("keep_open", "automatic"), listOf("false")))
        cases.forEach { server.enqueue(it); assertThrows(ItemCompletionApiException.Uncertain::class.java) {
            runBlocking { transport.put(configuration(), PROGRESS_ITEM, completionTestMutation().requestJson) }
        } }
    }

    @Test fun onlyExactTrustedErrorCodeAndStatusCanReleaseUncertainCustody() {
        for (code in ItemCompletionFailureCode.entries) {
            val body = """{"error":{"code":"${code.wire}","message":"Synthetic error"}}"""
            server.enqueue(response(body, status = code.status))
            assertEquals(code, assertThrows(ItemCompletionApiException.Definitive::class.java) {
                runBlocking { transport.put(configuration(), PROGRESS_ITEM, completionTestMutation().requestJson) }
            }.code)
            for (invalid in listOf(response(body, status = 502), response(body, status = code.status, cache = "public"),
                response(body.replace(code.wire, "gateway"), status = code.status))) {
                server.enqueue(invalid)
                assertThrows(ItemCompletionApiException.Uncertain::class.java) {
                    runBlocking { transport.put(configuration(), PROGRESS_ITEM, completionTestMutation().requestJson) }
                }
            }
        }
    }
    private fun configuration() = AuthenticatedApiConfiguration.createForLoopbackTest(server.url("/tenant/").toString(), "synthetic-completion-test")
    private fun response(body: String, replay: List<String> = emptyList(), status: Int = 200, cache: String = "no-store, max-age=0") =
        MockResponse.Builder().code(status).addHeader("Content-Type", "application/json").addHeader("Cache-Control", cache)
            .addHeader("Pragma", "no-cache").apply { replay.forEach { addHeader("Idempotency-Replayed", it) } }.body(body).build()
}
