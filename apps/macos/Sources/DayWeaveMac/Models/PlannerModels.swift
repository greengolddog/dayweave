import Foundation
import SwiftUI

enum PlannerItemKind: String, Codable, CaseIterable, Identifiable, Sendable {
    case event
    case task
    case habit
    case routine
    case goal
    case breakTime = "break"

    var id: Self { self }

    var title: String {
        switch self {
        case .event: "Event"
        case .task: "Task"
        case .habit: "Habit"
        case .routine: "Routine"
        case .goal: "Goal"
        case .breakTime: "Break"
        }
    }

    var symbol: String {
        switch self {
        case .event: "calendar"
        case .task: "checkmark.circle"
        case .habit: "repeat"
        case .routine: "list.number"
        case .goal: "scope"
        case .breakTime: "cup.and.saucer"
        }
    }

    var color: Color {
        switch self {
        case .event: .blue
        case .task: .indigo
        case .habit: .green
        case .routine: .orange
        case .goal: .purple
        case .breakTime: .mint
        }
    }
}

enum PlannerItemStatus: String, Codable, CaseIterable, Sendable {
    case notStarted
    case scheduled
    case active
    case paused
    case completed
    case skipped
    case canceled
    case blocked

    var title: String {
        switch self {
        case .notStarted: "Not started"
        case .scheduled: "Scheduled"
        case .active: "In progress"
        case .paused: "Paused"
        case .completed: "Completed"
        case .skipped: "Skipped"
        case .canceled: "Canceled"
        case .blocked: "Blocked"
        }
    }
}

enum EnergyLevel: String, Codable, CaseIterable, Identifiable, Sendable {
    case low
    case medium
    case deep

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

struct ScheduleBlock: Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var kind: PlannerItemKind
    var start: Date
    var end: Date
    var status: PlannerItemStatus
    var project: String?
    var notes: String
    var energy: EnergyLevel
    var isFlexible: Bool
    var isHardConstraint: Bool
    var actualMinutes: Int?

    var durationMinutes: Int {
        max(1, Int(end.timeIntervalSince(start) / 60))
    }

    var timeRange: String {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm"
        return "\(formatter.string(from: start))–\(formatter.string(from: end))"
    }
}

enum SidebarDestination: String, CaseIterable, Identifiable {
    case today
    case calendar
    case inbox
    case habits
    case projects
    case goals
    case statistics

    var id: Self { self }

    var title: String {
        switch self {
        case .today: "Today"
        case .calendar: "Calendar"
        case .inbox: "Inbox"
        case .habits: "Habits"
        case .projects: "Projects"
        case .goals: "Goals"
        case .statistics: "Statistics"
        }
    }

    var symbol: String {
        switch self {
        case .today: "sun.max"
        case .calendar: "calendar"
        case .inbox: "tray"
        case .habits: "repeat.circle"
        case .projects: "folder"
        case .goals: "scope"
        case .statistics: "chart.bar"
        }
    }
}

struct AssistantMessage: Identifiable, Hashable, Sendable {
    enum Role: Sendable { case user, assistant }

    let id: UUID
    let role: Role
    let text: String
    let createdAt: Date
}

struct PlanningSuggestion: Identifiable, Hashable, Sendable {
    enum State: String, Sendable { case pending, accepted, rejected }

    let id: UUID
    var title: String
    var summary: String
    var source: String
    var createdAt: Date
    var expiresAt: Date
    var state: State
}

