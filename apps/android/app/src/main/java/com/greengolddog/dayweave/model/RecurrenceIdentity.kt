package com.greengolddog.dayweave.model

import java.time.DateTimeException
import java.time.LocalDate
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull

/**
 * Validates and canonicalizes the server-issued, rule-specific occurrence identity.
 *
 * The returned string retains every scalar value, including RFC3339 offsets. Persisting the
 * bounded object lets a later planning horizon give the scheduler provenance it can verify.
 */
internal fun validatedRecurrenceIdentityJson(identity: JsonObject): String? =
    identity.takeIf(JsonObject::hasValidRecurrenceIdentityShape)?.toString()

/** A persisted identity must be canonical JSON and continue satisfying the exact tagged shape. */
internal fun recurrenceIdentityObject(raw: String?): JsonObject? {
    raw ?: return null
    val parsed = runCatching {
        RECURRENCE_IDENTITY_JSON.parseToJsonElement(raw) as? JsonObject
    }.getOrNull() ?: return null
    return parsed.takeIf { it.toString() == raw && it.hasValidRecurrenceIdentityShape() }
}

internal fun recurrenceIdentityType(raw: String?): String? =
    recurrenceIdentityObject(raw)?.exactString("type")

/** Validates the complete persisted source envelope against its current canonical series. */
internal fun RecurrenceOccurrenceSourceSnapshot.hasValidRecurrenceSourceFor(
    item: CanonicalItemSnapshot,
): Boolean = runCatching {
    require(itemId == item.id && itemRevision == item.revision && itemRevision > 0)
    require(ordinal in 0..UInt.MAX_VALUE.toLong())
    val identity = requireNotNull(recurrenceIdentityObject(identityJson))
    val parsedNominalStart = requireNotNull(parseValidRfc3339(nominalStart))
    val parsedNominalEnd = requireNotNull(parseValidRfc3339(nominalEnd))
    require(parsedNominalStart < parsedNominalEnd)
    require(stableOrdinal(identity) == ordinal)
    val type = identity.exactString("type")
    val isCalendarIdentity = type in CALENDAR_IDENTITY_TYPES
    require((localDate != null) == isCalendarIdentity)
    localDate?.let { rawDate ->
        val date = LocalDate.parse(rawDate)
        require(date.toString() == rawDate)
        require(date == parsedNominalStart.toLocalDate())
        when (type) {
            "calendar_day" -> require(identity.exactString("date") == rawDate)
            "calendar_week" -> {
                val localJulianDay = Math.toIntExact(
                    date.toEpochDay() + JULIAN_DAY_AT_UNIX_EPOCH,
                )
                val weekStart = Math.toIntExact(identity.exactLong("week_key"))
                require(localJulianDay in weekStart..Math.addExact(weekStart, 6))
            }
            "calendar_month" -> require(
                identity.exactLong("year") == date.year.toLong() &&
                    identity.exactLong("month") == date.monthValue.toLong(),
            )
            "custom_rule" -> require(identity.exactString("date") == rawDate)
        }
    }
}.isSuccess

internal fun JsonObject.hasValidRecurrenceIdentityShape(): Boolean = runCatching {
    when (exactString("type")) {
        "calendar_day" -> {
            require(keys == setOf("type", "date", "bucket_ordinal"))
            val date = LocalDate.parse(exactString("date"))
            require(date.toString() == exactString("date"))
            require(exactLong("bucket_ordinal") in 0..UShort.MAX_VALUE.toLong())
        }
        "calendar_week" -> {
            require(keys == setOf("type", "week_key", "bucket_ordinal"))
            require(exactLong("week_key") in Int.MIN_VALUE.toLong()..Int.MAX_VALUE.toLong())
            require(exactLong("bucket_ordinal") in 0..UShort.MAX_VALUE.toLong())
        }
        "calendar_month" -> {
            require(keys == setOf("type", "year", "month", "bucket_ordinal"))
            require(exactLong("year") in Int.MIN_VALUE.toLong()..Int.MAX_VALUE.toLong())
            require(exactLong("month") in 1..12)
            require(exactLong("bucket_ordinal") in 0..UShort.MAX_VALUE.toLong())
        }
        "rolling_minutes" -> {
            require(keys == setOf("type", "index", "anchor"))
            require(exactLong("index") in 0..UInt.MAX_VALUE.toLong())
            requireValidRfc3339(exactString("anchor"))
        }
        "after_completion" -> {
            require(keys == setOf("type", "anchor"))
            requireValidRfc3339(exactString("anchor"))
        }
        "rolling_month" -> {
            require(keys == setOf("type", "cycle", "index", "anchor"))
            require(exactLong("cycle") in 0..Int.MAX_VALUE.toLong())
            require(exactLong("index") in 0..UShort.MAX_VALUE.toLong())
            requireValidRfc3339(exactString("anchor"))
        }
        "custom" -> require(keys == setOf("type"))
        "custom_rule" -> {
            require(keys == setOf("type", "rule_id", "sequence", "date"))
            val ruleId = java.util.UUID.fromString(exactString("rule_id"))
            require(
                ruleId != java.util.UUID(0L, 0L) &&
                    ruleId.version() == 5 &&
                    ruleId.toString() == exactString("rule_id"),
            )
            require(exactLong("sequence") in 0..UInt.MAX_VALUE.toLong())
            val date = LocalDate.parse(exactString("date"))
            require(date.toString() == exactString("date"))
        }
        else -> error("Unknown recurrence identity")
    }
}.isSuccess

/** Exact relationship required before server-issued recurrence evidence enters the habit cache. */
internal fun JsonObject.matchesHabitEvidenceContext(
    localDate: LocalDate,
    timezone: ZoneId,
    nominalStart: Instant,
    nominalEnd: Instant,
): Boolean = runCatching {
    require(hasValidRecurrenceIdentityShape())
    require(nominalStart.atZone(timezone).toLocalDate() == localDate)
    when (exactString("type")) {
        "calendar_day" -> {
            require(nominalEnd.minusNanos(1).atZone(timezone).toLocalDate() == localDate)
            require(exactString("date") == localDate.toString())
        }
        "calendar_week" -> {
            require(nominalEnd.minusNanos(1).atZone(timezone).toLocalDate() == localDate)
            val localJulianDay = Math.toIntExact(localDate.toEpochDay() + JULIAN_DAY_AT_UNIX_EPOCH)
            val weekStart = Math.toIntExact(exactLong("week_key"))
            require(localJulianDay in weekStart..Math.addExact(weekStart, 6))
        }
        "calendar_month" -> {
            require(nominalEnd.minusNanos(1).atZone(timezone).toLocalDate() == localDate)
            require(
                exactLong("year") == localDate.year.toLong() &&
                    exactLong("month") == localDate.monthValue.toLong(),
            )
        }
        "custom_rule" -> {
            require(nominalEnd.minusNanos(1).atZone(timezone).toLocalDate() == localDate)
            require(exactString("date") == localDate.toString())
        }
        "rolling_minutes", "after_completion", "rolling_month" -> Unit
        // A legacy placeholder is valid only inside pre-expansion move envelopes, never newly
        // issued authoritative habit evidence.
        "custom" -> error("Legacy custom identity cannot authenticate habit evidence")
        else -> error("Unknown recurrence identity")
    }
}.isSuccess

private fun JsonObject.exactString(key: String): String =
    (this[key] as? JsonPrimitive)
        ?.takeIf { it.isString }
        ?.contentOrNull
        ?: error("$key must be a string")

private fun JsonObject.exactLong(key: String): Long =
    (this[key] as? JsonPrimitive)
        ?.takeUnless { it.isString }
        ?.longOrNull
        ?: error("$key must be an integer")

private fun requireValidRfc3339(raw: String) {
    requireNotNull(parseValidRfc3339(raw))
}

private fun parseValidRfc3339(raw: String): OffsetDateTime? {
    if (!RFC3339_PATTERN.matches(raw)) return null
    val parsed = try {
        OffsetDateTime.parse(raw, DateTimeFormatter.ISO_OFFSET_DATE_TIME)
    } catch (error: DateTimeException) {
        return null
    }
    return parsed.takeIf { it.nano % 1_000 == 0 }
}

private fun stableOrdinal(identity: JsonObject): Long? = when (identity.exactString("type")) {
    "calendar_day", "calendar_week", "calendar_month" -> identity.exactLong("bucket_ordinal")
    "rolling_minutes" -> identity.exactLong("index").takeIf { it in 0..UInt.MAX_VALUE.toLong() }
    "after_completion", "custom" -> 0
    "rolling_month" -> identity.exactLong("index")
    "custom_rule" -> identity.exactLong("sequence")
    else -> null
}

private val RECURRENCE_IDENTITY_JSON = Json { ignoreUnknownKeys = false }
private val CALENDAR_IDENTITY_TYPES =
    setOf("calendar_day", "calendar_week", "calendar_month", "custom_rule")
private const val JULIAN_DAY_AT_UNIX_EPOCH = 2_440_588L
private val RFC3339_PATTERN = Regex(
    """^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$""",
)
