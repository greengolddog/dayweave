package com.greengolddog.dayweave.network

import java.nio.charset.StandardCharsets
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
import org.junit.Assert.assertTrue
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
        assertTrue(page.changes.single().item?.isSensitive == true)
        assertEquals(7L, page.changes.single().item?.revision)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/items/delta", request.url.encodedPath)
        assertEquals("limit=50&cursor=previous%2B%2F%3D", request.url.encodedQuery)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun structuralNullKeysAreAtomicAndFutureValuesRemainExact() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"changes":[{"type":"upsert","item":${structuralItemJson("future_estimator_v2")}}],"next_cursor":"cursor","has_more":false}""",
            ),
        )

        val item = requireNotNull(transport.itemDelta(configuration(), null).changes.single().item)

        assertEquals("exact", item.durationKind?.wireValue)
        assertEquals("future_estimator_v2", item.durationSource?.wireValue)
        assertEquals("date_time", item.deadlineKind?.wireValue)
        assertEquals(null, item.deadlineDate)
        assertEquals(null, item.deadlineSoftWeight)
        assertEquals(null, item.blockedReasonKind)
        assertEquals(false, item.hasOwnEffort)
    }

    @Test
    fun partialStructuralWireShapeFailsClosedEvenWhenMissingFieldWouldBeNull() {
        val partial = structuralItemJson().replace("\"blocked_reason\":null,", "")
        server.enqueue(
            jsonResponse(
                """{"changes":[{"type":"upsert","item":$partial}],"next_cursor":"cursor","has_more":false}""",
            ),
        )

        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
        }
    }

    @Test
    fun completeStructuralWireShapeStillRejectsNullRequiredDiscriminator() {
        val invalid = structuralItemJson().replace(
            "\"duration_kind\":\"exact\"",
            "\"duration_kind\":null",
        )
        server.enqueue(
            jsonResponse(
                """{"changes":[{"type":"upsert","item":$invalid}],"next_cursor":"cursor","has_more":false}""",
            ),
        )

        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
        }
    }

    @Test
    fun foregroundDeltaProbeRequestsOnlyOneChangeWithoutChangingNormalDeltaLimit() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"changes":[],"next_cursor":"opaque_probe","has_more":false}""",
            ),
        )

        val page = transport.itemDeltaProbe(configuration(), "opaque_previous")

        assertEquals("opaque_probe", page.nextCursor)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/items/delta", request.url.encodedPath)
        assertEquals("limit=1&cursor=opaque_previous", request.url.encodedQuery)
        assertEquals("application/json", request.headers["Accept"])
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
            fixedBlocks = listOf(
                FixedScheduleBlockRequest(
                    id = "44444444-4444-4444-8444-444444444444",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-FIXED-ANDROID",
                    start = "2026-09-01T08:00:00Z",
                    end = "2026-09-01T08:30:00Z",
                    source = "google_calendar",
                ),
            ),
        )

        val preview = transport.preview(configuration(), request)

        assertEquals("sha256:${"a".repeat(64)}", preview.inputDigest)
        assertEquals(BLOCK_ID, preview.plan.blocks.single().id)
        assertTrue(preview.plan.blocks.single().isSensitive)
        assertEquals(60L, preview.plan.score.scheduledMinutes)
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/schedule/preview", recorded.url.encodedPath)
        assertEquals("application/json; charset=utf-8", recorded.headers["Content-Type"])
        assertEquals("Bearer unit-test-secret", recorded.headers["Authorization"])
        val body = Json.parseToJsonElement(requireNotNull(recorded.body).utf8()) as JsonObject
        assertEquals("Europe/Madrid", body["timezone_name"]?.jsonPrimitive?.content)
        val fixedBlock = body["fixed_blocks"]
            ?.let { it as kotlinx.serialization.json.JsonArray }
            ?.single() as JsonObject
        assertEquals("true", fixedBlock["is_sensitive"]?.jsonPrimitive?.content)
        assertEquals(JsonObject(emptyMap()), body["recurrence_context"])
    }

    @Test
    fun currentScheduleUsesReadOnlyHeadersAndStrictlyDecodesOccurrenceIdentity() = runBlocking {
        val occurrenceId = "aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa"
        val recurring = previewJson().replace(
            "\"occurrence_id\":null",
            "\"occurrence_id\":\"$occurrenceId\"",
        ).replace(
            "\"occurrences\":[]",
            """"occurrences":[{
              "id":"$occurrenceId",
              "series_item_id":"$TASK_ID",
              "identity":{"type":"weekly","local_date":"2026-09-01"},
              "nominal_start":"2026-09-01T09:00:00+02:00",
              "nominal_end":"2026-09-01T10:00:00+02:00",
              "window_start":"2026-09-01T09:00:00+02:00",
              "window_end":"2026-09-01T10:00:00+02:00",
              "local_date":"2026-09-01",
              "ordinal":0,
              "state":"generated"
            }]""",
        )
        server.enqueue(currentScheduleResponse(currentScheduleJson(recurring)))

        val current = requireNotNull(transport.currentSchedule(configuration()))

        assertEquals(9uL, current.revision.revisionNumber)
        assertEquals(
            "weekly",
            current.schedule.plan.occurrences.single().identity["type"]?.jsonPrimitive?.content,
        )
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/schedule/current", request.url.encodedPath)
        assertEquals("application/json", request.headers["Accept"])
        assertEquals("no-store, max-age=0", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
    }

    @Test
    fun currentScheduleDecodesExactUnsignedScoreAndPenaltyDomains() = runBlocking {
        val unsigned = previewJson()
            .replace(
                "\"violations\":[]",
                """"violations":[{"kind":"capacity","severity":"warning","item_ids":["$TASK_ID"],"occurrence_ids":[],"start":null,"end":null,"penalty":18446744073709551615,"message":"Unsigned penalty"}]""",
            )
            .replace("\"scheduled_minutes\":60", "\"scheduled_minutes\":4294967295")
            .replace("\"unscheduled_minutes\":0", "\"unscheduled_minutes\":4294967295")
            .replace("\"soft_penalty\":0", "\"soft_penalty\":18446744073709551615")
            .replace("\"moved_minutes\":0", "\"moved_minutes\":4294967295")
        server.enqueue(currentScheduleResponse(currentScheduleJson(unsigned)))

        val plan = requireNotNull(transport.currentSchedule(configuration())).schedule.plan

        assertEquals(4_294_967_295L, plan.score.scheduledMinutes)
        assertEquals(4_294_967_295L, plan.score.unscheduledMinutes)
        assertEquals(ULong.MAX_VALUE, plan.score.softPenalty)
        assertEquals(4_294_967_295L, plan.score.movedMinutes)
        assertEquals(ULong.MAX_VALUE, plan.violations.single().penalty)
    }

    @Test
    fun currentScheduleRejectsUnknownFieldsMissingIdentityAndUncacheableResponses() {
        server.enqueue(
            currentScheduleResponse(
                currentScheduleJson(previewJson()),
                etag = "\"8:55555555-5555-4555-8555-555555555555\"",
            ),
        )
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.currentSchedule(configuration()) }
        }

        val duplicateEscapedKey = currentScheduleJson(previewJson()).replace(
            "\"revision_number\":9",
            "\"revision_number\":9,\"revision\\u005fnumber\":9",
        )
        server.enqueue(currentScheduleResponse(duplicateEscapedKey))
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.currentSchedule(configuration()) }
        }

        server.enqueue(
            currentScheduleResponse(
                currentScheduleJson(previewJson()).dropLast(1) + ",\"future\":true}",
            ),
        )
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.currentSchedule(configuration()) }
        }

        val occurrenceId = "aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa"
        val missingIdentity = previewJson().replace(
            "\"occurrences\":[]",
            """"occurrences":[{
              "id":"$occurrenceId",
              "series_item_id":"$TASK_ID",
              "nominal_start":"2026-09-01T09:00:00+02:00",
              "nominal_end":"2026-09-01T10:00:00+02:00",
              "window_start":"2026-09-01T09:00:00+02:00",
              "window_end":"2026-09-01T10:00:00+02:00",
              "local_date":"2026-09-01",
              "ordinal":0,
              "state":"generated"
            }]""",
        )
        server.enqueue(currentScheduleResponse(currentScheduleJson(missingIdentity)))
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.currentSchedule(configuration()) }
        }

        server.enqueue(jsonResponse(currentScheduleJson(previewJson())))
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.currentSchedule(configuration()) }
        }
    }

    @Test
    fun currentScheduleRejectsOmittedRequiredNestedEvidenceArrays() {
        val base = previewJson()
        val withViolation = base.replace(
            "\"violations\":[],",
            """"violations":[{"kind":"capacity","severity":"warning","item_ids":["$TASK_ID"],"occurrence_ids":[],"start":null,"end":null,"penalty":1,"message":"Capacity"}],""",
        )
        val omissions = listOf(
            "decisions" to base.replace("\"decisions\":[],", ""),
            "violations" to base.replace("\"violations\":[],", ""),
            "occurrences" to base.replace(
                Regex(""",\s*"occurrences":\[\]"""),
                "",
            ),
            "block explanations" to
                base.replace(Regex(""",\s*"explanations":\[\]"""), ""),
            "violation occurrence_ids" to
                withViolation.replace("\"occurrence_ids\":[],", ""),
        )

        omissions.forEach { (label, schedule) ->
            assertFalse("$label fixture must omit its target", schedule == base)
            server.enqueue(currentScheduleResponse(currentScheduleJson(schedule)))
            assertThrows(label, PlannerApiException.InvalidResponse::class.java) {
                runBlocking { transport.currentSchedule(configuration()) }
            }
        }
    }

    @Test
    fun currentScheduleTrustsOnlyExactNoPublication404() {
        val missing =
            """{"error":{"code":"not_found","message":"Published schedule was not found"}}"""
        server.enqueue(trustedError(404, missing))
        assertEquals(null, runBlocking { transport.currentSchedule(configuration()) })

        listOf(
            MockResponse.Builder()
                .code(404)
                .addHeader("Content-Type", "application/json")
                .body(missing)
                .build(),
            trustedError(
                404,
                """{"error":{"code":"not_found","message":"Another resource was not found"}}""",
            ),
            trustedError(
                404,
                """{"error":{"code":"not_found","message":"Published schedule was not found","details":null}}""",
            ),
            trustedError(
                404,
                """{"error":{"code":"not_found","code":"not_found","message":"Published schedule was not found"}}""",
            ),
        ).forEach { response ->
            server.enqueue(response)
            val error = assertThrows(PlannerApiException.Http::class.java) {
                runBlocking { transport.currentSchedule(configuration()) }
            }
            assertEquals(404, error.statusCode)
        }
    }

    @Test
    fun publishSendsExactJournaledRequestAndDecodesStrictRevision() = runBlocking {
        val configuration = configuration()
        val schedule = scheduleRequest()
        val digest = "sha256:${"b".repeat(64)}"
        val idempotencyKey = "33333333-3333-4333-8333-333333333333"
        val request = buildSchedulePublishHttpRequest(
            configuration,
            SchedulePublishRequest(idempotencyKey, digest, schedule),
        )
        val revisionId = "55555555-5555-4555-8555-555555555555"
        server.enqueue(
            jsonResponse(
                """{"revision":{"id":"$revisionId","revision":"9:$revisionId","revision_number":9,"input_digest":"$digest","horizon_start":"${schedule.horizonStart}","horizon_end":"${schedule.horizonEnd}","timezone_name":"${schedule.timezoneName}","published_at":"2026-09-01T07:01:00Z"},"replayed":false}""",
            ),
        )

        val response = transport.publish(configuration, request)

        assertEquals(9uL, response.revision.revisionNumber)
        assertFalse(response.replayed)
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/schedule/publish", recorded.url.encodedPath)
        assertEquals("application/json", recorded.headers["Accept"])
        assertEquals("application/json; charset=utf-8", recorded.headers["Content-Type"])
        assertEquals("no-store", recorded.headers["Cache-Control"])
        assertEquals("no-cache", recorded.headers["Pragma"])
        assertEquals("Bearer unit-test-secret", recorded.headers["Authorization"])
        assertEquals(request.bodyJson, requireNotNull(recorded.body).utf8())
        assertEquals(plannerSha256(request.bodyJson), request.bodySha256)
    }

    @Test
    fun publicationBodyCeilingAcceptsExactLimitAndRejectsOneAdditionalByte() {
        val configuration = configuration()
        val emptyTitleRequest = SchedulePublishRequest(
            idempotencyKey = "33333333-3333-4333-8333-333333333333",
            expectedInputDigest = "sha256:${"b".repeat(64)}",
            schedule = scheduleRequest().copy(
                fixedBlocks = listOf(syntheticFixedBlock(title = "")),
            ),
        )
        val json = OkHttpCanonicalPlannerTransport.defaultJson()
        val emptyTitleBytes = json.encodeToString(emptyTitleRequest)
            .toByteArray(StandardCharsets.UTF_8)
            .size
        val paddingBytes = MAX_SCHEDULE_PUBLISH_BODY_BYTES - emptyTitleBytes
        assertTrue(paddingBytes > 0)

        val exactRequest = emptyTitleRequest.copy(
            schedule = emptyTitleRequest.schedule.copy(
                fixedBlocks = listOf(syntheticFixedBlock(title = "x".repeat(paddingBytes))),
            ),
        )
        val exact = buildSchedulePublishHttpRequest(configuration, exactRequest)

        assertEquals(
            MAX_SCHEDULE_PUBLISH_BODY_BYTES,
            exact.bodyJson.toByteArray(StandardCharsets.UTF_8).size,
        )
        assertEquals(exactRequest, validateSchedulePublishHttpRequest(configuration, exact))

        val overLimitRequest = exactRequest.copy(
            schedule = exactRequest.schedule.copy(
                fixedBlocks = listOf(
                    syntheticFixedBlock(title = "x".repeat(paddingBytes + 1)),
                ),
            ),
        )
        assertEquals(
            MAX_SCHEDULE_PUBLISH_BODY_BYTES + 1,
            json.encodeToString(overLimitRequest).toByteArray(StandardCharsets.UTF_8).size,
        )
        assertThrows(IllegalArgumentException::class.java) {
            buildSchedulePublishHttpRequest(configuration, overLimitRequest)
        }
    }

    @Test
    fun publishRejectsNon200AndNonJsonWithoutWeakeningExactJournal() {
        val configuration = configuration()
        val request = buildSchedulePublishHttpRequest(
            configuration,
            SchedulePublishRequest(
                "33333333-3333-4333-8333-333333333333",
                "sha256:${"b".repeat(64)}",
                scheduleRequest(),
            ),
        )
        listOf(201, 202, 204).forEach { status ->
            server.enqueue(
                MockResponse.Builder()
                    .code(status)
                    .addHeader("Content-Type", "application/json")
                    .apply { if (status != 204) body("{}") }
                    .build(),
            )
            val error = assertThrows(PlannerApiException.Http::class.java) {
                runBlocking { transport.publish(configuration, request) }
            }
            assertEquals(status, error.statusCode)
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/javascript")
                .body("{}")
                .build(),
        )
        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.publish(configuration, request) }
        }
        assertEquals(4, server.requestCount)
    }

    @Test
    fun publishTrustsOnlyTheExactTypedStaleConflictEnvelope() {
        val configuration = configuration()
        val request = buildSchedulePublishHttpRequest(
            configuration,
            SchedulePublishRequest(
                "33333333-3333-4333-8333-333333333333",
                "sha256:${"b".repeat(64)}",
                scheduleRequest(),
            ),
        )
        server.enqueue(
            MockResponse.Builder()
                .code(409)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .body(
                    """{"error":{"code":"schedule_publication_stale","message":"Synthetic item revision changed"}}""",
                )
                .build(),
        )
        assertThrows(PlannerApiException.SchedulePublicationStale::class.java) {
            runBlocking { transport.publish(configuration, request) }
        }

        listOf(
            """{"error":{"code":"schedule_publication_idempotency_conflict","message":"Synthetic tuple conflict"}}""",
            """{"error":{"code":"conflict","message":"Synthetic generic conflict"}}""",
            """{"error":{"code":"schedule_publication_stale","message":"Synthetic item revision changed","future":true}}""",
            """{"error":{"code":"schedule_publication_stale","message":"Synthetic item revision changed","details":{"unexpected":true}}}""",
            """{"error":{"code":"schedule_publication_stale","message":"Synthetic item revision changed","details":null}}""",
        ).forEach { body ->
            server.enqueue(
                MockResponse.Builder()
                    .code(409)
                    .addHeader("Content-Type", "application/json")
                    .body(body)
                    .build(),
            )
            assertThrows(PlannerApiException.Conflict::class.java) {
                runBlocking { transport.publish(configuration, request) }
            }
        }
        assertEquals(6, server.requestCount)
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
            isSensitive = false,
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
        assertEquals(
            "false",
            (body["item"] as JsonObject)["is_sensitive"]?.jsonPrimitive?.content,
        )
    }

    @Test
    fun createUsesFlatBodyIdempotencyHeaderAndExact201Status() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .code(201)
                .addHeader("Content-Type", "application/json")
                .body("""{"item":${itemJson()}}""")
                .build(),
        )
        val request = createRequest()

        val item = transport.createItem(
            configuration(),
            "android-item-33333333-3333-4333-8333-333333333333",
            request,
        )

        assertEquals(TASK_ID, item.id)
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("/tenant/v1/items", recorded.url.encodedPath)
        assertEquals(null, recorded.url.encodedQuery)
        assertEquals(
            "android-item-33333333-3333-4333-8333-333333333333",
            recorded.headers["Idempotency-Key"],
        )
        val body = Json.parseToJsonElement(requireNotNull(recorded.body).utf8()) as JsonObject
        assertEquals(TASK_ID, body["id"]?.jsonPrimitive?.content)
        assertEquals("true", body["is_sensitive"]?.jsonPrimitive?.content)
        assertEquals("planned", body["status"]?.jsonPrimitive?.content)
        assertFalse("item" in body)

        server.enqueue(jsonResponse("""{"item":${itemJson()}}"""))
        val wrongStatus = assertThrows(PlannerApiException.Http::class.java) {
            runBlocking {
                transport.createItem(
                    configuration(),
                    "android-item-33333333-3333-4333-8333-333333333333",
                    request,
                )
            }
        }
        assertEquals(200, wrongStatus.statusCode)
    }

    @Test
    fun trashAndRestoreUseExactRevisionContracts() = runBlocking {
        val trashed = itemJson()
            .replace("\"revision\":7", "\"revision\":8")
            .replace("\"deleted_at\":null", "\"deleted_at\":\"2026-09-01T07:05:00Z\"")
        val restored = itemJson()
            .replace("\"revision\":7", "\"revision\":9")
            .replace(
                "\"updated_at\":\"2026-08-29T10:00:00Z\"",
                "\"updated_at\":\"2026-09-01T07:06:00Z\"",
            )
        server.enqueue(jsonResponse("""{"item":$trashed}"""))
        server.enqueue(jsonResponse("""{"item":$restored}"""))
        val key = "android-item-33333333-3333-4333-8333-333333333333"

        assertEquals(8L, transport.trashItem(configuration(), TASK_ID, key, 7).revision)
        assertEquals(
            9L,
            transport.restoreItem(
                configuration(),
                TASK_ID,
                key,
                CanonicalItemRevisionRequest(8),
            ).revision,
        )

        val deletion = server.takeRequest()
        assertEquals("DELETE", deletion.method)
        assertEquals("/tenant/v1/items/$TASK_ID", deletion.url.encodedPath)
        assertEquals("expected_revision=7", deletion.url.encodedQuery)
        assertEquals(key, deletion.headers["Idempotency-Key"])
        assertEquals(0L, deletion.bodySize)
        val restoration = server.takeRequest()
        assertEquals("POST", restoration.method)
        assertEquals("/tenant/v1/items/$TASK_ID/restore", restoration.url.encodedPath)
        assertEquals(null, restoration.url.encodedQuery)
        assertEquals(key, restoration.headers["Idempotency-Key"])
        val body = Json.parseToJsonElement(requireNotNull(restoration.body).utf8()) as JsonObject
        assertEquals(setOf("expected_revision"), body.keys)
        assertEquals(8L, body["expected_revision"]?.jsonPrimitive?.content?.toLong())
    }

    @Test
    fun canonicalConflictTrustRequiresExactServerEnvelopeAndNoStoreHeaders() {
        val noEffect = """{"error":{"code":"conflict","message":"an item with active children cannot be deleted"}}"""
        server.enqueue(trustedConflict(noEffect))
        assertThrows(PlannerApiException.CanonicalMutationRejected::class.java) {
            runBlocking {
                transport.trashItem(
                    configuration(),
                    TASK_ID,
                    "android-item-33333333-3333-4333-8333-333333333333",
                    7,
                )
            }
        }

        server.enqueue(
            trustedConflict(
                """{"error":{"code":"conflict","message":"matching idempotent request is still in progress"}}""",
            ),
        )
        assertThrows(PlannerApiException.CanonicalMutationInProgress::class.java) {
            runBlocking {
                transport.trashItem(
                    configuration(),
                    TASK_ID,
                    "android-item-33333333-3333-4333-8333-333333333333",
                    7,
                )
            }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(409)
                .addHeader("Content-Type", "application/json")
                .body(noEffect)
                .build(),
        )
        assertThrows(PlannerApiException.Conflict::class.java) {
            runBlocking {
                transport.trashItem(
                    configuration(),
                    TASK_ID,
                    "android-item-33333333-3333-4333-8333-333333333333",
                    7,
                )
            }
        }

        val missing = """{"error":{"code":"not_found","message":"item was not found"}}"""
        server.enqueue(trustedError(404, missing))
        assertThrows(PlannerApiException.CanonicalMutationRejected::class.java) {
            runBlocking {
                transport.replaceItem(
                    configuration(),
                    TASK_ID,
                    "android-item-33333333-3333-4333-8333-333333333333",
                    ReplaceCanonicalItemRequest(7, createRequest().toReplacement()),
                )
            }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(404)
                .addHeader("Content-Type", "application/json")
                .body(missing)
                .build(),
        )
        val untrusted = assertThrows(PlannerApiException.Http::class.java) {
            runBlocking {
                transport.trashItem(
                    configuration(),
                    TASK_ID,
                    "android-item-33333333-3333-4333-8333-333333333333",
                    7,
                )
            }
        }
        assertEquals(404, untrusted.statusCode)
    }

    @Test
    fun missingCurrentSensitivityFieldFailsClosed() {
        val current = Json.parseToJsonElement(itemJson()) as JsonObject
        val missing = JsonObject(current - "is_sensitive").toString()
        server.enqueue(
            jsonResponse(
                """{"changes":[{"type":"upsert","item":$missing}],"next_cursor":"cursor","has_more":false}""",
            ),
        )

        assertThrows(PlannerApiException.InvalidResponse::class.java) {
            runBlocking { transport.itemDelta(configuration(), null) }
        }
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

    private fun currentScheduleResponse(
        body: String,
        etag: String = "\"9:55555555-5555-4555-8555-555555555555\"",
    ): MockResponse = MockResponse.Builder()
        .code(200)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .addHeader("ETag", etag)
        .body(body)
        .build()

    private fun trustedConflict(body: String): MockResponse = MockResponse.Builder()
        .code(409)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .body(body)
        .build()

    private fun trustedError(status: Int, body: String): MockResponse = MockResponse.Builder()
        .code(status)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .body(body)
        .build()

    private fun createRequest() = CreateCanonicalItemRequest(
        id = TASK_ID,
        isSensitive = true,
        kind = "task",
        status = "planned",
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

    private fun CreateCanonicalItemRequest.toReplacement() = CanonicalItemReplacement(
        isSensitive = isSensitive,
        kind = kind,
        status = status,
        title = title,
        notes = notes,
        timezoneName = timezoneName,
        durationSeconds = durationSeconds,
        deadlineAt = deadlineAt,
        earliestStartAt = earliestStartAt,
        recurrence = recurrence,
        flexibleConstraints = flexibleConstraints,
        splitPolicy = splitPolicy,
        importance = importance,
        urgency = urgency,
        parentId = parentId,
        siblingOrder = siblingOrder,
    )

    private fun itemJson(): String = """
        {
          "id":"$TASK_ID",
          "is_sensitive":true,
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

    private fun structuralItemJson(durationSource: String = "user"): String = itemJson().replace(
        "\"duration_seconds\":3600,",
        """"duration_seconds":3600,
          "duration_kind":"exact",
          "duration_min_seconds":3600,
          "duration_max_seconds":3600,
          "duration_source":"$durationSource",
          "deadline_kind":"date_time",
          "deadline_date":null,
          "deadline_strength":"hard",
          "deadline_soft_weight":null,
          "has_own_effort":false,
          "blocked_reason_kind":null,
          "blocked_by_item_id":null,
          "blocked_reason":null,""",
    )

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
              "is_sensitive":true,
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

    private fun currentScheduleJson(schedule: String): String {
        val revisionId = "55555555-5555-4555-8555-555555555555"
        return """{"revision":{"id":"$revisionId","revision":"9:$revisionId","revision_number":9,"input_digest":"sha256:${"a".repeat(64)}","horizon_start":"2026-08-31T22:00:00Z","horizon_end":"2026-09-01T22:00:00Z","timezone_name":"Europe/Madrid","published_at":"2026-09-01T07:01:00Z"},"schedule":$schedule}"""
    }

    private fun scheduleRequest() = SchedulePreviewRequest(
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

    private fun syntheticFixedBlock(title: String) = FixedScheduleBlockRequest(
        id = "44444444-4444-4444-8444-444444444444",
        isSensitive = false,
        title = title,
        start = "2026-09-01T08:00:00Z",
        end = "2026-09-01T08:30:00Z",
        source = "synthetic_boundary_fixture",
    )

    private companion object {
        const val TASK_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
    }
}
