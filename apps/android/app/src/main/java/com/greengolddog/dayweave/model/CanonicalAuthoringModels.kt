package com.greengolddog.dayweave.model

import java.net.URI
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
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
) {
    fun requireValid() {
        require(weekdays.distinct().size == weekdays.size) { "Recurrence weekdays repeat" }
        when (kind) {
            CanonicalRecurrenceKind.DAILY,
            CanonicalRecurrenceKind.MONTHLY,
            -> require(
                occurrencesPerPeriod?.let { it in 1..UShort.MAX_VALUE.toInt() } == true &&
                    weekdays.isEmpty() && intervalSeconds == null,
            ) { "Daily and monthly recurrence require only a positive frequency" }

            CanonicalRecurrenceKind.WEEKLY -> require(
                occurrencesPerPeriod?.let { it in 1..UShort.MAX_VALUE.toInt() } == true &&
                    weekdays.isNotEmpty() && intervalSeconds == null,
            ) { "Weekly recurrence requires a frequency and distinct weekdays" }

            CanonicalRecurrenceKind.EVERY_INTERVAL,
            CanonicalRecurrenceKind.AFTER_COMPLETION,
            -> require(
                occurrencesPerPeriod == null && weekdays.isEmpty() &&
                    intervalSeconds?.let {
                        it in SECONDS_PER_MINUTE..MAX_INTERVAL_SECONDS &&
                            it % SECONDS_PER_MINUTE == 0L
                    } == true,
            ) { "Interval recurrence requires only a positive whole-minute interval" }
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
            }
        }
    }

    private companion object {
        const val SECONDS_PER_MINUTE = 60L
        const val MAX_INTERVAL_SECONDS = 366L * 24L * 60L * 60L
    }
}

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

/** Common scheduling restrictions kept typed instead of retaining an unreviewable JSON blob. */
@Serializable
data class CanonicalFlexibleConstraintsDraft(
    val energy: EnergyLevel? = null,
    val tags: List<String> = emptyList(),
    val preferredStartMinute: Int? = null,
    val minimumGapMinutes: Int = 0,
    val maximumSessions: Int? = null,
) {
    fun normalized(): CanonicalFlexibleConstraintsDraft = copy(
        tags = tags.map(String::trim).sorted(),
    )

    fun requireValid(
        durationSeconds: Long? = null,
        eventTiming: CanonicalEventTimingDraft? = null,
    ) {
        require(tags.size <= MAX_TAGS && tags.distinct().size == tags.size) {
            "Constraint tags must be distinct and bounded"
        }
        require(tags.all { it.isNotEmpty() && it.codePointCount(0, it.length) <= MAX_TAG_CHARS }) {
            "Constraint tags must be non-empty and bounded"
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
        require(minimumGapMinutes in 0..MAX_GAP_MINUTES)
        require(maximumSessions == null || maximumSessions in 1..UShort.MAX_VALUE.toInt())
        if (eventTiming != null) {
            require(
                energy == null && tags.isEmpty() && preferredStartMinute == null &&
                    minimumGapMinutes == 0 && maximumSessions == null,
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
            energy?.let { put("energy", it.name.lowercase()) }
            if (tags.isNotEmpty()) {
                put("tags", JsonArray(tags.map { JsonPrimitive(it) }))
            }
            preferredStartMinute?.let { put("preferred_start_minute", it) }
            if (minimumGapMinutes > 0) put("minimum_gap_minutes", minimumGapMinutes)
            maximumSessions?.let { put("maximum_sessions", it) }
            eventTiming?.let { timing ->
                put("dayweave_firm_block", timing.toCanonicalJson(timezoneName))
            }
        }
    }

    private companion object {
        const val SECONDS_PER_MINUTE = 60L
        const val MINUTES_PER_DAY = 24 * 60
        const val MAX_TAGS = 100
        const val MAX_TAG_CHARS = 100
        const val MAX_GAP_MINUTES = 366 * 24 * 60
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
        require(Duration.between(start, end).seconds <= CanonicalItemDraft.MAX_DURATION_SECONDS)
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
        value.split.requireValid(value.durationSeconds)
        require(value.importance in 0..100 && value.urgency in 0..100)
        require(value.siblingOrder in 0..MAX_SIBLING_ORDER)
        value.parentId?.let {
            requireCanonicalUuid(it, "canonical draft parent")
            require(it != itemId) { "An item cannot be its own parent" }
        }
        when (value.kind) {
            ItemKind.HABIT -> require(value.recurrence != null) { "Habits require recurrence" }
            ItemKind.EVENT,
            ItemKind.GOAL,
            ItemKind.BREAK,
            -> require(value.recurrence == null) { "This item type cannot recur" }
            ItemKind.TASK,
            ItemKind.ROUTINE,
            -> Unit
        }
        if (value.kind == ItemKind.EVENT) {
            val timing = requireNotNull(value.eventTiming) { "Events require fixed timing" }
            timing.requireValid(value.timezoneName)
            val exactDuration = Duration.between(
                requireCanonicalInstant(timing.startsAt, "firm block start"),
                requireCanonicalInstant(timing.endsAt, "firm block end"),
            ).seconds
            require(value.durationSeconds == exactDuration) {
                "Event duration must match its fixed timing"
            }
            require(
                earliest == requireCanonicalInstant(timing.startsAt, "firm block start") &&
                    deadline == requireCanonicalInstant(timing.endsAt, "firm block end"),
            ) { "Event timing must match its hard scheduling bounds" }
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

    fun matches(item: CanonicalItemSnapshot): Boolean {
        val value = normalized()
        return runCatching {
            value.requireValid(item.id)
            item.requireCanonicalAuthoringShape()
            item.deletedAt == null &&
                item.kind == value.kind.name.lowercase() &&
                item.status == value.placement.wireValue &&
                item.isSensitive == value.isSensitive &&
                item.title == value.title &&
                item.notes == value.notes &&
                item.timezoneName == value.timezoneName &&
                item.durationSeconds == value.durationSeconds &&
                sameInstant(item.deadlineAt, value.deadlineAt) &&
                sameInstant(item.earliestStartAt, value.earliestStartAt) &&
                canonicalJson(item.recurrenceJson) == value.recurrence?.toCanonicalJson() &&
                canonicalJson(item.flexibleConstraintsJson) ==
                value.constraints.toCanonicalJson(
                    value.eventTiming,
                    value.durationSeconds,
                    value.timezoneName,
                ) &&
                canonicalJson(item.splitPolicyJson) ==
                value.split.toCanonicalJson(value.durationSeconds) &&
                item.importance == value.importance && item.urgency == value.urgency &&
                item.parentId == value.parentId && item.siblingOrder == value.siblingOrder
        }.getOrDefault(false)
    }

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

        private fun canonicalJson(raw: String?): JsonElement? = raw?.let {
            STRICT_JSON.parseToJsonElement(it)
        }

        private fun sameInstant(left: String?, right: String?): Boolean = when {
            left == null || right == null -> left == right
            else -> Instant.parse(left) == Instant.parse(right)
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
            )
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
        it.requireValid(id)
        require(it.matches(this)) { "Canonical item contains unsupported authoring fields" }
    }
}

private fun decodeCanonicalRecurrence(raw: String): CanonicalRecurrenceDraft {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported recurrence")
    val type = (objectValue["type"] as? JsonPrimitive)?.content
    val result = when (type) {
        "daily" -> CanonicalRecurrenceDraft(
            CanonicalRecurrenceKind.DAILY,
            occurrencesPerPeriod = objectValue.exactInt("times_per_day"),
        )
        "weekly" -> CanonicalRecurrenceDraft(
            CanonicalRecurrenceKind.WEEKLY,
            occurrencesPerPeriod = objectValue.exactInt("times_per_week"),
            weekdays = (objectValue["weekdays"] as? JsonArray)?.map { element ->
                val value = (element as? JsonPrimitive)?.content
                CanonicalWeekday.entries.singleOrNull { it.wireValue == value }
                    ?: throw IllegalArgumentException("Unsupported recurrence weekday")
            } ?: throw IllegalArgumentException("Unsupported weekly recurrence"),
        )
        "monthly" -> CanonicalRecurrenceDraft(
            CanonicalRecurrenceKind.MONTHLY,
            occurrencesPerPeriod = objectValue.exactInt("times_per_month"),
        )
        "every_interval", "after_completion" -> CanonicalRecurrenceDraft(
            if (type == "every_interval") CanonicalRecurrenceKind.EVERY_INTERVAL
            else CanonicalRecurrenceKind.AFTER_COMPLETION,
            intervalSeconds = Math.multiplyExact(objectValue.exactLong("interval"), 60L),
        )
        else -> throw IllegalArgumentException("Unsupported recurrence")
    }
    require(result.toCanonicalJson() == objectValue) { "Recurrence contains unsupported fields" }
    return result
}

private fun decodeCanonicalSplit(raw: String): CanonicalSplitDraft {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported split policy")
    val result = when ((objectValue["type"] as? JsonPrimitive)?.content) {
        "indivisible" -> CanonicalSplitDraft()
        "splittable" -> CanonicalSplitDraft(
            kind = CanonicalSplitKind.SPLITTABLE,
            minimumChunkSeconds = objectValue.exactLong("minimum_chunk_seconds"),
            maximumChunkSeconds = objectValue.exactLong("maximum_chunk_seconds"),
        )
        else -> throw IllegalArgumentException("Unsupported split policy")
    }
    return result
}

private fun decodeCanonicalConstraints(
    raw: String,
    timezoneName: String,
    durationSeconds: Long?,
): Pair<CanonicalFlexibleConstraintsDraft, CanonicalEventTimingDraft?> {
    val objectValue = AUTHORING_JSON.parseToJsonElement(raw) as? JsonObject
        ?: throw IllegalArgumentException("Unsupported constraints")
    val knownKeys = setOf(
        "energy", "tags", "preferred_start_minute", "minimum_gap_minutes",
        "maximum_sessions", "dayweave_firm_block",
    )
    require(objectValue.keys.all { it in knownKeys }) { "Constraints contain unsupported fields" }
    val energy = (objectValue["energy"] as? JsonPrimitive)?.content?.let { value ->
        EnergyLevel.entries.singleOrNull { it.name.equals(value, ignoreCase = true) }
            ?: throw IllegalArgumentException("Unsupported energy constraint")
    }
    val tags = (objectValue["tags"] as? JsonArray)?.map {
        (it as? JsonPrimitive)?.content
            ?: throw IllegalArgumentException("Unsupported tag constraint")
    }.orEmpty()
    val event = (objectValue["dayweave_firm_block"] as? JsonObject)?.let { block ->
        require((block["owned"] as? JsonPrimitive)?.content == "true")
        CanonicalEventTimingDraft(
            startsAt = block.exactString("starts_at"),
            endsAt = block.exactString("ends_at"),
            allDay = block.exactBoolean("all_day"),
            tentative = block.exactBoolean("tentative"),
            busy = block.exactBoolean("busy"),
        ).also { require(it.toCanonicalJson(timezoneName) == block) }
    }
    val constraints = CanonicalFlexibleConstraintsDraft(
        energy = energy,
        tags = tags,
        preferredStartMinute = objectValue.optionalInt("preferred_start_minute"),
        minimumGapMinutes = objectValue.optionalInt("minimum_gap_minutes") ?: 0,
        maximumSessions = objectValue.optionalInt("maximum_sessions"),
    ).normalized()
    require(
        constraints.toCanonicalJson(event, durationSeconds, timezoneName) == objectValue,
    )
    return constraints to event
}

private fun JsonObject.exactString(key: String): String =
    (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content
        ?: throw IllegalArgumentException("$key must be a string")

private fun JsonObject.exactBoolean(key: String): Boolean =
    (this[key] as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull
        ?: throw IllegalArgumentException("$key must be a Boolean")

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

internal fun requireCanonicalUuid(value: String, description: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed != UUID(0L, 0L) && parsed.toString() == value) {
        "Invalid $description identifier"
    }
}

internal fun requireCanonicalInstant(value: String, description: String): Instant {
    val instant = Instant.parse(value)
    require(instant.nano % 1_000 == 0) {
        "$description must use PostgreSQL microsecond precision"
    }
    return instant
}

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
    const val RETENTION_SECONDS = 7L * 24L * 60L * 60L
}

internal fun canonicalTrashItemBytes(item: CanonicalItemSnapshot): Int =
    TRASH_RETENTION_JSON.encodeToString(CanonicalItemSnapshot.serializer(), item)
        .toByteArray(Charsets.UTF_8)
        .size

/**
 * Keeps restore metadata for queued restores even after the ordinary seven-day window, while
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
