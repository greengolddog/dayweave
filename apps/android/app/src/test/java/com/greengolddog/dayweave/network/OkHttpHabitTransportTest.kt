package com.greengolddog.dayweave.network

import java.time.LocalDate
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class OkHttpHabitTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpHabitTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpHabitTransport()
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun listUsesBoundOriginStrictDatesAndAuthoritativeEvidenceIdentity() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"occurrences":[${occurrenceJson()}],"next_cursor":null,"has_more":false}""",
            ),
        )

        val page = transport.listOccurrences(
            configuration(),
            HABIT_ID,
            LocalDate.parse("2026-09-01"),
            LocalDate.parse("2026-09-07"),
        )

        assertEquals(OCCURRENCE_ID, page.occurrences.single().evidence.id)
        assertEquals(PLANNER_OCCURRENCE_ID, page.occurrences.single().evidence.plannerOccurrenceId)
        assertFalse(page.hasMore)
        val request = server.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/tenant/v1/habits/$HABIT_ID/occurrences", request.url.encodedPath)
        assertEquals("2026-09-01", request.url.queryParameter("start_date"))
        assertEquals("2026-09-07", request.url.queryParameter("end_date"))
        assertEquals("25", request.url.queryParameter("limit"))
        assertEquals("Bearer unit-test-secret", request.headers["Authorization"])
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
    }

    @Test
    fun outcomeMutationReplaysExactEncryptedOutboxBodyAndKey() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"occurrence":${occurrenceJson(withOutcome = true)},"replayed":true}""",
                replayed = true,
            ),
        )
        val body = """{"operation_id":"$OPERATION_ID","expected_revision":0,"outcome":{"status":"partial","progress_basis_points":3500,"quantity":7,"unit":"pages","actual_seconds":600,"note":"Good start","occurred_at":"2026-09-01T07:30:00Z"}}"""

        val mutation = transport.putOutcome(
            configuration(),
            HABIT_ID,
            OCCURRENCE_ID,
            OPERATION_ID,
            body,
        )

        assertTrue(mutation.replayed)
        assertEquals(3_500, mutation.value.outcome?.progressBasisPoints)
        val request = server.takeRequest()
        assertEquals("PUT", request.method)
        assertEquals(
            "/tenant/v1/habits/$HABIT_ID/occurrences/$OCCURRENCE_ID",
            request.url.encodedPath,
        )
        assertEquals(OPERATION_ID, request.headers["Idempotency-Key"])
        assertEquals(body, requireNotNull(request.body).utf8())
    }

    @Test
    fun deltaDecodesBothInternallyTaggedChangeKinds() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{
                  "changes":[
                    {"type":"occurrence_upsert","occurrence":${occurrenceJson()}},
                    {"type":"pause_upsert","pause":${pauseJson()}}
                  ],
                  "next_cursor":"42",
                  "has_more":false
                }""".trimIndent(),
            ),
        )

        val page = transport.delta(configuration(), cursor = "39")

        assertTrue(page.changes[0] is RemoteHabitDeltaChange.OccurrenceUpsert)
        assertTrue(page.changes[1] is RemoteHabitDeltaChange.PauseUpsert)
        assertEquals("42", page.nextCursor)
        val request = server.takeRequest()
        assertEquals("/tenant/v1/habits/occurrences/delta", request.url.encodedPath)
        assertEquals("39", request.url.queryParameter("cursor"))
        assertEquals("25", request.url.queryParameter("limit"))
    }

    @Test
    fun deltaRejectsAPageLimitThatCouldExceedTheResponseBudgetBeforeNetwork() {
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.delta(
                    configuration(),
                    limit = MAX_HABIT_RESPONSE_PAGE_LIMIT + 1,
                )
            }
        }

        assertEquals(0, server.requestCount)
    }

    @Test
    fun occurrenceListRejectsAPageLimitThatCouldExceedTheResponseBudgetBeforeNetwork() {
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                    limit = MAX_HABIT_RESPONSE_PAGE_LIMIT + 1,
                )
            }
        }

        assertEquals(0, server.requestCount)
    }

    @Test
    fun pauseMutationsUseDistinctEvidencePathsAndExactBodies() = runBlocking {
        server.enqueue(
            jsonResponse(
                """{"pause":${pauseJson()},"replayed":false}""",
                replayed = false,
            ),
        )
        server.enqueue(
            jsonResponse(
                """{"pause":${pauseJson(endedAt = "2026-09-03T08:00:00Z", revision = 2)},"replayed":false}""",
                replayed = false,
            ),
        )
        val startBody = """{"operation_id":"$OPERATION_ID","pause_id":"$PAUSE_ID","expected_revision":0,"started_at":"2026-09-02T08:00:00Z"}"""
        val resumeBody = """{"operation_id":"$SECOND_OPERATION_ID","expected_revision":1,"ended_at":"2026-09-03T08:00:00Z"}"""

        transport.startPause(configuration(), HABIT_ID, OPERATION_ID, startBody)
        transport.resumePause(
            configuration(),
            HABIT_ID,
            PAUSE_ID,
            SECOND_OPERATION_ID,
            resumeBody,
        )

        val start = server.takeRequest()
        assertEquals("POST", start.method)
        assertEquals("/tenant/v1/habits/$HABIT_ID/pauses", start.url.encodedPath)
        assertEquals(startBody, requireNotNull(start.body).utf8())
        val resume = server.takeRequest()
        assertEquals("/tenant/v1/habits/$HABIT_ID/pauses/$PAUSE_ID/resume", resume.url.encodedPath)
        assertEquals(SECOND_OPERATION_ID, resume.headers["Idempotency-Key"])
        assertEquals(resumeBody, requireNotNull(resume.body).utf8())
    }

    @Test
    fun pauseResponseAllowsAcceptedClientClockSkewAfterServerRecordTime() = runBlocking {
        val futureStartedAt = "2026-09-02T08:04:00Z"
        server.enqueue(
            jsonResponse(
                """{"pause":${pauseJson().replaceFirst("2026-09-02T08:00:00Z", futureStartedAt)},"replayed":false}""",
                replayed = false,
            ),
        )

        val mutation = transport.startPause(
            configuration(),
            HABIT_ID,
            OPERATION_ID,
            """{"operation_id":"$OPERATION_ID"}""",
        )

        assertEquals(futureStartedAt, mutation.value.startedAt)
        assertEquals("2026-09-02T08:00:00Z", mutation.value.createdAt)
    }

    @Test
    fun analyticsDecodesFlattenedTotalsAndSupportiveFactCodes() = runBlocking {
        server.enqueue(jsonResponse("""{"analytics":${analyticsJson()}}"""))

        val analytics = transport.analytics(
            configuration(),
            HABIT_ID,
            LocalDate.parse("2026-08-31"),
            LocalDate.parse("2026-09-06"),
            RemoteHabitAnalyticsBucket.WEEK,
        )

        assertEquals(7L, analytics.expected)
        assertEquals(8_571, analytics.adherenceBasisPoints)
        assertEquals(4, analytics.currentStreak)
        assertEquals(RemoteHabitSupportiveFactCode.ACTIVE_STREAK, analytics.supportiveFactCodes[0])
        val request = server.takeRequest()
        assertEquals("week", request.url.queryParameter("bucket"))
    }

    @Test
    fun analyticsRejectsNonCalendarBucketsAndTrendAggregateMismatch() {
        server.enqueue(
            jsonResponse(
                """{"analytics":${analyticsJson(trendStart = "2026-09-01")}}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.analytics(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-08-31"),
                    LocalDate.parse("2026-09-06"),
                    RemoteHabitAnalyticsBucket.WEEK,
                )
            }
        }

        server.enqueue(
            jsonResponse(
                """{"analytics":${analyticsJson(trendCompleted = 4, trendUnresolved = 1)}}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.analytics(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-08-31"),
                    LocalDate.parse("2026-09-06"),
                    RemoteHabitAnalyticsBucket.WEEK,
                )
            }
        }
    }

    @Test
    fun skippedEvidenceAndAnalyticsRetainLegitimateSignedQuantities() = runBlocking {
        val skipped = occurrenceJson(withOutcome = true)
            .replace("\"status\":\"partial\"", "\"status\":\"skipped\"")
            .replace("\"progress_basis_points\":3500", "\"progress_basis_points\":2500")
            .replace("\"quantity\":7", "\"quantity\":-7")
        server.enqueue(
            jsonResponse(
                """{"occurrences":[$skipped],"next_cursor":null,"has_more":false}""",
            ),
        )
        server.enqueue(
            jsonResponse(
                """{"analytics":${analyticsJson().replace("\"amount\":105", "\"amount\":-105")}}""",
            ),
        )

        val occurrence = transport.listOccurrences(
            configuration(),
            HABIT_ID,
            LocalDate.parse("2026-09-01"),
            LocalDate.parse("2026-09-07"),
        ).occurrences.single()
        val analytics = transport.analytics(
            configuration(),
            HABIT_ID,
            LocalDate.parse("2026-08-31"),
            LocalDate.parse("2026-09-06"),
            RemoteHabitAnalyticsBucket.WEEK,
        )

        assertEquals(RemoteHabitOutcomeStatus.SKIPPED, occurrence.outcome?.status)
        assertEquals(2_500, occurrence.outcome?.progressBasisPoints)
        assertEquals(-7L, occurrence.outcome?.quantity)
        assertEquals(-105L, analytics.quantityTotals.single().amount)
    }

    @Test
    fun responseTextLimitsCountUnicodeScalarsRatherThanUtf16CodeUnits() = runBlocking {
        val note = "😀".repeat(10_000)
        val occurrence = occurrenceJson(withOutcome = true)
            .replace("\"note\":\"Good start\"", "\"note\":\"$note\"")
        server.enqueue(
            jsonResponse(
                """{"occurrences":[$occurrence],"next_cursor":null,"has_more":false}""",
            ),
        )

        val page = transport.listOccurrences(
            configuration(),
            HABIT_ID,
            LocalDate.parse("2026-09-01"),
            LocalDate.parse("2026-09-07"),
        )

        assertEquals(note, page.occurrences.single().outcome?.note)
    }

    @Test
    fun unknownMissingOrInconsistentResponseFieldsFailClosed() {
        server.enqueue(
            jsonResponse(
                """{"occurrences":[${occurrenceJson().dropLast(1)},"future":true}],"next_cursor":null,"has_more":false}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }

        server.enqueue(
            jsonResponse(
                """{"occurrences":[${occurrenceJson().replace(Regex(",\\s*\"outcome\":null"), "")}],"next_cursor":null,"has_more":false}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }

        server.enqueue(
            jsonResponse(
                """{"occurrences":[${occurrenceJson(withOutcome = true).replace("\"progress_basis_points\":3500", "\"progress_basis_points\":10000")}],"next_cursor":null,"has_more":false}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }
    }

    @Test
    fun occurrenceEvidenceRequiresPlannerUuidV5ExactIdentityAndMicroseconds() {
        val invalidOccurrences = listOf(
            occurrenceJson().replace(
                PLANNER_OCCURRENCE_ID,
                "33333333-3333-4333-8333-333333333333",
            ),
            occurrenceJson().replace(
                "\"id\":\"$OCCURRENCE_ID\"",
                "\"id\":\"$PLANNER_OCCURRENCE_ID\"",
            ),
            occurrenceJson().replace(
                "{\"type\":\"calendar_day\",\"date\":\"2026-09-01\",\"bucket_ordinal\":0}",
                "{\"type\":\"custom\"}",
            ),
            occurrenceJson().replace(
                "{\"type\":\"calendar_day\",\"date\":\"2026-09-01\",\"bucket_ordinal\":0}",
                "{\"type\":\"rolling_minutes\",\"index\":4294967296,\"anchor\":\"2026-09-01T07:00:00Z\"}",
            ),
            occurrenceJson().replace(
                "{\"type\":\"calendar_day\",\"date\":\"2026-09-01\",\"bucket_ordinal\":0}",
                "{\"type\":\"rolling_month\",\"cycle\":2147483648,\"index\":0,\"anchor\":\"2026-09-01T07:00:00Z\"}",
            ),
            occurrenceJson().replace(
                "\"date\":\"2026-09-01\"",
                "\"date\":\"2026-09-02\"",
            ),
            occurrenceJson(withOutcome = true).replace(
                "\"updated_at\":\"2026-09-01T07:31:00Z\"",
                "\"updated_at\":\"2026-09-01T07:31:00.000000001Z\"",
            ),
        )

        invalidOccurrences.forEach { occurrence ->
            server.enqueue(
                jsonResponse(
                    """{"occurrences":[$occurrence],"next_cursor":null,"has_more":false}""",
                ),
            )
            assertThrows(HabitApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.listOccurrences(
                        configuration(),
                        HABIT_ID,
                        LocalDate.parse("2026-09-01"),
                        LocalDate.parse("2026-09-07"),
                    )
                }
            }
        }
    }

    @Test
    fun occurrenceEvidenceRequiresRfcUuidVariantBoundedInstantsAndIanaTimezone() {
        val endpointYearEvidence = occurrenceJson()
            .replace(
                "\"window_start\":\"2026-09-01T06:00:00Z\"",
                "\"window_start\":\"0001-01-01T00:00:00Z\"",
            )
            .replace(
                "\"window_end\":\"2026-09-01T09:00:00Z\"",
                "\"window_end\":\"9999-12-31T23:59:59.999999Z\"",
            )
        server.enqueue(
            jsonResponse(
                """{"occurrences":[$endpointYearEvidence],"next_cursor":null,"has_more":false}""",
            ),
        )
        val endpointPage = runBlocking {
            transport.listOccurrences(
                configuration(),
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-07"),
            )
        }
        assertEquals("0001-01-01T00:00:00Z", endpointPage.occurrences.single().evidence.windowStart)

        val invalidOccurrences = listOf(
            occurrenceJson().replace(
                PLANNER_OCCURRENCE_ID,
                "33333333-3333-5333-0333-333333333333",
            ),
            occurrenceJson().replace(
                "{\"type\":\"calendar_day\",\"date\":\"2026-09-01\",\"bucket_ordinal\":0}",
                "{\"type\":\"custom_rule\",\"rule_id\":\"aaaaaaaa-aaaa-5aaa-0aaa-aaaaaaaaaaaa\",\"sequence\":0,\"date\":\"2026-09-01\"}",
            ),
            occurrenceJson().replace(
                "\"window_start\":\"2026-09-01T06:00:00Z\"",
                "\"window_start\":\"0000-01-01T00:00:00Z\"",
            ),
            occurrenceJson().replace(
                "\"window_end\":\"2026-09-01T09:00:00Z\"",
                "\"window_end\":\"+10000-01-01T00:00:00Z\"",
            ),
            occurrenceJson().replace(
                "\"timezone_name\":\"Europe/Paris\"",
                "\"timezone_name\":\"+02:00\"",
            ),
            occurrenceJson().replace(
                "\"timezone_name\":\"Europe/Paris\"",
                "\"timezone_name\":\"SystemV/EST5\"",
            ),
        )

        invalidOccurrences.forEach { occurrence ->
            server.enqueue(
                jsonResponse(
                    """{"occurrences":[$occurrence],"next_cursor":null,"has_more":false}""",
                ),
            )
            assertThrows(HabitApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.listOccurrences(
                        configuration(),
                        HABIT_ID,
                        LocalDate.parse("2026-09-01"),
                        LocalDate.parse("2026-09-07"),
                    )
                }
            }
        }
    }

    @Test
    fun rawHabitResponsesRequireCanonicalIntegerLexemesAndIdentityAnchors() {
        val signedIntegerOccurrence = occurrenceJson(withOutcome = true).replace(
            "\"quantity\":7",
            "\"quantity\":-7",
        )
        server.enqueue(
            jsonResponse(
                """{"occurrences":[$signedIntegerOccurrence],"next_cursor":null,"has_more":false}""",
            ),
        )
        val signedIntegerPage = runBlocking {
            transport.listOccurrences(
                configuration(),
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-07"),
            )
        }
        assertEquals(-7L, signedIntegerPage.occurrences.single().outcome?.quantity)

        val invalidOccurrences = listOf("-0", "0.0", "0e0").map { token ->
            occurrenceJson(withOutcome = true).replace(
                "\"actual_seconds\":600",
                "\"actual_seconds\":$token",
            )
        } + listOf(
            "2026-09-01T07:00:00.123400Z",
            "2026-09-01T07:00:00+00:00",
            "2026-09-01T07:00:00-00:00",
            "2026-09-01t07:00:00Z",
            "2026-09-01T07:00:00z",
        ).map { anchor ->
            occurrenceJson().replace(
                "{\"type\":\"calendar_day\",\"date\":\"2026-09-01\",\"bucket_ordinal\":0}",
                "{\"type\":\"after_completion\",\"anchor\":\"$anchor\"}",
            )
        }

        invalidOccurrences.forEach { occurrence ->
            server.enqueue(
                jsonResponse(
                    """{"occurrences":[$occurrence],"next_cursor":null,"has_more":false}""",
                ),
            )
            assertThrows(HabitApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.listOccurrences(
                        configuration(),
                        HABIT_ID,
                        LocalDate.parse("2026-09-01"),
                        LocalDate.parse("2026-09-07"),
                    )
                }
            }
        }
    }

    @Test
    fun deltaHabitEvidenceDateHorizonIsInclusiveAndRejectsOutsideYears() {
        listOf(1900, 2200).forEach { year ->
            server.enqueue(
                jsonResponse(
                    """{
                      "changes":[
                        {"type":"occurrence_upsert","occurrence":${occurrenceJsonForYear(year)}}
                      ],
                      "next_cursor":"42",
                      "has_more":false
                    }""".trimIndent(),
                ),
            )

            val page = runBlocking { transport.delta(configuration()) }

            val change = page.changes.single() as RemoteHabitDeltaChange.OccurrenceUpsert
            assertEquals("$year-09-01", change.occurrence.evidence.localDate)
        }

        listOf(1899, 2201).forEach { year ->
            server.enqueue(
                jsonResponse(
                    """{
                      "changes":[
                        {"type":"occurrence_upsert","occurrence":${occurrenceJsonForYear(year)}}
                      ],
                      "next_cursor":"42",
                      "has_more":false
                    }""".trimIndent(),
                ),
            )

            assertThrows(HabitApiException.InvalidResponse::class.java) {
                runBlocking { transport.delta(configuration()) }
            }
        }
    }

    @Test
    fun malformedRemoteDatesAndInstantsAreTypedAsProtocolFailures() {
        val malformedOccurrences = listOf(
            occurrenceJson().replace(
                "\"nominal_start\":\"2026-09-01T07:00:00Z\"",
                "\"nominal_start\":\"not-an-instant\"",
            ),
            occurrenceJson().replace(
                "\"local_date\":\"2026-09-01\"",
                "\"local_date\":\"not-a-date\"",
            ),
        )

        malformedOccurrences.forEach { occurrence ->
            server.enqueue(
                jsonResponse(
                    """{"occurrences":[$occurrence],"next_cursor":null,"has_more":false}""",
                ),
            )
            assertThrows(HabitApiException.InvalidResponse::class.java) {
                runBlocking {
                    transport.listOccurrences(
                        configuration(),
                        HABIT_ID,
                        LocalDate.parse("2026-09-01"),
                        LocalDate.parse("2026-09-07"),
                    )
                }
            }
        }
    }

    @Test
    fun occurrencePageRejectsRowsOutsideRequestedWindowOrOutOfServerOrder() {
        server.enqueue(
            jsonResponse(
                """{"occurrences":[${occurrenceJson().replace("2026-09-01", "2026-09-08")}],"next_cursor":null,"has_more":false}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }

        val later = occurrenceJson()
            .replace(OCCURRENCE_ID, SECOND_OCCURRENCE_ID)
            .replace(PLANNER_OCCURRENCE_ID, SECOND_PLANNER_OCCURRENCE_ID)
            .replace("2026-09-01", "2026-09-02")
        server.enqueue(
            jsonResponse(
                """{"occurrences":[$later,${occurrenceJson()}],"next_cursor":null,"has_more":false}""",
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }
    }

    @Test
    fun duplicateKeysPrivateHeadersAndReplayEvidenceFailClosed() {
        val escapedDuplicate =
            """{"occurrences":[],"next_cursor":null,"has_more":false,"has_m\u006fre":false}"""
        server.enqueue(jsonResponse(escapedDuplicate))
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(200)
                .addHeader("Content-Type", "application/json")
                .body("""{"occurrences":[],"next_cursor":null,"has_more":false}""")
                .build(),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }

        server.enqueue(
            jsonResponse(
                """{"occurrence":${occurrenceJson(withOutcome = true)},"replayed":true}""",
                replayed = false,
            ),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.putOutcome(
                    configuration(),
                    HABIT_ID,
                    OCCURRENCE_ID,
                    OPERATION_ID,
                    """{"operation_id":"$OPERATION_ID"}""",
                )
            }
        }
    }

    @Test
    fun invalidLocalIdentityAndRangesFailBeforeNetworkIo() {
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    "not-a-uuid",
                    LocalDate.parse("2026-09-01"),
                    LocalDate.parse("2026-09-07"),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2026-09-07"),
                    LocalDate.parse("2026-09-01"),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.listOccurrences(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("1899-12-31"),
                    LocalDate.parse("1900-01-01"),
                )
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                transport.analytics(
                    configuration(),
                    HABIT_ID,
                    LocalDate.parse("2200-12-31"),
                    LocalDate.parse("2201-01-01"),
                    RemoteHabitAnalyticsBucket.DAY,
                )
            }
        }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun typedErrorsAndOversizedResponsesNeverExposeBearer() {
        server.enqueue(errorResponse(401, "unauthorized"))
        val authentication = assertThrows(HabitApiException.Authentication::class.java) {
            runBlocking { transport.delta(configuration()) }
        }
        assertFalse(authentication.toString().contains("unit-test-secret"))

        server.enqueue(errorResponse(409, "conflict"))
        assertThrows(HabitApiException.Conflict::class.java) {
            runBlocking { transport.delta(configuration()) }
        }

        server.enqueue(
            MockResponse.Builder()
                .code(409)
                .addHeader("Content-Type", "application/json")
                .body("""{"error":{"code":"conflict","message":"stale"}}""")
                .build(),
        )
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking { transport.delta(configuration()) }
        }

        server.enqueue(jsonResponse("{" + "x".repeat(2_100_000) + "}"))
        assertThrows(HabitApiException.InvalidResponse::class.java) {
            runBlocking { transport.delta(configuration()) }
        }
    }

    private fun configuration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createForLoopbackTest(
            server.url("/tenant/").toString(),
            "unit-test-secret",
        )

    private fun jsonResponse(body: String, replayed: Boolean? = null): MockResponse =
        MockResponse.Builder()
            .code(200)
            .addHeader("Content-Type", "application/json")
            .addHeader("Cache-Control", "no-store, max-age=0")
            .addHeader("Pragma", "no-cache")
            .apply {
                replayed?.let { addHeader("Idempotency-Replayed", it.toString()) }
            }
            .body(body)
            .build()

    private fun errorResponse(status: Int, code: String): MockResponse =
        MockResponse.Builder()
            .code(status)
            .addHeader("Content-Type", "application/json")
            .addHeader("Cache-Control", "no-store, max-age=0")
            .addHeader("Pragma", "no-cache")
            .body("""{"error":{"code":"$code","message":"test error"}}""")
            .build()

    private fun occurrenceJson(withOutcome: Boolean = false): String = """
        {
          "evidence":{
            "id":"$OCCURRENCE_ID",
            "habit_id":"$HABIT_ID",
            "planner_occurrence_id":"$PLANNER_OCCURRENCE_ID",
            "source_schedule_revision_id":"$SCHEDULE_REVISION_ID",
            "source_item_revision":7,
            "policy_fingerprint":"sha256:${"a".repeat(64)}",
            "identity":{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0},
            "nominal_start":"2026-09-01T07:00:00Z",
            "nominal_end":"2026-09-01T07:30:00Z",
            "window_start":"2026-09-01T06:00:00Z",
            "window_end":"2026-09-01T09:00:00Z",
            "local_date":"2026-09-01",
            "timezone_name":"Europe/Paris",
            "expected_duration_seconds":1800,
            "expected_quantity":20,
            "expected_unit":"pages"
          },
          "outcome":${if (withOutcome) outcomeJson() else "null"}
        }
    """.trimIndent()

    private fun occurrenceJsonForYear(year: Int): String =
        occurrenceJson().replace("2026-09-01", "$year-09-01")

    private fun outcomeJson(): String = """
        {
          "revision":1,
          "status":"partial",
          "progress_basis_points":3500,
          "quantity":7,
          "unit":"pages",
          "actual_seconds":600,
          "note":"Good start",
          "occurred_at":"2026-09-01T07:30:00Z",
          "updated_at":"2026-09-01T07:31:00Z"
        }
    """.trimIndent()

    private fun pauseJson(
        endedAt: String? = null,
        revision: Long = 1,
    ): String = """
        {
          "id":"$PAUSE_ID",
          "habit_id":"$HABIT_ID",
          "revision":$revision,
          "started_at":"2026-09-02T08:00:00Z",
          "ended_at":${endedAt?.let { "\"$it\"" } ?: "null"},
          "preserves_streak":true,
          "created_at":"2026-09-02T08:00:00Z",
          "updated_at":"${endedAt ?: "2026-09-02T08:00:00Z"}"
        }
    """.trimIndent()

    private fun analyticsJson(
        trendStart: String = "2026-08-31",
        trendCompleted: Long = 5,
        trendUnresolved: Long = 0,
    ): String = """
        {
          "habit_id":"$HABIT_ID",
          "start_date":"2026-08-31",
          "end_date":"2026-09-06",
          "bucket":"week",
          "expected":7,
          "eligible":7,
          "completed":5,
          "partial":1,
          "skipped":0,
          "missed":1,
          "excused":0,
          "unresolved":0,
          "adherence_basis_points":8571,
          "actual_seconds_total":9000,
          "quantity_totals":[{"unit":"pages","amount":105}],
          "current_streak":4,
          "longest_streak":9,
          "trends":[{
            "start_date":"$trendStart",
            "end_date":"2026-09-06",
            "expected":7,
            "eligible":7,
            "completed":$trendCompleted,
            "partial":1,
            "skipped":0,
            "missed":1,
            "excused":0,
            "unresolved":$trendUnresolved,
            "adherence_basis_points":8571,
            "actual_seconds_total":9000,
            "quantity_totals":[{"unit":"pages","amount":105}]
          }],
          "supportive_fact_codes":["active_streak","strong_adherence","fresh_start_available"]
        }
    """.trimIndent()

    private companion object {
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PAUSE_ID = "55555555-5555-4555-8555-555555555555"
        const val OPERATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SECOND_OPERATION_ID = "77777777-7777-4777-8777-777777777777"
        const val SECOND_OCCURRENCE_ID = "88888888-8888-4888-8888-888888888888"
        const val SECOND_PLANNER_OCCURRENCE_ID = "99999999-9999-5999-8999-999999999999"
    }
}
