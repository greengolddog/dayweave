import Foundation
import SwiftUI

@MainActor
final class PlannerStore: ObservableObject {
    @Published var destination: SidebarDestination? = .today
    @Published var selectedBlockID: UUID?
    @Published var blocks: [ScheduleBlock]
    @Published var suggestions: [PlanningSuggestion]
    @Published var assistantMessages: [AssistantMessage]
    @Published var isQuickAddPresented = false
    @Published var lastScheduleMessage = "Schedule is balanced"
    @Published var protectedFreeMinutes = 90
    @Published var freezeHours = 2
    @Published var showCompleted = true

    init(
        blocks: [ScheduleBlock] = [],
        suggestions: [PlanningSuggestion] = [],
        assistantMessages: [AssistantMessage] = []
    ) {
        self.blocks = blocks
        self.suggestions = suggestions
        self.assistantMessages = assistantMessages
        selectedBlockID = blocks.first?.id
    }

    var selectedBlock: ScheduleBlock? {
        blocks.first(where: { $0.id == selectedBlockID })
    }

    var activeItem: ScheduleBlock? {
        blocks.first(where: { $0.status == .active })
    }

    var completedCount: Int {
        blocks.count(where: { $0.status == .completed })
    }

    var visibleBlocks: [ScheduleBlock] {
        blocks
            .filter { showCompleted || $0.status != .completed }
            .sorted { $0.start < $1.start }
    }

    func select(_ block: ScheduleBlock) {
        selectedBlockID = block.id
    }

    func start(_ id: UUID) {
        for index in blocks.indices {
            if blocks[index].status == .active {
                blocks[index].status = .paused
            }
            if blocks[index].id == id {
                blocks[index].status = .active
            }
        }
        objectWillChange.send()
    }

    func pauseActive() {
        guard let index = blocks.firstIndex(where: { $0.status == .active }) else { return }
        blocks[index].status = .paused
        lastScheduleMessage = "Paused — remaining work is held tentatively"
    }

    func complete(_ id: UUID) {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return }
        blocks[index].status = .completed
        blocks[index].actualMinutes = blocks[index].durationMinutes
        lastScheduleMessage = "Completed — later flexible work was checked"
    }

    func skip(_ id: UUID) {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return }
        blocks[index].status = .skipped
        lastScheduleMessage = "Skipped — recurrence policy will decide the next occurrence"
    }

    func doLater(_ id: UUID) {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return }
        let delta: TimeInterval = 60 * 60
        blocks[index].start.addTimeInterval(delta)
        blocks[index].end.addTimeInterval(delta)
        blocks.sort { $0.start < $1.start }
        lastScheduleMessage = "Moved one hour later; no hard constraints were crossed"
    }

    func recomposeSchedule() {
        let now = Date()
        let frozenUntil = now.addingTimeInterval(TimeInterval(freezeHours * 3_600))
        var cursor: Date?

        for index in blocks.indices where blocks[index].isFlexible && blocks[index].start > frozenUntil {
            if let cursor, blocks[index].start < cursor {
                let duration = blocks[index].end.timeIntervalSince(blocks[index].start)
                blocks[index].start = cursor
                blocks[index].end = cursor.addingTimeInterval(duration)
            }
            cursor = blocks[index].end.addingTimeInterval(10 * 60)
        }
        blocks.sort { $0.start < $1.start }
        lastScheduleMessage = "Recomposed with a \(freezeHours)-hour freeze horizon"
    }

    func quickAdd(title: String, kind: PlannerItemKind, minutes: Int) {
        let lastEnd = blocks.map(\.end).max() ?? Date()
        let start = max(lastEnd.addingTimeInterval(10 * 60), Date())
        let block = ScheduleBlock(
            id: UUID(),
            title: title,
            kind: kind,
            start: start,
            end: start.addingTimeInterval(TimeInterval(minutes * 60)),
            status: .scheduled,
            project: nil,
            notes: "Captured with Quick Add",
            energy: .medium,
            isFlexible: kind != .event,
            isHardConstraint: kind == .event,
            actualMinutes: nil
        )
        blocks.append(block)
        blocks.sort { $0.start < $1.start }
        selectedBlockID = block.id
        lastScheduleMessage = "Added \"\(title)\" and placed it in the next safe opening"
    }

    func sendAssistantMessage(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        assistantMessages.append(.init(id: UUID(), role: .user, text: trimmed, createdAt: Date()))
        assistantMessages.append(.init(
            id: UUID(),
            role: .assistant,
            text: "I’ll evaluate that against hard constraints, deadlines, energy, and protected free time. Any change will be shown as a proposal before it is applied.",
            createdAt: Date()
        ))
    }

    func acceptSuggestion(_ id: UUID) {
        guard let index = suggestions.firstIndex(where: { $0.id == id }) else { return }
        suggestions[index].state = .accepted
    }

    func rejectSuggestion(_ id: UUID) {
        guard let index = suggestions.firstIndex(where: { $0.id == id }) else { return }
        suggestions[index].state = .rejected
    }

    static func preview(now: Date = Date()) -> PlannerStore {
        let calendar = Calendar.current
        let day = calendar.startOfDay(for: now)
        func at(_ hour: Int, _ minute: Int = 0) -> Date {
            calendar.date(byAdding: .minute, value: hour * 60 + minute, to: day) ?? day
        }

        let blocks: [ScheduleBlock] = [
            .init(id: UUID(), title: "Morning reset", kind: .routine, start: at(7, 30), end: at(8), status: .completed, project: nil, notes: "Water, plan, and prepare", energy: .low, isFlexible: true, isHardConstraint: false, actualMinutes: 27),
            .init(id: UUID(), title: "Walk outside", kind: .habit, start: at(8, 10), end: at(8, 40), status: .completed, project: "Health", notes: "Habit target: 30 minutes", energy: .low, isFlexible: true, isHardConstraint: false, actualMinutes: 31),
            .init(id: UUID(), title: "Architecture deep work", kind: .task, start: at(9), end: at(10, 30), status: .active, project: "DayWeave", notes: "Finish sync boundary and review the scheduler contract.", energy: .deep, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
            .init(id: UUID(), title: "Coffee & reset", kind: .breakTime, start: at(10, 30), end: at(10, 45), status: .scheduled, project: nil, notes: "Protected break", energy: .low, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Weekly planning call", kind: .event, start: at(11), end: at(11, 45), status: .scheduled, project: "DayWeave", notes: "Google Calendar · attendee event", energy: .medium, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Review scheduler tests", kind: .task, start: at(12), end: at(12, 45), status: .scheduled, project: "DayWeave", notes: "Can split into sessions of at least 20 minutes.", energy: .deep, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
            .init(id: UUID(), title: "Lunch", kind: .breakTime, start: at(13), end: at(13, 45), status: .scheduled, project: nil, notes: "Protected meal", energy: .low, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Read 20 pages", kind: .habit, start: at(16), end: at(16, 30), status: .scheduled, project: "Learning", notes: "Preferred after 15:00", energy: .medium, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
        ]

        let suggestions = [
            PlanningSuggestion(
                id: UUID(),
                title: "Protect a recovery window",
                summary: "Move “Read 20 pages” to 17:10 and keep 16:00–17:00 free after the dense work block.",
                source: "DayWeave assistant",
                createdAt: now,
                expiresAt: calendar.date(byAdding: .day, value: 7, to: now) ?? now,
                state: .pending
            )
        ]

        let messages = [
            AssistantMessage(
                id: UUID(),
                role: .assistant,
                text: "Your hard commitments fit. The afternoon is intentionally lighter because the morning has two deep-focus blocks.",
                createdAt: now
            )
        ]
        return PlannerStore(blocks: blocks, suggestions: suggestions, assistantMessages: messages)
    }
}
