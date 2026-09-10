package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.*
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneOffset
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*

/** Complete server schedule shape. Do not project normalized input through the narrower v1 DTO. */
@Serializable
data class RoutinePlanningSchedule(
    @SerialName("as_of") val asOf: String,
    @SerialName("horizon_start") val horizonStart: String,
    @SerialName("horizon_end") val horizonEnd: String,
    @SerialName("timezone_name") val timezoneName: String,
    val availability: List<ScheduleAvailabilityRequest>,
    @SerialName("fixed_blocks") val fixedBlocks: List<FixedScheduleBlockRequest>,
    @SerialName("previous_assignments") val previousAssignments: List<PreviousScheduleAssignmentRequest>,
    @SerialName("manual_placements") val manualPlacements: List<RoutinePlanningManualPlacement>,
    @SerialName("manual_placement_releases") val manualPlacementReleases: List<RoutinePlanningManualRelease>,
    val config: ScheduleConfigRequest,
    @SerialName("recurrence_context") val recurrenceContext: JsonObject,
) {
    fun requireValid() {
        requirePlanningInstant(asOf)
        val start = requirePlanningInstant(horizonStart); val end = requirePlanningInstant(horizonEnd)
        require(start < end && java.time.Duration.between(start, end) <= java.time.Duration.ofDays(90))
        requireCanonicalTimezoneName(timezoneName)
        require(availability.size <= 10_000 && fixedBlocks.size <= 10_000 && previousAssignments.size <= 10_000)
        require(config.slotGranularityMinutes in 1..60 && config.stabilityWeight in 0..1_000_000 && config.defaultSoftWeight in 0..1_000_000)
        availability.forEach {
            requirePlanningInterval(it.start, it.end)
            require(it.energy in setOf("low", "medium", "deep"))
            require(it.contexts.size <= 10_000); it.contexts.forEach(::requirePlanningUnicode)
            it.location?.let(::requirePlanningUnicode)
        }
        require(fixedBlocks.map { it.id }.toSet().size == fixedBlocks.size)
        fixedBlocks.forEach {
            requirePlanningUuid(it.id); requirePlanningInterval(it.start, it.end)
            requireProgressText(it.title, 500)
            require(it.source in setOf("google_calendar", "sleep", "protected_time", "travel", "manual"))
        }
        require(previousAssignments.sumOf { it.blocks.size.toLong() } <= 50_000)
        previousAssignments.forEach { requirePlanningAssignment(it.itemId, it.itemRevision, it.occurrenceId, it.blocks) }
        require(manualPlacements.size <= 64 && manualPlacementReleases.size <= 64)
        require(manualPlacements.sumOf { it.assignments.size.toLong() } <= 128)
        require(manualPlacements.sumOf { placement -> placement.assignments.sumOf { it.blocks.size.toLong() } } <= 256)
        require(manualPlacements.map { it.id }.toSet().size == manualPlacements.size)
        manualPlacements.forEach { placement ->
            requirePlanningUuid(placement.id); placement.sourceScheduleRevisionId?.let(::requirePlanningUuid)
            require(placement.assignments.isNotEmpty() && placement.assignments.size <= 10_000)
            placement.assignments.forEach { requirePlanningAssignment(it.itemId, it.itemRevision, it.occurrenceId, it.blocks) }
        }
        manualPlacementReleases.forEach {
            requirePlanningUuid(it.id); requirePlanningUuid(it.placementId); requirePlanningUuid(it.sourceScheduleRevisionId)
        }
        requirePlanningRecurrenceContext(recurrenceContext)
    }

    internal fun requireImmutableInput(original: RoutinePlanningSchedule) {
        require(requirePlanningInstant(asOf) == requirePlanningInstant(original.asOf))
        require(requirePlanningInstant(horizonStart) == requirePlanningInstant(original.horizonStart))
        require(requirePlanningInstant(horizonEnd) == requirePlanningInstant(original.horizonEnd))
        require(timezoneName == original.timezoneName && config == original.config)
        require(availability.size == original.availability.size && fixedBlocks.size == original.fixedBlocks.size)
        availability.zip(original.availability).forEach { (current, prior) ->
            require(requirePlanningInstant(current.start) == requirePlanningInstant(prior.start) && requirePlanningInstant(current.end) == requirePlanningInstant(prior.end))
            require(current.copy(start = prior.start, end = prior.end) == prior)
        }
        fixedBlocks.zip(original.fixedBlocks).forEach { (current, prior) ->
            require(requirePlanningInstant(current.start) == requirePlanningInstant(prior.start) && requirePlanningInstant(current.end) == requirePlanningInstant(prior.end))
            require(current.copy(start = prior.start, end = prior.end) == prior)
        }
        require(manualPlacements == original.manualPlacements && manualPlacementReleases == original.manualPlacementReleases)
        val before = completePlanningRecurrenceContext(original.recurrenceContext)
        val after = completePlanningRecurrenceContext(recurrenceContext)
        for (field in listOf("calendar", "rolling_anchors", "minimum_spacing")) require(before[field] == after[field])
    }

    internal fun requireNormalizationOf(original: RoutinePlanningSchedule, items: List<CanonicalItemSnapshot>, lifecycle: RoutinePlanningLifecycle) {
        requireImmutableInput(original)
        val sources = items.associateBy { it.id }
        previousAssignments.forEach { require(sources[it.itemId]?.revision == it.itemRevision) }
        val habits = items.filter { it.kind == "habit" }.map { it.id }.toSet()
        val before = completePlanningRecurrenceContext(original.recurrenceContext)
        val after = completePlanningRecurrenceContext(recurrenceContext)
        fun nonHabitMap(value: JsonElement?) = requireNotNull(value).jsonObject.filterKeys { it !in habits }
        require(nonHabitMap(before["completion_anchors"]) == nonHabitMap(after["completion_anchors"]))
        fun nonHabitRows(value: JsonElement?) = requireNotNull(value).jsonArray.filter { it.jsonObject.text("item_id") !in habits }
        for (field in listOf("pauses", "exceptions")) require(nonHabitRows(before[field]) == nonHabitRows(after[field]))
        val managed = lifecycle.instances.map { it.occurrenceId }.toSet()
        fun completion(value: JsonElement?) = requireNotNull(value).jsonArray.map { it.jsonPrimitive.content }.toSet()
        val remaining = completion(after["completed_occurrence_ids"])
        require(remaining.intersect(managed).isEmpty())
        require(after.getValue("partial_progress").jsonObject.keys.intersect(managed).isEmpty())
        // Only Habit normalization can rewrite non-managed opaque occurrence IDs.
        // Their exact generated ownership is subsequently checked by helper-v2.
        if (habits.isEmpty()) {
            require(remaining == completion(before["completed_occurrence_ids"]) - managed)
            require(after.getValue("partial_progress").jsonObject == JsonObject(before.getValue("partial_progress").jsonObject.filterKeys { it !in managed }))
        }
    }

    override fun toString() = "RoutinePlanningSchedule(<protected>)"

    companion object {
        fun from(request: SchedulePreviewRequest) = RoutinePlanningSchedule(
            request.asOf, request.horizonStart, request.horizonEnd, request.timezoneName,
            request.availability, request.fixedBlocks, request.previousAssignments, emptyList(), emptyList(), request.config,
            completePlanningRecurrenceContext(request.recurrenceContext).let { JsonObject(it.filterKeys { key -> key != "partial_progress" || it.getValue(key).jsonObject.isNotEmpty() }) },
        )
    }
}

@Serializable
data class RoutinePlanningManualPlacement(
    val id: String,
    @SerialName("source_schedule_revision_id") val sourceScheduleRevisionId: String?,
    val assignments: List<RoutinePlanningManualAssignment>,
)
@Serializable
data class RoutinePlanningManualAssignment(
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    @SerialName("occurrence_id") val occurrenceId: String?,
    val blocks: List<PreviousScheduleBlockRequest>,
)
@Serializable
data class RoutinePlanningManualRelease(
    val id: String,
    @SerialName("placement_id") val placementId: String,
    @SerialName("source_schedule_revision_id") val sourceScheduleRevisionId: String,
)

internal fun requirePlanningInstant(value: String): Instant {
    val match = requireNotNull(Regex("([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,6})?)(Z|([+-])([0-9]{2}):([0-9]{2}))").matchEntire(value))
    val local = LocalDateTime.parse(match.groupValues[1]); require(local.year in 1..9999)
    val offset = if (match.groupValues[2] == "Z") 0 else {
        val hour = match.groupValues[4].toInt(); val minute = match.groupValues[5].toInt()
        require(hour <= 23 && minute <= 59)
        (hour * 3600 + minute * 60) * if (match.groupValues[3] == "-") -1 else 1
    }
    return local.toInstant(ZoneOffset.UTC).minusSeconds(offset.toLong())
}

private fun requirePlanningInterval(start: String, end: String) { require(requirePlanningInstant(start) < requirePlanningInstant(end)) }
private fun requirePlanningAssignment(id: String, revision: Long, occurrence: String?, blocks: List<PreviousScheduleBlockRequest>) {
    requirePlanningUuid(id); require(revision > 0); occurrence?.let(::requirePlanningOccurrenceId)
    require(blocks.size <= 50_000)
    blocks.forEach { requirePlanningInterval(it.start, it.end); require(it.sessionIndex in 0..65_535) }
}

internal fun requirePlanningIdentity(value: JsonObject) {
    fun ordinal() { require(value.integer("bucket_ordinal") in 0..65_535) }
    when (value.text("type")) {
        "calendar_day" -> { value.fields("type", "date", "bucket_ordinal"); requirePlanningDate(value.text("date")); ordinal() }
        "calendar_week" -> { value.fields("type", "week_key", "bucket_ordinal"); require(value.integer("week_key") in Int.MIN_VALUE..Int.MAX_VALUE); ordinal() }
        "calendar_month" -> { value.fields("type", "year", "month", "bucket_ordinal"); require(value.integer("year") in 1..9999 && value.integer("month") in 1..12); ordinal() }
        "rolling_minutes" -> { value.fields("type", "index", "anchor"); require(value.integer("index") >= 0); requirePlanningInstant(value.text("anchor")) }
        "after_completion" -> { value.fields("type", "anchor"); requirePlanningInstant(value.text("anchor")) }
        "rolling_month" -> { value.fields("type", "cycle", "index", "anchor"); require(value.integer("cycle") >= 0 && value.integer("index") in 0..65_535); requirePlanningInstant(value.text("anchor")) }
        "custom_rule" -> { value.fields("type", "rule_id", "sequence", "date"); requirePlanningOccurrenceId(value.text("rule_id")); require(value.integer("sequence") in 0..4_294_967_295L); requirePlanningDate(value.text("date")) }
        else -> error("Unsupported planning identity")
    }
}

private fun requirePlanningDate(value: String) {
    require(Regex("[0-9]{4}-[0-9]{2}-[0-9]{2}").matches(value))
    require(LocalDate.parse(value).year in 1..9999)
}

internal fun completePlanningRecurrenceContext(value: JsonObject): JsonObject {
    val defaults = buildJsonObject {
        put("calendar", buildJsonObject { put("time_zone_id", JsonNull); put("week_starts_on", "monday"); put("days", JsonArray(emptyList())) })
        for (key in listOf("completion_anchors", "rolling_anchors", "minimum_spacing", "partial_progress")) put(key, JsonObject(emptyMap()))
        for (key in listOf("completed_occurrence_ids", "pauses", "exceptions")) put(key, JsonArray(emptyList()))
    }
    require(value.keys.all { it in defaults })
    return JsonObject(defaults + value)
}

private fun requirePlanningRecurrenceContext(raw: JsonObject) {
    val value = completePlanningRecurrenceContext(raw)
    val calendar = value.getValue("calendar").jsonObject
    calendar.fields("time_zone_id", "week_starts_on", "days")
    if (calendar.getValue("time_zone_id") != JsonNull) requireCanonicalTimezoneName(calendar.text("time_zone_id"))
    require(calendar.text("week_starts_on") in setOf("monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"))
    val days = calendar.getValue("days").jsonArray
    require(days.size <= 92)
    days.forEach { day ->
        val row = day.jsonObject; row.fields("local_date", "start", "end")
        requirePlanningCoreDate(row.getValue("local_date"))
        requirePlanningInterval(row.text("start"), row.text("end"))
    }
    var entries = 0L
    for (field in listOf("completion_anchors", "rolling_anchors", "minimum_spacing", "partial_progress")) {
        val map = value.getValue(field).jsonObject; entries += map.size
        map.forEach { (id, entry) ->
            requirePlanningUuid(id)
            when (field) {
                "completion_anchors", "rolling_anchors" -> requirePlanningInstant(entry.string())
                "minimum_spacing" -> require(entry.integer() in 0..527_040)
                else -> {
                    requirePlanningOccurrenceId(id)
                    val progress = entry.jsonObject
                    require(progress.keys == setOf("progress_basis_points", "expected_duration_minutes") || progress.keys == setOf("progress_basis_points", "expected_duration_minutes", "remaining_duration_minutes"))
                    require(progress.integer("progress_basis_points") in 1..9999)
                    val duration = progress.integer("expected_duration_minutes"); require(duration in 1..527_040)
                    progress["remaining_duration_minutes"]?.let { require(it.integer() in 1..duration) }
                }
            }
        }
    }
    val completed = value.getValue("completed_occurrence_ids").jsonArray; entries += completed.size
    require(completed.map { it.string().also(::requirePlanningOccurrenceId) }.toSet().size == completed.size)
    val pauses = value.getValue("pauses").jsonArray; entries += pauses.size
    pauses.forEach { pause ->
        val row = pause.jsonObject; row.fields("item_id", "start", "end")
        requirePlanningUuid(row.text("item_id")); requirePlanningInterval(row.text("start"), row.text("end"))
    }
    val exceptions = value.getValue("exceptions").jsonArray; entries += exceptions.size
    exceptions.forEach { exception ->
        val row = exception.jsonObject; row.fields("item_id", "selector", "action"); requirePlanningUuid(row.text("item_id"))
        val selector = row.getValue("selector").jsonObject
        when (selector.text("type")) {
            "occurrence" -> { selector.fields("type", "id"); requirePlanningOccurrenceId(selector.text("id")) }
            "local_date" -> { selector.fields("type", "date"); requirePlanningCoreDate(selector.getValue("date")) }
            "nominal_start" -> { selector.fields("type", "at"); requirePlanningInstant(selector.text("at")) }
            else -> error("Unsupported planning selector")
        }
        val action = row.getValue("action").jsonObject
        when (action.text("type")) {
            "skip" -> action.fields("type")
            "move" -> {
                action.fields("type", "start", "end", "source"); requirePlanningInterval(action.text("start"), action.text("end"))
                val source = action.getValue("source").jsonObject
                source.fields("item_revision", "identity", "nominal_start", "nominal_end", "local_date", "ordinal")
                require(source.integer("item_revision") > 0 && source.integer("ordinal") in 0..4_294_967_295L)
                requirePlanningIdentity(source.getValue("identity").jsonObject)
                requirePlanningInterval(source.text("nominal_start"), source.text("nominal_end"))
                source.getValue("local_date").let { if (it != JsonNull) requirePlanningDate(it.string()) }
            }
            else -> error("Unsupported planning action")
        }
    }
    require(entries <= 10_000)
}

/** time::Date's explicit core serde tuple is not an ISO identity date. */
private fun requirePlanningCoreDate(value: JsonElement) {
    val tuple = value.jsonArray; require(tuple.size == 2)
    val year = tuple[0].integer(); val ordinal = tuple[1].integer()
    require(year in 1..9999 && ordinal in 1..366)
    LocalDate.ofYearDay(year.toInt(), ordinal.toInt())
}

private fun JsonObject.fields(vararg keys: String) { require(this.keys == keys.toSet()) }
private fun JsonElement.string(): String = jsonPrimitive.also { require(it.isString) }.content
private fun JsonObject.text(key: String) = getValue(key).string()
private fun JsonElement.integer(): Long = jsonPrimitive.also { require(!it.isString && Regex("(?:0|[1-9][0-9]*|-[1-9][0-9]*)").matches(it.content)) }.content.toLong()
private fun JsonObject.integer(key: String) = getValue(key).integer()
