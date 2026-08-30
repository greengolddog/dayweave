import Foundation
import SwiftUI

enum PlannerTimeZone {
    private static let utc = TimeZone(secondsFromGMT: 0)!

    static func resolve(_ timezoneName: String) -> TimeZone {
        DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName) ?? utc
    }

    static func dateTimeLabel(_ date: Date, timezoneName: String) -> String {
        var style = Date.FormatStyle()
            .year()
            .month(.abbreviated)
            .day()
            .hour(.twoDigits(amPM: .omitted))
            .minute(.twoDigits)
            .timeZone(.iso8601(.long))
        style.timeZone = resolve(timezoneName)
        return date.formatted(style)
    }
}

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
    case localComposition
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

    func timeRange(timezoneName: String) -> String {
        "\(startTimeLabel(timezoneName: timezoneName))–\(endTimeLabel(timezoneName: timezoneName))"
    }

    func startTimeLabel(timezoneName: String) -> String {
        Self.offsetTimeLabel(for: start, timezoneName: timezoneName)
    }

    func endTimeLabel(timezoneName: String) -> String {
        Self.offsetTimeLabel(for: end, timezoneName: timezoneName)
    }

    var isLocallyAuthored: Bool {
        syncOrigin == nil || syncOrigin == .local
    }

    /// External fixed inputs constrain composition, but they are not executable
    /// DayWeave work and must not contribute to project or execution rollups.
    var contributesToExecutionPresentation: Bool {
        previewKind != "external_fixed"
    }

    var isExternalFixedBlock: Bool {
        !contributesToExecutionPresentation
    }

    private static func offsetTimeLabel(
        for date: Date,
        timezoneName: String
    ) -> String {
        // The numeric offset disambiguates the repeated local hour during a
        // DST fall-back even when a zone happens to reuse an abbreviation.
        var style = Date.FormatStyle()
            .hour(.twoDigits(amPM: .omitted))
            .minute(.twoDigits)
            .timeZone(.iso8601(.long))
        style.timeZone = PlannerTimeZone.resolve(timezoneName)
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

/// Durable, revision-bound intent to change only an item's own privacy marker.
/// Inherited sensitivity is derived from the canonical hierarchy and is never
/// overridden by a child edit.
struct PendingCanonicalSensitivityMutation: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    let itemID: UUID
    var desiredIsSensitive: Bool
    var baseRevision: UInt64
    let createdAt: Date
    var disposition: CanonicalMutationDisposition
    var diagnostic: String?
    /// Set and durably flushed before the first request byte is sent. Once
    /// submitted, the current replacement cannot be canceled or inverted
    /// until its exact outcome is reconciled.
    var hasBeenSubmitted: Bool = false
    /// A user-requested final classification that must run only after the
    /// submitted replacement above is observed or replayed exactly.
    var followUpIsSensitive: Bool? = nil

    var requestedIsSensitive: Bool {
        followUpIsSensitive ?? desiredIsSensitive
    }

    /// Privacy is a one-way presentation fence across an ambiguous chain. If
    /// either the submitted replacement or its queued final replacement marks
    /// the item sensitive, content must remain redacted until both are
    /// authoritatively reconciled.
    var requiresSensitivePresentation: Bool {
        desiredIsSensitive || followUpIsSensitive == true
    }
}

extension PendingCanonicalSensitivityMutation {
    private enum CodingKeys: String, CodingKey {
        case id, itemID, desiredIsSensitive, baseRevision, createdAt
        case disposition, diagnostic, hasBeenSubmitted, followUpIsSensitive
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        itemID = try container.decode(UUID.self, forKey: .itemID)
        desiredIsSensitive = try container.decode(Bool.self, forKey: .desiredIsSensitive)
        baseRevision = try container.decode(UInt64.self, forKey: .baseRevision)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        disposition = try container.decode(CanonicalMutationDisposition.self, forKey: .disposition)
        diagnostic = try container.decodeIfPresent(String.self, forKey: .diagnostic)
        // A pre-fence schema cannot prove that a retained request was never
        // sent. Missing attempt state is therefore ambiguous, never cancelable.
        hasBeenSubmitted = try container.decodeIfPresent(
            Bool.self,
            forKey: .hasBeenSubmitted
        ) ?? true
        followUpIsSensitive = try container.decodeIfPresent(
            Bool.self,
            forKey: .followUpIsSensitive
        )
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(itemID, forKey: .itemID)
        try container.encode(desiredIsSensitive, forKey: .desiredIsSensitive)
        try container.encode(baseRevision, forKey: .baseRevision)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(disposition, forKey: .disposition)
        try container.encodeIfPresent(diagnostic, forKey: .diagnostic)
        try container.encode(hasBeenSubmitted, forKey: .hasBeenSubmitted)
        try container.encodeIfPresent(followUpIsSensitive, forKey: .followUpIsSensitive)
    }
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

/// Evidence for a schedule composed by the signed helper on this Mac. This is
/// intentionally disjoint from `SchedulePreviewProvenance`: a local
/// fingerprint is not a server `input_digest` and cannot authorize schedule
/// publication.
struct LocalScheduleCompositionProvenance: Equatable, Codable, Sendable {
    let configurationIdentifier: String
    let localInputFingerprint: String
    let generatedAt: Date
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let sourceItemRevisions: [UUID: UInt64]

    var hasValidShape: Bool {
        let prefix = "local-sha256:"
        let digest = localInputFingerprint.dropFirst(prefix.count)
        return localInputFingerprint.hasPrefix(prefix)
            && digest.count == 64
            && digest.utf8.allSatisfy {
                (48...57).contains($0) || (97...102).contains($0)
            }
            && !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 4_096
            && !configurationIdentifier.unicodeScalars.contains(
                where: CharacterSet.controlCharacters.contains
            )
            && generatedAt.timeIntervalSinceReferenceDate.isFinite
            && asOf.timeIntervalSinceReferenceDate.isFinite
            && horizonStart.timeIntervalSinceReferenceDate.isFinite
            && horizonEnd.timeIntervalSinceReferenceDate.isFinite
            && horizonStart < horizonEnd
            && TimeZone(identifier: timezoneName) != nil
            && sourceItemRevisions.count <= 10_000
            && sourceItemRevisions.values.allSatisfy { $0 > 0 }
    }
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
