package com.greengolddog.dayweave.network

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test

class OkHttpProposalApplicationsTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpProposalApplicationsTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpProposalApplicationsTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun previewUsesExactCreatedStatusPathHeadersAndStrictTypedResponse() = runBlocking {
        server.enqueue(jsonResponse(previewJson(), status = 201))

        val preview = transport.preview(
            configuration(),
            ProposalPreviewRequest(listOf(ProposalPreviewMember(PROPOSAL_ID, 4))),
        )

        assertEquals(PREVIEW_ID, preview.previewId)
        assertEquals(PROPOSAL_CHANGE_SET_SCHEMA_V1, preview.changeSetSchema)
        assertEquals(listOf(COMMAND_ID), preview.commandIds)
        assertEquals(RemoteProposalRiskLevel.LOW, preview.maximumRisk)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("/tenant/v1/suggestions/application-previews", request.url.encodedPath)
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
        assertEquals("application/json", request.headers["Accept"])
        assertEquals("application/json; charset=utf-8", request.headers["Content-Type"])
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        assertEquals(null, request.headers["Idempotency-Key"])
        assertEquals(
            Json.parseToJsonElement(
                """{"proposals":[{"proposal_id":"$PROPOSAL_ID","expected_revision":4}]}""",
            ),
            Json.parseToJsonElement(requireNotNull(request.body).utf8()),
        )
    }

    @Test
    fun previewRequires201AndRejectsUnknownOrSemanticallyInconsistentFields() {
        server.enqueue(jsonResponse(previewJson(), status = 200))
        val wrongStatus = assertThrows(ProposalApplicationApiException.Http::class.java) {
            runBlocking { preview() }
        }
        assertEquals(200, wrongStatus.statusCode)

        server.enqueue(
            jsonResponse(
                previewJson().replace(
                    "\"conflicts\":[]",
                    "\"conflicts\":[],\"future_contract\":true",
                ),
                status = 201,
            ),
        )
        assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
            runBlocking { preview() }
        }

        server.enqueue(
            jsonResponse(
                previewJson().replace("\"can_apply\":true", "\"can_apply\":false"),
                status = 201,
            ),
        )
        assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
            runBlocking { preview() }
        }
    }

    @Test
    fun previewRecomputesExactChangedFieldsAndRejectsOmissionInjectionOrReordering() {
        server.enqueue(
            jsonResponse(replacePreviewJson("[\"title\",\"revision\"]"), status = 201),
        )
        assertEquals(
            listOf(RemoteProposalItemField.TITLE, RemoteProposalItemField.REVISION),
            runBlocking { preview() }.diffs.single().changedFields,
        )

        listOf(
            "[\"title\"]",
            "[\"title\",\"status\",\"revision\"]",
            "[\"revision\",\"title\"]",
        ).forEach { changedFields ->
            server.enqueue(jsonResponse(replacePreviewJson(changedFields), status = 201))
            assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }

        server.enqueue(
            jsonResponse(
                replacePreviewWithImplicitJson("[\"is_executable\",\"revision\"]"),
                status = 201,
            ),
        )
        assertEquals(
            listOf(RemoteProposalItemField.IS_EXECUTABLE, RemoteProposalItemField.REVISION),
            runBlocking { preview() }.implicitDiffs.single().changedFields,
        )
        listOf(
            "[\"is_executable\"]",
            "[\"is_executable\",\"status\",\"revision\"]",
            "[\"revision\",\"is_executable\"]",
        ).forEach { changedFields ->
            server.enqueue(
                jsonResponse(replacePreviewWithImplicitJson(changedFields), status = 201),
            )
            assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun applyBuilderBindsExactUrlBodyDigestAndSecurityHeaders() {
        val request = prepareProposalApplyHttpRequest(
            configuration(),
            PREVIEW_ID,
            REVIEW_HASH,
        )

        assertEquals(
            server.url("/tenant/v1/suggestions/application-previews/$PREVIEW_ID/apply").toString(),
            request.url,
        )
        assertEquals("POST", request.method)
        assertEquals("application/json", request.acceptHeader)
        assertEquals("application/json; charset=utf-8", request.contentTypeHeader)
        assertEquals("no-store", request.cacheControlHeader)
        assertEquals("no-cache", request.pragmaHeader)
        assertEquals("""{"expected_review_hash":"$REVIEW_HASH"}""", request.bodyJson)
        assertEquals(plannerSha256(request.bodyJson), request.bodySha256)
        assertFalse(request.toString().contains(REVIEW_HASH))
        validateProposalApplyHttpRequest(
            configuration().baseUrl.toString(),
            request,
            PREVIEW_ID,
            REVIEW_HASH,
        )

        listOf(
            request.copy(url = request.url + "?unsafe=1"),
            request.copy(cacheControlHeader = "max-age=60"),
            request.copy(bodyJson = request.bodyJson + " "),
            request.copy(bodySha256 = "sha256:${"0".repeat(64)}"),
        ).forEach { tampered ->
            assertThrows(IllegalArgumentException::class.java) {
                validateProposalApplyHttpRequest(
                    configuration().baseUrl.toString(),
                    tampered,
                    PREVIEW_ID,
                    REVIEW_HASH,
                )
            }
        }
    }

    @Test
    fun applySendsExactPersistedBytesAndIdempotencyHeader() = runBlocking {
        server.enqueue(jsonResponse("""{"application":${receiptJson()},"replayed":false}"""))
        val durableRequest = prepareProposalApplyHttpRequest(
            configuration(),
            PREVIEW_ID,
            REVIEW_HASH,
        )

        val result = transport.apply(
            configuration(),
            PREVIEW_ID,
            REVIEW_HASH,
            IDEMPOTENCY_KEY,
            durableRequest,
        )

        assertFalse(result.replayed)
        assertEquals(APPLICATION_ID, result.application.applicationId)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals(
            "/tenant/v1/suggestions/application-previews/$PREVIEW_ID/apply",
            request.url.encodedPath,
        )
        assertEquals(IDEMPOTENCY_KEY, request.headers["Idempotency-Key"])
        assertEquals(durableRequest.bodyJson, requireNotNull(request.body).utf8())
        assertEquals(durableRequest.contentTypeHeader, request.headers["Content-Type"])
        assertEquals(durableRequest.cacheControlHeader, request.headers["Cache-Control"])
        assertEquals(durableRequest.pragmaHeader, request.headers["Pragma"])
    }

    @Test
    fun applyRejectsTamperingAndInvalidIdempotencyBeforeNetworkIo() {
        val request = prepareProposalApplyHttpRequest(
            configuration(),
            PREVIEW_ID,
            REVIEW_HASH,
        )
        assertThrows(ProposalApplicationApiException.InvalidRequest::class.java) {
            runBlocking {
                transport.apply(
                    configuration(),
                    PREVIEW_ID,
                    REVIEW_HASH,
                    IDEMPOTENCY_KEY,
                    request.copy(bodyJson = "{}"),
                )
            }
        }
        assertThrows(ProposalApplicationApiException.InvalidRequest::class.java) {
            runBlocking {
                transport.apply(
                    configuration(),
                    PREVIEW_ID,
                    REVIEW_HASH,
                    "unsafe/key",
                    request,
                )
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun getsUseExactPathsAndGetByProposalHasTypedNotFound() = runBlocking {
        server.enqueue(jsonResponse(receiptJson()))
        server.enqueue(jsonResponse(receiptJson()))
        server.enqueue(errorResponse(status = 404, code = "not_found"))

        assertEquals(APPLICATION_ID, transport.getById(configuration(), APPLICATION_ID).applicationId)
        assertEquals(
            APPLICATION_ID,
            transport.getByProposal(configuration(), PROPOSAL_ID).applicationId,
        )
        assertThrows(ProposalApplicationApiException.NotFound::class.java) {
            runBlocking { transport.getByProposal(configuration(), OTHER_PROPOSAL_ID) }
        }

        val byId = server.takeRequest()
        assertEquals("GET", byId.method)
        assertEquals(
            "/tenant/v1/suggestions/applications/$APPLICATION_ID",
            byId.url.encodedPath,
        )
        assertEquals(null, byId.headers["Idempotency-Key"])
        val byProposal = server.takeRequest()
        assertEquals(
            "/tenant/v1/suggestions/$PROPOSAL_ID/application",
            byProposal.url.encodedPath,
        )
        assertEquals(
            "/tenant/v1/suggestions/$OTHER_PROPOSAL_ID/application",
            server.takeRequest().url.encodedPath,
        )
    }

    @Test
    fun undoBuilderAndTransportBindRevisionUrlBodyAndReceipt() = runBlocking {
        val durableRequest = prepareProposalUndoHttpRequest(configuration(), APPLICATION_ID, 1)
        validateProposalUndoHttpRequest(
            configuration().baseUrl.toString(),
            durableRequest,
            APPLICATION_ID,
            1,
        )
        assertEquals("""{"expected_application_revision":1}""", durableRequest.bodyJson)
        server.enqueue(
            jsonResponse(
                """{"application":${receiptJson(undone = true)},"replayed":true}""",
            ),
        )

        val response = transport.undo(
            configuration(),
            APPLICATION_ID,
            1,
            UNDO_IDEMPOTENCY_KEY,
            durableRequest,
        )

        assertEquals(RemoteProposalApplicationStatus.UNDONE, response.application.status)
        assertEquals(2L, response.application.applicationRevision)
        assertEquals(true, response.replayed)
        val request = server.takeRequest()
        assertEquals(
            "/tenant/v1/suggestions/applications/$APPLICATION_ID/undo",
            request.url.encodedPath,
        )
        assertEquals(UNDO_IDEMPOTENCY_KEY, request.headers["Idempotency-Key"])
        assertEquals(durableRequest.bodyJson, requireNotNull(request.body).utf8())
    }

    @Test
    fun receiptValidationRejectsMissingNullableFieldAndImpossibleStatusRevision() {
        server.enqueue(
            jsonResponse(
                receiptJson().replace(Regex(""",\s*"undone_at":null"""), ""),
            ),
        )
        assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }

        server.enqueue(
            jsonResponse(receiptJson().replace("\"application_revision\":1", "\"application_revision\":2")),
        )
        assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }
    }

    @Test
    fun trustedErrorsAreStrictlyTypedWithoutLeakingBearer() {
        server.enqueue(
            errorResponse(
                status = 401,
                code = "unauthorized",
                authenticate = true,
            ),
        )
        val authentication = assertThrows(ProposalApplicationApiException.Authentication::class.java) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }
        assertFalse(authentication.toString().contains("unit-test-secret"))

        server.enqueue(
            errorResponse(
                status = 409,
                code = "conflict",
                details = """{"conflict_code":"preview_expired"}""",
            ),
        )
        val conflict = assertThrows(ProposalApplicationApiException.Conflict::class.java) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }
        assertEquals(RemoteProposalConflictCode.PREVIEW_EXPIRED, conflict.conflictCode)

        server.enqueue(errorResponse(status = 422, code = "validation_failed"))
        val validation = assertThrows(ProposalApplicationApiException.Validation::class.java) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }
        assertEquals(422, validation.statusCode)
    }

    @Test
    fun untrustedOrUnknownConflictErrorsFailClosed() {
        listOf(
            MockResponse.Builder()
                .code(409)
                .addHeader("Content-Type", "application/json")
                .body("""{"error":{"code":"conflict","message":"Synthetic"}}""")
                .build(),
            errorResponse(
                status = 409,
                code = "conflict",
                details = """{"conflict_code":"future_conflict"}""",
            ),
            errorResponse(
                status = 409,
                code = "conflict",
                details = """{"conflict_code":"preview_expired","future":true}""",
            ),
        ).forEach { response ->
            server.enqueue(response)
            assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
                runBlocking { transport.getById(configuration(), APPLICATION_ID) }
            }
        }

        server.enqueue(errorResponse(status = 409, code = "conflict"))
        val idempotencyConflict = assertThrows(
            ProposalApplicationApiException.Conflict::class.java,
        ) {
            runBlocking { transport.getById(configuration(), APPLICATION_ID) }
        }
        assertEquals(null, idempotencyConflict.conflictCode)
    }

    @Test
    fun boundedResponseReaderRejectsDeclaredOversizeBody() {
        val boundedTransport = OkHttpProposalApplicationsTransport(maximumResponseBytes = 512)
        server.enqueue(jsonResponse("{" + "x".repeat(1_024) + "}"))

        assertThrows(ProposalApplicationApiException.InvalidResponse::class.java) {
            runBlocking { boundedTransport.getById(configuration(), APPLICATION_ID) }
        }
    }

    private suspend fun preview(): RemoteProposalApplicationPreview = transport.preview(
        configuration(),
        ProposalPreviewRequest(listOf(ProposalPreviewMember(PROPOSAL_ID, 4))),
    )

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun jsonResponse(body: String, status: Int = 200): MockResponse =
        MockResponse.Builder()
            .code(status)
            .addHeader("Content-Type", "application/json; charset=utf-8")
            .body(body)
            .build()

    private fun errorResponse(
        status: Int,
        code: String,
        details: String? = null,
        authenticate: Boolean = false,
    ): MockResponse = MockResponse.Builder()
        .code(status)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .apply {
            if (authenticate) addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
        }
        .body(
            """{"error":{"code":"$code","message":"Synthetic"${
                details?.let { value -> ",\"details\":$value" }.orEmpty()
            }}}""",
        )
        .build()

    private fun previewJson(): String = """
        {
          "preview_id":"$PREVIEW_ID",
          "proposals":[{"proposal_id":"$PROPOSAL_ID","expected_revision":4}],
          "change_set_schema":"$PROPOSAL_CHANGE_SET_SCHEMA_V1",
          "command_ids":["$COMMAND_ID"],
          "review_hash":"$REVIEW_HASH",
          "expires_at":"2026-09-01T10:15:00Z",
          "can_apply":true,
          "maximum_risk":"low",
          "requires_explicit_approval":false,
          "diffs":[{
            "command_id":"$COMMAND_ID",
            "operation":"create_item",
            "item_id":"$ITEM_ID",
            "changed_fields":${allMaterialFieldsJson()},
            "before":null,
            "after":${itemJson()}
          }],
          "implicit_diffs":[],
          "risks":[{
            "code":"creates_item",
            "level":"low",
            "command_id":"$COMMAND_ID",
            "item_id":"$ITEM_ID",
            "requires_explicit_approval":false,
            "summary":"Creates a new local item."
          }],
          "conflicts":[]
        }
    """.trimIndent()

    private fun replacePreviewJson(changedFields: String): String {
        val before = itemJson().replace(
            "\"title\":\"Review the proposal\"",
            "\"title\":\"Original title\"",
        )
        val after = before
            .replace("\"title\":\"Original title\"", "\"title\":\"Updated title\"")
            .replace("\"revision\":1", "\"revision\":2")
            .replace(
                "\"updated_at\":\"2026-09-01T10:00:00Z\"",
                "\"updated_at\":\"2026-09-01T10:05:00Z\"",
            )
        return """
            {
              "preview_id":"$PREVIEW_ID",
              "proposals":[{"proposal_id":"$PROPOSAL_ID","expected_revision":4}],
              "change_set_schema":"$PROPOSAL_CHANGE_SET_SCHEMA_V1",
              "command_ids":["$COMMAND_ID"],
              "review_hash":"$REVIEW_HASH",
              "expires_at":"2026-09-01T10:15:00Z",
              "can_apply":true,
              "maximum_risk":"low",
              "requires_explicit_approval":false,
              "diffs":[{
                "command_id":"$COMMAND_ID",
                "operation":"replace_item",
                "item_id":"$ITEM_ID",
                "changed_fields":$changedFields,
                "before":$before,
                "after":$after
              }],
              "implicit_diffs":[],
              "risks":[],
              "conflicts":[]
            }
        """.trimIndent()
    }

    private fun allMaterialFieldsJson(): String = """
        ["is_sensitive","kind","status","title","notes","timezone_name",
        "duration_seconds","deadline_at","earliest_start_at","recurrence",
        "flexible_constraints","split_policy","importance","urgency","parent_id",
        "sibling_order","is_executable","revision","completed_at","deleted_at"]
    """.trimIndent()

    private fun replacePreviewWithImplicitJson(changedFields: String): String {
        val before = itemJson()
            .replace(ITEM_ID, IMPLICIT_ITEM_ID)
            .replace("\"title\":\"Review the proposal\"", "\"title\":\"Parent item\"")
        val after = before
            .replace("\"is_executable\":true", "\"is_executable\":false")
            .replace("\"revision\":1", "\"revision\":2")
            .replace(
                "\"updated_at\":\"2026-09-01T10:00:00Z\"",
                "\"updated_at\":\"2026-09-01T10:05:00Z\"",
            )
        val implicitDiff = """
            [{
              "item_id":"$IMPLICIT_ITEM_ID",
              "reason":"hierarchy_refresh",
              "changed_fields":$changedFields,
              "before":$before,
              "after":$after
            }]
        """.trimIndent()
        return replacePreviewJson("[\"title\",\"revision\"]").replace(
            "\"implicit_diffs\":[]",
            "\"implicit_diffs\":$implicitDiff",
        )
    }

    private fun itemJson(): String = """
        {
          "id":"$ITEM_ID",
          "is_sensitive":false,
          "kind":"task",
          "status":"planned",
          "title":"Review the proposal",
          "notes":null,
          "timezone_name":"Europe/Madrid",
          "duration_seconds":1800,
          "deadline_at":null,
          "earliest_start_at":null,
          "recurrence":null,
          "flexible_constraints":{},
          "split_policy":{"type":"indivisible"},
          "importance":50,
          "urgency":40,
          "parent_id":null,
          "sibling_order":0,
          "is_executable":true,
          "revision":1,
          "created_at":"2026-09-01T10:00:00Z",
          "updated_at":"2026-09-01T10:00:00Z",
          "completed_at":null,
          "deleted_at":null
        }
    """.trimIndent()

    private fun receiptJson(undone: Boolean = false): String = """
        {
          "application_id":"$APPLICATION_ID",
          "proposals":[{"proposal_id":"$PROPOSAL_ID","applied_revision":4}],
          "application_revision":${if (undone) 2 else 1},
          "status":"${if (undone) "undone" else "applied"}",
          "command_ids":["$COMMAND_ID"],
          "affected_item_ids":["$ITEM_ID"],
          "applied_at":"2026-09-01T10:01:00Z",
          "undo_expires_at":"2026-09-02T10:01:00Z",
          "undone_at":${if (undone) "\"2026-09-01T10:05:00Z\"" else "null"}
        }
    """.trimIndent()

    private companion object {
        const val PREVIEW_ID = "11111111-1111-4111-8111-111111111111"
        const val PROPOSAL_ID = "22222222-2222-4222-8222-222222222222"
        const val OTHER_PROPOSAL_ID = "2aaaaaaa-2222-4222-8222-222222222222"
        const val COMMAND_ID = "33333333-3333-4333-8333-333333333333"
        const val ITEM_ID = "44444444-4444-4444-8444-444444444444"
        const val IMPLICIT_ITEM_ID = "4aaaaaaa-4444-4444-8444-444444444444"
        const val APPLICATION_ID = "55555555-5555-4555-8555-555555555555"
        const val IDEMPOTENCY_KEY = "apply-key_1234~safe"
        const val UNDO_IDEMPOTENCY_KEY = "undo-key_1234~safe"
        const val REVIEW_HASH =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
