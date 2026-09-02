package com.greengolddog.dayweave.assistant

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.util.Locale
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

sealed class AssistantContextProjectionException(message: String) : IllegalArgumentException(message) {
    class ContextTooLarge : AssistantContextProjectionException(
        "The redacted assistant context exceeds 64 KiB",
    )
}

/**
 * Projects encrypted planner state into the only planner shape that may cross the assistant API.
 * Stable identities and free-form private fields do not exist in this type boundary.
 */
object AssistantContextProjector {
    const val MAX_SCHEDULED_BLOCKS = 48
    const val MAX_PRIVATE_BUSY_SPANS = 48
    const val MAX_PLANNER_ITEMS = 64

    val OMITTED_FIELDS: List<String> = listOf(
        "account identity and credentials",
        "app-storage paths and server configuration",
        "notes and placement diagnostics",
        "raw recurrence and flexible-constraint payloads",
        "stable item, occurrence, and revision identifiers",
        "sensitive item content; occupancy is represented only as generic busy spans",
    )

    fun project(
        state: DayWeaveUiState,
        generatedAt: Instant = Instant.now(),
    ): AssistantContext {
        val itemIdCounts = state.canonicalItems.groupingBy(CanonicalItemSnapshot::id).eachCount()
        fun itemIsSensitive(itemId: String): Boolean =
            itemIdCounts[itemId] != 1 || effectiveCanonicalSensitivity(
                items = state.canonicalItems,
                itemId = itemId,
                pendingMutation = state.pendingCanonicalMutation,
                pendingAuthoringMutations = state.pendingCanonicalAuthoringMutations,
            )

        val classifiedBlocks = state.schedule.mapNotNull { block ->
            block.validInterval()?.let { interval ->
                ClassifiedBlock(
                    block = block,
                    start = interval.first,
                    end = interval.second,
                    isSensitive = block.isSensitive || when (val itemId = block.canonicalItemId) {
                        null -> block.canonicalBlockKind in CANONICAL_IDENTITY_REQUIRED_BLOCK_KINDS
                        else -> itemIsSensitive(itemId)
                    },
                )
            }
        }
        val publicBlocks = classifiedBlocks
            .asSequence()
            .filterNot(ClassifiedBlock::isSensitive)
            .sortedWith(
                compareBy<ClassifiedBlock>(ClassifiedBlock::start, ClassifiedBlock::end)
                    .thenBy { it.block.title }
                    .thenBy { it.block.kind.name }
                    .thenBy { it.block.status.name }
                    .thenBy { it.block.project ?: "" }
                    .thenBy { it.block.energy.name }
                    .thenBy { it.block.isFlexible }
                    .thenBy { it.block.isHardConstraint }
                    .thenBy { it.block.id },
            )
            .take(MAX_SCHEDULED_BLOCKS)
            .mapIndexed { index, classified -> classified.toPublicBlock(index + 1) }
            .toList()
        val privateSpans = classifiedBlocks
            .asSequence()
            .filter(ClassifiedBlock::isSensitive)
            .sortedWith(
                compareBy<ClassifiedBlock>(ClassifiedBlock::start, ClassifiedBlock::end)
                    .thenBy { it.block.id },
            )
            .take(MAX_PRIVATE_BUSY_SPANS)
            .map(ClassifiedBlock::toPrivateSpan)
            .toList()

        val nonSensitiveItems = state.canonicalItems
            .asSequence()
            .filter { item -> itemIdCounts[item.id] == 1 && !itemIsSensitive(item.id) }
            .sortedWith(CANONICAL_ITEM_ORDER)
            .toList()
        val includedItems = nonSensitiveItems.take(MAX_PLANNER_ITEMS)
        val references = includedItems.mapIndexed { index, item ->
            item.id to "item-${index + 1}"
        }.toMap()
        val plannerItems = includedItems.mapIndexed { index, item ->
            item.toAssistantItem(index + 1, references)
        }

        val timezone = sequenceOf(state.schedulePlanningZoneId, ZoneId.systemDefault().id)
            .filterNotNull()
            .firstOrNull(::isNamedTimezone)
            ?: "UTC"
        val context = AssistantContext(
            generatedAt = generatedAt.toString(),
            timezone = safeText(timezone, MAX_TIMEZONE_BYTES),
            scheduledBlocks = publicBlocks,
            privateBusySpans = privateSpans,
            totalScheduledBlockCount = state.schedule.size,
            plannerItems = plannerItems,
            totalPlannerItemCount = nonSensitiveItems.size,
            pendingSuggestionCount = state.suggestions.count {
                it.disposition == SuggestionDisposition.PENDING
            },
            omittedFields = OMITTED_FIELDS,
        )
        if (ASSISTANT_JSON.encodeToString(context).utf8Size() > MAX_ASSISTANT_CONTEXT_BYTES) {
            throw AssistantContextProjectionException.ContextTooLarge()
        }
        return context
    }

    private data class ClassifiedBlock(
        val block: ScheduleItem,
        val start: Instant,
        val end: Instant,
        val isSensitive: Boolean,
    ) {
        private val durationMinutes: Int
            get() = Duration.between(start, end).toMinutes().coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

        fun toPublicBlock(referenceNumber: Int) = AssistantScheduledBlock(
            reference = "block-$referenceNumber",
            title = safeText(block.title, MAX_TITLE_BYTES),
            kind = block.kind.name.lowercase(Locale.ROOT),
            startsAt = start.toString(),
            endsAt = end.toString(),
            durationMinutes = durationMinutes,
            status = block.status.name.lowercase(Locale.ROOT),
            project = block.project?.let { safeText(it, MAX_PROJECT_BYTES) },
            energy = block.energy.name.lowercase(Locale.ROOT),
            isFlexible = block.isFlexible,
            isHardConstraint = block.isHardConstraint,
        )

        fun toPrivateSpan() = AssistantPrivateBusySpan(
            startsAt = start.toString(),
            endsAt = end.toString(),
            durationMinutes = durationMinutes,
        )
    }

    private fun ScheduleItem.validInterval(): Pair<Instant, Instant>? {
        val start = absoluteStartAt?.let { runCatching { Instant.parse(it) }.getOrNull() }
            ?: return null
        val end = absoluteEndAt?.let { runCatching { Instant.parse(it) }.getOrNull() }
            ?: return null
        return (start to end).takeIf {
            end > start && Duration.between(start, end).toMinutes() in 1..Int.MAX_VALUE.toLong()
        }
    }

    private fun CanonicalItemSnapshot.toAssistantItem(
        referenceNumber: Int,
        references: Map<String, String>,
    ) = AssistantPlannerItem(
        reference = "item-$referenceNumber",
        parentReference = parentId?.let(references::get),
        title = safeText(title, MAX_TITLE_BYTES),
        kind = safeText(kind.lowercase(Locale.ROOT), MAX_ENUM_BYTES),
        status = safeText(status.lowercase(Locale.ROOT), MAX_ENUM_BYTES),
        timezone = safeText(timezoneName, MAX_TIMEZONE_BYTES),
        durationMinutes = durationSeconds?.let { seconds ->
            (seconds / 60L).takeIf { it in 0..Int.MAX_VALUE.toLong() }?.toInt()
        },
        deadlineAt = deadlineAt.canonicalInstantOrNull(),
        earliestStartAt = earliestStartAt.canonicalInstantOrNull(),
        splitPolicy = displaySplitPolicy(splitPolicyJson),
        importance = importance,
        urgency = urgency,
        isRecurring = recurrenceJson != null,
        isExecutable = isExecutable,
    )

    private fun String?.canonicalInstantOrNull(): String? = this?.let { raw ->
        runCatching { Instant.parse(raw).toString() }.getOrNull()
    }

    private fun displaySplitPolicy(raw: String): String {
        val value = runCatching { ASSISTANT_JSON.parseToJsonElement(raw) as? JsonObject }.getOrNull()
            ?: return UNSUPPORTED_SPLIT_POLICY
        val type = (value["type"] as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        return when {
            type == "indivisible" && value.keys == setOf("type") -> "indivisible"
            type == "splittable" && value.keys == SPLITTABLE_KEYS -> {
                val minimum = (value["minimum_chunk_seconds"] as? JsonPrimitive)
                    ?.takeUnless(JsonPrimitive::isString)?.longOrNull
                val maximum = (value["maximum_chunk_seconds"] as? JsonPrimitive)
                    ?.takeUnless(JsonPrimitive::isString)?.longOrNull
                if (minimum != null && maximum != null && minimum > 0 && maximum >= minimum) {
                    "splittable ${minimum / 60L}-${maximum / 60L} minutes"
                } else {
                    UNSUPPORTED_SPLIT_POLICY
                }
            }
            else -> UNSUPPORTED_SPLIT_POLICY
        }
    }

    private fun safeText(value: String, maximumBytes: Int): String {
        val candidate = value.trim()
        val result = StringBuilder()
        var usedBytes = 0
        var inspectedCodePoints = 0
        var offset = 0
        while (offset < candidate.length && inspectedCodePoints < maximumBytes * 4) {
            val codePoint = candidate.codePointAt(offset)
            offset += Character.charCount(codePoint)
            inspectedCodePoints += 1
            if (codePoint.isForbiddenAssistantContextCodePoint()) continue
            val scalar = String(Character.toChars(codePoint))
            val scalarBytes = scalar.utf8Size()
            if (usedBytes > maximumBytes - scalarBytes) break
            result.append(scalar)
            usedBytes += scalarBytes
        }
        return result.toString().trim()
    }

    private fun isNamedTimezone(value: String): Boolean =
        value in ZoneId.getAvailableZoneIds() && runCatching { ZoneId.of(value) }.isSuccess

    private val CANONICAL_ITEM_ORDER = compareBy<CanonicalItemSnapshot>(
        { it.kind.lowercase(Locale.ROOT) },
        { it.status.lowercase(Locale.ROOT) },
        { it.title },
        { it.timezoneName },
        { it.deadlineAt ?: "" },
        { it.earliestStartAt ?: "" },
        { it.parentId ?: "" },
        CanonicalItemSnapshot::siblingOrder,
        CanonicalItemSnapshot::id,
    )

    private val CANONICAL_IDENTITY_REQUIRED_BLOCK_KINDS = setOf(
        "planned",
        "pinned",
        "remote_execution_lease",
    )
    private val SPLITTABLE_KEYS = setOf(
        "type",
        "minimum_chunk_seconds",
        "maximum_chunk_seconds",
    )
    private fun Int.isForbiddenAssistantContextCodePoint(): Boolean =
        Character.isISOControl(this) || this == 0x061C || this == 0x200E || this == 0x200F ||
            this in 0x202A..0x202E || this in 0x2066..0x2069 || this in 0xD800..0xDFFF

    private const val MAX_TITLE_BYTES = 160
    private const val MAX_PROJECT_BYTES = 80
    private const val MAX_TIMEZONE_BYTES = 64
    private const val MAX_ENUM_BYTES = 32
    private const val UNSUPPORTED_SPLIT_POLICY = "unsupported/read-only"
}
