package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.RemotePlanOccurrence
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
            json("""{"type":"calendar_week","week_key":2460920,"bucket_ordinal":3}"""),
            json(
                """{"type":"calendar_month","year":2026,"month":9,"bucket_ordinal":4}""",
            ),
            json(
                """{"type":"rolling_minutes","index":5,"anchor":"2026-08-31T23:00:00.123456+02:00"}""",
            ),
            json(
                """{"type":"after_completion","anchor":"2026-08-31T23:00:00+02:00"}""",
            ),
            json(
                """{"type":"rolling_month","cycle":6,"index":7,"anchor":"2026-08-31T23:00:00+02:00"}""",
            ),
            json("""{"type":"custom"}"""),
        )

        identities.forEach { identity ->
            val persisted = validatedRecurrenceIdentityJson(identity)
            assertEquals(identity.toString(), persisted)
            assertEquals(identity, recurrenceIdentityObject(persisted))
        }
        assertTrue(requireNotNull(identities[3]["anchor"]).toString().contains("+02:00"))
    }

    @Test
    fun missingUnknownOrMalformedIdentityIsRejected() {
        listOf(
            "{}",
            """{"type":"future_identity"}""",
            """{"type":"custom","unexpected":true}""",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":65536}""",
            """{"type":"calendar_month","year":2026,"month":13,"bucket_ordinal":0}""",
            """{"type":"rolling_minutes","index":0,"anchor":"tomorrow"}""",
            """{"type":"after_completion","anchor":"2026-08-31T23:00:00.123456789+02:00"}""",
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
        val item = canonicalItem().copy(timezoneName = "America/Los_Angeles")
        val source = RecurrenceOccurrenceSourceSnapshot(
            itemId = item.id,
            itemRevision = item.revision,
            identityJson =
                """{"type":"calendar_week","week_key":2460920,"bucket_ordinal":3}""",
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
    }

    private fun json(raw: String) = Json.parseToJsonElement(raw).let {
        requireNotNull(it as? kotlinx.serialization.json.JsonObject)
    }

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
}
