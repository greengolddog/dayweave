package com.greengolddog.dayweave.network

import java.util.UUID
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class OkHttpGoogleCalendarInboundTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpGoogleCalendarInboundTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpGoogleCalendarInboundTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun listAndDiscoveryUseBoundAuthenticatedNoStoreRequests() = runBlocking {
        repeat(2) { server.enqueue(jsonResponse(200, collectionsJson())) }

        val listed = transport.collections(configuration(), ACCOUNT_ID)
        val discovered = transport.discover(configuration(), ACCOUNT_ID)

        assertEquals(listed, discovered)
        assertEquals(RemoteGoogleCollectionKind.CALENDAR, listed.collections.single().kind)
        assertEquals(RemoteGoogleSyncRole.READ_ONLY, listed.collections.single().syncRole)

        val listRequest = server.takeRequest()
        assertEquals("GET", listRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/collections",
            listRequest.url.encodedPath,
        )
        assertRequestSecurityHeaders(listRequest.headers.toMultimap())

        val discoverRequest = server.takeRequest()
        assertEquals("POST", discoverRequest.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/collections/discover",
            discoverRequest.url.encodedPath,
        )
        assertEquals(0, requireNotNull(discoverRequest.body).size)
        assertRequestSecurityHeaders(discoverRequest.headers.toMultimap())
    }

    @Test
    fun configureSendsIndependentCalendarFlagsEveryRoleAndSanitizedPolicies() = runBlocking {
        val outboundPolicy = RemoteGoogleCalendarPolicy.inboundDefault().copy(
            tentative = RemoteGoogleEventDisposition.BLOCKING,
            allDay = RemoteGoogleEventDisposition.IGNORE,
            publishAllDay = true,
            publishTentative = true,
            publishFree = true,
        )
        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        revision = 8,
                        selected = false,
                        visible = true,
                        policy = policyJson(tentative = "blocking", allDay = "ignore"),
                    ),
                ),
            ),
        )
        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        revision = 9,
                        selected = true,
                        visible = false,
                        syncRole = "blocking",
                    ),
                ),
            ),
        )
        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        revision = 10,
                        selected = true,
                        visible = true,
                        syncRole = "writable",
                        providerAccessRole = "owner",
                        policy = policyJson(
                            tentative = "blocking",
                            allDay = "ignore",
                            publishAllDay = true,
                            publishTentative = true,
                            publishFree = true,
                        ),
                    ),
                ),
            ),
        )

        transport.configure(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ConfigureGoogleCollectionRequest(
                expectedRevision = 7,
                kind = RemoteGoogleCollectionKind.CALENDAR,
                selected = false,
                visible = true,
                syncRole = RemoteGoogleSyncRole.READ_ONLY,
                calendarPolicy = outboundPolicy,
            ),
        )
        transport.configure(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ConfigureGoogleCollectionRequest(
                expectedRevision = 8,
                kind = RemoteGoogleCollectionKind.CALENDAR,
                selected = true,
                visible = false,
                syncRole = RemoteGoogleSyncRole.BLOCKING,
            ),
        )
        transport.configure(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ConfigureGoogleCollectionRequest(
                expectedRevision = 9,
                kind = RemoteGoogleCollectionKind.CALENDAR,
                selected = true,
                visible = true,
                syncRole = RemoteGoogleSyncRole.WRITABLE,
                calendarPolicy = outboundPolicy,
            ),
        )

        val requests = List(3) { server.takeRequest() }
        requests.forEach { request ->
            assertEquals("PUT", request.method)
            assertEquals(
                "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/collections/$COLLECTION_ID",
                request.url.encodedPath,
            )
            assertEquals("application/json; charset=utf-8", request.headers["Content-Type"])
            assertRequestSecurityHeaders(request.headers.toMultimap())
        }
        val bodies = requests.map { request ->
            Json.parseToJsonElement(requireNotNull(request.body).utf8()).jsonObject
        }
        assertEquals("7", bodies[0].getValue("expected_revision").jsonPrimitive.content)
        assertEquals("false", bodies[0].getValue("selected").jsonPrimitive.content)
        assertEquals("true", bodies[0].getValue("visible").jsonPrimitive.content)
        assertEquals("read_only", bodies[0].getValue("sync_role").jsonPrimitive.content)
        assertEquals(
            "false",
            bodies[0].getValue("calendar_policy").jsonObject
                .getValue("publish_all_day").jsonPrimitive.content,
        )
        assertEquals("true", bodies[1].getValue("selected").jsonPrimitive.content)
        assertEquals("false", bodies[1].getValue("visible").jsonPrimitive.content)
        assertEquals("blocking", bodies[1].getValue("sync_role").jsonPrimitive.content)
        assertEquals("writable", bodies[2].getValue("sync_role").jsonPrimitive.content)
        assertEquals(
            "blocking",
            bodies[2].getValue("calendar_policy").jsonObject
                .getValue("tentative").jsonPrimitive.content,
        )
        assertEquals(
            "true",
            bodies[2].getValue("calendar_policy").jsonObject
                .getValue("publish_free").jsonPrimitive.content,
        )
        assertTrue(bodies.all { "kind" !in it })
    }

    @Test
    fun configureSupportsTaskImportAndPublishButAlwaysStripsCalendarPublication() = runBlocking {
        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        kind = "task_list",
                        revision = 8,
                        selected = true,
                        visible = false,
                        syncRole = "writable",
                    ),
                ),
            ),
        )
        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        kind = "task_list",
                        revision = 9,
                        selected = false,
                        visible = true,
                    ),
                ),
            ),
        )

        val enabled = transport.configure(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ConfigureGoogleCollectionRequest(
                expectedRevision = 7,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                selected = true,
                visible = false,
                syncRole = RemoteGoogleSyncRole.WRITABLE,
                calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault().copy(
                    publishFree = true,
                ),
            ),
        )
        val disabled = transport.configure(
            configuration(),
            ACCOUNT_ID,
            COLLECTION_ID,
            ConfigureGoogleCollectionRequest(
                expectedRevision = 8,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                selected = false,
                visible = true,
                syncRole = RemoteGoogleSyncRole.READ_ONLY,
            ),
        )

        assertEquals(RemoteGoogleCollectionKind.TASK_LIST, enabled.kind)
        assertTrue(enabled.selected)
        assertFalse(disabled.selected)
        val bodies = List(2) {
            Json.parseToJsonElement(requireNotNull(server.takeRequest().body).utf8()).jsonObject
        }
        assertEquals("true", bodies[0].getValue("selected").jsonPrimitive.content)
        assertEquals("false", bodies[0].getValue("visible").jsonPrimitive.content)
        assertEquals("writable", bodies[0].getValue("sync_role").jsonPrimitive.content)
        assertEquals(
            "false",
            bodies[0].getValue("calendar_policy").jsonObject
                .getValue("publish_free").jsonPrimitive.content,
        )
        assertEquals("false", bodies[1].getValue("selected").jsonPrimitive.content)
        assertEquals("true", bodies[1].getValue("visible").jsonPrimitive.content)
        assertEquals("read_only", bodies[1].getValue("sync_role").jsonPrimitive.content)
        assertTrue(bodies.all { "kind" !in it })
    }

    @Test
    fun configureRejectsInvalidRolesIdentityAndMismatchedMutationResponseBeforeTrust() {
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = false,
                        syncRole = RemoteGoogleSyncRole.WRITABLE,
                    ),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.TASK_LIST,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.BLOCKING,
                    ),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 0,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    ),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.collections(configuration(), "not-a-uuid") }
        }
        assertEquals(0, server.requestCount)

        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(revision = 9, selected = true, visible = true),
                ),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    ),
                )
            }
        }

        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        kind = "task_list",
                        revision = 8,
                        selected = true,
                    ),
                ),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    ),
                )
            }
        }

        server.enqueue(
            jsonResponse(
                200,
                collectionEnvelopeJson(
                    collectionJson(
                        kind = "task_list",
                        revision = 8,
                        selected = true,
                        syncRole = "writable",
                    ),
                ),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.TASK_LIST,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    ),
                )
            }
        }
        listOf(
            collectionJson(
                id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                revision = 8,
                selected = true,
            ),
            collectionJson(
                accountId = "ffffffff-ffff-4fff-8fff-ffffffffffff",
                revision = 8,
                selected = true,
            ),
            collectionJson(
                revision = 8,
                selected = true,
                policy = policyJson(tentative = "ignore"),
            ),
        ).forEach { mismatchedCollection ->
            server.enqueue(
                jsonResponse(200, collectionEnvelopeJson(mismatchedCollection)),
            )
            assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.configure(
                        configuration(),
                        ACCOUNT_ID,
                        COLLECTION_ID,
                        ConfigureGoogleCollectionRequest(
                            expectedRevision = 7,
                            kind = RemoteGoogleCollectionKind.CALENDAR,
                            selected = true,
                            syncRole = RemoteGoogleSyncRole.READ_ONLY,
                        ),
                    )
                }
            }
        }
        server.enqueue(
            jsonResponse(
                200,
                "{\"collection\":${collectionJson(revision = 8, selected = true)}}",
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.configure(
                    configuration(),
                    ACCOUNT_ID,
                    COLLECTION_ID,
                    ConfigureGoogleCollectionRequest(
                        expectedRevision = 7,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = true,
                        syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    ),
                )
            }
        }
        assertEquals(
            setOf("READ_ONLY", "BLOCKING", "WRITABLE"),
            RemoteGoogleSyncRole.entries.map { it.name }.toSet(),
        )
    }

    @Test
    fun syncStatusDecodesTheFullCausalFenceAndRejectsImpossibleDomains() = runBlocking {
        server.enqueue(jsonResponse(200, syncStatusEnvelopeJson()))
        val status = transport.syncStatus(configuration(), ACCOUNT_ID)

        assertEquals(RemoteGoogleSyncRunState.IDLE, requireNotNull(status.run).state)
        assertEquals(4_294_967_295L, status.run.consecutiveFailures)
        assertEquals(6L, status.run.refreshGeneration)
        assertEquals(5L, status.run.completedRefreshGeneration)
        assertEquals(2L, status.importConflicts)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/sync",
            server.takeRequest().url.encodedPath,
        )

        server.enqueue(
            jsonResponse(
                200,
                syncStatusEnvelopeJson().replace(
                    "\"claimed_refresh_generation\":5",
                    "\"claimed_refresh_generation\":7",
                ),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.syncStatus(configuration(), ACCOUNT_ID) }
        }
        Unit

        server.enqueue(
            jsonResponse(
                200,
                syncStatusEnvelopeJson().replace(
                    "\"consecutive_failures\":4294967295",
                    "\"consecutive_failures\":4294967296",
                ),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.syncStatus(configuration(), ACCOUNT_ID) }
        }
        Unit

        server.enqueue(
            jsonResponse(
                200,
                syncStatusEnvelopeJson().replace("\"import_conflicts\":2", "\"import_conflicts\":-1"),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.syncStatus(configuration(), ACCOUNT_ID) }
        }
        Unit
    }

    @Test
    fun refreshUsesTheRequestUuidAsItsStableBodyIdentityAndRequiresExactEcho() = runBlocking {
        server.enqueue(jsonResponse(202, refreshEnvelopeJson()))
        val accepted = transport.refresh(configuration(), ACCOUNT_ID, REFRESH_ID)

        assertEquals(REFRESH_ID.toString(), accepted.requestId)
        assertEquals(6L, accepted.refreshGeneration)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals(
            "/tenant/v1/integrations/google/accounts/$ACCOUNT_ID/sync/refresh",
            request.url.encodedPath,
        )
        assertEquals(
            REFRESH_ID.toString(),
            Json.parseToJsonElement(requireNotNull(request.body).utf8()).jsonObject
                .getValue("request_id").jsonPrimitive.content,
        )
        assertEquals(null, request.headers["Idempotency-Key"])

        server.enqueue(
            jsonResponse(
                202,
                refreshEnvelopeJson(requestId = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"),
            ),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.refresh(configuration(), ACCOUNT_ID, REFRESH_ID) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { transport.refresh(configuration(), ACCOUNT_ID, UUID(0, 0)) }
        }
        Unit
    }

    @Test
    fun collectionResponsesAreClosedDuplicateSafeAndRequireNullableAndPolicyMembers() {
        val escapedDuplicate =
            "{\"collections\":[],\"\\u0063ollections\":[${collectionJson()}]}"
        val invalidBodies = listOf(
            "{\"collections\":[${collectionJson()}],\"unexpected\":true}",
            "{\"collections\":[${collectionJson().dropLast(1)},\"unexpected\":true}]}",
            escapedDuplicate,
            collectionsJson(
                collectionJson().replace("\"kind\":\"calendar\"", "\"kind\":\"future_kind\""),
            ),
            collectionsJson(
                collectionJson().replace(
                    "\"sync_role\":\"read_only\"",
                    "\"sync_role\":\"future_role\"",
                ),
            ),
            collectionsJson(
                collectionJson().replace(",\"provider_access_role\":null", ""),
            ),
            collectionsJson(
                collectionJson().replace(",\"publish_free\":false", ""),
            ),
        )

        invalidBodies.forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
                runBlocking { transport.collections(configuration(), ACCOUNT_ID) }
            }
        }
    }

    @Test
    fun collectionSemanticBoundsAndResponseHeadersAreFailClosed() {
        val invalidBodies = listOf(
            collectionsJson(
                collectionJson(displayName = "x".repeat(4_097)),
            ),
            collectionsJson(
                collectionJson(kind = "task_list", syncRole = "blocking"),
            ),
            collectionsJson(
                collectionJson(providerDeleted = true, selected = true),
            ),
            collectionsJson(
                collectionJson(
                    syncRole = "writable",
                    providerAccessRole = "reader",
                ),
            ),
            collectionsJson(
                collectionJson(planningGeneration = -1),
            ),
            "{\"collections\":[${collectionJson()},${collectionJson()}]}",
            collectionsJson(
                collectionJson(accountId = "ffffffff-ffff-4fff-8fff-ffffffffffff"),
            ),
        )
        invalidBodies.forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
                runBlocking { transport.collections(configuration(), ACCOUNT_ID) }
            }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body(collectionsJson())
                .build(),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.collections(configuration(), ACCOUNT_ID) }
        }
        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "text/plain")
                .addHeader("Cache-Control", "no-store")
                .body(collectionsJson())
                .build(),
        )
        assertThrows(GoogleCalendarInboundApiException.InvalidResponse::class.java) {
            runBlocking { transport.collections(configuration(), ACCOUNT_ID) }
        }
    }

    @Test
    fun collectionListAcceptsServerRetainedPolicyAndUnselectedWritableStates() = runBlocking {
        val validBodies = listOf(
            collectionsJson(
                collectionJson(
                    syncRole = "writable",
                    selected = false,
                    providerAccessRole = "owner",
                ),
            ),
            collectionsJson(
                collectionJson(
                    kind = "task_list",
                    syncRole = "writable",
                    policy = policyJson(publishFree = true),
                ),
            ),
            collectionsJson(
                collectionJson(
                    syncRole = "read_only",
                    providerAccessRole = "reader",
                    policy = policyJson(publishAllDay = true),
                ),
            ),
        )

        validBodies.forEach { body ->
            server.enqueue(jsonResponse(200, body))
            assertEquals(1, transport.collections(configuration(), ACCOUNT_ID).collections.size)
        }
    }

    @Test
    fun authenticationConflictAndUnavailableErrorsAreTypedAndTokenSafe() {
        listOf(
            401 to GoogleCalendarInboundApiException.Authentication::class.java,
            404 to GoogleCalendarInboundApiException.NotFound::class.java,
            409 to GoogleCalendarInboundApiException.Conflict::class.java,
            502 to GoogleCalendarInboundApiException.Upstream::class.java,
            503 to GoogleCalendarInboundApiException.Unavailable::class.java,
        ).forEach { (status, expectedClass) ->
            server.enqueue(MockResponse.Builder().code(status).build())
            val error = assertThrows(expectedClass) {
                runBlocking { transport.collections(configuration(), ACCOUNT_ID) }
            }
            assertFalse(error.toString().contains("unit-test-secret"))
        }
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun jsonResponse(status: Int, body: String): MockResponse = MockResponse.Builder()
        .code(status)
        .addHeader("Content-Type", "application/json")
        .addHeader("Cache-Control", "no-store")
        .body(body)
        .build()

    private fun assertRequestSecurityHeaders(headers: Map<String, List<String>>) {
        assertEquals(listOf("application/json"), headers["Accept"])
        assertEquals(listOf("Bearer unit-test-secret"), headers["Authorization"])
        assertEquals(listOf("no-store"), headers["Cache-Control"])
        assertEquals(listOf("no-cache"), headers["Pragma"])
    }

    private fun collectionsJson(collection: String = collectionJson()): String =
        "{\"collections\":[$collection]}"

    private fun collectionEnvelopeJson(collection: String): String =
        "{\"collection\":${collection.replace(
            "\"configured_at\":null",
            "\"configured_at\":\"2026-09-01T08:09:00Z\"",
        )}}"

    private fun collectionJson(
        id: String = COLLECTION_ID,
        accountId: String = ACCOUNT_ID,
        kind: String = "calendar",
        displayName: String = "Primary calendar",
        providerAccessRole: String? = null,
        providerDeleted: Boolean = false,
        selected: Boolean = true,
        visible: Boolean = true,
        syncRole: String = "read_only",
        revision: Long = 7,
        planningGeneration: Long = 5,
        policy: String = policyJson(),
    ): String = """
        {"id":"$id","account_id":"$accountId","kind":"$kind","remote_collection_id":"primary","display_name":"$displayName","provider_access_role":${providerAccessRole?.let { "\"$it\"" } ?: "null"},"provider_primary":true,"provider_selected":true,"provider_hidden":false,"provider_deleted":$providerDeleted,"selected":$selected,"visible":$visible,"sync_role":"$syncRole","calendar_policy":$policy,"revision":$revision,"discovered_at":"2026-09-01T08:00:00Z","configured_at":null,"last_import_at":null,"planning_projection_state":"complete","planning_generation":$planningGeneration,"planning_collection_revision":7,"planning_window_start":"2026-08-25T00:00:00Z","planning_window_end":"2026-09-09T00:00:00Z","planning_window_refreshed_at":"2026-09-01T08:10:00Z","created_at":"2026-09-01T08:00:00Z","updated_at":"2026-09-01T08:10:00Z"}
    """.trimIndent()

    private fun policyJson(
        tentative: String = "visible_nonblocking",
        allDay: String = "visible_nonblocking",
        publishAllDay: Boolean = false,
        publishTentative: Boolean = false,
        publishFree: Boolean = false,
    ): String =
        "{\"confirmed_busy\":\"blocking\",\"tentative\":\"$tentative\",\"free\":\"visible_nonblocking\",\"all_day\":\"$allDay\",\"publish_all_day\":$publishAllDay,\"publish_tentative\":$publishTentative,\"publish_free\":$publishFree}"

    private fun syncStatusEnvelopeJson(): String = """
        {"sync":{"run":{"account_id":"$ACCOUNT_ID","state":"idle","requested_at":"2026-09-01T08:00:00Z","started_at":"2026-09-01T08:00:01Z","completed_at":"2026-09-01T08:00:02Z","next_attempt_at":"2026-09-01T08:15:00Z","consecutive_failures":4294967295,"last_error_code":null,"last_error_at":null,"imported_count":8,"updated_count":3,"deleted_count":1,"conflict_count":2,"rejected_count":0,"refresh_generation":6,"claimed_refresh_generation":5,"completed_refresh_generation":5,"revision":9},"import_conflicts":2,"pending_outbound":0,"conflicted_outbound":0,"failed_outbound":0,"last_outbound_error_code":null,"last_outbound_error_at":null,"next_outbound_attempt_at":null}}
    """.trimIndent()

    private fun refreshEnvelopeJson(
        requestId: String = REFRESH_ID.toString(),
    ): String =
        "{\"refresh\":{\"account_id\":\"$ACCOUNT_ID\",\"request_id\":\"$requestId\",\"refresh_generation\":6,\"requested_at\":\"2026-09-01T08:05:00Z\"}}"

    private companion object {
        const val ACCOUNT_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val COLLECTION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val REFRESH_ID: UUID = UUID.fromString("dddddddd-dddd-4ddd-8ddd-dddddddddddd")
    }
}
