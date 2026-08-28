package com.greengolddog.dayweave.model

import java.time.LocalTime
import java.time.format.DateTimeFormatter
import kotlinx.serialization.Serializable

@Serializable
enum class AppDestination(val label: String) {
    TODAY("Today"),
    CALENDAR("Calendar"),
    INBOX("Inbox"),
    ASSISTANT("Assistant"),
    MORE("More"),
}

@Serializable
enum class ItemKind(val label: String) {
    EVENT("Event"),
    TASK("Task"),
    HABIT("Habit"),
    ROUTINE("Routine"),
    GOAL("Goal"),
    BREAK("Break"),
}

@Serializable
enum class ItemStatus(val label: String) {
    NOT_STARTED("Not started"),
    SCHEDULED("Scheduled"),
    ACTIVE("In progress"),
    PAUSED("Paused"),
    COMPLETED("Completed"),
    SKIPPED("Skipped"),
    CANCELED("Canceled"),
    BLOCKED("Blocked"),
}

@Serializable
enum class EnergyLevel(val label: String) {
    LOW("Low"),
    MEDIUM("Medium"),
    DEEP("Deep"),
}

@Serializable
data class ScheduleItem(
    val id: String,
    val title: String,
    val kind: ItemKind,
    val startMinute: Int,
    val durationMinutes: Int,
    val status: ItemStatus,
    val project: String? = null,
    val energy: EnergyLevel = EnergyLevel.MEDIUM,
    val isFlexible: Boolean = true,
    val isHardConstraint: Boolean = false,
    val isSplittable: Boolean = false,
    val note: String = "",
    val actualMinutes: Int? = null,
) {
    val endMinute: Int get() = startMinute + durationMinutes

    fun timeRange(): String = "${formatMinutes(startMinute)}–${formatMinutes(endMinute)}"

    private fun formatMinutes(total: Int): String {
        val normalized = total.coerceIn(0, 24 * 60 - 1)
        return LocalTime.of(normalized / 60, normalized % 60).format(TIME_FORMAT)
    }

    private companion object {
        val TIME_FORMAT: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm")
    }
}

@Serializable
data class ActiveSession(
    val itemId: String,
    val elapsedMinutes: Int,
    val isPaused: Boolean,
    val pauseLabel: String? = null,
)

@Serializable
enum class SuggestionDisposition {
    PENDING,
    APPROVED_FOR_INBOX,
    REJECTED,
}

@Serializable
enum class SuggestionKind(val label: String) {
    SCHEDULE_CHANGE("Schedule change"),
    NEW_TASK("New task"),
    GOAL_BREAKDOWN("Goal breakdown"),
    CONSTRAINT_CHANGE("Constraint change"),
}

@Serializable
data class PlanningSuggestion(
    val id: String,
    val title: String,
    val summary: String,
    val source: String,
    val kind: SuggestionKind,
    val expiresInDays: Int,
    val disposition: SuggestionDisposition = SuggestionDisposition.PENDING,
)

@Serializable
enum class InboxSource(val label: String) {
    QUICK_CAPTURE("Quick capture"),
    EXTERNAL_PROPOSAL("External proposal"),
    GOOGLE_TASKS("Google Tasks"),
}

@Serializable
data class InboxItem(
    val id: String,
    val title: String,
    val source: InboxSource,
    val detail: String = "",
    val requiresReview: Boolean = true,
)

@Serializable
enum class ChatRole { USER, ASSISTANT }

@Serializable
data class ChatMessage(
    val id: String,
    val role: ChatRole,
    val text: String,
)

@Serializable
data class DayWeaveUiState(
    val destination: AppDestination = AppDestination.TODAY,
    val schedule: List<ScheduleItem> = emptyList(),
    val activeSession: ActiveSession? = null,
    val inbox: List<InboxItem> = emptyList(),
    val suggestions: List<PlanningSuggestion> = emptyList(),
    val messages: List<ChatMessage> = emptyList(),
    val scheduleMessage: String = "Capture something to compose your first day",
    val protectedFreeMinutes: Int = 90,
    val dayScore: Int = 0,
    val showCompleted: Boolean = true,
    val quietSuggestions: Boolean = true,
    val useDynamicColor: Boolean = false,
) {
    val visibleSchedule: List<ScheduleItem>
        get() = schedule
            .filter { showCompleted || it.status != ItemStatus.COMPLETED }
            .sortedBy(ScheduleItem::startMinute)

    val activeItem: ScheduleItem?
        get() = activeSession?.let { session -> schedule.firstOrNull { it.id == session.itemId } }

    val completedCount: Int get() = schedule.count { it.status == ItemStatus.COMPLETED }
    val pendingSuggestionCount: Int
        get() = suggestions.count { it.disposition == SuggestionDisposition.PENDING }

    companion object {
        fun preview(): DayWeaveUiState = DayWeaveUiState(
            schedule = listOf(
                ScheduleItem(
                    id = "morning-reset",
                    title = "Morning reset",
                    kind = ItemKind.ROUTINE,
                    startMinute = 7 * 60 + 30,
                    durationMinutes = 30,
                    status = ItemStatus.COMPLETED,
                    energy = EnergyLevel.LOW,
                    note = "Water, plan, and prepare",
                    actualMinutes = 27,
                ),
                ScheduleItem(
                    id = "walk-outside",
                    title = "Walk outside",
                    kind = ItemKind.HABIT,
                    startMinute = 8 * 60 + 10,
                    durationMinutes = 30,
                    status = ItemStatus.COMPLETED,
                    project = "Health",
                    energy = EnergyLevel.LOW,
                    note = "Habit target: 30 minutes",
                    actualMinutes = 31,
                ),
                ScheduleItem(
                    id = "architecture",
                    title = "Architecture deep work",
                    kind = ItemKind.TASK,
                    startMinute = 9 * 60,
                    durationMinutes = 90,
                    status = ItemStatus.ACTIVE,
                    project = "DayWeave",
                    energy = EnergyLevel.DEEP,
                    isSplittable = true,
                    note = "Finish sync boundary and review the scheduler contract.",
                ),
                ScheduleItem(
                    id = "coffee-break",
                    title = "Coffee & reset",
                    kind = ItemKind.BREAK,
                    startMinute = 10 * 60 + 30,
                    durationMinutes = 15,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.LOW,
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Protected break",
                ),
                ScheduleItem(
                    id = "planning-call",
                    title = "Weekly planning call",
                    kind = ItemKind.EVENT,
                    startMinute = 11 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    project = "DayWeave",
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Google Calendar · attendee event",
                ),
                ScheduleItem(
                    id = "scheduler-tests",
                    title = "Review scheduler tests",
                    kind = ItemKind.TASK,
                    startMinute = 12 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    project = "DayWeave",
                    energy = EnergyLevel.DEEP,
                    isSplittable = true,
                    note = "Can split into sessions of at least 20 minutes.",
                ),
                ScheduleItem(
                    id = "lunch",
                    title = "Lunch",
                    kind = ItemKind.BREAK,
                    startMinute = 13 * 60,
                    durationMinutes = 45,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.LOW,
                    isFlexible = false,
                    isHardConstraint = true,
                    note = "Protected meal",
                ),
                ScheduleItem(
                    id = "reading",
                    title = "Read 20 pages",
                    kind = ItemKind.HABIT,
                    startMinute = 16 * 60,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                    project = "Learning",
                    note = "Preferred after 15:00",
                ),
            ),
            activeSession = ActiveSession(
                itemId = "architecture",
                elapsedMinutes = 38,
                isPaused = false,
            ),
            inbox = listOf(
                InboxItem(
                    id = "inbox-google-task",
                    title = "Renew travel insurance",
                    source = InboxSource.GOOGLE_TASKS,
                    detail = "Duration and scheduling constraints needed",
                ),
            ),
            suggestions = listOf(
                PlanningSuggestion(
                    id = "recovery-window",
                    title = "Protect a recovery window",
                    summary = "Move ‘Read 20 pages’ to 17:10 and keep 16:00–17:00 free after the dense work block.",
                    source = "DayWeave assistant",
                    kind = SuggestionKind.SCHEDULE_CHANGE,
                    expiresInDays = 7,
                ),
                PlanningSuggestion(
                    id = "external-goal-draft",
                    title = "Break down fitness goal",
                    summary = "A ChatGPT conversation proposed three weekly strength sessions and a Sunday review.",
                    source = "ChatGPT · Goals conversation",
                    kind = SuggestionKind.GOAL_BREAKDOWN,
                    expiresInDays = 7,
                ),
            ),
            messages = listOf(
                ChatMessage(
                    id = "welcome",
                    role = ChatRole.ASSISTANT,
                    text = "Your hard commitments fit. The afternoon is intentionally lighter because the morning has two deep-focus blocks.",
                ),
            ),
            scheduleMessage = "Schedule is balanced",
            dayScore = 82,
        )
    }
}
