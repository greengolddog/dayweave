package com.greengolddog.dayweave.network

import java.util.Base64
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import okio.Buffer

class OkHttpGoogleCalendarOutboundTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpGoogleCalendarOutboundTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpGoogleCalendarOutboundTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun previewApproveAndEnqueueUseExactBoundNoStoreContracts() = runBlocking {
        server.enqueue(jsonResponse(200, previewEnvelopeJson()))
        server.enqueue(jsonResponse(200, approvalEnvelopeJson()))
        server.enqueue(jsonResponse(202, acceptedEnvelopeJson()))

        val preview = transport.preview(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ITEM_ID,
            ITEM_REVISION,
        )
        val approval = transport.approve(
            configuration(),
            ACCOUNT_ID,
            PREVIEW_ID,
            PREVIEW_HASH,
        )
        val accepted = transport.enqueue(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ITEM_ID,
            ITEM_REVISION,
            APPROVAL_CAPABILITY,
        )

        assertEquals(PREVIEW_ID, preview.id)
        assertEquals(ACCOUNT_ID, preview.accountId)
        assertEquals(COLLECTION_ID, preview.collectionId)
        assertEquals(7L, preview.collectionRevision)
        assertEquals("Primary calendar", preview.collectionDisplayName)
        assertEquals(ITEM_ID, preview.itemId)
        assertEquals(ITEM_REVISION, preview.itemRevision)
        assertEquals(GoogleCalendarOutboundEntityKind.CALENDAR_EVENT, preview.entityKind)
        assertEquals(GoogleCalendarOutboundOperation.UPSERT, preview.operation)
        assertEquals(PREVIEW_HASH, preview.previewHash)
        assertEquals("Private meeting", preview.providerPayload["summary"]?.jsonPrimitive?.content)
        assertEquals(PREVIEW_ID, approval.previewId)
        assertEquals(APPROVAL_CAPABILITY, approval.approvalCapability)
        assertEquals(OUTBOX_ID, accepted.outboxId)
        assertFalse(accepted.replayed)

        val previewRequest = server.takeRequest()
        assertEquals("POST", previewRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/outbound/previews",
            previewRequest.url.encodedPath,
        )
        assertOutboundHeaders(previewRequest.headers.toMultimap())
        assertEquals("application/json; charset=utf-8", previewRequest.headers["Content-Type"])
        assertNull(previewRequest.headers["Idempotency-Key"])
        assertEquals(
            mapOf(
                "collection_id" to COLLECTION_ID,
                "item_id" to ITEM_ID,
                "expected_item_revision" to ITEM_REVISION.toString(),
                "operation" to "upsert",
            ),
            stringBody(previewRequest.body?.utf8()),
        )

        val approveRequest = server.takeRequest()
        assertEquals("POST", approveRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/outbound/previews/" +
                "$PREVIEW_ID/approve",
            approveRequest.url.encodedPath,
        )
        assertOutboundHeaders(approveRequest.headers.toMultimap())
        assertEquals(
            mapOf("expected_preview_hash" to PREVIEW_HASH),
            stringBody(approveRequest.body?.utf8()),
        )

        val enqueueRequest = server.takeRequest()
        assertEquals("POST", enqueueRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/outbound",
            enqueueRequest.url.encodedPath,
        )
        assertOutboundHeaders(enqueueRequest.headers.toMultimap())
        assertNull(enqueueRequest.headers["Idempotency-Key"])
        assertEquals(
            mapOf(
                "collection_id" to COLLECTION_ID,
                "item_id" to ITEM_ID,
                "expected_item_revision" to ITEM_REVISION.toString(),
                "operation" to "upsert",
                "approval_capability" to APPROVAL_CAPABILITY,
            ),
            stringBody(enqueueRequest.body?.utf8()),
        )
    }

    @Test
    fun approvalIsOneShotAndTransportDisablesAutomaticConnectionRetries() = runBlocking {
        server.enqueue(jsonResponse(200, approvalEnvelopeJson()))
        var observedOneShot = false
        var observedConnectionRetries = true
        val configuration = AuthenticatedApiConfiguration.createCoordinated(
            baseUrl = server.url("/tenant/").toString(),
            bearerToken = "unit-test-secret",
            configurationId = "outbound-test-configuration",
            executor = object : DeviceAuthRequestExecutor {
                override suspend fun executeAuthenticated(
                    configuration: AuthenticatedApiConfiguration,
                    client: okhttp3.OkHttpClient,
                    request: okhttp3.Request,
                ): okhttp3.Response {
                    observedOneShot = request.body?.isOneShot() == true
                    observedConnectionRetries = client.retryOnConnectionFailure
                    return client.newCall(request).execute()
                }
            },
            allowCleartextLoopback = true,
        )

        transport.approve(configuration, ACCOUNT_ID, PREVIEW_ID, PREVIEW_HASH)

        assertTrue(observedOneShot)
        assertFalse(observedConnectionRetries)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun outboundSurfaceCannotExpressDeleteOrNonCalendarEntity() {
        assertEquals(
            listOf(GoogleCalendarOutboundOperation.UPSERT),
            GoogleCalendarOutboundOperation.entries,
        )
        assertEquals(
            listOf(GoogleCalendarOutboundEntityKind.CALENDAR_EVENT),
            GoogleCalendarOutboundEntityKind.entries,
        )

        listOf(
            previewEnvelopeJson().replace("\"operation\":\"upsert\"", "\"operation\":\"delete\""),
            previewEnvelopeJson().replace(
                "\"entity_kind\":\"calendar_event\"",
                "\"entity_kind\":\"task\"",
            ),
        ).forEach { response ->
            server.enqueue(jsonResponse(200, response))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun previewRejectsEveryMismatchedEchoedIdentity() {
        val mismatches = listOf(
            "\"account_id\":\"$ACCOUNT_ID\"" to
                "\"account_id\":\"ffffffff-ffff-4fff-8fff-ffffffffffff\"",
            "\"collection_id\":\"$COLLECTION_ID\"" to
                "\"collection_id\":\"ffffffff-ffff-4fff-8fff-ffffffffffff\"",
            "\"item_id\":\"$ITEM_ID\"" to
                "\"item_id\":\"ffffffff-ffff-4fff-8fff-ffffffffffff\"",
            "\"item_revision\":$ITEM_REVISION" to "\"item_revision\":${ITEM_REVISION + 1}",
        )
        mismatches.forEach { (old, new) ->
            server.enqueue(jsonResponse(200, previewEnvelopeJson().replace(old, new)))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun previewRejectsInvalidProviderBindingPayloadAndSemanticBounds() {
        val deeplyNestedPayload = buildString {
            repeat(34) { append("{\"nested\":") }
            append("\"value\"")
            repeat(34) { append('}') }
        }
        val invalidBodies = listOf(
            previewEnvelopeJson(collectionRevision = 0),
            previewEnvelopeJson(collectionDisplayName = "x".repeat(4_097)),
            previewEnvelopeJson(providerResourceId = "remote-1", providerEtag = null),
            previewEnvelopeJson(providerPayload = "{}"),
            previewEnvelopeJson(providerPayload = deeplyNestedPayload),
            previewEnvelopeJson(previewHash = "A".repeat(64)),
            previewEnvelopeJson(expiresAt = "not-an-instant"),
        )
        invalidBodies.forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun previewRejectsMeetingsRecurrenceConferenceAttachmentsAndAliases() {
        val unsafePayloads = listOf(
            PROVIDER_PAYLOAD.replace("\"attendees\":[]", "\"attendees\":[{\"email\":\"guest@example.test\"}]"),
            PROVIDER_PAYLOAD.replace("\"attachments\":[]", "\"attachments\":[{\"fileUrl\":\"https://example.test\"}]"),
            PROVIDER_PAYLOAD.replace("\"conferenceData\":null", "\"conferenceData\":{}"),
            PROVIDER_PAYLOAD.replace("\"recurrence\":[]", "\"recurrence\":[\"RRULE:FREQ=DAILY\"]"),
            PROVIDER_PAYLOAD.replace("\"recurringEventId\":null", "\"recurringEventId\":\"series-1\""),
            PROVIDER_PAYLOAD.dropLast(1) + ",\"guestsCanModify\":false}",
            PROVIDER_PAYLOAD.dropLast(1) + ",\"conference_data\":null}",
            PROVIDER_PAYLOAD.dropLast(1) + ",\"recurring_event_id\":null}",
            PROVIDER_PAYLOAD.dropLast(1) + ",\"original_start_time\":null}",
            PROVIDER_PAYLOAD.dropLast(1) + ",\"hangoutLink\":\"https://meet.example.test\"}",
            PROVIDER_PAYLOAD.dropLast(1) + ",\"organizer\":{\"email\":\"owner@example.test\"}}",
        )
        unsafePayloads.forEach { payload ->
            server.enqueue(jsonResponse(200, previewEnvelopeJson(providerPayload = payload)))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun previewRequiresPrivateDefaultOrderedFixedEventReviewFields() {
        val invalidPayloads = listOf(
            PROVIDER_PAYLOAD.dropLast(1) + ",\"reminders\":{}}",
            PROVIDER_PAYLOAD.replace("\"etag\":null,", ""),
            PROVIDER_PAYLOAD.replace("\"id\":\"$PROVIDER_EVENT_ID\"", "\"id\":\"foreign-id\""),
            PROVIDER_PAYLOAD.replace("\"etag\":null", "\"etag\":\"provider-etag\""),
            PROVIDER_PAYLOAD.replace("\"summary\":\"Private meeting\"", "\"summary\":null"),
            PROVIDER_PAYLOAD.replace("\"status\":\"confirmed\"", "\"status\":null"),
            PROVIDER_PAYLOAD.replace("\"status\":\"confirmed\"", "\"status\":\"tentative\""),
            PROVIDER_PAYLOAD.replace("\"transparency\":\"opaque\"", "\"transparency\":null"),
            PROVIDER_PAYLOAD.replace(
                "\"transparency\":\"opaque\"",
                "\"transparency\":\"transparent\"",
            ),
            PROVIDER_PAYLOAD.replace("\"visibility\":\"private\"", "\"visibility\":\"public\""),
            PROVIDER_PAYLOAD.replace("\"eventType\":\"default\"", "\"eventType\":\"focusTime\""),
            PROVIDER_PAYLOAD.replace("\"location\":null", "\"location\":\"Unreviewed room\""),
            PROVIDER_PAYLOAD.replace("\"updated\":null", "\"updated\":\"2026-09-02T12:00:00Z\""),
            PROVIDER_PAYLOAD.replace("\"sequence\":null", "\"sequence\":1"),
            PROVIDER_PAYLOAD.replace(OWNERSHIP_PROOF, "invalid-ownership-proof"),
            PROVIDER_PAYLOAD.replace("\"shared\":{}", "\"shared\":{\"foreign\":\"value\"}"),
            PROVIDER_PAYLOAD
                .replace(
                    "\"date\":null,\"dateTime\":\"2026-09-02T12:00:00Z\"",
                    "\"date\":\"2026-09-02\",\"dateTime\":null",
                )
                .replace(
                    "\"date\":null,\"dateTime\":\"2026-09-02T12:30:00Z\"",
                    "\"date\":\"2026-09-03\",\"dateTime\":null",
                ),
            PROVIDER_PAYLOAD.replace("2026-09-02T12:30:00Z", "2026-09-02T11:30:00Z"),
            PROVIDER_PAYLOAD.replace("\"timeZone\":\"UTC\"", "\"timeZone\":\"Not/AZone\""),
        )
        invalidPayloads.forEach { payload ->
            server.enqueue(jsonResponse(200, previewEnvelopeJson(providerPayload = payload)))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun strictDecoderRejectsUnknownMissingDuplicateAndMalformedMembers() {
        val duplicateTopLevel =
            "{\"preview\":${previewJson()},\"\\u0070review\":${previewJson()}}"
        val duplicatePrivatePayload = previewEnvelopeJson(
            providerPayload = "{\"summary\":\"Private meeting\",\"\\u0073ummary\":\"other\"}",
        )
        val invalidBodies = listOf(
            previewEnvelopeJson().dropLast(1) + ",\"unexpected\":true}",
            previewEnvelopeJson().replace(
                "\"preview_hash\":\"$PREVIEW_HASH\"",
                "\"preview_hash\":\"$PREVIEW_HASH\",\"unexpected\":true",
            ),
            previewEnvelopeJson().replace(",\"provider_etag\":null", ""),
            duplicateTopLevel,
            duplicatePrivatePayload,
            "{\"preview\":",
        )
        invalidBodies.forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun successResponsesRequireSingleStrictMediaAndNoStoreHeaders() {
        val responses = listOf(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body(previewEnvelopeJson())
                .build(),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/plain")
                .addHeader("Cache-Control", "no-store")
                .body(previewEnvelopeJson())
                .build(),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .addHeader("Content-Type", "application/json")
                .addHeader("Cache-Control", "no-store")
                .body(previewEnvelopeJson())
                .build(),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "no-store, max-age=0")
                .body(previewEnvelopeJson())
                .build(),
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .addHeader("Cache-Control", "no-store")
                .addHeader("Cache-Control", "no-store")
                .body(previewEnvelopeJson())
                .build(),
        )
        responses.forEach { response ->
            server.enqueue(response)
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { preview() }
            }
        }
    }

    @Test
    fun responseBytesAreBoundedAndMalformedUtf8IsRejected() {
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .addHeader("Cache-Control", "no-store")
                .body("x".repeat(16 * 1024 * 1024 + 1))
                .build(),
        )
        assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
            runBlocking { preview() }
        }

        val invalidUtf8 = Buffer().writeUtf8("{\"preview\":\"").writeByte(0xc3).writeUtf8("\"}")
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "no-store")
                .body(invalidUtf8)
                .build(),
        )
        assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
            runBlocking { preview() }
        }
    }

    @Test
    fun approvalRequiresExactPreviewAndCanonicalOneTimeCapability() {
        server.enqueue(jsonResponse(200, approvalEnvelopeJson()))
        val approval = runBlocking {
            transport.approve(configuration(), ACCOUNT_ID, PREVIEW_ID, PREVIEW_HASH)
        }
        assertEquals(APPROVAL_CAPABILITY, approval.approvalCapability)

        server.enqueue(
            jsonResponse(
                200,
                approvalEnvelopeJson(
                    previewId = "ffffffff-ffff-4fff-8fff-ffffffffffff",
                ),
            ),
        )
        assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.approve(configuration(), ACCOUNT_ID, PREVIEW_ID, PREVIEW_HASH)
            }
        }

        listOf(
            approvalEnvelopeJson(capability = "dw_ga1_${"a".repeat(43)}"),
            approvalEnvelopeJson(expiresAt = "yesterday"),
            approvalEnvelopeJson().replace(
                "\"approval_capability\":\"$APPROVAL_CAPABILITY\"",
                "\"approval_capability\":\"$APPROVAL_CAPABILITY\",\"future\":true",
            ),
        ).forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.approve(configuration(), ACCOUNT_ID, PREVIEW_ID, PREVIEW_HASH)
                }
            }
        }
    }

    @Test
    fun enqueueRequires202AndValidatesTheAcceptanceEnvelope() {
        server.enqueue(jsonResponse(200, acceptedEnvelopeJson()))
        val wrongStatus = assertThrows(GoogleCalendarOutboundApiException.Http::class.java) {
            runBlocking { enqueue() }
        }
        assertEquals(200, wrongStatus.statusCode)

        listOf(
            acceptedEnvelopeJson(outboxId = "00000000-0000-0000-0000-000000000000"),
            acceptedEnvelopeJson().replaceFirst("}", ",\"future\":true}"),
            "{\"outbound\":{\"outbox_id\":\"$OUTBOX_ID\"}}",
        ).forEach { body ->
            server.enqueue(jsonResponse(202, body))
            assertThrows(GoogleCalendarOutboundApiException.InvalidResponse::class.java) {
                runBlocking { enqueue() }
            }
        }
    }

    @Test
    fun eachEndpointRequiresItsExactSuccessStatus() {
        server.enqueue(jsonResponse(202, previewEnvelopeJson()))
        assertEquals(
            202,
            assertThrows(GoogleCalendarOutboundApiException.Http::class.java) {
                runBlocking { preview() }
            }.statusCode,
        )

        server.enqueue(jsonResponse(202, approvalEnvelopeJson()))
        assertEquals(
            202,
            assertThrows(GoogleCalendarOutboundApiException.Http::class.java) {
                runBlocking {
                    transport.approve(configuration(), ACCOUNT_ID, PREVIEW_ID, PREVIEW_HASH)
                }
            }.statusCode,
        )

        server.enqueue(jsonResponse(200, acceptedEnvelopeJson()))
        assertEquals(
            200,
            assertThrows(GoogleCalendarOutboundApiException.Http::class.java) {
                runBlocking { enqueue() }
            }.statusCode,
        )
    }

    @Test
    fun invalidRequestIdentitiesHashesAndCapabilitiesFailBeforeNetworkIo() {
        val invalidUuid = "not-a-uuid"
        val zeroUuid = "00000000-0000-0000-0000-000000000000"
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.preview(configuration(), invalidUuid, COLLECTION_ID, ITEM_ID, 1) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.preview(configuration(), ACCOUNT_ID, zeroUuid, ITEM_ID, 1) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.preview(configuration(), ACCOUNT_ID, COLLECTION_ID, ITEM_ID, 0) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.approve(configuration(), ACCOUNT_ID, PREVIEW_ID, "A".repeat(64))
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.enqueue(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ITEM_ID,
                    ITEM_REVISION,
                    APPROVAL_CAPABILITY + "x",
                )
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun typedFailuresAndDiagnosticsNeverRevealTokensCapabilitiesPayloadsOrIds() {
        listOf(
            401 to GoogleCalendarOutboundApiException.Authentication::class.java,
            404 to GoogleCalendarOutboundApiException.NotFound::class.java,
            409 to GoogleCalendarOutboundApiException.Conflict::class.java,
            422 to GoogleCalendarOutboundApiException.Validation::class.java,
            502 to GoogleCalendarOutboundApiException.Upstream::class.java,
            503 to GoogleCalendarOutboundApiException.Unavailable::class.java,
        ).forEach { (status, expectedClass) ->
            server.enqueue(
                MockResponse.Builder()
                    .code(status)
                    .body("unit-test-secret $APPROVAL_CAPABILITY Private meeting")
                    .build(),
            )
            val error = assertThrows(expectedClass) { runBlocking { preview() } }
            val diagnostic = error.toString()
            assertFalse(diagnostic.contains("unit-test-secret"))
            assertFalse(diagnostic.contains(APPROVAL_CAPABILITY))
            assertFalse(diagnostic.contains("Private meeting"))
        }

        val preview = Json.decodeFromString<RemoteGoogleOutboundPreview>(previewJson())
        val approval = Json.decodeFromString<RemoteGoogleOutboundApproval>(approvalJson())
        val accepted = Json.decodeFromString<RemoteGoogleOutboundAccepted>(acceptedJson())
        val diagnostics = listOf(preview.toString(), approval.toString(), accepted.toString())
        diagnostics.forEach { diagnostic ->
            assertFalse(diagnostic.contains(ACCOUNT_ID))
            assertFalse(diagnostic.contains(COLLECTION_ID))
            assertFalse(diagnostic.contains(ITEM_ID))
            assertFalse(diagnostic.contains(PREVIEW_ID))
            assertFalse(diagnostic.contains(OUTBOX_ID))
            assertFalse(diagnostic.contains(APPROVAL_CAPABILITY))
            assertFalse(diagnostic.contains("Private meeting"))
        }
        assertTrue(approval.toString().contains("<redacted>"))
        assertTrue(preview.toString().contains("<redacted>"))
    }

    private suspend fun preview(): RemoteGoogleOutboundPreview = transport.preview(
        configuration(),
        ACCOUNT_ID,
        COLLECTION_ID,
        ITEM_ID,
        ITEM_REVISION,
    )

    private suspend fun enqueue(): RemoteGoogleOutboundAccepted = transport.enqueue(
        configuration(),
        ACCOUNT_ID,
        COLLECTION_ID,
        ITEM_ID,
        ITEM_REVISION,
        APPROVAL_CAPABILITY,
    )

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun jsonResponse(status: Int, body: String): MockResponse = MockResponse.Builder()
        .code(status)
        .addHeader("Content-Type", "application/json; charset=utf-8")
        .addHeader("Cache-Control", "no-store")
        .body(body)
        .build()

    private fun assertOutboundHeaders(headers: Map<String, List<String>>) {
        assertEquals(listOf("application/json"), headers["Accept"])
        assertEquals(listOf("Bearer unit-test-secret"), headers["Authorization"])
        assertEquals(listOf("no-store"), headers["Cache-Control"])
        assertEquals(listOf("no-cache"), headers["Pragma"])
    }

    private fun stringBody(value: String?): Map<String, String> =
        Json.parseToJsonElement(requireNotNull(value)).jsonObject.mapValues { (_, element) ->
            element.jsonPrimitive.content
        }

    private fun previewEnvelopeJson(
        collectionRevision: Long = 7,
        collectionDisplayName: String = "Primary calendar",
        providerResourceId: String? = null,
        providerEtag: String? = null,
        previewHash: String = PREVIEW_HASH,
        providerPayload: String = PROVIDER_PAYLOAD,
        expiresAt: String = "2026-09-02T12:10:00Z",
    ): String =
        "{\"preview\":" + previewJson(
            collectionRevision,
            collectionDisplayName,
            providerResourceId,
            providerEtag,
            previewHash,
            providerPayload,
            expiresAt,
        ) + "}"

    private fun previewJson(
        collectionRevision: Long = 7,
        collectionDisplayName: String = "Primary calendar",
        providerResourceId: String? = null,
        providerEtag: String? = null,
        previewHash: String = PREVIEW_HASH,
        providerPayload: String = PROVIDER_PAYLOAD,
        expiresAt: String = "2026-09-02T12:10:00Z",
    ): String {
        val resource = providerResourceId?.let { "\"$it\"" } ?: "null"
        val etag = providerEtag?.let { "\"$it\"" } ?: "null"
        return "{\"id\":\"$PREVIEW_ID\",\"account_id\":\"$ACCOUNT_ID\"," +
            "\"collection_id\":\"$COLLECTION_ID\",\"collection_revision\":" +
            "$collectionRevision,\"collection_display_name\":" +
            Json.encodeToString(collectionDisplayName) + ",\"item_id\":\"$ITEM_ID\"," +
            "\"item_revision\":$ITEM_REVISION,\"entity_kind\":\"calendar_event\"," +
            "\"operation\":\"upsert\",\"provider_resource_id\":$resource," +
            "\"provider_etag\":$etag,\"preview_hash\":\"$previewHash\"," +
            "\"provider_payload\":$providerPayload,\"expires_at\":\"$expiresAt\"}"
    }

    private fun approvalEnvelopeJson(
        previewId: String = PREVIEW_ID,
        capability: String = APPROVAL_CAPABILITY,
        expiresAt: String = "2026-09-02T12:09:00Z",
    ): String = "{\"approval\":${approvalJson(previewId, capability, expiresAt)}}"

    private fun approvalJson(
        previewId: String = PREVIEW_ID,
        capability: String = APPROVAL_CAPABILITY,
        expiresAt: String = "2026-09-02T12:09:00Z",
    ): String =
        "{\"preview_id\":\"$previewId\",\"approval_capability\":\"$capability\"," +
            "\"expires_at\":\"$expiresAt\"}"

    private fun acceptedEnvelopeJson(
        outboxId: String = OUTBOX_ID,
        replayed: Boolean = false,
    ): String = "{\"outbound\":${acceptedJson(outboxId, replayed)}}"

    private fun acceptedJson(
        outboxId: String = OUTBOX_ID,
        replayed: Boolean = false,
    ): String = "{\"outbox_id\":\"$outboxId\",\"replayed\":$replayed}"

    private companion object {
        const val ACCOUNT_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val COLLECTION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val ITEM_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val PREVIEW_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val OUTBOX_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        const val ITEM_REVISION = 11L
        val PREVIEW_HASH = "a".repeat(64)
        val APPROVAL_CAPABILITY = "dw_ga1_" + Base64.getUrlEncoder().withoutPadding()
            .encodeToString(ByteArray(32) { index -> (index + 1).toByte() })
        val PROVIDER_EVENT_ID = "d1" + "a".repeat(64)
        const val OWNERSHIP_PROOF = "[server-managed]"
        val PROVIDER_PAYLOAD =
            "{\"id\":\"$PROVIDER_EVENT_ID\",\"etag\":null,\"status\":\"confirmed\"," +
                "\"summary\":\"Private meeting\",\"description\":\"Private notes\"," +
                "\"location\":null,\"start\":{\"date\":null," +
                "\"dateTime\":\"2026-09-02T12:00:00Z\",\"timeZone\":\"UTC\"}," +
                "\"end\":{\"date\":null,\"dateTime\":\"2026-09-02T12:30:00Z\"," +
                "\"timeZone\":\"UTC\"},\"recurringEventId\":null," +
                "\"originalStartTime\":null,\"recurrence\":[]," +
                "\"transparency\":\"opaque\",\"visibility\":\"private\"," +
                "\"eventType\":\"default\",\"attendees\":[]," +
                "\"conferenceData\":null,\"attachments\":[],\"updated\":null," +
                "\"sequence\":null,\"extendedProperties\":{\"private\":{" +
                "\"dayweaveOwnershipProof\":\"$OWNERSHIP_PROOF\"},\"shared\":{}}}"
    }
}
