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

enum ScheduleBlockOrigin: String, Codable, Sendable {
    case local
    case canonicalPreview
    case externalPreview
    case remoteExecutionLease
}

struct ScheduleBlock: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    /// Effective sensitivity after canonical ancestor propagation.
    var isSensitive: Bool = false
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
    var sourceItemID: UUID? = nil
    var sourceItemRevision: UInt64? = nil
    var occurrenceID: UUID? = nil
    var sessionIndex: UInt16? = nil
    var syncOrigin: ScheduleBlockOrigin? = nil
    var placementReason: String? = nil
    /// The scheduler's wire kind (for example `planned`, `pinned`, or
    /// `calendar_event`). Keeping it avoids guessing stability eligibility.
    var previewKind: String? = nil
    /// False when the server reports remaining work for this occurrence.
    /// A partial preview must never advance a recurrence completion anchor.
    var occurrenceFullyScheduled: Bool = true

    var durationMinutes: Int {
        max(1, Int(end.timeIntervalSince(start) / 60))
    }

    var timeRange: String {
        "\(startTimeLabel)–\(endTimeLabel)"
    }

    var startTimeLabel: String {
        Self.offsetTimeLabel(for: start)
    }

    var endTimeLabel: String {
        Self.offsetTimeLabel(for: end)
    }

    var isLocallyAuthored: Bool {
        syncOrigin == nil || syncOrigin == .local
    }

    private static func offsetTimeLabel(for date: Date) -> String {
        // The numeric offset disambiguates the repeated local hour during a
        // DST fall-back even when a zone happens to reuse an abbreviation.
        var style = Date.FormatStyle()
            .hour(.twoDigits(amPM: .omitted))
            .minute(.twoDigits)
            .timeZone(.iso8601(.long))
        style.timeZone = .autoupdatingCurrent
        return date.formatted(style)
    }
}

extension ScheduleBlock {
    private enum CodingKeys: String, CodingKey {
        case id, title, kind, start, end, status, project, notes, energy
        case isSensitive
        case isFlexible, isHardConstraint, actualMinutes
        case sourceItemID, sourceItemRevision, occurrenceID, sessionIndex
        case syncOrigin, placementReason, previewKind, occurrenceFullyScheduled
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        if decoder.userInfo[.dayWeaveAllowsMissingSensitivity] as? Bool == true {
            isSensitive = try container.decodeIfPresent(Bool.self, forKey: .isSensitive) ?? false
        } else {
            isSensitive = try container.decode(Bool.self, forKey: .isSensitive)
        }
        title = try container.decode(String.self, forKey: .title)
        kind = try container.decode(PlannerItemKind.self, forKey: .kind)
        start = try container.decode(Date.self, forKey: .start)
        end = try container.decode(Date.self, forKey: .end)
        status = try container.decode(PlannerItemStatus.self, forKey: .status)
        project = try container.decodeIfPresent(String.self, forKey: .project)
        notes = try container.decode(String.self, forKey: .notes)
        energy = try container.decode(EnergyLevel.self, forKey: .energy)
        isFlexible = try container.decode(Bool.self, forKey: .isFlexible)
        isHardConstraint = try container.decode(Bool.self, forKey: .isHardConstraint)
        actualMinutes = try container.decodeIfPresent(Int.self, forKey: .actualMinutes)
        sourceItemID = try container.decodeIfPresent(UUID.self, forKey: .sourceItemID)
        sourceItemRevision = try container.decodeIfPresent(UInt64.self, forKey: .sourceItemRevision)
        occurrenceID = try container.decodeIfPresent(UUID.self, forKey: .occurrenceID)
        sessionIndex = try container.decodeIfPresent(UInt16.self, forKey: .sessionIndex)
        syncOrigin = try container.decodeIfPresent(ScheduleBlockOrigin.self, forKey: .syncOrigin)
        placementReason = try container.decodeIfPresent(String.self, forKey: .placementReason)
        previewKind = try container.decodeIfPresent(String.self, forKey: .previewKind)
        // Schema 1 predates this recurrence safety marker. Defaulting to true
        // preserves the old block shape so migration can reset terminal
        // recurrence state before it is exposed.
        occurrenceFullyScheduled = try container.decodeIfPresent(
            Bool.self,
            forKey: .occurrenceFullyScheduled
        ) ?? true
    }
}

enum CanonicalMutationDisposition: String, Codable, Hashable, Sendable {
    case pending
    case conflicted
}

struct PendingCanonicalMutation: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    let itemID: UUID
    let occurrenceID: UUID?
    let sessionIndex: UInt16?
    var desiredStatus: PlannerItemStatus
    var baseRevision: UInt64
    let createdAt: Date
    var disposition: CanonicalMutationDisposition
    var diagnostic: String?
    /// Links an approval-gated canonical status projection to the immutable
    /// execution outcome that requested it. Older snapshots decode this as nil.
    var executionSessionID: UUID? = nil
}

enum RecurrenceSessionDisposition: String, Codable, Hashable, Sendable {
    case completed
    case skipped
}

struct RecurrenceSessionOutcome: Hashable, Codable, Sendable {
    let itemID: UUID
    let occurrenceID: UUID
    let sessionIndex: UInt16
    var disposition: RecurrenceSessionDisposition
    var occurredAt: Date
    var occurrenceFullyScheduled: Bool
}

struct SchedulePreviewProvenance: Equatable, Codable, Sendable {
    let configurationIdentifier: String
    let generatedAt: Date
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
}

enum SidebarDestination: String, Codable, CaseIterable, Identifiable, Sendable {
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

struct AssistantMessage: Identifiable, Hashable, Codable, Sendable {
    enum Role: String, Codable, Sendable { case user, assistant }

    let id: UUID
    let role: Role
    let text: String
    let createdAt: Date
}

struct PlanningSuggestion: Identifiable, Hashable, Codable, Sendable {
    enum State: String, Codable, Sendable { case pending, accepted, rejected }

    let id: UUID
    var title: String
    var summary: String
    var source: String
    var createdAt: Date
    var expiresAt: Date
    var state: State
}
