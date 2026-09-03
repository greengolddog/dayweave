package com.greengolddog.dayweave.network

import java.util.Base64
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

class OkHttpGoogleSchedulePublicationTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpGoogleCalendarOutboundTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpGoogleCalendarOutboundTransport()
    }

    @After
    fun tearDown() = server.close()

    @Test
    fun previewApproveEnqueueAndStatusUseExactScheduleContracts() = runBlocking {
        server.enqueue(jsonResponse(200, previewJson()))
        server.enqueue(jsonResponse(200, approvalJson()))
        server.enqueue(jsonResponse(202, acceptedJson()))
        server.enqueue(jsonResponse(200, statusJson()))

        val configuration = AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )
        val preview = transport.previewSchedulePublication(
            configuration,
            ACCOUNT_ID,
            COLLECTION_ID,
            SCHEDULE_REVISION_ID,
        )
        val approval = transport.approveSchedulePublication(
            configuration,
            ACCOUNT_ID,
            PREVIEW_ID,
            PREVIEW_HASH,
        )
        val accepted = transport.enqueueSchedulePublication(
            configuration,
            ACCOUNT_ID,
            PREVIEW_ID,
            COLLECTION_ID,
            SCHEDULE_REVISION_ID,
            CAPABILITY,
        )
        val status = transport.schedulePublicationStatus(
            configuration,
            ACCOUNT_ID,
            PUBLICATION_ID,
        )

        assertEquals(1, preview.createCount)
        assertEquals(CAPABILITY, approval.approvalCapability)
        assertEquals(PUBLICATION_ID, accepted.publicationId)
        assertEquals(ScheduleGooglePublicationState.PUBLISHED, status.state)

        val previewRequest = server.takeRequest()
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/" +
                "schedule-publications/previews",
            previewRequest.url.encodedPath,
        )
        assertEquals(
            mapOf(
                "collection_id" to COLLECTION_ID,
                "expected_schedule_revision_id" to SCHEDULE_REVISION_ID,
            ),
            stringBody(previewRequest.body?.utf8()),
        )

        val approvalRequest = server.takeRequest()
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/" +
                "schedule-publications/previews/$PREVIEW_ID/approve",
            approvalRequest.url.encodedPath,
        )
        assertEquals(
            mapOf("expected_preview_hash" to PREVIEW_HASH),
            stringBody(approvalRequest.body?.utf8()),
        )

        val enqueueRequest = server.takeRequest()
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/schedule-publications",
            enqueueRequest.url.encodedPath,
        )
        assertNull(enqueueRequest.headers["Idempotency-Key"])
        assertEquals(CAPABILITY, stringBody(enqueueRequest.body?.utf8())["approval_capability"])

        val statusRequest = server.takeRequest()
        assertEquals("GET", statusRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/" +
                "schedule-publications/$PUBLICATION_ID",
            statusRequest.url.encodedPath,
        )
        listOf(previewRequest, approvalRequest, enqueueRequest, statusRequest).forEach {
            assertEquals("Bearer unit-test-secret", it.headers["Authorization"])
            assertEquals("no-store", it.headers["Cache-Control"])
        }
        assertFalse(approval.toString().contains(CAPABILITY))
        assertFalse(preview.toString().contains("Focus block"))
    }

    private fun jsonResponse(status: Int, body: String) = MockResponse.Builder()
        .code(status)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store")
        .body(body)
        .build()

    private fun stringBody(value: String?): Map<String, String> =
        Json.parseToJsonElement(requireNotNull(value)).jsonObject.mapValues { (_, element) ->
            element.jsonPrimitive.content
        }

    private fun previewJson(): String =
        "{\"id\":\"$PREVIEW_ID\",\"account_id\":\"$ACCOUNT_ID\"," +
            "\"collection_id\":\"$COLLECTION_ID\",\"collection_revision\":7," +
            "\"collection_display_name\":\"Planning\"," +
            "\"schedule_revision_id\":\"$SCHEDULE_REVISION_ID\"," +
            "\"schedule_revision_number\":11,\"preview_hash\":\"$PREVIEW_HASH\"," +
            "\"create_count\":1,\"update_count\":0,\"delete_count\":0," +
            "\"noop_count\":0,\"changes\":[{\"ordinal\":0," +
            "\"slot_id\":\"$SLOT_ID\",\"source_block_id\":\"$SOURCE_BLOCK_ID\"," +
            "\"operation\":\"create\",\"provider_resource_id\":null," +
            "\"provider_etag\":null,\"summary\":\"Focus block\"," +
            "\"starts_at\":\"2026-09-03T13:00:00Z\"," +
            "\"ends_at\":\"2026-09-03T14:00:00Z\"}]," +
            "\"expires_at\":\"2026-09-03T12:20:00Z\"}"

    private fun approvalJson(): String =
        "{\"preview_id\":\"$PREVIEW_ID\",\"approval_capability\":\"$CAPABILITY\"," +
            "\"expires_at\":\"2026-09-03T12:15:00Z\"}"

    private fun acceptedJson(): String =
        "{\"publication_id\":\"$PUBLICATION_ID\",\"replayed\":false}"

    private fun statusJson(): String =
        "{\"publication_id\":\"$PUBLICATION_ID\",\"account_id\":\"$ACCOUNT_ID\"," +
            "\"collection_id\":\"$COLLECTION_ID\"," +
            "\"schedule_revision_id\":\"$SCHEDULE_REVISION_ID\"," +
            "\"state\":\"published\",\"total_count\":1,\"pending_count\":0," +
            "\"delivering_count\":0,\"published_count\":1,\"conflicted_count\":0," +
            "\"failed_count\":0,\"superseded_count\":0," +
            "\"created_at\":\"2026-09-03T12:10:00Z\"," +
            "\"completed_at\":\"2026-09-03T12:11:00Z\",\"last_error_code\":null}"

    private companion object {
        const val ACCOUNT_ID = "22222222-2222-4222-8222-222222222222"
        const val COLLECTION_ID = "33333333-3333-4333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PREVIEW_ID = "55555555-5555-4555-8555-555555555555"
        const val SLOT_ID = "66666666-6666-4666-8666-666666666666"
        const val SOURCE_BLOCK_ID = "77777777-7777-4777-8777-777777777777"
        const val PUBLICATION_ID = "88888888-8888-4888-8888-888888888888"
        val PREVIEW_HASH = "a".repeat(64)
        val CAPABILITY = "dw_gsa1_" + Base64.getUrlEncoder().withoutPadding()
            .encodeToString(ByteArray(32) { (it + 1).toByte() })
    }
}
