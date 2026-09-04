package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.RemotePlanOccurrence
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RecurrenceIdentityTest {
    @Test
    fun everyTypedIdentityVariantIsAcceptedWithoutRewritingOffsets() {
        val identities = listOf(
            json("""{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":2}"""),
            json("""{"type":"calendar_week","week_key":2461284,"bucket_ordinal":3}"""),
            json(
                """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":4}""",
            ),
            json(
                """{"type":"rolling_minutes","index":5,"anchor":"2026-08-31T23:00:00.123456+02:00"}""",
            ),
            json(
                """{"type":"rolling_minutes","index":4294967295,"anchor":"2026-08-31T23:00:00Z"}""",
            ),
            json(
                """{"type":"after_completion","anchor":"2026-08-31T23:00:00+02:00"}""",
            ),
            json(
                """{"type":"rolling_month","cycle":6,"index":7,"anchor":"2026-08-31T23:00:00+02:00"}""",
            ),
            json(
                """{"type":"rolling_month","cycle":2147483647,"index":65534,"anchor":"2026-08-31T23:00:00Z"}""",
            ),
            json("""{"type":"custom"}"""),
            json(
                """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":8,"date":"2026-09-01"}""",
            ),
        )

        identities.forEach { identity ->
            val persisted = validatedRecurrenceIdentityJson(identity)
            assertEquals(identity.toString(), persisted)
            assertEquals(identity, recurrenceIdentityObject(persisted))
        }
        assertTrue(requireNotNull(identities[3]["anchor"]).toString().contains("+02:00"))
    }

    @Test
    fun schedulerEmissionBoundsAcceptTheirInclusiveMaxima() {
        listOf(
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":65534}""",
            """{"type":"calendar_week","week_key":2461284,"bucket_ordinal":65534}""",
            """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":65534}""",
            """{"type":"rolling_month","cycle":2147483647,"index":65534,"anchor":"0001-01-01T00:00:00Z"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":9999,"date":"2026-09-01"}""",
            """{"type":"after_completion","anchor":"9999-12-31T23:59:59.999999Z"}""",
        ).forEach { raw ->
            val identity = json(raw)
            assertEquals(identity.toString(), validatedRecurrenceIdentityJson(identity))
        }
    }

    @Test
    fun integerIdentityFieldsRequireCanonicalBaseTenLexemes() {
        val signedWeek = json(
            """{"type":"calendar_week","week_key":-2147483648,"bucket_ordinal":0}""",
        )
        assertEquals(signedWeek.toString(), validatedRecurrenceIdentityJson(signedWeek))

        listOf("-0", "0.0", "0e0").forEach { token ->
            val identity = json(
                """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":$token}""",
            )
            assertNull(validatedRecurrenceIdentityJson(identity))
            assertNull(recurrenceIdentityObject(identity.toString()))
        }
    }

    @Test
    fun recurrenceAnchorsRequireCanonicalRustRfc3339Serialization() {
        listOf(
            "2026-08-31T23:00:00Z",
            "2026-08-31T23:00:00.1234+02:00",
            "2026-08-31T23:00:00.000001-02:30",
        ).forEach { anchor ->
            val identity = json("""{"type":"after_completion","anchor":"$anchor"}""")
            assertEquals(identity.toString(), validatedRecurrenceIdentityJson(identity))
        }

        listOf(
            "2026-08-31T23:00:00.123400+02:00",
            "2026-08-31T23:00:00.000000Z",
            "2026-08-31T23:00:00+00:00",
            "2026-08-31T23:00:00-00:00",
            "2026-08-31t23:00:00Z",
            "2026-08-31T23:00:00z",
            "2026-08-31T23:00:00+02",
        ).forEach { anchor ->
            val identity = json("""{"type":"after_completion","anchor":"$anchor"}""")
            assertNull(validatedRecurrenceIdentityJson(identity))
            assertNull(recurrenceIdentityObject(identity.toString()))
        }
    }

    @Test
    fun missingUnknownOrMalformedIdentityIsRejected() {
        listOf(
            "{}",
            """{"type":"future_identity"}""",
            """{"type":"custom","unexpected":true}""",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":65535}""",
            """{"type":"calendar_week","week_key":2461284,"bucket_ordinal":65535}""",
            """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":65535}""",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":65536}""",
            """{"type":"calendar_month","year":2026,"month":13,"bucket_ordinal":0}""",
            """{"type":"rolling_minutes","index":0,"anchor":"tomorrow"}""",
            """{"type":"rolling_minutes","index":0,"anchor":"0000-01-01T00:00:00Z"}""",
            """{"type":"rolling_minutes","index":-1,"anchor":"2026-08-31T23:00:00Z"}""",
            """{"type":"rolling_minutes","index":4294967296,"anchor":"2026-08-31T23:00:00Z"}""",
            """{"type":"after_completion","anchor":"2026-08-31T23:00:00.123456789+02:00"}""",
            """{"type":"after_completion","anchor":"0000-01-01T00:00:00Z"}""",
            """{"type":"rolling_month","cycle":-1,"index":0,"anchor":"2026-08-31T23:00:00Z"}""",
            """{"type":"rolling_month","cycle":2147483648,"index":0,"anchor":"2026-08-31T23:00:00Z"}""",
            """{"type":"rolling_month","cycle":0,"index":65535,"anchor":"2026-08-31T23:00:00Z"}""",
            """{"type":"rolling_month","cycle":0,"index":0,"anchor":"0000-01-01T00:00:00Z"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","sequence":8,"date":"2026-09-01"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-0aaa-aaaaaaaaaaaa","sequence":8,"date":"2026-09-01"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":-1,"date":"2026-09-01"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":10000,"date":"2026-09-01"}""",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":8,"date":"tomorrow"}""",
        ).forEach { raw ->
            assertNull(validatedRecurrenceIdentityJson(json(raw)))
        }
        assertNull(recurrenceIdentityObject(" {\"type\":\"custom\"}"))

        val missingIdentity = """
            {
              "id":"44444444-4444-5444-8444-444444444444",
              "series_item_id":"11111111-1111-4111-8111-111111111111",
              "nominal_start":"2026-09-01T09:00:00+02:00",
              "nominal_end":"2026-09-01T10:00:00+02:00",
              "window_start":"2026-09-01T09:00:00+02:00",
              "window_end":"2026-09-01T10:00:00+02:00",
              "local_date":"2026-09-01",
              "ordinal":0,
              "state":"generated"
            }
        """.trimIndent()
        val failure = runCatching {
            Json.decodeFromString<RemotePlanOccurrence>(missingIdentity)
        }.exceptionOrNull()
        assertNotNull(failure)
        assertTrue(failure is SerializationException)
    }

    @Test
    fun sourceEnvelopeValidatesStableOrdinalLocalDateAndRawOffset() {
        val item = canonicalItem()
        val valid = RecurrenceOccurrenceSourceSnapshot(
            itemId = item.id,
            itemRevision = item.revision,
            identityJson =
                """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":2}""",
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 2,
        )

        assertTrue(valid.hasValidRecurrenceSourceFor(item))
        assertEquals("2026-09-01T09:00:00+02:00", valid.nominalStart)
        assertTrue(!valid.copy(ordinal = 1).hasValidRecurrenceSourceFor(item))
        assertTrue(!valid.copy(localDate = null).hasValidRecurrenceSourceFor(item))
        assertTrue(
            !valid.copy(identityJson = """{"type":"custom"}""")
                .hasValidRecurrenceSourceFor(item),
        )
    }

    @Test
    fun sourceLocalDateUsesNominalTimestampOffsetNotSeriesTimezone() {
        val item = canonicalItem().copy(timezoneName = "America/Los_Angeles")
        val source = RecurrenceOccurrenceSourceSnapshot(
            itemId = item.id,
            itemRevision = item.revision,
            identityJson =
                """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
            nominalStart = "2026-09-01T00:00:00Z",
            nominalEnd = "2026-09-01T01:00:00Z",
            localDate = "2026-09-01",
            ordinal = 0,
        )

        assertTrue(source.hasValidRecurrenceSourceFor(item))
    }

    @Test
    fun weeklySourceRequiresAndValidatesEmbeddedOffsetLocalDate() {
        val item = canonicalItem().copy(
            timezoneName = "America/Los_Angeles",
            recurrenceJson =
                """{"type":"weekly","times_per_week":4,"weekdays":[]}""",
        )
        val source = RecurrenceOccurrenceSourceSnapshot(
            itemId = item.id,
            itemRevision = item.revision,
            identityJson =
                """{"type":"calendar_week","week_key":2461284,"bucket_ordinal":3}""",
            nominalStart = "2026-09-01T00:00:00Z",
            nominalEnd = "2026-09-01T01:00:00Z",
            localDate = "2026-09-01",
            ordinal = 3,
        )

        assertTrue(source.hasValidRecurrenceSourceFor(item))
        assertTrue(!source.copy(localDate = null).hasValidRecurrenceSourceFor(item))
        assertTrue(
            !source.copy(localDate = "2026-08-31").hasValidRecurrenceSourceFor(item),
        )
        assertTrue(
            !source.copy(
                identityJson =
                    """{"type":"calendar_week","week_key":2461277,"bucket_ordinal":3}""",
            ).hasValidRecurrenceSourceFor(item),
        )
    }

    @Test
    fun customRuleSourceRequiresItsExactGeneratedDate() {
        val item = canonicalItem().copy(
            recurrenceJson = """{"type":"custom","rrule":"FREQ=DAILY;COUNT=10"}""",
        )
        val source = RecurrenceOccurrenceSourceSnapshot(
            itemId = item.id,
            itemRevision = item.revision,
            identityJson =
                """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":3,"date":"2026-09-01"}""",
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 3,
        )

        assertTrue(source.hasValidRecurrenceSourceFor(item))
        assertTrue(
            !source.copy(
                identityJson = requireNotNull(source.identityJson)
                    .replace("2026-09-01", "2026-09-02"),
            ).hasValidRecurrenceSourceFor(item),
        )
        assertTrue(
            source.copy(
                nominalStart = "2026-09-01T23:00:00+02:00",
                nominalEnd = "2026-09-02T00:00:00+02:00",
            ).hasValidRecurrenceSourceFor(item),
        )
        assertTrue(
            !source.copy(
                nominalStart = "2026-09-01T23:30:00+02:00",
                nominalEnd = "2026-09-02T00:30:00+02:00",
            ).hasValidRecurrenceSourceFor(item),
        )
    }

    @Test
    fun everyRecurrenceFamilyAuthorizesItsExpectedIdentityKind() {
        recurrenceIdentityCompatibilityCases().forEach { recurrenceCase ->
            val identity = identityFixtures().getValue(recurrenceCase.expectedIdentityType)
            val item = canonicalItem().copy(recurrenceJson = recurrenceCase.recurrenceJson)

            assertTrue(source(identity, item).hasValidRecurrenceSourceFor(item))
        }
    }

    @Test
    fun recurrenceRulesRejectMismatchedKindsAndScopeLegacyCustomToCustomSeries() {
        val identities = identityFixtures()
        recurrenceIdentityCompatibilityCases().forEach { recurrenceCase ->
            val mismatched = identities.values.first {
                it.type != recurrenceCase.expectedIdentityType
            }
            val item = canonicalItem().copy(recurrenceJson = recurrenceCase.recurrenceJson)

            assertTrue(!source(mismatched, item).hasValidRecurrenceSourceFor(item))
        }

        val customItem = canonicalItem().copy(
            recurrenceJson = """{"type":"custom","rrule":"FREQ=DAILY;COUNT=10"}""",
        )
        val legacyCustom = IdentityFixture("custom", """{"type":"custom"}""", null, 0)
        assertTrue(source(legacyCustom, customItem).hasValidRecurrenceSourceFor(customItem))
        val dailyItem = canonicalItem()
        assertTrue(!source(legacyCustom, dailyItem).hasValidRecurrenceSourceFor(dailyItem))
    }

    @Test
    fun boundedRecurrenceSelectorsRejectOrdinalEqualToTheirCount() {
        boundedRecurrenceCases().forEach { recurrenceCase ->
            val item = canonicalItem().copy(recurrenceJson = recurrenceCase.recurrenceJson)
            val lastValidIdentity = boundedIdentity(
                recurrenceCase.identityType,
                recurrenceCase.upperBoundExclusive - 1,
            )
            val firstInvalidIdentity = boundedIdentity(
                recurrenceCase.identityType,
                recurrenceCase.upperBoundExclusive,
            )

            assertTrue(source(lastValidIdentity, item).hasValidRecurrenceSourceFor(item))
            assertTrue(!source(firstInvalidIdentity, item).hasValidRecurrenceSourceFor(item))
        }
    }

    @Test
    fun rollingDayAndWeekFrequencyKeepTheirGlobalIndexUnboundedByTarget() {
        val identity = IdentityFixture(
            "rolling_minutes",
            """{"type":"rolling_minutes","index":7,"anchor":"2026-08-01T00:00:00Z"}""",
            null,
            7,
        )
        listOf("day", "week").forEach { period ->
            val recurrenceCase = frequencyCase(period, "rolling", "rolling_minutes")
            val item = canonicalItem().copy(recurrenceJson = recurrenceCase.recurrenceJson)

            assertTrue(source(identity, item).hasValidRecurrenceSourceFor(item))
        }
    }

    @Test
    fun everyIntervalUsesCoreSignedIndexRangeWhileRollingFrequencyKeepsUIntRange() {
        fun rollingIdentity(index: Long) = IdentityFixture(
            "rolling_minutes",
            """{"type":"rolling_minutes","index":$index,"anchor":"2026-08-01T00:00:00Z"}""",
            null,
            index,
        )

        val intervalItem = canonicalItem().copy(
            recurrenceJson = """{"type":"every_interval","interval":60}""",
        )
        val lastIntervalIdentity = rollingIdentity(Int.MAX_VALUE.toLong())
        val firstOverflowIdentity = rollingIdentity(Int.MAX_VALUE.toLong() + 1)
        assertTrue(source(lastIntervalIdentity, intervalItem).hasValidRecurrenceSourceFor(intervalItem))
        assertTrue(!source(firstOverflowIdentity, intervalItem).hasValidRecurrenceSourceFor(intervalItem))

        val rollingFrequencyItem = canonicalItem().copy(
            recurrenceJson = frequencyCase("day", "rolling", "rolling_minutes").recurrenceJson,
        )
        assertTrue(
            source(firstOverflowIdentity, rollingFrequencyItem)
                .hasValidRecurrenceSourceFor(rollingFrequencyItem),
        )
    }

    @Test
    fun habitEvidenceContextUsesIanaCalendarDatesAcrossDstAndRejectsCrossMidnight() {
        val identity = json(
            """{"type":"calendar_day","date":"2026-03-29","bucket_ordinal":0}""",
        )
        val localDate = LocalDate.parse("2026-03-29")
        val timezone = ZoneId.of("Europe/Paris")

        assertTrue(
            identity.matchesHabitEvidenceContext(
                localDate,
                timezone,
                Instant.parse("2026-03-29T00:30:00Z"),
                Instant.parse("2026-03-29T01:30:00Z"),
            ),
        )
        assertTrue(
            !identity.matchesHabitEvidenceContext(
                localDate,
                timezone,
                Instant.parse("2026-03-29T00:30:00Z"),
                Instant.parse("2026-03-29T22:30:00.000001Z"),
            ),
        )
        assertTrue(
            !json("""{"type":"custom"}""").matchesHabitEvidenceContext(
                localDate,
                timezone,
                Instant.parse("2026-03-29T00:30:00Z"),
                Instant.parse("2026-03-29T01:30:00Z"),
            ),
        )
    }

    private fun json(raw: String) = Json.parseToJsonElement(raw).let {
        requireNotNull(it as? kotlinx.serialization.json.JsonObject)
    }

    private fun source(
        identity: IdentityFixture,
        item: CanonicalItemSnapshot,
    ) = RecurrenceOccurrenceSourceSnapshot(
        itemId = item.id,
        itemRevision = item.revision,
        identityJson = identity.json,
        nominalStart = "2026-09-01T09:00:00+02:00",
        nominalEnd = "2026-09-01T10:00:00+02:00",
        localDate = identity.localDate,
        ordinal = identity.ordinal,
    )

    private fun identityFixtures(): Map<String, IdentityFixture> = listOf(
        IdentityFixture(
            "calendar_day",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
            "2026-09-01",
            0,
        ),
        IdentityFixture(
            "calendar_week",
            """{"type":"calendar_week","week_key":2461284,"bucket_ordinal":0}""",
            "2026-09-01",
            0,
        ),
        IdentityFixture(
            "calendar_month",
            """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":0}""",
            "2026-09-01",
            0,
        ),
        IdentityFixture(
            "rolling_minutes",
            """{"type":"rolling_minutes","index":0,"anchor":"2026-08-01T00:00:00Z"}""",
            null,
            0,
        ),
        IdentityFixture(
            "after_completion",
            """{"type":"after_completion","anchor":"2026-08-01T00:00:00Z"}""",
            null,
            0,
        ),
        IdentityFixture(
            "rolling_month",
            """{"type":"rolling_month","cycle":0,"index":0,"anchor":"2026-08-01T00:00:00Z"}""",
            null,
            0,
        ),
        IdentityFixture(
            "custom_rule",
            """{"type":"custom_rule","rule_id":"aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa","sequence":0,"date":"2026-09-01"}""",
            "2026-09-01",
            0,
        ),
    ).associateBy(IdentityFixture::type)

    private fun boundedIdentity(type: String, ordinal: Long): IdentityFixture = when (type) {
        "calendar_day" -> IdentityFixture(
            type,
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":$ordinal}""",
            "2026-09-01",
            ordinal,
        )
        "calendar_week" -> IdentityFixture(
            type,
            """{"type":"calendar_week","week_key":2461284,"bucket_ordinal":$ordinal}""",
            "2026-09-01",
            ordinal,
        )
        "calendar_month" -> IdentityFixture(
            type,
            """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":$ordinal}""",
            "2026-09-01",
            ordinal,
        )
        "rolling_month" -> IdentityFixture(
            type,
            """{"type":"rolling_month","cycle":0,"index":$ordinal,"anchor":"2026-08-01T00:00:00Z"}""",
            null,
            ordinal,
        )
        else -> error("Unsupported bounded recurrence identity")
    }

    private fun boundedRecurrenceCases(): List<BoundedRecurrenceCase> = listOf(
        BoundedRecurrenceCase("""{"type":"daily","times_per_day":2}""", "calendar_day", 2),
        BoundedRecurrenceCase("""{"type":"daily"}""", "calendar_day", 1),
        BoundedRecurrenceCase(
            """{"type":"weekly","times_per_week":3,"weekdays":[]}""",
            "calendar_week",
            3,
        ),
        BoundedRecurrenceCase(
            """{"type":"weekly","weekdays":["monday","wednesday"]}""",
            "calendar_week",
            2,
        ),
        BoundedRecurrenceCase("""{"type":"weekly"}""", "calendar_week", 1),
        BoundedRecurrenceCase(
            """{"type":"monthly","times_per_month":2}""",
            "calendar_month",
            2,
        ),
        BoundedRecurrenceCase("""{"type":"monthly"}""", "calendar_month", 1),
        boundedFrequencyCase("day", "calendar_day"),
        boundedFrequencyCase("week", "calendar_week"),
        boundedFrequencyCase("month", "calendar_month"),
        BoundedRecurrenceCase(
            """{"type":"frequency","target":2,"period":"month","semantics":"rolling","weekdays":[],"minimum_spacing":0,"anchor":null}""",
            "rolling_month",
            2,
        ),
    )

    private fun boundedFrequencyCase(
        period: String,
        identityType: String,
    ) = BoundedRecurrenceCase(
        """{"type":"frequency","target":2,"period":"$period","semantics":"calendar","weekdays":[],"minimum_spacing":0,"anchor":null}""",
        identityType,
        2,
    )

    private fun recurrenceIdentityCompatibilityCases(): List<RecurrenceCompatibilityCase> = listOf(
        RecurrenceCompatibilityCase(
            """{"type":"daily","times_per_day":1}""",
            "calendar_day",
        ),
        RecurrenceCompatibilityCase(
            """{"type":"weekly","times_per_week":1,"weekdays":[]}""",
            "calendar_week",
        ),
        RecurrenceCompatibilityCase(
            """{"type":"monthly","times_per_month":1}""",
            "calendar_month",
        ),
        RecurrenceCompatibilityCase(
            """{"type":"every_interval","interval":60}""",
            "rolling_minutes",
        ),
        RecurrenceCompatibilityCase(
            """{"type":"after_completion","interval":60}""",
            "after_completion",
        ),
        frequencyCase("day", "calendar", "calendar_day"),
        frequencyCase("week", "calendar", "calendar_week"),
        frequencyCase("month", "calendar", "calendar_month"),
        frequencyCase("day", "rolling", "rolling_minutes"),
        frequencyCase("week", "rolling", "rolling_minutes"),
        frequencyCase("month", "rolling", "rolling_month"),
        RecurrenceCompatibilityCase(
            """{"type":"custom","rrule":"FREQ=DAILY;COUNT=10"}""",
            "custom_rule",
        ),
    )

    private fun frequencyCase(
        period: String,
        semantics: String,
        identityType: String,
    ) = RecurrenceCompatibilityCase(
        """{"type":"frequency","target":1,"period":"$period","semantics":"$semantics","weekdays":[],"minimum_spacing":0,"anchor":null}""",
        identityType,
    )

    private fun canonicalItem() = CanonicalItemSnapshot(
        id = "11111111-1111-4111-8111-111111111111",
        kind = "habit",
        status = "planned",
        title = "Daily practice",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        recurrenceJson = buildJsonObject {
            put("type", "daily")
            put("times_per_day", 3)
        }.toString(),
        flexibleConstraintsJson = "{}",
        splitPolicyJson = """{"type":"indivisible"}""",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-08-01T00:00:00Z",
        updatedAt = "2026-08-01T00:00:00Z",
    )

    private data class IdentityFixture(
        val type: String,
        val json: String,
        val localDate: String?,
        val ordinal: Long,
    )

    private data class RecurrenceCompatibilityCase(
        val recurrenceJson: String,
        val expectedIdentityType: String,
    )

    private data class BoundedRecurrenceCase(
        val recurrenceJson: String,
        val identityType: String,
        val upperBoundExclusive: Long,
    )
}
