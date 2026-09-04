package com.greengolddog.dayweave.model

import java.net.URI
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/** Matches `dayweave_compose::MAX_SCHEDULING_OFFSET_MINUTES`. */
internal const val MAX_SCHEDULING_OFFSET_MINUTES = 527_040L

/** Matches the habit service's canonical quantity-unit text limit. */
internal const val MAX_CANONICAL_HABIT_TARGET_UNIT_SCALARS = 200

@Serializable
enum class CanonicalDraftPlacement(val wireValue: String) {
    INBOX("inbox"),
    PLANNED("planned"),
}

@Serializable
enum class CanonicalRecurrenceKind(val wireValue: String) {
    DAILY("daily"),
    WEEKLY("weekly"),
    MONTHLY("monthly"),
    EVERY_INTERVAL("every_interval"),
    AFTER_COMPLETION("after_completion"),
    FREQUENCY("frequency"),
    CUSTOM("custom"),
}

@Serializable
enum class CanonicalRecurrencePeriod(val wireValue: String) {
    DAY("day"),
    WEEK("week"),
    MONTH("month"),
}

@Serializable
enum class CanonicalRecurrenceSemantics(val wireValue: String) {
    CALENDAR("calendar"),
    ROLLING("rolling"),
}

@Serializable
enum class CanonicalWeekday(val wireValue: String) {
    MONDAY("monday"),
    TUESDAY("tuesday"),
    WEDNESDAY("wednesday"),
    THURSDAY("thursday"),
    FRIDAY("friday"),
    SATURDAY("saturday"),
    SUNDAY("sunday"),
}

/** Typed subset of the server recurrence contract supported by local authoring. */
@Serializable
data class CanonicalRecurrenceDraft(
    val kind: CanonicalRecurrenceKind,
    val occurrencesPerPeriod: Int? = null,
    val weekdays: List<CanonicalWeekday> = emptyList(),
    val intervalSeconds: Long? = null,
    val period: CanonicalRecurrencePeriod? = null,
    val semantics: CanonicalRecurrenceSemantics? = null,
    val minimumSpacingMinutes: Long? = null,
    val anchorAt: String? = null,
    val rrule: String? = null,
) {
    fun normalized(): CanonicalRecurrenceDraft = if (
        kind == CanonicalRecurrenceKind.CUSTOM && rrule != null
    ) {
        copy(rrule = canonicalizeCustomRrule(rrule))
    } else {
        this
    }

    fun requireValid() {
        require(weekdays.distinct().size == weekdays.size) { "Recurrence weekdays repeat" }
        when (kind) {
            CanonicalRecurrenceKind.DAILY,
            CanonicalRecurrenceKind.MONTHLY,
            -> require(
                occurrencesPerPeriod?.let { it in 1..UShort.MAX_VALUE.toInt() } == true &&
                    weekdays.isEmpty() && intervalSeconds == null && period == null &&
                    semantics == null && minimumSpacingMinutes == null && anchorAt == null &&
                    rrule == null,
            ) { "Daily and monthly recurrence require only a positive frequency" }

            CanonicalRecurrenceKind.WEEKLY -> require(
                occurrencesPerPeriod?.let { it in 1..UShort.MAX_VALUE.toInt() } == true &&
                    intervalSeconds == null && period == null && semantics == null &&
                    minimumSpacingMinutes == null && anchorAt == null && rrule == null,
            ) { "Weekly recurrence requires a frequency and optional distinct weekdays" }

            CanonicalRecurrenceKind.EVERY_INTERVAL,
            CanonicalRecurrenceKind.AFTER_COMPLETION,
            -> require(
                occurrencesPerPeriod == null && weekdays.isEmpty() &&
                    intervalSeconds?.let {
                        it % SECONDS_PER_MINUTE == 0L &&
                            it / SECONDS_PER_MINUTE in 1..MAX_SCHEDULING_OFFSET_MINUTES
                    } == true && period == null && semantics == null &&
                    minimumSpacingMinutes == null && anchorAt == null && rrule == null,
            ) {
                "Interval recurrence requires a positive whole-minute interval of at most " +
                    "$MAX_SCHEDULING_OFFSET_MINUTES minutes"
            }

            CanonicalRecurrenceKind.FREQUENCY -> {
                val target = requireNotNull(occurrencesPerPeriod) {
                    "Frequency recurrence requires a target"
                }
                require(target in 1..UShort.MAX_VALUE.toInt())
                val frequencyPeriod = requireNotNull(period) {
                    "Frequency recurrence requires a period"
                }
                val frequencySemantics = requireNotNull(semantics) {
                    "Frequency recurrence requires calendar or rolling semantics"
                }
                require(intervalSeconds == null)
                require(rrule == null)
                require(
                    minimumSpacingMinutes?.let { it in 0..MAX_SCHEDULING_OFFSET_MINUTES } != false,
                ) {
                    "Frequency minimum spacing must be at most " +
                        "$MAX_SCHEDULING_OFFSET_MINUTES minutes"
                }
                anchorAt?.let { requireCanonicalInstant(it, "recurrence anchor") }
                require(
                    frequencySemantics == CanonicalRecurrenceSemantics.ROLLING || anchorAt == null,
                ) { "Calendar frequency recurrence cannot carry a rolling anchor" }
                if (frequencySemantics == CanonicalRecurrenceSemantics.ROLLING) {
                    require(weekdays.isEmpty()) {
                        "Rolling frequency recurrence cannot restrict weekdays"
                    }
                    val rollingLimit = when (frequencyPeriod) {
                        CanonicalRecurrencePeriod.DAY -> MINUTES_PER_DAY
                        CanonicalRecurrencePeriod.WEEK -> 7 * MINUTES_PER_DAY
                        CanonicalRecurrencePeriod.MONTH -> UShort.MAX_VALUE.toInt()
                    }
                    require(target <= rollingLimit) {
                        "Rolling frequency exceeds minute precision"
                    }
                }
            }

            CanonicalRecurrenceKind.CUSTOM -> {
                require(
                    occurrencesPerPeriod == null && weekdays.isEmpty() &&
                        intervalSeconds == null && period == null && semantics == null &&
                        minimumSpacingMinutes == null && anchorAt == null && rrule != null,
                ) { "Custom recurrence requires only one RRULE" }
                canonicalizeCustomRrule(requireNotNull(rrule))
            }
        }
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            put("type", kind.wireValue)
            when (kind) {
                CanonicalRecurrenceKind.DAILY ->
                    put("times_per_day", requireNotNull(occurrencesPerPeriod))
                CanonicalRecurrenceKind.WEEKLY -> {
                    put("times_per_week", requireNotNull(occurrencesPerPeriod))
                    put(
                        "weekdays",
                        JsonArray(weekdays.map { JsonPrimitive(it.wireValue) }),
                    )
                }
                CanonicalRecurrenceKind.MONTHLY ->
                    put("times_per_month", requireNotNull(occurrencesPerPeriod))
                CanonicalRecurrenceKind.EVERY_INTERVAL,
                CanonicalRecurrenceKind.AFTER_COMPLETION,
                -> put("interval", requireNotNull(intervalSeconds) / SECONDS_PER_MINUTE)
                CanonicalRecurrenceKind.FREQUENCY -> {
                    put("target", requireNotNull(occurrencesPerPeriod))
                    put("period", requireNotNull(period).wireValue)
                    put("semantics", requireNotNull(semantics).wireValue)
                    put(
                        "weekdays",
                        JsonArray(weekdays.map { JsonPrimitive(it.wireValue) }),
                    )
                    put("minimum_spacing", minimumSpacingMinutes ?: 0)
                    if (anchorAt == null) put("anchor", JsonNull) else put("anchor", anchorAt)
                }
                CanonicalRecurrenceKind.CUSTOM ->
                    put("rrule", canonicalizeCustomRrule(requireNotNull(rrule)))
            }
        }
    }

    private companion object {
        const val SECONDS_PER_MINUTE = 60L
        const val MINUTES_PER_DAY = 24 * 60
    }
}

/**
 * Mirrors the finite custom-RRULE subset in `dayweave-core/custom_recurrence.rs`.
 *
 * Android validates and emits the same canonical spelling before a draft enters the durable
 * authoring journal. Authoritative server item writes validate the actual creation anchor under
 * every supported week-start setting. An offline draft can still be rejected at sync because its
 * server-owned creation instant does not exist while Android performs this static validation.
 */
internal fun canonicalizeCustomRrule(rrule: String): String {
    val bytes = rrule.toByteArray(Charsets.UTF_8)
    require(bytes.isNotEmpty()) { "Custom RRULE cannot be empty" }
    require(bytes.size <= MAX_CUSTOM_RRULE_BYTES) {
        "Custom RRULE exceeds $MAX_CUSTOM_RRULE_BYTES bytes"
    }
    require(bytes.all { byte -> (byte.toInt() and 0xff) in 0x21..0x7e }) {
        "Custom RRULE must contain printable ASCII without whitespace"
    }
    val body = if (rrule.startsWith("RRULE:", ignoreCase = true)) rrule.drop(6) else rrule
    require(body.isNotEmpty()) { "Custom RRULE cannot be empty" }

    var frequency: String? = null
    var interval: Long? = null
    var byDay: Set<String>? = null
    var byMonthDay: Set<Int>? = null
    var count: Long? = null
    var until: String? = null
    val seen = mutableSetOf<String>()
    body.split(';').forEach { part ->
        val separator = part.indexOf('=')
        require(
            separator > 0 && separator == part.lastIndexOf('=') && separator < part.lastIndex,
        ) { "Custom RRULE contains a malformed part" }
        val name = part.substring(0, separator).uppercase()
        val value = part.substring(separator + 1).uppercase()
        require(seen.add(name)) { "Custom RRULE contains duplicate part $name" }
        when (name) {
            "FREQ" -> {
                require(value in CUSTOM_RRULE_FREQUENCIES) {
                    "Custom RRULE frequency $value is unsupported"
                }
                frequency = value
            }
            "INTERVAL" -> interval = value.requireCustomRruleNumber(
                "INTERVAL",
                MAX_CUSTOM_RRULE_INTERVAL,
            )
            "BYDAY" -> {
                val values = value.split(',')
                require(values.isNotEmpty() && values.none(String::isEmpty)) {
                    "Custom RRULE BYDAY is invalid"
                }
                require(values.none { token ->
                    token.any { it.isDigit() || it == '+' || it == '-' }
                }) { "Custom RRULE does not support ordinal BYDAY entries" }
                require(values.all(CUSTOM_RRULE_WEEKDAYS::contains)) {
                    "Custom RRULE BYDAY is invalid"
                }
                require(values.distinct().size == values.size) {
                    "Custom RRULE BYDAY contains duplicates"
                }
                byDay = values.toSet()
            }
            "BYMONTHDAY" -> {
                val values = value.split(',')
                require(values.isNotEmpty() && values.none(String::isEmpty)) {
                    "Custom RRULE BYMONTHDAY is invalid"
                }
                val parsed = values.map { token ->
                    val digits = token.removePrefix("-")
                    require(
                        digits.isNotEmpty() && digits.all(Char::isDigit) &&
                            (token.first() != '+' && token.count { it == '-' } <= 1 &&
                                ('-' !in token || token.first() == '-')),
                    ) { "Custom RRULE BYMONTHDAY is invalid" }
                    token.toIntOrNull()?.takeIf { it != 0 && it in -31..31 }
                        ?: throw IllegalArgumentException("Custom RRULE BYMONTHDAY is invalid")
                }
                require(parsed.distinct().size == parsed.size) {
                    "Custom RRULE BYMONTHDAY contains duplicates"
                }
                byMonthDay = parsed.toSet()
            }
            "COUNT" -> count = value.requireCustomRruleNumber(
                "COUNT",
                MAX_CUSTOM_RRULE_OCCURRENCES,
            )
            "UNTIL" -> {
                require(value.length == 8 && value.all(Char::isDigit)) {
                    "Custom RRULE UNTIL must be a valid date-only YYYYMMDD value"
                }
                val date = runCatching {
                    LocalDate.parse(value, DateTimeFormatter.BASIC_ISO_DATE)
                }.getOrNull()
                require(date != null && date.year != 0) {
                    "Custom RRULE UNTIL must be a valid date-only YYYYMMDD value"
                }
                until = value
            }
            "BYSETPOS" -> throw IllegalArgumentException(
                "Custom RRULE does not support BYSETPOS",
            )
            "BYHOUR", "BYMINUTE", "BYSECOND" ->
                throw IllegalArgumentException(
                    "Custom RRULE does not support time component $name",
                )
            else -> throw IllegalArgumentException("Custom RRULE part $name is unsupported")
        }
    }

    val parsedFrequency = requireNotNull(frequency) { "Custom RRULE requires FREQ" }
    require((count == null) != (until == null)) {
        if (count == null && until == null) {
            "Custom RRULE must define exactly one finite COUNT or UNTIL"
        } else {
            "Custom RRULE cannot combine COUNT and UNTIL"
        }
    }
    require(parsedFrequency != "WEEKLY" || byMonthDay.isNullOrEmpty()) {
        "Custom weekly RRULE cannot combine FREQ=WEEKLY with BYMONTHDAY"
    }

    return buildList {
        add("FREQ=$parsedFrequency")
        add("INTERVAL=${interval ?: 1}")
        byDay?.let { days ->
            add("BYDAY=${CUSTOM_RRULE_WEEKDAYS.filter(days::contains).joinToString(",")}")
        }
        byMonthDay?.let { days -> add("BYMONTHDAY=${days.sorted().joinToString(",")}") }
        count?.let { add("COUNT=$it") }
        until?.let { add("UNTIL=$it") }
    }.joinToString(";")
}

private fun String.requireCustomRruleNumber(label: String, maximum: Long): Long {
    require(isNotEmpty() && all(Char::isDigit)) {
        "Custom RRULE $label must be in 1..=$maximum"
    }
    return toLongOrNull()?.takeIf { it in 1..maximum }
        ?: throw IllegalArgumentException("Custom RRULE $label must be in 1..=$maximum")
}

private const val MAX_CUSTOM_RRULE_BYTES = 1_024
private const val MAX_CUSTOM_RRULE_INTERVAL = 1_200L
private const val MAX_CUSTOM_RRULE_OCCURRENCES = 10_000L
private val CUSTOM_RRULE_FREQUENCIES = setOf("DAILY", "WEEKLY", "MONTHLY")
private val CUSTOM_RRULE_WEEKDAYS = listOf("MO", "TU", "WE", "TH", "FR", "SA", "SU")

@Serializable
enum class CanonicalSplitKind {
    INDIVISIBLE,
    SPLITTABLE,
}

@Serializable
data class CanonicalSplitDraft(
    val kind: CanonicalSplitKind = CanonicalSplitKind.INDIVISIBLE,
    val minimumChunkSeconds: Long? = null,
    val maximumChunkSeconds: Long? = null,
) {
    fun requireValid(durationSeconds: Long?) {
        when (kind) {
            CanonicalSplitKind.INDIVISIBLE -> require(
                minimumChunkSeconds == null && maximumChunkSeconds == null,
            ) { "Indivisible work cannot have chunk bounds" }
            CanonicalSplitKind.SPLITTABLE -> {
                val duration = requireNotNull(durationSeconds) {
                    "Splittable work requires a duration"
                }
                val minimum = requireNotNull(minimumChunkSeconds)
                val maximum = requireNotNull(maximumChunkSeconds)
                require(minimum > 0 && maximum >= minimum && maximum <= duration) {
                    "Split bounds must be positive, ordered, and within the duration"
                }
            }
        }
    }

    fun toCanonicalJson(durationSeconds: Long?): JsonObject {
        requireValid(durationSeconds)
        return buildJsonObject {
            put("type", if (kind == CanonicalSplitKind.INDIVISIBLE) "indivisible" else "splittable")
            if (kind == CanonicalSplitKind.SPLITTABLE) {
                put("minimum_chunk_seconds", requireNotNull(minimumChunkSeconds))
                put("maximum_chunk_seconds", requireNotNull(maximumChunkSeconds))
            }
        }
    }
}

@Serializable
enum class CanonicalConstraintLevel(val wireValue: String) {
    HARD("hard"),
    SOFT("soft"),
}

/** Constraint priority in the exact shape consumed by the Rust scheduler. */
@Serializable
data class CanonicalConstraintStrengthDraft(
    val level: CanonicalConstraintLevel,
    val weight: Long? = null,
) {
    fun requireValid() {
        when (level) {
            CanonicalConstraintLevel.HARD -> require(weight == null) {
                "Hard constraints cannot carry a soft weight"
            }
            CanonicalConstraintLevel.SOFT -> require(weight in 0..MAX_SOFT_WEIGHT) {
                "Soft constraint weight must be between zero and $MAX_SOFT_WEIGHT"
            }
        }
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            put("level", level.wireValue)
            if (level == CanonicalConstraintLevel.SOFT) put("weight", requireNotNull(weight))
        }
    }

    companion object {
        const val MAX_SOFT_WEIGHT = 1_000_000L

        fun hard() = CanonicalConstraintStrengthDraft(CanonicalConstraintLevel.HARD)
        fun soft(weight: Long = 100) =
            CanonicalConstraintStrengthDraft(CanonicalConstraintLevel.SOFT, weight)
    }
}

@Serializable
data class CanonicalQualifiedInstantDraft(
    val value: String,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun requireValid(description: String) {
        requireCanonicalInstant(value, description)
        strength.requireValid()
    }

    fun toCanonicalJson(description: String): JsonObject {
        requireValid(description)
        return qualifiedJson(JsonPrimitive(value), strength)
    }
}

@Serializable
data class CanonicalQualifiedMinutesDraft(
    val value: Long,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun requireValid(
        description: String,
        allowZero: Boolean = true,
        maximum: Long = MAX_UNSIGNED_INT,
    ) {
        require(value in (if (allowZero) 0L else 1L)..maximum) {
            "$description must be at most $maximum minutes"
        }
        strength.requireValid()
    }

    fun toCanonicalJson(
        description: String,
        allowZero: Boolean = true,
        maximum: Long = MAX_UNSIGNED_INT,
    ): JsonObject {
        requireValid(description, allowZero, maximum)
        return qualifiedJson(JsonPrimitive(value), strength)
    }

    private companion object {
        const val MAX_UNSIGNED_INT = 4_294_967_295L
    }
}

@Serializable
data class CanonicalQualifiedWeekdaysDraft(
    val value: List<CanonicalWeekday>,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun normalized() = copy(value = value.sortedBy(CanonicalWeekday.entries::indexOf))

    fun requireValid() {
        require(value.isNotEmpty() && value.distinct().size == value.size) {
            "Allowed weekdays must be non-empty and distinct"
        }
        strength.requireValid()
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return qualifiedJson(
            JsonArray(value.map { JsonPrimitive(it.wireValue) }),
            strength,
        )
    }
}

@Serializable
data class CanonicalDailyWindowDraft(
    val weekdays: List<CanonicalWeekday> = emptyList(),
    val startMinute: Int,
    val endMinute: Int,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun normalized() = copy(weekdays = weekdays.sortedBy(CanonicalWeekday.entries::indexOf))

    fun requireValid() {
        require(weekdays.distinct().size == weekdays.size)
        require(startMinute in 0..1_439 && endMinute in 1..1_440 && startMinute != endMinute) {
            "Daily windows require distinct valid minute-of-day bounds"
        }
        strength.requireValid()
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        val value = buildJsonObject {
            put("weekdays", JsonArray(weekdays.map { JsonPrimitive(it.wireValue) }))
            put("start_minute", startMinute)
            put("end_minute", endMinute)
        }
        return qualifiedJson(value, strength)
    }
}

@Serializable
data class CanonicalAbsoluteWindowDraft(
    val startsAt: String,
    val endsAt: String,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun requireValid(description: String) {
        val start = requireCanonicalInstant(startsAt, "$description start")
        val end = requireCanonicalInstant(endsAt, "$description end")
        require(start < end) { "$description end must follow its start" }
        strength.requireValid()
    }

    fun toCanonicalJson(description: String): JsonObject {
        requireValid(description)
        val value = buildJsonObject {
            put("start", startsAt)
            put("end", endsAt)
        }
        return qualifiedJson(value, strength)
    }
}

@Serializable
data class CanonicalQualifiedStringDraft(
    val value: String,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun normalized() = this

    fun requireValid(description: String) {
        require(value.isNotBlank()) {
            "$description must be non-empty"
        }
        strength.requireValid()
    }

    fun toCanonicalJson(description: String): JsonObject {
        requireValid(description)
        return qualifiedJson(JsonPrimitive(value), strength)
    }
}

@Serializable
enum class CanonicalDependencyRelation(val wireValue: String) {
    FINISH_TO_START("finish_to_start"),
    START_TO_START("start_to_start"),
    FINISH_TO_FINISH("finish_to_finish"),
    START_TO_FINISH("start_to_finish"),
}

@Serializable
data class CanonicalDependencyDraft(
    val itemId: String,
    val relation: CanonicalDependencyRelation,
    val minimumLagMinutes: Long = 0,
    val strength: CanonicalConstraintStrengthDraft,
) {
    fun normalized() = copy(itemId = UUID.fromString(itemId).toString())

    fun requireValid() {
        requireNonNilUuid(itemId, "dependency")
        require(minimumLagMinutes in 0..MAX_SCHEDULING_OFFSET_MINUTES) {
            "Dependency minimum lag must be at most " +
                "$MAX_SCHEDULING_OFFSET_MINUTES minutes"
        }
        strength.requireValid()
    }

    fun toCanonicalJson(): JsonObject {
        val value = normalized().also(CanonicalDependencyDraft::requireValid)
        return buildJsonObject {
            put("item_id", value.itemId)
            put("relation", value.relation.wireValue)
            put("minimum_lag", value.minimumLagMinutes)
            put("strength", value.strength.toCanonicalJson())
        }
    }
}

@Serializable
data class CanonicalBufferPolicyDraft(
    val beforeMinutes: Long,
    val afterMinutes: Long,
    val strength: CanonicalConstraintStrengthDraft?,
) {
    fun requireValid() {
        require(
            beforeMinutes in 0..MAX_SCHEDULING_OFFSET_MINUTES &&
                afterMinutes in 0..MAX_SCHEDULING_OFFSET_MINUTES,
        ) {
            "Buffers must be at most $MAX_SCHEDULING_OFFSET_MINUTES minutes"
        }
        require(strength == null || beforeMinutes > 0 || afterMinutes > 0) {
            "A qualified buffer needs non-zero preparation or decompression time"
        }
        strength?.requireValid()
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            put("before", beforeMinutes)
            put("after", afterMinutes)
            if (strength == null) put("strength", JsonNull) else put("strength", strength.toCanonicalJson())
        }
    }
}

/** The typed `constraints` object nested in canonical flexible metadata. */
@Serializable
data class CanonicalSchedulingConstraintsDraft(
    val earliestStart: CanonicalQualifiedInstantDraft? = null,
    val latestFinish: CanonicalQualifiedInstantDraft? = null,
    val minimumNotice: CanonicalQualifiedMinutesDraft? = null,
    val allowedWeekdays: CanonicalQualifiedWeekdaysDraft? = null,
    val preferredDailyWindows: List<CanonicalDailyWindowDraft> = emptyList(),
    val preferredAbsoluteWindows: List<CanonicalAbsoluteWindowDraft> = emptyList(),
    val forbiddenWindows: List<CanonicalAbsoluteWindowDraft> = emptyList(),
    val requiredContexts: List<CanonicalQualifiedStringDraft> = emptyList(),
    val requiredLocation: CanonicalQualifiedStringDraft? = null,
    val dependencies: List<CanonicalDependencyDraft> = emptyList(),
    val maximumDailyWork: CanonicalQualifiedMinutesDraft? = null,
    val maximumWeeklyWork: CanonicalQualifiedMinutesDraft? = null,
    val buffers: CanonicalBufferPolicyDraft? = null,
    /** System-owned when non-null; only the core's explicit null placeholder round-trips here. */
    val includesNullOccurrenceWindow: Boolean = false,
) {
    fun normalized() = copy(
        allowedWeekdays = allowedWeekdays?.normalized(),
        preferredDailyWindows = preferredDailyWindows.map(CanonicalDailyWindowDraft::normalized),
        requiredContexts = requiredContexts.map(CanonicalQualifiedStringDraft::normalized),
        requiredLocation = requiredLocation?.normalized(),
        dependencies = dependencies.map(CanonicalDependencyDraft::normalized)
            .sortedBy(CanonicalDependencyDraft::itemId),
    )

    fun requireValid() {
        earliestStart?.requireValid("constraint earliest start")
        latestFinish?.requireValid("constraint latest finish")
        minimumNotice?.requireValid(
            "Minimum notice",
            maximum = MAX_SCHEDULING_OFFSET_MINUTES,
        )
        allowedWeekdays?.requireValid()
        preferredDailyWindows.forEach { it.requireValid() }
        preferredAbsoluteWindows.forEach { it.requireValid("preferred absolute window") }
        forbiddenWindows.forEach { it.requireValid("forbidden window") }
        requiredContexts.forEach { it.requireValid("required context") }
        requiredLocation?.requireValid("required location")
        require(
            dependencies.map { UUID.fromString(it.itemId) }.distinct().size == dependencies.size,
        ) {
            "Dependencies must identify distinct items"
        }
        dependencies.forEach(CanonicalDependencyDraft::requireValid)
        maximumDailyWork?.requireValid("maximum daily work")
        maximumWeeklyWork?.requireValid("maximum weekly work")
        buffers?.requireValid()
        val earliest = earliestStart?.value?.let {
            requireCanonicalInstant(it, "constraint earliest start")
        }
        val latest = latestFinish?.value?.let {
            requireCanonicalInstant(it, "constraint latest finish")
        }
        require(earliest == null || latest == null || earliest < latest) {
            "Constraint earliest start must precede latest finish"
        }
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            earliestStart?.let { put("earliest_start", it.toCanonicalJson("constraint earliest start")) }
            latestFinish?.let { put("latest_finish", it.toCanonicalJson("constraint latest finish")) }
            minimumNotice?.let {
                put(
                    "minimum_notice",
                    it.toCanonicalJson(
                        "Minimum notice",
                        maximum = MAX_SCHEDULING_OFFSET_MINUTES,
                    ),
                )
            }
            allowedWeekdays?.let { put("allowed_weekdays", it.toCanonicalJson()) }
            if (preferredDailyWindows.isNotEmpty()) {
                put("preferred_daily_windows", JsonArray(preferredDailyWindows.map { it.toCanonicalJson() }))
            }
            if (preferredAbsoluteWindows.isNotEmpty()) {
                put(
                    "preferred_absolute_windows",
                    JsonArray(preferredAbsoluteWindows.map {
                        it.toCanonicalJson("preferred absolute window")
                    }),
                )
            }
            if (forbiddenWindows.isNotEmpty()) {
                put(
                    "forbidden_windows",
                    JsonArray(forbiddenWindows.map { it.toCanonicalJson("forbidden window") }),
                )
            }
            if (requiredContexts.isNotEmpty()) {
                put(
                    "required_contexts",
                    JsonArray(requiredContexts.map { it.toCanonicalJson("required context") }),
                )
            }
            requiredLocation?.let {
                put("required_location", it.toCanonicalJson("required location"))
            }
            if (dependencies.isNotEmpty()) {
                put(
                    "dependencies",
                    JsonArray(
                        dependencies.map(CanonicalDependencyDraft::normalized)
                            .sortedBy(CanonicalDependencyDraft::itemId)
                            .map(CanonicalDependencyDraft::toCanonicalJson),
                    ),
                )
            }
            maximumDailyWork?.let {
                put("maximum_daily_work", it.toCanonicalJson("maximum daily work"))
            }
            maximumWeeklyWork?.let {
                put("maximum_weekly_work", it.toCanonicalJson("maximum weekly work"))
            }
            buffers?.let { put("buffers", it.toCanonicalJson()) }
            if (includesNullOccurrenceWindow) put("occurrence_window", JsonNull)
        }
    }
}

@Serializable
data class CanonicalHabitTargetDraft(val amount: Long, val unit: String) {
    fun normalized() = this

    fun requireValid() {
        require(amount in 1..4_294_967_295L)
        require(
            unit.isNotBlank() &&
                unit.hasAtMostUnicodeScalars(MAX_CANONICAL_HABIT_TARGET_UNIT_SCALARS) &&
                unit.none(Char::isISOControl),
        ) {
            "Habit target unit must contain 1-$MAX_CANONICAL_HABIT_TARGET_UNIT_SCALARS " +
                "Unicode scalar values without control characters"
        }
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject { put("amount", amount); put("unit", unit) }
    }
}

@Serializable
data class CanonicalGoalMeasureDraft(
    val name: String,
    val target: Long,
    val current: Long,
    val unit: String,
) {
    fun normalized() = this

    fun requireValid() {
        require(name.isNotBlank())
        require(unit.isNotBlank())
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            put("name", name)
            put("target", target)
            put("current", current)
            put("unit", unit)
        }
    }
}

@Serializable
data class CanonicalWeeklyAllocationDraft(
    val minimumMinutes: Long,
    val maximumMinutes: Long? = null,
) {
    fun requireValid() {
        require(minimumMinutes in 0..4_294_967_295L)
        require(maximumMinutes == null || maximumMinutes in minimumMinutes..4_294_967_295L)
    }

    fun toCanonicalJson(): JsonObject {
        requireValid()
        return buildJsonObject {
            put("minimum", minimumMinutes)
            if (maximumMinutes == null) {
                put("maximum", JsonNull)
            } else {
                put("maximum", maximumMinutes)
            }
        }
    }
}

@Serializable
enum class CanonicalBreakCategory(val wireValue: String) {
    REST("rest"),
    MEAL("meal"),
    MOVEMENT("movement"),
    POMODORO("pomodoro"),
    DECOMPRESSION("decompression"),
    OTHER("other"),
}

private fun qualifiedJson(
    value: JsonElement,
    strength: CanonicalConstraintStrengthDraft,
): JsonObject = buildJsonObject {
    put("value", value)
    put("strength", strength.toCanonicalJson())
}

/** Common scheduling restrictions kept typed instead of retaining an unreviewable JSON blob. */
@Serializable
data class CanonicalFlexibleConstraintsDraft(
    val energy: EnergyLevel? = null,
    val tags: List<String> = emptyList(),
    val preferredStartMinute: Int? = null,
    val minimumGapMinutes: Long = 0,
    val maximumSessions: Int? = null,
    val maximumSplitDays: Int? = null,
    val energyStrength: CanonicalConstraintStrengthDraft? = null,
    val scheduling: CanonicalSchedulingConstraintsDraft? = null,
    val hasOwnEffort: Boolean? = null,
    val goalIds: List<String> = emptyList(),
    val habitTarget: CanonicalHabitTargetDraft? = null,
    val preservesStreakWhenPaused: Boolean? = null,
    val routineOrdered: Boolean? = null,
    val goalMeasures: List<CanonicalGoalMeasureDraft>? = null,
    val goalWeeklyAllocation: CanonicalWeeklyAllocationDraft? = null,
    val breakCategory: CanonicalBreakCategory? = null,
    val breakMandatory: Boolean? = null,
    val breakPromptToResume: Boolean? = null,
) {
    fun normalized(): CanonicalFlexibleConstraintsDraft = copy(
        tags = tags.sorted(),
        scheduling = scheduling?.normalized()?.takeUnless {
            it == CanonicalSchedulingConstraintsDraft()
        },
        goalIds = goalIds.sorted(),
        habitTarget = habitTarget?.normalized(),
        goalMeasures = goalMeasures?.map(CanonicalGoalMeasureDraft::normalized),
    )

    fun requireValid(
        durationSeconds: Long? = null,
        eventTiming: CanonicalEventTimingDraft? = null,
    ) {
        require(tags.distinct().size == tags.size) {
            "Constraint tags must be distinct"
        }
        require(tags.all { it.isNotBlank() }) {
            "Constraint tags must be non-empty"
        }
        preferredStartMinute?.let { preferred ->
            require(preferred in 0 until MINUTES_PER_DAY)
            val duration = requireNotNull(durationSeconds) {
                "A preferred start requires a duration"
            }
            val durationMinutes = (duration + SECONDS_PER_MINUTE - 1) / SECONDS_PER_MINUTE
            require(preferred.toLong() + durationMinutes <= MINUTES_PER_DAY) {
                "A preferred start and duration must finish within the same day"
            }
        }
        require(minimumGapMinutes in 0..MAX_SCHEDULING_OFFSET_MINUTES) {
            "Minimum gap must be at most $MAX_SCHEDULING_OFFSET_MINUTES minutes"
        }
        require(maximumSessions == null || maximumSessions in 1..UShort.MAX_VALUE.toInt())
        require(maximumSplitDays == null || maximumSplitDays in 1..UShort.MAX_VALUE.toInt())
        require(energy != null || energyStrength == null) {
            "An energy strength requires an energy level"
        }
        energyStrength?.requireValid()
        scheduling?.requireValid()
        val parsedGoalIds = goalIds.map { requireNonNilUuid(it, "goal reference") }
        require(parsedGoalIds.distinct().size == parsedGoalIds.size) {
            "Goal references must identify distinct items"
        }
        habitTarget?.requireValid()
        goalMeasures?.forEach(CanonicalGoalMeasureDraft::requireValid)
        goalWeeklyAllocation?.requireValid()
        if (eventTiming != null) {
            require(
                energy == null && tags.isEmpty() && preferredStartMinute == null &&
                    minimumGapMinutes == 0L && maximumSessions == null &&
                    maximumSplitDays == null && energyStrength == null && scheduling == null &&
                    hasOwnEffort == null && habitTarget == null &&
                    goalIds.isEmpty() &&
                    preservesStreakWhenPaused == null && routineOrdered == null &&
                    goalMeasures == null && goalWeeklyAllocation == null &&
                    breakCategory == null && breakMandatory == null &&
                    breakPromptToResume == null,
            ) { "A DayWeave firm block must be the event's sole metadata" }
        }
    }

    fun toCanonicalJson(
        eventTiming: CanonicalEventTimingDraft?,
        durationSeconds: Long? = null,
        timezoneName: String? = null,
    ): JsonObject {
        requireValid(durationSeconds, eventTiming)
        eventTiming?.requireValid(timezoneName)
        return buildJsonObject {
            energy?.let { level ->
                val strength = energyStrength
                if (strength == null) {
                    put("energy", level.name.lowercase())
                } else {
                    put(
                        "energy",
                        qualifiedJson(JsonPrimitive(level.name.lowercase()), strength),
                    )
                }
            }
            if (tags.isNotEmpty()) {
                put("tags", JsonArray(tags.map { JsonPrimitive(it) }))
            }
            scheduling?.let { put("constraints", it.toCanonicalJson()) }
            hasOwnEffort?.let { put("has_own_effort", it) }
            if (goalIds.isNotEmpty()) {
                put("goal_ids", JsonArray(goalIds.map(::JsonPrimitive)))
            }
            habitTarget?.let { put("habit_target", it.toCanonicalJson()) }
            preservesStreakWhenPaused?.let { put("preserves_streak_when_paused", it) }
            routineOrdered?.let { put("routine_ordered", it) }
            goalMeasures?.let { measures ->
                put("goal_measures", JsonArray(measures.map { it.toCanonicalJson() }))
            }
            goalWeeklyAllocation?.let { put("goal_weekly_allocation", it.toCanonicalJson()) }
            breakCategory?.let { put("break_category", it.wireValue) }
            breakMandatory?.let { put("break_mandatory", it) }
            breakPromptToResume?.let { put("break_prompt_to_resume", it) }
            preferredStartMinute?.let { put("preferred_start_minute", it) }
            if (minimumGapMinutes > 0) put("minimum_gap_minutes", minimumGapMinutes)
            maximumSessions?.let { put("maximum_sessions", it) }
            maximumSplitDays?.let { put("maximum_split_days", it) }
            eventTiming?.let { timing ->
                put("dayweave_firm_block", timing.toCanonicalJson(timezoneName))
            }
        }
    }

    private companion object {
        const val SECONDS_PER_MINUTE = 60L
        const val MINUTES_PER_DAY = 24 * 60
    }
}

/** Fixed timing for a locally owned event; provider identifiers never enter this model. */
@Serializable
data class CanonicalEventTimingDraft(
    val startsAt: String,
    val endsAt: String,
    val allDay: Boolean = false,
    val tentative: Boolean = false,
    val busy: Boolean = true,
) {
    fun requireValid(timezoneName: String? = null) {
        val start = requireCanonicalInstant(startsAt, "firm block start")
        val end = requireCanonicalInstant(endsAt, "firm block end")
        require(end > start) { "Event end must follow its start" }
        if (allDay) {
            val zone = requireCanonicalTimezoneName(requireNotNull(timezoneName) {
                "All-day event validation requires the canonical item timezone"
            })
            val localStart = start.atZone(zone)
            val localEnd = end.atZone(zone)
            require(
                localStart.toLocalTime() == java.time.LocalTime.MIDNIGHT &&
                    localEnd.toLocalTime() == java.time.LocalTime.MIDNIGHT &&
                    localEnd.toLocalDate().isAfter(localStart.toLocalDate()),
            ) { "All-day firm blocks require local-midnight, exclusive bounds" }
        }
    }

    fun toCanonicalJson(timezoneName: String? = null): JsonObject {
        requireValid(timezoneName)
        return buildJsonObject {
            put("owned", true)
            put("starts_at", startsAt)
            put("ends_at", endsAt)
            put("all_day", allDay)
            put("tentative", tentative)
            put("busy", busy)
        }
    }
}

/** Complete local authoring body, excluding server-owned revisions and timestamps. */
@Serializable
data class CanonicalItemDraft(
    val schemaVersion: Int = CURRENT_VERSION,
    val placement: CanonicalDraftPlacement = CanonicalDraftPlacement.INBOX,
    val kind: ItemKind = ItemKind.TASK,
    val isSensitive: Boolean = false,
    val title: String,
    val notes: String? = null,
    val timezoneName: String,
    val durationSeconds: Long? = null,
    val deadlineAt: String? = null,
    val earliestStartAt: String? = null,
    val recurrence: CanonicalRecurrenceDraft? = null,
    val constraints: CanonicalFlexibleConstraintsDraft = CanonicalFlexibleConstraintsDraft(),
    val split: CanonicalSplitDraft = CanonicalSplitDraft(),
    val importance: Int = 50,
    val urgency: Int = 50,
    val parentId: String? = null,
    val siblingOrder: Long = 0,
    val eventTiming: CanonicalEventTimingDraft? = null,
) {
    fun normalized(): CanonicalItemDraft = copy(
        title = title.trim(),
        notes = notes?.takeUnless(String::isBlank),
        recurrence = recurrence?.normalized(),
        constraints = constraints.normalized(),
    )

    fun requireValid(itemId: String) {
        val value = normalized()
        require(schemaVersion == CURRENT_VERSION)
        requireCanonicalUuid(itemId, "canonical draft item")
        require(value.title.isNotEmpty() &&
            value.title.codePointCount(0, value.title.length) <= MAX_TITLE_CHARS)
        require(
            (value.notes?.let { it.codePointCount(0, it.length) } ?: 0) <= MAX_NOTES_CHARS,
        )
        requireCanonicalTimezoneName(value.timezoneName)
        require(value.durationSeconds == null || value.durationSeconds in 1..MAX_DURATION_SECONDS)
        val earliest = value.earliestStartAt?.let {
            requireCanonicalInstant(it, "canonical earliest start")
        }
        val deadline = value.deadlineAt?.let {
            requireCanonicalInstant(it, "canonical deadline")
        }
        require(earliest == null || deadline == null || earliest < deadline)
        value.recurrence?.requireValid()
        value.constraints.requireValid(value.durationSeconds, value.eventTiming)
        require(value.constraints.goalIds.isEmpty()) {
            "Goal links are read-only until safe graph editing is available"
        }
        val parsedItemId = UUID.fromString(itemId)
        require(
            value.constraints.scheduling?.dependencies.orEmpty().none {
                UUID.fromString(it.itemId) == parsedItemId
            },
        ) {
            "An item cannot depend on itself"
        }
        value.split.requireValid(value.durationSeconds)
        require(value.split.kind == CanonicalSplitKind.SPLITTABLE ||
            value.constraints.maximumSessions == null &&
            value.constraints.minimumGapMinutes == 0L &&
            value.constraints.maximumSplitDays == null) {
            "Session, gap, and day limits require a splittable item"
        }
        if (value.split.kind == CanonicalSplitKind.SPLITTABLE) {
            val maximumSessions = value.constraints.maximumSessions
            val duration = value.durationSeconds
            val maximumChunk = value.split.maximumChunkSeconds
            if (maximumSessions != null && duration != null && maximumChunk != null) {
                val requiredSessions = (duration + maximumChunk - 1) / maximumChunk
                require(requiredSessions <= maximumSessions.toLong()) {
                    "Maximum sessions cannot contain the duration within maximum chunks"
                }
            }
        }
        require(value.importance in 0..100 && value.urgency in 0..100)
        require(value.siblingOrder in 0..MAX_SIBLING_ORDER)
        value.parentId?.let {
            requireCanonicalUuid(it, "canonical draft parent")
            require(it != itemId) { "An item cannot be its own parent" }
        }
        when (value.kind) {
            ItemKind.PROJECT -> error(
                "Project structure is read-only until typed structural authoring is available",
            )
            ItemKind.HABIT -> {
                require(
                    value.constraints.routineOrdered == null &&
                        value.constraints.goalMeasures == null &&
                        value.constraints.goalWeeklyAllocation == null &&
                        value.constraints.breakCategory == null &&
                        value.constraints.breakMandatory == null &&
                        value.constraints.breakPromptToResume == null,
                ) { "Habit metadata cannot describe another item kind" }
                if (value.placement == CanonicalDraftPlacement.PLANNED) {
                    require(value.recurrence != null) { "Planned habits require recurrence" }
                }
            }
            ItemKind.ROUTINE -> require(
                value.constraints.habitTarget == null &&
                    value.constraints.preservesStreakWhenPaused == null &&
                    value.constraints.goalMeasures == null &&
                    value.constraints.goalWeeklyAllocation == null &&
                    value.constraints.breakCategory == null &&
                    value.constraints.breakMandatory == null &&
                    value.constraints.breakPromptToResume == null,
            ) { "Routine metadata cannot describe another item kind" }
            ItemKind.GOAL -> require(
                value.constraints.habitTarget == null &&
                    value.constraints.preservesStreakWhenPaused == null &&
                    value.constraints.routineOrdered == null &&
                    value.constraints.breakCategory == null &&
                    value.constraints.breakMandatory == null &&
                    value.constraints.breakPromptToResume == null,
            ) { "Goal metadata cannot describe another item kind" }
            ItemKind.BREAK -> require(
                value.constraints.habitTarget == null &&
                    value.constraints.preservesStreakWhenPaused == null &&
                    value.constraints.routineOrdered == null &&
                    value.constraints.goalMeasures == null &&
                    value.constraints.goalWeeklyAllocation == null,
            ) { "Break metadata cannot describe another item kind" }
            ItemKind.TASK -> require(
                value.constraints.habitTarget == null &&
                    value.constraints.preservesStreakWhenPaused == null &&
                    value.constraints.routineOrdered == null &&
                    value.constraints.goalMeasures == null &&
                    value.constraints.goalWeeklyAllocation == null &&
                    value.constraints.breakCategory == null &&
                    value.constraints.breakMandatory == null &&
                    value.constraints.breakPromptToResume == null,
            ) { "Task metadata cannot describe another item kind" }
            ItemKind.EVENT,
            -> require(value.recurrence == null) { "This item type cannot recur" }
        }
        if (value.kind in setOf(ItemKind.GOAL, ItemKind.BREAK)) {
            require(value.recurrence == null) { "This item type cannot recur" }
        }
        val richConstraints = value.constraints.scheduling
        require(value.earliestStartAt == null || richConstraints?.earliestStart == null) {
            "Earliest start cannot be defined in both canonical and flexible fields"
        }
        require(value.deadlineAt == null || richConstraints?.latestFinish == null) {
            "Deadline cannot be defined in both canonical and flexible fields"
        }
        val effectiveEarliest = earliest ?: richConstraints?.earliestStart?.value?.let {
            requireCanonicalInstant(it, "constraint earliest start")
        }
        val effectiveLatest = deadline ?: richConstraints?.latestFinish?.value?.let {
            requireCanonicalInstant(it, "constraint latest finish")
        }
        require(effectiveEarliest == null || effectiveLatest == null || effectiveEarliest < effectiveLatest) {
            "Earliest start must precede latest finish"
        }
        if (value.kind == ItemKind.EVENT) {
            val timing = value.eventTiming
            if (value.placement == CanonicalDraftPlacement.PLANNED) {
                requireNotNull(timing) {
                    "Event requires timing metadata after it leaves the Inbox"
                }
            }
            timing?.let {
                it.requireValid(value.timezoneName)
                val timingDuration = Duration.between(
                    requireCanonicalInstant(it.startsAt, "firm block start"),
                    requireCanonicalInstant(it.endsAt, "firm block end"),
                )
                require(
                    value.durationSeconds == null ||
                        timingDuration.nano == 0 && value.durationSeconds == timingDuration.seconds,
                ) {
                    "Event duration, when supplied, must match its fixed timing"
                }
                require(
                    (earliest == null ||
                        earliest == requireCanonicalInstant(it.startsAt, "firm block start")) &&
                        (deadline == null ||
                            deadline == requireCanonicalInstant(it.endsAt, "firm block end")),
                ) { "Event timing must match any supplied hard scheduling bounds" }
            }
            if (timing == null) {
                require(value.durationSeconds == null && earliest == null && deadline == null) {
                    "Incomplete Inbox events cannot carry partial fixed timing"
                }
            }
            require(value.split.kind == CanonicalSplitKind.INDIVISIBLE)
        } else {
            require(value.eventTiming == null) { "Only events can have fixed event timing" }
        }
        require(encodedBytes(value.recurrence?.toCanonicalJson()) <= MAX_RECURRENCE_BYTES)
        require(
            encodedBytes(
                value.constraints.toCanonicalJson(
                    value.eventTiming,
                    value.durationSeconds,
                    value.timezoneName,
                ),
            ) <=
                MAX_CONSTRAINT_BYTES,
        )
    }

    internal fun requireValidCanonicalRead(itemId: String) = requireValid(itemId)

    fun matches(item: CanonicalItemSnapshot): Boolean = runCatching {
        val value = normalized()
        value.requireValid(item.id)
        item.requireCanonicalAuthoringShape()
        val decodedConstraints = decodeCanonicalConstraints(
            item.flexibleConstraintsJson,
            value.kind,
            value.timezoneName,
            value.durationSeconds,
        )
        !item.hasExplicitStructuralMetadata && item.deletedAt == null &&
            item.kind == value.kind.name.lowercase() &&
            item.status == value.placement.wireValue &&
            item.isSensitive == value.isSensitive &&
            item.title == value.title &&
            item.notes == value.notes &&
            item.timezoneName == value.timezoneName &&
            item.durationSeconds == value.durationSeconds &&
            sameInstant(item.deadlineAt, value.deadlineAt) &&
            sameInstant(item.earliestStartAt, value.earliestStartAt) &&
            normalizedRecurrenceJson(item.recurrenceJson) ==
            value.recurrence?.toCanonicalJson() &&
            decodedConstraints.first == value.constraints &&
            decodedConstraints.second == value.eventTiming &&
            decodeCanonicalSplit(item.splitPolicyJson) == value.split &&
            item.importance == value.importance && item.urgency == value.urgency &&
            item.parentId == value.parentId && item.siblingOrder == value.siblingOrder
    }.getOrDefault(false)

    internal fun matchesCanonicalRead(item: CanonicalItemSnapshot): Boolean = matches(item)

    companion object {
        const val CURRENT_VERSION = 1
        const val MAX_DURATION_SECONDS = 366L * 24L * 60L * 60L
        private const val MAX_TITLE_CHARS = 500
        private const val MAX_NOTES_CHARS = 100_000
        private const val MAX_RECURRENCE_BYTES = 16 * 1_024
        private const val MAX_CONSTRAINT_BYTES = 32 * 1_024
        private const val MAX_SIBLING_ORDER = 1_000_000L

        private val STRICT_JSON = Json { ignoreUnknownKeys = false }

        private fun encodedBytes(value: JsonElement?): Int = when (value) {
            null, JsonNull -> 0
            else -> STRICT_JSON.encodeToString(JsonElement.serializer(), value).toByteArray().size
        }

        private fun normalizedRecurrenceJson(raw: String?): JsonElement? = raw?.let {
            decodeCanonicalRecurrence(it).toCanonicalJson()
        }

        private fun sameInstant(left: String?, right: String?): Boolean = when {
            left == null || right == null -> left == right
            else -> requireCanonicalInstant(left, "canonical instant") ==
                requireCanonicalInstant(right, "draft instant")
        }
    }
}

@Serializable
enum class CanonicalAuthoringOperation {
    CREATE,
    REPLACE,
    TRASH,
    RESTORE,
}

@Serializable
enum class CanonicalAuthoringDisposition {
    PENDING,
    CONFLICTED,
}

/** One exact local authoring operation. Submitted fields are replaced only as a whole. */
@Serializable
data class PendingCanonicalAuthoringMutation(
    val schemaVersion: Int = CURRENT_VERSION,
    val id: String,
    val itemId: String,
    val operation: CanonicalAuthoringOperation,
    val draft: CanonicalItemDraft? = null,
    val expectedRevision: Long? = null,
    val baseItem: CanonicalItemSnapshot? = null,
    val idempotencyKey: String = "android-item-$id",
    val createdAt: String,
    val syncOrigin: String? = null,
    val configurationId: String? = null,
    val submittedAt: String? = null,
    val disposition: CanonicalAuthoringDisposition = CanonicalAuthoringDisposition.PENDING,
    val diagnostic: String? = null,
) {
    val isSubmitted: Boolean get() = submittedAt != null

    fun requireValid() {
        require(schemaVersion == CURRENT_VERSION)
        requireCanonicalUuid(id, "canonical authoring mutation")
        requireCanonicalUuid(itemId, "canonical authoring item")
        require(idempotencyKey == "android-item-$id")
        val created = requireCanonicalInstant(createdAt, "canonical authoring creation")
        require(syncOrigin != null || configurationId == null)
        syncOrigin?.let(::requireCanonicalOrigin)
        require(configurationId == null || configurationId.isNotBlank() &&
            configurationId.toByteArray().size <= MAX_BINDING_BYTES)
        submittedAt?.let {
            require(
                syncOrigin != null &&
                    requireCanonicalInstant(it, "canonical authoring submission") >= created,
            )
        }
        when (disposition) {
            CanonicalAuthoringDisposition.PENDING -> require(diagnostic == null)
            CanonicalAuthoringDisposition.CONFLICTED -> require(
                !diagnostic.isNullOrBlank() && diagnostic.length <= MAX_DIAGNOSTIC_CHARS &&
                    diagnostic.toByteArray(Charsets.UTF_8).size <=
                    CanonicalAuthoringJournalPolicy.MAX_DIAGNOSTIC_BYTES,
            )
        }
        when (operation) {
            CanonicalAuthoringOperation.CREATE -> require(
                expectedRevision == null && baseItem == null && draft != null,
            )
            CanonicalAuthoringOperation.REPLACE -> require(
                expectedRevision != null && baseItem?.id == itemId &&
                    baseItem.revision == expectedRevision && baseItem.deletedAt == null &&
                    draft != null,
            ).also {
                requireNotNull(baseItem).requireCanonicalReplacementSupport()
            }
            CanonicalAuthoringOperation.TRASH -> require(
                draft == null && expectedRevision != null &&
                    (baseItem == null || baseItem.id == itemId &&
                        baseItem.revision == expectedRevision && baseItem.deletedAt == null),
            )
            CanonicalAuthoringOperation.RESTORE -> require(
                draft == null && expectedRevision != null &&
                    (baseItem == null || baseItem.id == itemId &&
                        baseItem.revision == expectedRevision && baseItem.deletedAt != null),
            )
        }
        require(expectedRevision == null || expectedRevision > 0)
        draft?.requireValid(itemId)
        baseItem?.requireCanonicalAuthoringShape()
        require(
            canonicalAuthoringMutationBytes(this) <=
                CanonicalAuthoringJournalPolicy.MAX_MUTATION_BYTES,
        ) { "Canonical authoring mutation exceeds its encoded-size budget" }
    }

    companion object {
        const val CURRENT_VERSION = 1
        private const val MAX_BINDING_BYTES = 4_096
        const val MAX_DIAGNOSTIC_CHARS = 500
    }
}

@Serializable
data class CanonicalRecentlyDeletedRecord(
    val schemaVersion: Int = CURRENT_VERSION,
    val id: String,
    val revision: Long,
    val deletedAt: String,
    val parentId: String? = null,
    val lastKnownItem: CanonicalItemSnapshot? = null,
    val effectiveIsSensitive: Boolean = true,
    /**
     * Earliest locally known instant for retention. The server's deletion timestamp may be in the
     * future, so it must never be the sole authority for keeping recovery content on device.
     * Null is accepted only as a migration or inbound-delta value and is normalized before state
     * reaches encrypted persistence.
     */
    val retentionAnchorAt: String? = null,
) {
    val isSensitive: Boolean get() = effectiveIsSensitive

    fun requireValid() {
        require(schemaVersion == CURRENT_VERSION)
        requireCanonicalUuid(id, "recently deleted item")
        require(revision > 0)
        val deletion = requireCanonicalInstant(deletedAt, "canonical deletion")
        retentionAnchorAt?.let {
            require(requireCanonicalInstant(it, "canonical deletion retention anchor") <= deletion)
        }
        parentId?.let { requireCanonicalUuid(it, "recently deleted parent") }
        lastKnownItem?.let {
            it.requireCanonicalAuthoringShape()
            require(it.id == id && it.revision <= revision)
            if (it.revision == revision) require(it.deletedAt != null)
            require(effectiveIsSensitive || !it.isSensitive) {
                "A recently-deleted record cannot lower its retained own sensitivity"
            }
        }
        require(lastKnownItem != null || effectiveIsSensitive) {
            "A bodyless recently-deleted record must fail closed for sensitivity"
        }
    }

    companion object {
        const val CURRENT_VERSION = 1
    }
}

fun CanonicalItemSnapshot.toCanonicalDraft(): CanonicalItemDraft {
    requireCanonicalAuthoringShape()
    val kindValue = ItemKind.entries.firstOrNull { it.name.equals(kind, ignoreCase = true) }
        ?: throw IllegalArgumentException("Unsupported canonical item kind")
    val placement = CanonicalDraftPlacement.entries.firstOrNull {
        it.wireValue == status
    } ?: throw IllegalArgumentException("Only Inbox or Planned items can be authored")
    val recurrenceValue = recurrenceJson?.let(::decodeCanonicalRecurrence)
    val splitValue = decodeCanonicalSplit(splitPolicyJson)
    val constraintsValue = decodeCanonicalConstraints(
        flexibleConstraintsJson,
        kindValue,
        timezoneName,
        durationSeconds,
    )
    return CanonicalItemDraft(
        placement = placement,
        kind = kindValue,
        isSensitive = isSensitive,
        title = title,
        notes = notes,
        timezoneName = timezoneName,
        durationSeconds = durationSeconds,
        deadlineAt = deadlineAt,
        earliestStartAt = earliestStartAt,
        recurrence = recurrenceValue,
        constraints = constraintsValue.first,
        split = splitValue,
        importance = importance,
        urgency = urgency,
        parentId = parentId,
        siblingOrder = siblingOrder,
        eventTiming = constraintsValue.second,
    ).also {
        it.requireValidCanonicalRead(id)
        require(it.matchesCanonicalRead(this)) {
            "Canonical item contains unsupported authoring fields"
        }
    }
}

/**
 * Decodes only the typed flexible-constraint payload for graph presentation and validation.
 *
 * Unlike [toCanonicalDraft], this remains usable for completed, blocked, and structurally rich
 * items that cannot safely be replaced by this client. Callers must continue to fail closed when
 * decoding returns an error; this helper grants no mutation authority.
 */
internal fun CanonicalItemSnapshot.decodeCanonicalFlexibleConstraints():
    CanonicalFlexibleConstraintsDraft {
    requireCanonicalAuthoringShape()
    val kindValue = ItemKind.entries.firstOrNull { it.name.equals(kind, ignoreCase = true) }
        ?: throw IllegalArgumentException("Unsupported canonical item kind")
    return decodeCanonicalConstraints(
        flexibleConstraintsJson,
        kindValue,
        timezoneName,
        durationSeconds,
    ).first
}

/**
 * Proves that replacing this canonical row cannot silently discard server-owned or unsupported
 * state. Destructive identity-only operations deliberately do not use this fence: an unsupported
 * row must remain trashable (and an exact tombstone restorable).
 */
internal fun CanonicalItemSnapshot.requireCanonicalReplacementSupport(): CanonicalItemDraft {
    require(!hasExplicitStructuralMetadata) {
        "Typed structural metadata is read-only until full-item authoring supports it"
    }
    val roundTripped = toCanonicalDraft()
    require(roundTripped.matches(this)) {
        "Canonical item cannot be replaced without losing unsupported authoring fields"
    }
    return roundTripped
}

private fun decodeCanonicalRecurrence(raw: String): CanonicalRecurrenceDraft {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported recurrence")
    val type = (objectValue["type"] as? JsonPrimitive)?.content
    val result = when (type) {
        "daily" -> {
            objectValue.requireOnlyKeys("type", "times_per_day")
            CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.DAILY,
                occurrencesPerPeriod = objectValue.optionalInt("times_per_day") ?: 1,
            )
        }
        "weekly" -> {
            objectValue.requireOnlyKeys("type", "times_per_week", "weekdays")
            val weekdays = if (objectValue.containsKey("weekdays")) {
                objectValue.exactWeekdays("weekdays")
            } else {
                emptyList()
            }
            CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.WEEKLY,
                occurrencesPerPeriod = objectValue.optionalInt("times_per_week")
                    ?: weekdays.size.coerceAtLeast(1),
                weekdays = weekdays,
            )
        }
        "monthly" -> {
            objectValue.requireOnlyKeys("type", "times_per_month")
            CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.MONTHLY,
                occurrencesPerPeriod = objectValue.optionalInt("times_per_month") ?: 1,
            )
        }
        "every_interval", "after_completion" -> {
            objectValue.requireOnlyKeys("type", "interval")
            CanonicalRecurrenceDraft(
                if (type == "every_interval") CanonicalRecurrenceKind.EVERY_INTERVAL
                else CanonicalRecurrenceKind.AFTER_COMPLETION,
                intervalSeconds = Math.multiplyExact(objectValue.exactLong("interval"), 60L),
            )
        }
        "frequency" -> {
            objectValue.requireOnlyKeys(
                "type", "target", "period", "semantics", "weekdays", "minimum_spacing", "anchor",
            )
            CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.FREQUENCY,
                occurrencesPerPeriod = objectValue.exactInt("target"),
                period = objectValue.exactEnum("period", CanonicalRecurrencePeriod.entries) {
                    it.wireValue
                },
                semantics = objectValue.exactEnum(
                    "semantics",
                    CanonicalRecurrenceSemantics.entries,
                ) { it.wireValue },
                weekdays = if (objectValue.containsKey("weekdays")) {
                    objectValue.exactWeekdays("weekdays")
                } else {
                    emptyList()
                },
                minimumSpacingMinutes = if (objectValue.containsKey("minimum_spacing")) {
                    objectValue.exactLong("minimum_spacing")
                } else {
                    0
                },
                anchorAt = objectValue.optionalExactString("anchor"),
            )
        }
        "custom" -> {
            objectValue.requireOnlyKeys("type", "rrule")
            CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.CUSTOM,
                rrule = objectValue.exactString("rrule"),
            )
        }
        else -> throw IllegalArgumentException("Unsupported recurrence")
    }
    result.requireValid()
    return result.normalized()
}

private fun decodeCanonicalSplit(raw: String): CanonicalSplitDraft {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported split policy")
    val result = when ((objectValue["type"] as? JsonPrimitive)?.content) {
        "indivisible" -> {
            objectValue.requireOnlyKeys("type")
            CanonicalSplitDraft()
        }
        "splittable" -> {
            objectValue.requireOnlyKeys(
                "type",
                "minimum_chunk_seconds",
                "maximum_chunk_seconds",
            )
            CanonicalSplitDraft(
                kind = CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = objectValue.exactLong("minimum_chunk_seconds"),
                maximumChunkSeconds = objectValue.exactLong("maximum_chunk_seconds"),
            )
        }
        else -> throw IllegalArgumentException("Unsupported split policy")
    }
    return result
}

private fun decodeCanonicalConstraints(
    raw: String,
    kind: ItemKind,
    timezoneName: String,
    durationSeconds: Long?,
): Pair<CanonicalFlexibleConstraintsDraft, CanonicalEventTimingDraft?> {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported constraints")
    for (key in listOf("calendar_event", "calendar_context")) {
        require(objectValue[key] == null || objectValue[key] == JsonNull) {
            "Provider-managed calendar event metadata is read-only on Android"
        }
    }
    val knownKeys = setOf(
        "energy", "tags", "preferred_start_minute", "minimum_gap_minutes",
        "maximum_sessions", "maximum_split_days", "constraints", "has_own_effort", "goal_ids",
        "habit_target", "preserves_streak_when_paused", "routine_ordered",
        "goal_measures", "goal_weekly_allocation", "break_category", "break_mandatory",
        "break_prompt_to_resume", "calendar_event", "calendar_context", "dayweave_firm_block",
    )
    require(objectValue.keys.all { it in knownKeys }) { "Constraints contain unsupported fields" }
    objectValue.requireKindMetadataKeys(kind)
    var energyStrength: CanonicalConstraintStrengthDraft? = null
    val energy = when (val element = objectValue["energy"]) {
        null, JsonNull -> null
        else -> {
            val value = when (element) {
                is JsonPrimitive -> element.takeIf(JsonPrimitive::isString)?.content
                is JsonObject -> {
                    element.requireOnlyKeys("value", "strength")
                    energyStrength = decodeConstraintStrength(element.exactObject("strength"))
                    element.exactString("value")
                }
                else -> null
            } ?: throw IllegalArgumentException("Unsupported energy constraint")
            EnergyLevel.entries.singleOrNull { it.name.lowercase() == value }
                ?: throw IllegalArgumentException("Unsupported energy constraint")
        }
    }
    val tags = objectValue.arrayOrEmpty("tags").mapIndexed { index, element ->
        element.exactArrayString("tags[$index]")
    }
    val event = objectValue.nullableObject("dayweave_firm_block")?.let { block ->
        block.requireOnlyKeys(
            "owned",
            "starts_at",
            "ends_at",
            "all_day",
            "tentative",
            "busy",
        )
        require(block.exactBoolean("owned")) {
            "A locally owned firm block must set owned to true"
        }
        CanonicalEventTimingDraft(
            startsAt = block.exactString("starts_at"),
            endsAt = block.exactString("ends_at"),
            allDay = block.defaultBoolean("all_day", false),
            tentative = block.defaultBoolean("tentative", false),
            busy = block.defaultBoolean("busy", true),
        ).also { it.requireValid(timezoneName) }
    }
    if (event != null) {
        require(objectValue.keys == setOf("dayweave_firm_block")) {
            "A DayWeave firm block must be the event's sole metadata"
        }
    }
    val goalMeasures = objectValue.arrayWhenPresent("goal_measures")?.mapIndexed { index, element ->
        val measure = element as? JsonObject
            ?: throw IllegalArgumentException("goal_measures[$index] must be an object")
        measure.requireOnlyKeys("name", "target", "current", "unit")
        CanonicalGoalMeasureDraft(
            name = measure.exactString("name"),
            target = measure.exactLong("target"),
            current = measure.exactLong("current"),
            unit = measure.exactString("unit"),
        )
    }
    val constraints = CanonicalFlexibleConstraintsDraft(
        energy = energy,
        tags = tags,
        preferredStartMinute = objectValue.nullableInt("preferred_start_minute"),
        minimumGapMinutes = objectValue.defaultLong("minimum_gap_minutes", 0),
        maximumSessions = objectValue.nullableInt("maximum_sessions"),
        maximumSplitDays = objectValue.nullableInt("maximum_split_days"),
        energyStrength = energyStrength,
        scheduling = objectValue.objectWhenPresent("constraints")?.let(
            ::decodeSchedulingConstraints,
        ),
        hasOwnEffort = objectValue.optionalBoolean("has_own_effort"),
        goalIds = objectValue.arrayOrEmpty("goal_ids").mapIndexed { index, element ->
            element.exactArrayString("goal_ids[$index]")
        },
        habitTarget = objectValue.nullableObject("habit_target")?.let { target ->
            target.requireOnlyKeys("amount", "unit")
            CanonicalHabitTargetDraft(
                amount = target.exactLong("amount"),
                unit = target.exactString("unit"),
            )
        },
        preservesStreakWhenPaused = objectValue.optionalBoolean("preserves_streak_when_paused"),
        routineOrdered = objectValue.optionalBoolean("routine_ordered"),
        goalMeasures = goalMeasures,
        goalWeeklyAllocation = objectValue.nullableObject("goal_weekly_allocation")?.let { allocation ->
            allocation.requireOnlyKeys("minimum", "maximum")
            CanonicalWeeklyAllocationDraft(
                minimumMinutes = allocation.exactLong("minimum"),
                maximumMinutes = allocation.optionalExactLong("maximum"),
            )
        },
        breakCategory = objectValue.nullableString("break_category")?.let { value ->
            CanonicalBreakCategory.entries.singleOrNull { it.wireValue == value }
                ?: throw IllegalArgumentException("Unsupported break category")
        },
        breakMandatory = objectValue.optionalBoolean("break_mandatory"),
        breakPromptToResume = objectValue.optionalBoolean("break_prompt_to_resume"),
    ).normalized()
    constraints.requireValid(durationSeconds, event)
    return constraints to event
}

private fun decodeSchedulingConstraints(value: JsonObject): CanonicalSchedulingConstraintsDraft {
    val knownKeys = setOf(
        "earliest_start", "latest_finish", "minimum_notice", "allowed_weekdays",
        "preferred_daily_windows", "preferred_absolute_windows", "forbidden_windows",
        "required_contexts", "required_location", "maximum_daily_work",
        "maximum_weekly_work", "buffers", "dependencies", "occurrence_window",
    )
    require(value.keys.all { it in knownKeys }) {
        "Scheduling constraints contain unsupported fields"
    }
    fun qualifiedInstant(key: String, description: String) =
        value.nullableObject(key)?.let { wrapper ->
            wrapper.requireOnlyKeys("value", "strength")
            CanonicalQualifiedInstantDraft(
                value = wrapper.exactString("value"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also { it.requireValid(description) }
        }
    fun qualifiedMinutes(key: String, description: String) =
        value.nullableObject(key)?.let { wrapper ->
            wrapper.requireOnlyKeys("value", "strength")
            CanonicalQualifiedMinutesDraft(
                value = wrapper.exactLong("value"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also { it.requireValid(description) }
        }
    fun absoluteWindows(key: String, description: String) =
        value.arrayOrEmpty(key).mapIndexed { index, element ->
            val wrapper = element as? JsonObject
                ?: throw IllegalArgumentException("$key[$index] must be an object")
            wrapper.requireOnlyKeys("value", "strength")
            val window = wrapper.exactObject("value")
            window.requireOnlyKeys("start", "end")
            CanonicalAbsoluteWindowDraft(
                startsAt = window.exactString("start"),
                endsAt = window.exactString("end"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also { it.requireValid(description) }
        }

    val result = CanonicalSchedulingConstraintsDraft(
        earliestStart = qualifiedInstant("earliest_start", "constraint earliest start"),
        latestFinish = qualifiedInstant("latest_finish", "constraint latest finish"),
        minimumNotice = qualifiedMinutes("minimum_notice", "minimum notice"),
        allowedWeekdays = value.nullableObject("allowed_weekdays")?.let { wrapper ->
            wrapper.requireOnlyKeys("value", "strength")
            CanonicalQualifiedWeekdaysDraft(
                value = wrapper.exactWeekdays("value"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also(CanonicalQualifiedWeekdaysDraft::requireValid)
        },
        preferredDailyWindows =
            value.arrayOrEmpty("preferred_daily_windows").mapIndexed { index, element ->
                val wrapper = element as? JsonObject
                    ?: throw IllegalArgumentException(
                        "preferred_daily_windows[$index] must be an object",
                    )
                wrapper.requireOnlyKeys("value", "strength")
                val window = wrapper.exactObject("value")
                window.requireOnlyKeys("weekdays", "start_minute", "end_minute")
                CanonicalDailyWindowDraft(
                    weekdays = window.exactWeekdays("weekdays"),
                    startMinute = window.exactInt("start_minute"),
                    endMinute = window.exactInt("end_minute"),
                    strength = decodeConstraintStrength(wrapper.exactObject("strength")),
                ).also(CanonicalDailyWindowDraft::requireValid)
            },
        preferredAbsoluteWindows = absoluteWindows(
            "preferred_absolute_windows",
            "preferred absolute window",
        ),
        forbiddenWindows = absoluteWindows("forbidden_windows", "forbidden window"),
        requiredContexts = value.arrayOrEmpty("required_contexts").mapIndexed { index, element ->
            val wrapper = element as? JsonObject
                ?: throw IllegalArgumentException("required_contexts[$index] must be an object")
            wrapper.requireOnlyKeys("value", "strength")
            CanonicalQualifiedStringDraft(
                value = wrapper.exactString("value"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also { it.requireValid("required context") }
        },
        requiredLocation = value.nullableObject("required_location")?.let { wrapper ->
            wrapper.requireOnlyKeys("value", "strength")
            CanonicalQualifiedStringDraft(
                value = wrapper.exactString("value"),
                strength = decodeConstraintStrength(wrapper.exactObject("strength")),
            ).also { it.requireValid("required location") }
        },
        dependencies = value.arrayOrEmpty("dependencies").mapIndexed { index, element ->
            val dependency = element as? JsonObject
                ?: throw IllegalArgumentException("dependencies[$index] must be an object")
            dependency.requireOnlyKeys("item_id", "relation", "minimum_lag", "strength")
            CanonicalDependencyDraft(
                itemId = dependency.exactString("item_id"),
                relation = dependency.exactEnum(
                    "relation",
                    CanonicalDependencyRelation.entries,
                ) { it.wireValue },
                minimumLagMinutes = dependency.exactLong("minimum_lag"),
                strength = decodeConstraintStrength(dependency.exactObject("strength")),
            ).also(CanonicalDependencyDraft::requireValid)
        },
        maximumDailyWork = qualifiedMinutes(
            "maximum_daily_work",
            "maximum daily work",
        ),
        maximumWeeklyWork = qualifiedMinutes(
            "maximum_weekly_work",
            "maximum weekly work",
        ),
        buffers = value.objectWhenPresent("buffers")?.let { buffer ->
            buffer.requireOnlyKeys("before", "after", "strength")
            CanonicalBufferPolicyDraft(
                beforeMinutes = buffer.exactLong("before"),
                afterMinutes = buffer.exactLong("after"),
                strength = buffer.nullableObject("strength")?.let(::decodeConstraintStrength),
            ).also(CanonicalBufferPolicyDraft::requireValid)
        },
        includesNullOccurrenceWindow = value.containsKey("occurrence_window").also { present ->
            if (present) require(value["occurrence_window"] == JsonNull) {
                "A materialized occurrence window is system-owned and read-only"
            }
        },
    ).normalized()
    result.requireValid()
    return result
}

private fun decodeConstraintStrength(value: JsonObject): CanonicalConstraintStrengthDraft {
    val level = value.exactEnum("level", CanonicalConstraintLevel.entries) { it.wireValue }
    when (level) {
        CanonicalConstraintLevel.HARD -> value.requireOnlyKeys("level")
        CanonicalConstraintLevel.SOFT -> value.requireOnlyKeys("level", "weight")
    }
    return CanonicalConstraintStrengthDraft(
        level = level,
        weight = if (level == CanonicalConstraintLevel.SOFT) value.exactLong("weight") else null,
    ).also(CanonicalConstraintStrengthDraft::requireValid)
}

private fun JsonObject.requireKindMetadataKeys(kind: ItemKind) {
    fun rejectUnless(expected: ItemKind, vararg keys: String) {
        require(kind == expected || keys.none(::containsKey)) {
            "${keys.joinToString()} metadata is only valid for ${expected.name.lowercase()} items"
        }
    }
    rejectUnless(ItemKind.EVENT, "calendar_event", "calendar_context", "dayweave_firm_block")
    rejectUnless(ItemKind.HABIT, "habit_target", "preserves_streak_when_paused")
    rejectUnless(ItemKind.ROUTINE, "routine_ordered")
    rejectUnless(ItemKind.GOAL, "goal_measures", "goal_weekly_allocation")
    rejectUnless(
        ItemKind.BREAK,
        "break_category",
        "break_mandatory",
        "break_prompt_to_resume",
    )
}

private fun JsonElement.exactArrayString(description: String): String =
    (this as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        ?: throw IllegalArgumentException("$description must be a string")

private fun JsonObject.arrayOrEmpty(key: String): JsonArray = when (val value = this[key]) {
    null -> JsonArray(emptyList())
    is JsonArray -> value
    else -> throw IllegalArgumentException("$key must be an array")
}

private fun JsonObject.arrayWhenPresent(key: String): JsonArray? = when (val value = this[key]) {
    null -> null
    is JsonArray -> value
    else -> throw IllegalArgumentException("$key must be an array")
}

/** A defaulted concrete Rust field may be omitted, but cannot be explicitly null. */
private fun JsonObject.objectWhenPresent(key: String): JsonObject? = when (val value = this[key]) {
    null -> null
    is JsonObject -> value
    else -> throw IllegalArgumentException("$key must be an object")
}

/** A Rust `Option<T>` accepts both an omitted key and explicit JSON null. */
private fun JsonObject.nullableObject(key: String): JsonObject? = when (val value = this[key]) {
    null, JsonNull -> null
    is JsonObject -> value
    else -> throw IllegalArgumentException("$key must be an object or null")
}

private fun JsonObject.nullableString(key: String): String? = when (val value = this[key]) {
    null, JsonNull -> null
    else -> (value as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        ?: throw IllegalArgumentException("$key must be a string or null")
}

private fun JsonObject.nullableInt(key: String): Int? = when (val value = this[key]) {
    null, JsonNull -> null
    else -> (value as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.intOrNull
        ?: throw IllegalArgumentException("$key must be an integer or null")
}

private fun JsonObject.defaultLong(key: String, default: Long): Long = when (val value = this[key]) {
    null -> default
    else -> (value as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.longOrNull
        ?: throw IllegalArgumentException("$key must be an integer")
}

private fun JsonObject.defaultBoolean(key: String, default: Boolean): Boolean =
    if (containsKey(key)) exactBoolean(key) else default

private fun JsonObject.exactString(key: String): String =
    (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content
        ?: throw IllegalArgumentException("$key must be a string")

private fun JsonObject.exactBoolean(key: String): Boolean =
    (this[key] as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull
        ?: throw IllegalArgumentException("$key must be a Boolean")

private fun JsonObject.optionalBoolean(key: String): Boolean? = when (this[key]) {
    null -> null
    else -> exactBoolean(key)
}

private fun JsonObject.exactObject(key: String): JsonObject = this[key] as? JsonObject
    ?: throw IllegalArgumentException("$key must be an object")

private fun JsonObject.requireOnlyKeys(vararg keys: String) {
    require(this.keys.all { it in keys }) { "Object contains unsupported fields" }
}

private fun JsonObject.exactWeekdays(key: String): List<CanonicalWeekday> =
    (this[key] as? JsonArray)?.map { element ->
        val value = (element as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
            ?: throw IllegalArgumentException("$key must contain weekday strings")
        CanonicalWeekday.entries.singleOrNull { it.wireValue == value }
            ?: throw IllegalArgumentException("Unsupported recurrence weekday")
    } ?: throw IllegalArgumentException("$key must be an array")

private fun <T> JsonObject.exactEnum(
    key: String,
    values: Iterable<T>,
    wireValue: (T) -> String,
): T {
    val value = exactString(key)
    return values.singleOrNull { wireValue(it) == value }
        ?: throw IllegalArgumentException("Unsupported $key")
}

private fun JsonObject.optionalExactString(key: String): String? = when (val value = this[key]) {
    null, JsonNull -> null
    else -> (value as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        ?: throw IllegalArgumentException("$key must be a string or null")
}

private fun JsonObject.exactInt(key: String): Int = optionalInt(key)
    ?: throw IllegalArgumentException("$key must be an integer")

private fun JsonObject.optionalInt(key: String): Int? = this[key]?.let { element ->
    val primitive = element as? JsonPrimitive
        ?: throw IllegalArgumentException("$key must be an integer")
    primitive.takeUnless { it.isString }?.intOrNull
        ?: throw IllegalArgumentException("$key must be an integer")
}

private fun JsonObject.exactLong(key: String): Long {
    val primitive = this[key] as? JsonPrimitive
        ?: throw IllegalArgumentException("$key must be an integer")
    return primitive.takeUnless { it.isString }?.longOrNull
        ?: throw IllegalArgumentException("$key must be an integer")
}

private fun JsonObject.optionalExactLong(key: String): Long? = when (val value = this[key]) {
    null, JsonNull -> null
    else -> (value as? JsonPrimitive)?.takeUnless { it.isString }?.longOrNull
        ?: throw IllegalArgumentException("$key must be an integer or null")
}

internal fun requireCanonicalUuid(value: String, description: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed != UUID(0L, 0L) && parsed.toString() == value) {
        "Invalid $description identifier"
    }
}

private fun requireNonNilUuid(value: String, description: String): UUID {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(UUID_PATTERN.matches(value) && parsed != null && parsed != UUID(0L, 0L)) {
        "Invalid $description identifier"
    }
    return parsed
}

internal fun requireCanonicalInstant(value: String, description: String): Instant {
    val match = RFC3339_INSTANT_PATTERN.matchEntire(value)
    require(match != null) {
        "$description must be a strict RFC 3339 instant"
    }
    val groups = match.groupValues
    val fraction = groups[3]
    val normalizedLocal = buildString {
        append(groups[1])
        append('T')
        append(groups[2])
        if (fraction.isNotEmpty()) {
            append('.')
            append(fraction.take(9))
        }
    }
    val local = runCatching { LocalDateTime.parse(normalizedLocal) }.getOrElse {
        throw IllegalArgumentException("$description must be a strict RFC 3339 instant", it)
    }
    require(local.year in 1..9_999) { "$description has an unsupported RFC 3339 year" }
    val offsetSeconds = if (groups[4] == "Z") {
        0L
    } else {
        val offsetHours = groups[7].toInt()
        val offsetMinutes = groups[8].toInt()
        require(
            offsetMinutes <= 59 &&
                (offsetHours < 18 || offsetHours == 18 && offsetMinutes == 0),
        ) {
            "$description has an invalid RFC 3339 offset"
        }
        (offsetHours * 60L + offsetMinutes) * 60L * if (groups[6] == "-") -1L else 1L
    }
    val instant = runCatching {
        local.toInstant(ZoneOffset.UTC).minusSeconds(offsetSeconds)
    }.getOrElse {
        throw IllegalArgumentException("$description is outside the supported instant range", it)
    }
    require(instant.nano % 1_000 == 0) {
        "$description must use PostgreSQL microsecond precision"
    }
    return instant
}

private val UUID_PATTERN =
    "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        .toRegex()

private val RFC3339_INSTANT_PATTERN =
    ("^(\\d{4}-\\d{2}-\\d{2})T(\\d{2}:\\d{2}:\\d{2})" +
        "(?:\\.(\\d{1,9}))?((Z)|([+-])(\\d{2}):(\\d{2}))$").toRegex()

/**
 * Android's `ZoneId` parser also accepts fixed offsets and Java-only legacy identifiers that the
 * server's `chrono-tz` parser rejects. Keep local authoring to the conservative shared named-IANA
 * subset: chrono-tz's unqualified aliases or a slash-qualified tzdb identifier, excluding Java's
 * `SystemV` namespace.
 */
internal fun requireCanonicalTimezoneName(value: String): ZoneId {
    require(value.length in 1..100)
    require(
        value in CHRONO_TZ_UNQUALIFIED_NAMES ||
            value.contains('/') && !value.startsWith("SystemV/"),
    ) { "Canonical timezone must be a chrono-tz-compatible named identifier" }
    require(value in ZoneId.getAvailableZoneIds()) {
        "Canonical timezone is unavailable in the device tzdb"
    }
    return ZoneId.of(value)
}

private val CHRONO_TZ_UNQUALIFIED_NAMES = setOf(
    "CET", "CST6CDT", "Cuba", "EET", "EST", "EST5EDT", "Egypt", "Eire", "GB",
    "GB-Eire", "GMT", "GMT+0", "GMT-0", "GMT0", "Greenwich", "HST", "Hongkong",
    "Iceland", "Iran", "Israel", "Jamaica", "Japan", "Kwajalein", "Libya", "MET", "MST",
    "MST7MDT", "NZ", "NZ-CHAT", "Navajo", "PRC", "PST8PDT", "Poland", "Portugal", "ROC",
    "ROK", "Singapore", "Turkey", "UCT", "UTC", "Universal", "W-SU", "WET", "Zulu",
)

internal fun CanonicalItemSnapshot.requireCanonicalAuthoringShape() {
    requireCanonicalUuid(id, "canonical item")
    require(revision > 0)
    requireCanonicalTimezoneName(timezoneName)
    deadlineAt?.let { requireCanonicalInstant(it, "canonical deadline") }
    earliestStartAt?.let { requireCanonicalInstant(it, "canonical earliest start") }
    val created = requireCanonicalInstant(createdAt, "canonical item creation")
    val updated = requireCanonicalInstant(updatedAt, "canonical item update")
    require(updated >= created)
    completedAt?.let { requireCanonicalInstant(it, "canonical item completion") }
    deletedAt?.let { requireCanonicalInstant(it, "canonical item deletion") }
}

private fun requireCanonicalOrigin(value: String) {
    require(value.length <= 4_096)
    val uri = URI(value)
    require(uri.userInfo == null && uri.query == null && uri.fragment == null && uri.host != null)
    val isLoopbackHttp = uri.scheme == "http" && uri.host.lowercase() in setOf("localhost", "127.0.0.1", "::1")
    require(uri.scheme == "https" || isLoopbackHttp)
}

private val AUTHORING_JSON = Json { ignoreUnknownKeys = false }

internal object CanonicalAuthoringJournalPolicy {
    const val MAX_DIAGNOSTIC_BYTES = 2 * 1_024
    const val MAX_MUTATION_BYTES = 2 * 1_048_576
    const val MAX_AGGREGATE_MUTATION_BYTES = 4 * 1_048_576
}

internal fun canonicalAuthoringMutationBytes(
    mutation: PendingCanonicalAuthoringMutation,
): Int = AUTHORING_JOURNAL_JSON
    .encodeToString(PendingCanonicalAuthoringMutation.serializer(), mutation)
    .toByteArray(Charsets.UTF_8)
    .size

internal fun requireCanonicalAuthoringJournalBudget(
    mutations: List<PendingCanonicalAuthoringMutation>,
) {
    var aggregateBytes = 0
    mutations.forEach { mutation ->
        mutation.requireValid()
        val encoded = canonicalAuthoringMutationBytes(mutation)
        require(encoded <= CanonicalAuthoringJournalPolicy.MAX_MUTATION_BYTES)
        require(
            aggregateBytes <= CanonicalAuthoringJournalPolicy.MAX_AGGREGATE_MUTATION_BYTES - encoded,
        ) { "Canonical authoring journal exceeds its aggregate encoded-size budget" }
        aggregateBytes += encoded
    }
}

/** Bounded recovery window for encrypted recently-deleted canonical item bodies. */
internal object CanonicalTrashRetentionPolicy {
    const val MAX_ENTRIES = 500
    const val MAX_ITEM_BYTES = 256 * 1_024
    const val MAX_RETAINED_ITEM_BYTES = 4 * 1_024 * 1_024
    const val RETENTION_SECONDS = 30L * 24L * 60L * 60L
}

internal fun canonicalTrashItemBytes(item: CanonicalItemSnapshot): Int =
    TRASH_RETENTION_JSON.encodeToString(CanonicalItemSnapshot.serializer(), item)
        .toByteArray(Charsets.UTF_8)
        .size

/**
 * Keeps restore metadata for queued restores even after the ordinary thirty-day window, while
 * stripping expired or over-budget item bodies. A tombstone revision remains useful without its
 * body and therefore does not need to grow encrypted storage without bound.
 */
internal fun List<CanonicalRecentlyDeletedRecord>.boundedCanonicalTrash(
    referenceEpochMillis: Long,
    pinnedItemIds: Set<String>,
): List<CanonicalRecentlyDeletedRecord> {
    require(distinctBy(CanonicalRecentlyDeletedRecord::id).size == size) {
        "Recently-deleted canonical identifiers repeat"
    }
    val anchored = map { it.withLocalCanonicalRetentionAnchor(referenceEpochMillis) }
    anchored.forEach(CanonicalRecentlyDeletedRecord::requireValid)
    val cutoff = Instant.ofEpochMilli(referenceEpochMillis)
        .minusSeconds(CanonicalTrashRetentionPolicy.RETENTION_SECONDS)
    val sorted = anchored.sortedWith(
        compareByDescending<CanonicalRecentlyDeletedRecord> { it.canonicalRetentionAnchor() }
            .thenByDescending { Instant.parse(it.deletedAt) }
            .thenBy { it.id },
    )
    val pinned = sorted.filter { it.id in pinnedItemIds }
        .take(CanonicalTrashRetentionPolicy.MAX_ENTRIES)
    val unpinnedSlots = CanonicalTrashRetentionPolicy.MAX_ENTRIES - pinned.size
    val recent = sorted.asSequence()
        .filter { it.id !in pinnedItemIds && it.canonicalRetentionAnchor() >= cutoff }
        .take(unpinnedSlots)
        .toList()
    val candidates = (pinned + recent).sortedWith(
        compareByDescending<CanonicalRecentlyDeletedRecord> { it.canonicalRetentionAnchor() }
            .thenByDescending { Instant.parse(it.deletedAt) }
            .thenBy { it.id },
    )
    var retainedBodyBytes = 0
    return candidates.map { record ->
        val item = record.lastKnownItem
        val bodyBytes = item?.let(::canonicalTrashItemBytes)
        val canRetainBody = record.canonicalRetentionAnchor() >= cutoff &&
            bodyBytes != null &&
            bodyBytes <= CanonicalTrashRetentionPolicy.MAX_ITEM_BYTES &&
            bodyBytes <= CanonicalTrashRetentionPolicy.MAX_RETAINED_ITEM_BYTES - retainedBodyBytes
        if (canRetainBody) {
            retainedBodyBytes += requireNotNull(bodyBytes)
            record
        } else {
            record.copy(
                lastKnownItem = null,
                // Once the body is gone there is no safe own/ancestor privacy proof to display.
                effectiveIsSensitive = true,
            ).also(CanonicalRecentlyDeletedRecord::requireValid)
        }
    }
}

internal fun DayWeaveUiState.withCanonicalTrashRetention(
    referenceEpochMillis: Long,
): DayWeaveUiState {
    val cutoff = Instant.ofEpochMilli(referenceEpochMillis)
        .minusSeconds(CanonicalTrashRetentionPolicy.RETENTION_SECONDS)
    val anchoredDeleted = canonicalRecentlyDeleted.map {
        it.withLocalCanonicalRetentionAnchor(referenceEpochMillis)
    }
    val deletedById = anchoredDeleted.associateBy(CanonicalRecentlyDeletedRecord::id)
    val retainedMutations = pendingCanonicalAuthoringMutations.map { mutation ->
        val retentionAnchor = when (mutation.operation) {
            CanonicalAuthoringOperation.TRASH ->
                requireCanonicalInstant(mutation.createdAt, "canonical trash creation")
            CanonicalAuthoringOperation.RESTORE -> deletedById[mutation.itemId]
                ?.canonicalRetentionAnchor()
                ?: mutation.baseItem?.deletedAt?.let {
                    requireCanonicalInstant(it, "canonical restore deletion")
                }
            CanonicalAuthoringOperation.CREATE,
            CanonicalAuthoringOperation.REPLACE,
            -> null
        }
        if (mutation.baseItem != null && retentionAnchor != null && retentionAnchor < cutoff &&
            mutation.operation in setOf(
                CanonicalAuthoringOperation.TRASH,
                CanonicalAuthoringOperation.RESTORE,
            )
        ) {
                mutation.copy(baseItem = null).also(PendingCanonicalAuthoringMutation::requireValid)
        } else {
            mutation
        }
    }
    val pinned = retainedMutations.asSequence()
        .filter { it.operation == CanonicalAuthoringOperation.RESTORE }
        .map(PendingCanonicalAuthoringMutation::itemId)
        .toSet()
    val bounded = anchoredDeleted.boundedCanonicalTrash(referenceEpochMillis, pinned)
    return if (bounded == canonicalRecentlyDeleted &&
        retainedMutations == pendingCanonicalAuthoringMutations) {
        this
    } else {
        copy(
            pendingCanonicalAuthoringMutations = retainedMutations,
            canonicalRecentlyDeleted = bounded,
        )
    }
}

/** The next exclusive retention boundary that can change RAM or encrypted persistence. */
internal fun DayWeaveUiState.nextCanonicalTrashRetentionExpiryEpochMillis(
    referenceEpochMillis: Long,
): Long? {
    val pinned = pendingCanonicalAuthoringMutations.asSequence()
        .filter { it.operation == CanonicalAuthoringOperation.RESTORE }
        .map(PendingCanonicalAuthoringMutation::itemId)
        .toSet()
    val deletedById = canonicalRecentlyDeleted.associateBy(CanonicalRecentlyDeletedRecord::id)
    val deadlines = buildList {
        canonicalRecentlyDeleted.forEach { record ->
            if (record.lastKnownItem != null || record.id !in pinned) {
                add(record.canonicalRetentionAnchor().canonicalRetentionDeadlineEpochMillis())
            }
        }
        pendingCanonicalAuthoringMutations.forEach { mutation ->
            if (mutation.baseItem == null) return@forEach
            when (mutation.operation) {
                CanonicalAuthoringOperation.TRASH -> add(
                    requireCanonicalInstant(
                        mutation.createdAt,
                        "canonical trash creation",
                    ).canonicalRetentionDeadlineEpochMillis(),
                )
                CanonicalAuthoringOperation.RESTORE -> deletedById[mutation.itemId]
                    ?.canonicalRetentionAnchor()
                    ?.canonicalRetentionDeadlineEpochMillis()
                    ?.let(::add)
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> Unit
            }
        }
    }
    return deadlines.filter { it > referenceEpochMillis }.minOrNull()
}

private fun CanonicalRecentlyDeletedRecord.withLocalCanonicalRetentionAnchor(
    referenceEpochMillis: Long,
): CanonicalRecentlyDeletedRecord {
    val deletion = requireCanonicalInstant(deletedAt, "canonical deletion")
    val reference = Instant.ofEpochMilli(referenceEpochMillis)
    val existing = retentionAnchorAt?.let {
        requireCanonicalInstant(it, "canonical deletion retention anchor")
    }
    val anchor = listOfNotNull(deletion, reference, existing).minOrNull()
        ?: error("Canonical retention anchor is unavailable")
    return if (retentionAnchorAt == anchor.toString()) this else copy(
        retentionAnchorAt = anchor.toString(),
    )
}

private fun CanonicalRecentlyDeletedRecord.canonicalRetentionAnchor(): Instant =
    requireCanonicalInstant(
        requireNotNull(retentionAnchorAt) { "Canonical retention anchor is unavailable" },
        "canonical deletion retention anchor",
    )

private fun Instant.canonicalRetentionDeadlineEpochMillis(): Long {
    val retentionMillis = Math.multiplyExact(CanonicalTrashRetentionPolicy.RETENTION_SECONDS, 1_000L)
    val anchorMillis = runCatching { toEpochMilli() }.getOrElse {
        return if (this < Instant.EPOCH) Long.MIN_VALUE else Long.MAX_VALUE
    }
    if (anchorMillis > Long.MAX_VALUE - retentionMillis - 1L) return Long.MAX_VALUE
    return anchorMillis + retentionMillis + 1L
}

private val AUTHORING_JOURNAL_JSON = Json { encodeDefaults = true }
private val TRASH_RETENTION_JSON = Json { encodeDefaults = true }
