package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class OkHttpCanonicalPlannerTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpCanonicalPlannerTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpCanonicalPlannerTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun deltaUsesOpaqueCursorPathPrefixBearerAndTypedItem() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"changes":[{"type":"upsert","item":${itemJson()}}],"next_cursor":"opaque+/=","has_more":true}""",
            ),
        )

        val page = transport.itemDelta(configuration(), "previous+/=")

        assertEquals("opaque+/=", page.nextCursor)
        assertEquals("Compose Android timeline", page.changes.single().item?.title)
        assertEquals(7L, page.changes.single().item?.revision)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/items/delta", request.url.encodedPath)
        assertEquals("limit=50&cursor=previous%2B%2F%3D", request.url.encodedQuery)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun previewSendsStrictRfc3339RequestAndDecodesSchedule() = runBlocking {
        server.enqueue(jsonResponse(previewJson()))
        val request = SchedulePreviewRequest(
            asOf = "2026-09-01T07:00:00Z",
            horizonStart = "2026-08-31T22:00:00Z",
            horizonEnd = "2026-09-01T22:00:00Z",
            timezoneName = "Europe/Madrid",
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = "2026-09-01T05:00:00Z",
                    end = "2026-09-01T20:00:00Z",
                ),
            ),
        )

        val preview = transport.preview(configuration(), request)

        assertEquals("sha256:${"a".repeat(64)}", preview.inputDigest)
        assertEquals(BLOCK_ID, preview.plan.blocks.single().id)
        assertEquals(60L, preview.plan.score.scheduledMinutes)
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/schedule/preview", recorded.url.encodedPath)
        assertEquals("application/json; charset=utf-8", recorded.headers["Content-Type"])
        assertEquals("Bearer unit-test-secret", recorded.headers["Authorization"])
        val body = Json.parseToJsonElement(requireNotNull(recorded.body).utf8()) as JsonObject
        assertEquals("Europe/Madrid", body["timezone_name"]?.jsonPrimitive?.content)
        assertEquals(JsonObject(emptyMap()), body["recurrence_context"])
    }

    @Test
    fun authenticationAndRedirectFailuresAreTypedWithoutTokenLeakage() {
        server.enqueue(MockResponse.Builder().code(401).body("{}").build())
        val authentication = assertThrows(PlannerApiException.Authentication::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
        }
        assertFalse(authentication.toString().contains("unit-test-secret"))

        server.enqueue(
            MockResponse.Builder()
                .code(302)
                .addHeader("Location", "https://redirected.example.test/v1/items/delta")
                .build(),
        )
        val redirect = assertThrows(PlannerApiException.Http::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
        }
        assertEquals(302, redirect.statusCode)
        assertEquals(2, server.requestCount)
    }

    @Test
    fun replacementUsesPutRevisionGuardAndIdempotencyKey() = runBlocking {
        val replaced = itemJson()
            .replace("\"status\":\"planned\"", "\"status\":\"in_progress\"")
            .replace("\"revision\":7", "\"revision\":8")
            .replace(
                "\"updated_at\":\"2026-08-29T10:00:00Z\"",
                "\"updated_at\":\"2026-09-01T07:01:00Z\"",
            )
        server.enqueue(jsonResponse("""{"item":$replaced}"""))
        val replacement = CanonicalItemReplacement(
            kind = "task",
            status = "in_progress",
            title = "Compose Android timeline",
            notes = "Server-owned canonical task",
            timezoneName = "Europe/Madrid",
            durationSeconds = 3_600,
            deadlineAt = "2026-09-01T12:00:00Z",
            flexibleConstraints = buildJsonObject { put("energy", "deep") },
            splitPolicy = buildJsonObject { put("type", "indivisible") },
            importance = 80,
            urgency = 60,
            siblingOrder = 0,
        )

        val item = transport.replaceItem(
            configuration(),
            TASK_ID,
            "33333333-3333-4333-8333-333333333333",
            ReplaceCanonicalItemRequest(expectedRevision = 7, item = replacement),
        )

        assertEquals(8L, item.revision)
        assertEquals("in_progress", item.status)
        val recorded = server.takeRequest()
        assertEquals("PUT", recorded.method)
        assertEquals("/tenant/v1/items/$TASK_ID", recorded.url.encodedPath)
        assertEquals(
            "33333333-3333-4333-8333-333333333333",
            recorded.headers["Idempotency-Key"],
        )
        val body = Json.parseToJsonElement(requireNotNull(recorded.body).utf8()) as JsonObject
        assertEquals(7L, body["expected_revision"]?.jsonPrimitive?.content?.toLong())
        assertEquals(
            "in_progress",
            (body["item"] as JsonObject)["status"]?.jsonPrimitive?.content,
        )
    }

    @Test
    fun unknownResponseFieldFailsClosed() {
        server.enqueue(
            jsonResponse(
                """{"changes":[],"next_cursor":"cursor","has_more":false,"future":true}""",
            ),
        )

        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
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

    private fun itemJson(): String = """
        {
          "id":"$TASK_ID",
          "kind":"task",
          "status":"planned",
          "title":"Compose Android timeline",
          "notes":"Server-owned canonical task",
          "timezone_name":"Europe/Madrid",
          "duration_seconds":3600,
          "deadline_at":"2026-09-01T12:00:00Z",
          "earliest_start_at":null,
          "recurrence":null,
          "flexible_constraints":{"energy":"deep"},
          "split_policy":{"type":"indivisible"},
          "importance":80,
          "urgency":60,
          "parent_id":null,
          "sibling_order":0,
          "is_executable":true,
          "revision":7,
          "created_at":"2026-08-29T09:00:00Z",
          "updated_at":"2026-08-29T10:00:00Z",
          "completed_at":null,
          "deleted_at":null
        }
    """.trimIndent()

    private fun previewJson(): String = """
        {
          "input_digest":"sha256:${"a".repeat(64)}",
          "source_item_count":1,
          "source_item_revisions":{"$TASK_ID":7},
          "accepted_item_count":1,
          "rejected_items":[],
          "ignored_previous_assignments":[],
          "plan":{
            "as_of":"2026-09-01T07:00:00Z",
            "horizon_start":"2026-08-31T22:00:00Z",
            "horizon_end":"2026-09-01T22:00:00Z",
            "blocks":[{
              "id":"$BLOCK_ID",
              "item_id":"$TASK_ID",
              "occurrence_id":null,
              "external_block_id":null,
              "title":"Compose Android timeline",
              "start":"2026-09-01T09:00:00+02:00",
              "end":"2026-09-01T10:00:00+02:00",
              "session_index":0,
              "kind":"planned",
              "explanations":[]
            }],
            "unscheduled":[],
            "decisions":[],
            "violations":[],
            "score":{
              "scheduled_minutes":60,
              "unscheduled_minutes":0,
              "soft_penalty":0,
              "moved_minutes":0
            },
            "occurrences":[]
          }
        }
    """.trimIndent()

    private companion object {
        const val TASK_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
    }
}
