package com.greengolddog.dayweave.assistant

import java.nio.charset.StandardCharsets
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

const val DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1 = "dayweave.assistant-context/1"

const val MAX_ASSISTANT_MESSAGE_BYTES = 8 * 1_024
const val MAX_ASSISTANT_CONTEXT_BYTES = 64 * 1_024
const val MAX_ASSISTANT_HISTORY_ENTRIES = 20
const val MAX_ASSISTANT_HISTORY_BYTES = 32 * 1_024

@Serializable
data class AssistantScheduledBlock(
    val reference: String,
    val title: String,
    val kind: String,
    @SerialName("starts_at") val startsAt: String,
    @SerialName("ends_at") val endsAt: String,
    @SerialName("duration_minutes") val durationMinutes: Int,
    val status: String,
    val project: String?,
    val energy: String,
    @SerialName("is_flexible") val isFlexible: Boolean,
    @SerialName("is_hard_constraint") val isHardConstraint: Boolean,
)

@Serializable
data class AssistantPrivateBusySpan(
    @SerialName("starts_at") val startsAt: String,
    @SerialName("ends_at") val endsAt: String,
    @SerialName("duration_minutes") val durationMinutes: Int,
)

@Serializable
data class AssistantPlannerItem(
    val reference: String,
    @SerialName("parent_reference") val parentReference: String?,
    val title: String,
    val kind: String,
    val status: String,
    val timezone: String,
    @SerialName("duration_minutes") val durationMinutes: Int?,
    @SerialName("deadline_at") val deadlineAt: String?,
    @SerialName("earliest_start_at") val earliestStartAt: String?,
    @SerialName("split_policy") val splitPolicy: String,
    val importance: Int,
    val urgency: Int,
    @SerialName("is_recurring") val isRecurring: Boolean,
    @SerialName("is_executable") val isExecutable: Boolean,
)

@Serializable
data class AssistantContext(
    val schema: String = DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1,
    @SerialName("generated_at") val generatedAt: String,
    val timezone: String,
    @SerialName("scheduled_blocks") val scheduledBlocks: List<AssistantScheduledBlock>,
    @SerialName("private_busy_spans") val privateBusySpans: List<AssistantPrivateBusySpan>,
    @SerialName("total_scheduled_block_count") val totalScheduledBlockCount: Int,
    @SerialName("planner_items") val plannerItems: List<AssistantPlannerItem>,
    @SerialName("total_planner_item_count") val totalPlannerItemCount: Int,
    @SerialName("pending_suggestion_count") val pendingSuggestionCount: Int,
    @SerialName("omitted_fields") val omittedFields: List<String>,
)

@Serializable
enum class AssistantRole {
    @SerialName("user")
    USER,

    @SerialName("assistant")
    ASSISTANT,
}

@Serializable
data class AssistantHistoryMessage(
    val role: AssistantRole,
    val content: String,
)

@Serializable
data class AssistantTurnRequest(
    @SerialName("request_id") val requestId: String,
    val message: String,
    val history: List<AssistantHistoryMessage>,
    val context: AssistantContext,
)

@Serializable
data class AssistantTurnResponse(
    @SerialName("request_id") val requestId: String,
    val reply: String,
    val model: String,
    @SerialName("generated_at") val generatedAt: String,
)

internal val ASSISTANT_JSON: Json = Json {
    ignoreUnknownKeys = false
    explicitNulls = true
    encodeDefaults = true
}

internal fun String.utf8Size(): Int = toByteArray(StandardCharsets.UTF_8).size

internal fun String.isValidAssistantConversationText(maximumBytes: Int): Boolean =
    isNotBlank() && utf8Size() <= maximumBytes && hasOnlyValidAssistantScalars(
        allowConversationWhitespace = true,
    )

internal fun String.isValidAssistantContextText(
    maximumBytes: Int,
    allowEmpty: Boolean,
): Boolean =
    (allowEmpty || isNotBlank()) && utf8Size() <= maximumBytes && hasOnlyValidAssistantScalars(
        allowConversationWhitespace = false,
    )

private fun String.hasOnlyValidAssistantScalars(allowConversationWhitespace: Boolean): Boolean {
    var index = 0
    while (index < length) {
        val character = this[index]
        when {
            Character.isHighSurrogate(character) -> {
                if (index + 1 >= length || !Character.isLowSurrogate(this[index + 1])) return false
                index += 2
            }
            Character.isLowSurrogate(character) -> return false
            character.isAssistantDirectionalCharacter() -> return false
            character.isISOControl() &&
                !(allowConversationWhitespace &&
                    (character == '\n' || character == '\r' || character == '\t')) -> return false
            else -> index += 1
        }
    }
    return true
}

private fun Char.isAssistantDirectionalCharacter(): Boolean =
    this == '\u061C' || this == '\u200E' || this == '\u200F' ||
        this in '\u202A'..'\u202E' || this in '\u2066'..'\u2069'
