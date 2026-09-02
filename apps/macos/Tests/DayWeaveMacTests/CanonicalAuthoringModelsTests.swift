import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical authoring models")
struct CanonicalAuthoringModelsTests {
    @Test("an Inbox task may be captured without duration")
    func durationlessInboxDraftIsValid() {
        let itemID = UUID()
        let draft = DayWeaveCanonicalItemDraft(
            title: "Remember this",
            timezoneName: "Europe/Madrid"
        )

        #expect(draft.validationIssue(itemID: itemID) == nil)
        #expect(draft.durationSeconds == nil)
        #expect(draft.status == .inbox)
    }

    @Test("habit recurrence and split bounds are validated locally")
    func recurrenceAndSplitValidation() {
        let itemID = UUID()
        var habit = DayWeaveCanonicalItemDraft(
            kind: .habit,
            status: .planned,
            title: "Stretch",
            timezoneName: "Europe/Madrid",
            durationSeconds: 1_800
        )
        #expect(habit.validationIssue(itemID: itemID) == "Habits require a recurrence.")

        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(JSONNumber(UInt64(2))),
        ])
        habit.splitPolicy = .splittable(
            minimumChunkSeconds: 600,
            maximumChunkSeconds: 1_200
        )
        #expect(habit.validationIssue(itemID: itemID) == nil)

        habit.splitPolicy = .splittable(
            minimumChunkSeconds: 1_200,
            maximumChunkSeconds: 2_400
        )
        #expect(habit.validationIssue(itemID: itemID)?.contains("Split bounds") == true)
    }

    @Test("known recurrence remains writable after encrypted JSON round trip")
    func recurrenceSurvivesRoundTrip() throws {
        let recurrence = JSONValue.object([
            "type": .string("weekly"),
            "times_per_week": .number(JSONNumber(UInt64(3))),
            "weekdays": .array([.string("monday"), .string("wednesday"), .string("friday")]),
        ])
        let encoded = try JSONEncoder().encode(recurrence)
        let decoded = try JSONDecoder().decode(JSONValue.self, from: encoded)

        #expect(decoded.supportsCanonicalAuthoringRecurrence)
        let draft = DayWeaveCanonicalItemDraft(
            kind: .habit,
            status: .planned,
            title: "Train",
            timezoneName: "Europe/Madrid",
            durationSeconds: 2_700,
            recurrence: decoded,
            flexibleConstraints: .object(["energy": .string("deep")])
        )
        #expect(draft.validationIssue(itemID: UUID()) == nil)
        _ = try JSONEncoder().encode(draft.requestFields)
    }

    @Test("unknown advanced fields stay read-only")
    func unknownConstraintsFailClosed() {
        let draft = DayWeaveCanonicalItemDraft(
            title: "Future task",
            timezoneName: "Europe/Madrid",
            flexibleConstraints: .object(["future_rule": .bool(true)])
        )
        #expect(draft.validationIssue(itemID: UUID())?.contains("read-only") == true)
    }

    @Test("calendar events require strict ordered RFC3339 timing metadata")
    func calendarEventTimingValidation() {
        let validConstraints = calendarEvent(
            start: "2026-09-01T10:00:00.000000001+02:00",
            end: "2026-09-01T08:30:00.000000002Z"
        )
        let valid = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Planning call",
            timezoneName: "Europe/Madrid",
            flexibleConstraints: validConstraints
        )
        #expect(valid.validationIssue(itemID: UUID()) == nil)

        let malformedBounds = [
            ("2026-02-30T10:00:00Z", "2026-03-01T11:00:00Z"),
            ("2026-09-01T10:00:00+0200", "2026-09-01T11:00:00+02:00"),
            ("2026-09-01T10:00:00", "2026-09-01T11:00:00Z"),
            ("2026-09-01T10:00Z", "2026-09-01T11:00:00Z"),
        ]
        for (start, end) in malformedBounds {
            #expect(!calendarEvent(start: start, end: end).supportsCanonicalAuthoringConstraints)
        }

        #expect(!calendarEvent(
            start: "2026-09-01T10:00:00Z",
            end: "2026-09-01T10:00:00.000000000Z"
        ).supportsCanonicalAuthoringConstraints)
        #expect(!calendarEvent(
            start: "2026-09-01T10:00:00.000000002Z",
            end: "2026-09-01T10:00:00.000000001Z"
        ).supportsCanonicalAuthoringConstraints)
        #expect(calendarEvent(
            start: "2026-09-01T10:00:00.000000001Z",
            end: "2026-09-01T10:00:00.000000002Z"
        ).supportsCanonicalAuthoringConstraints)
    }

    @Test("calendar event metadata is required only for event drafts")
    func calendarEventKindRelationship() {
        let itemID = UUID()
        let missing = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Missing bounds",
            timezoneName: "UTC"
        )
        #expect(
            missing.validationIssue(itemID: itemID)
                == "Events require calendar event timing metadata."
        )

        let misplaced = DayWeaveCanonicalItemDraft(
            kind: .task,
            status: .planned,
            title: "Not an event",
            timezoneName: "UTC",
            flexibleConstraints: firmBlock(
                start: "2026-09-01T10:00:00Z",
                end: "2026-09-01T11:00:00Z"
            )
        )
        #expect(
            misplaced.validationIssue(itemID: itemID)
                == "Calendar event timing metadata is only valid for event items."
        )
    }

    @Test("all-day bounds use exact local midnights and timezone aliases fail closed")
    func allDayAndTimezoneValidation() {
        let valid = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "DST day",
            timezoneName: "Europe/Madrid",
            flexibleConstraints: calendarEvent(
                start: "2026-03-28T23:00:00Z",
                end: "2026-03-29T22:00:00Z",
                allDay: true
            )
        )
        #expect(valid.validationIssue(itemID: UUID()) == nil)

        var invalidClock = valid
        invalidClock.flexibleConstraints = firmBlock(
            start: "2026-03-29T00:00:00Z",
            end: "2026-03-30T00:00:00Z",
            allDay: true
        )
        #expect(invalidClock.validationIssue(itemID: UUID())?.contains("local midnight") == true)

        var fractionalMidnight = valid
        fractionalMidnight.flexibleConstraints = firmBlock(
            start: "2026-03-28T23:00:00.000000001Z",
            end: "2026-03-29T22:00:00Z",
            allDay: true
        )
        #expect(
            fractionalMidnight.validationIssue(itemID: UUID())?.contains("local midnight") == true
        )

        var alias = valid
        alias.timezoneName = "PST"
        #expect(alias.validationIssue(itemID: UUID()) == "Choose a valid IANA timezone.")
        alias.timezoneName = "GMT+2"
        #expect(alias.validationIssue(itemID: UUID()) == "Choose a valid IANA timezone.")
        #expect(DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: "Etc/GMT+2") != nil)
        #expect(DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: "GMT") != nil)
        #expect(DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: "CET") != nil)
        alias.timezoneName = "UTC"
        alias.flexibleConstraints = firmBlock(
            start: "2026-03-29T00:00:00Z",
            end: "2026-03-30T00:00:00Z",
            allDay: true
        )
        #expect(alias.validationIssue(itemID: UUID()) == nil)
    }

    @Test("owned event authoring uses the sole Google-publication firm block")
    func ownedFirmBlockContract() {
        let timing = firmBlock(
            start: "2026-09-01T10:00:00Z",
            end: "2026-09-01T11:00:00Z"
        )
        let event = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Publish me",
            timezoneName: "UTC",
            durationSeconds: 3_600,
            flexibleConstraints: timing
        )
        #expect(event.validationIssue(itemID: UUID()) == nil)

        guard case let .object(root) = timing,
              case var .object(firm)? = root["dayweave_firm_block"] else {
            Issue.record("Expected a firm-block fixture")
            return
        }
        firm["owned"] = .bool(false)
        #expect(!JSONValue.object([
            "dayweave_firm_block": .object(firm),
        ]).supportsCanonicalAuthoringConstraints)
        #expect(!JSONValue.object([
            "dayweave_firm_block": root["dayweave_firm_block"]!,
            "energy": .string("deep"),
        ]).supportsCanonicalAuthoringConstraints)
    }

    @Test("recurrence values match scheduler enums and integer widths")
    func recurrenceSemanticValidation() {
        let tooLarge = JSONValue.object([
            "type": .string("daily"),
            "times_per_day": .number(JSONNumber(UInt64(UInt16.max) + 1)),
        ])
        let invalidWeekday = JSONValue.object([
            "type": .string("weekly"),
            "times_per_week": .number(JSONNumber(UInt64(1))),
            "weekdays": .array([.string("funday")]),
        ])
        let invalidFrequency = JSONValue.object([
            "type": .string("frequency"),
            "target": .number(JSONNumber(UInt64(2))),
            "period": .string("quarter"),
            "semantics": .string("rolling"),
            "anchor": .string("not-a-timestamp"),
        ])
        let validFrequency = JSONValue.object([
            "type": .string("frequency"),
            "target": .number(JSONNumber(UInt64(2))),
            "period": .string("week"),
            "semantics": .string("rolling"),
            "weekdays": .array([.string("monday"), .string("friday")]),
            "minimum_spacing": .number(JSONNumber(UInt64(90))),
            "anchor": .string("2026-08-30T12:00:00Z"),
        ])

        #expect(!tooLarge.supportsCanonicalAuthoringRecurrence)
        #expect(!invalidWeekday.supportsCanonicalAuthoringRecurrence)
        #expect(!invalidFrequency.supportsCanonicalAuthoringRecurrence)
        #expect(validFrequency.supportsCanonicalAuthoringRecurrence)
        #expect(!JSONValue.object([
            "type": .string("custom"),
            "rrule": .string("FREQ=DAILY"),
        ]).supportsCanonicalAuthoringRecurrence)
    }

    @Test("authoring status is limited to Inbox and Planned")
    func authoredStatusValidation() {
        var draft = DayWeaveCanonicalItemDraft(
            title: "Ready state",
            timezoneName: "UTC"
        )
        #expect(draft.validationIssue(itemID: UUID()) == nil)
        draft.status = .planned
        #expect(draft.validationIssue(itemID: UUID()) == nil)
        draft.status = .scheduled
        #expect(draft.validationIssue(itemID: UUID())?.contains("Inbox or Planned") == true)
    }

    @Test("journal identity is deterministic and malformed combinations fail closed")
    func journalValidation() {
        let itemID = UUID()
        let draft = DayWeaveCanonicalItemDraft(
            title: "Queued task",
            timezoneName: "Europe/Madrid"
        )
        let mutationID = UUID()
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            id: mutationID,
            itemID: itemID,
            operation: .create,
            draft: draft
        )

        #expect(mutation.isValid)
        #expect(mutation.idempotencyKey == "mac-item-\(mutationID.uuidString.lowercased())")
        let invalid = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .trash,
            draft: draft,
            expectedRevision: 1
        )
        #expect(!invalid.isValid)
    }

    private func calendarEvent(
        start: String,
        end: String,
        allDay: Bool = false
    ) -> JSONValue {
        .object([
            "calendar_event": .object([
                "start": .string(start),
                "end": .string(end),
                "immutable": .bool(true),
                "all_day": .bool(allDay),
                "source_calendar_id": .null,
            ]),
        ])
    }

    private func firmBlock(
        start: String,
        end: String,
        allDay: Bool = false
    ) -> JSONValue {
        .object([
            "dayweave_firm_block": .object([
                "owned": .bool(true),
                "starts_at": .string(start),
                "ends_at": .string(end),
                "all_day": .bool(allDay),
                "tentative": .bool(false),
                "busy": .bool(true),
            ]),
        ])
    }
}

@Suite("Canonical Inbox presentation")
struct CanonicalInboxPresentationTests {
    @Test("pending hierarchy is flattened iteratively without a depth limit")
    func deepHierarchy() throws {
        let count = 5_000
        var previous: UUID?
        var mutations: [DayWeavePendingCanonicalAuthoringMutation] = []
        mutations.reserveCapacity(count)
        for index in 0..<count {
            let id = UUID()
            let draft = DayWeaveCanonicalItemDraft(
                title: "Node \(index)",
                timezoneName: "Europe/Madrid",
                parentID: previous,
                siblingOrder: UInt32(index)
            )
            mutations.append(.init(itemID: id, operation: .create, draft: draft))
            previous = id
        }

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [],
            pendingMutations: mutations,
            trashEntries: []
        )

        #expect(presentation.inbox.count == count)
        #expect(try #require(presentation.inbox.last).depth == count - 1)
        #expect(
            try #require(presentation.inbox.last).breadcrumb.count
                == CanonicalInboxPresentation.maximumBreadcrumbDepth
        )
        #expect(try #require(presentation.inbox.last).breadcrumb.last == "Node 4998")
    }

    @Test("conflicts and recent trash are explicit and deduplicated")
    func conflictAndTrashSections() throws {
        let createID = UUID()
        var conflict = DayWeavePendingCanonicalAuthoringMutation(
            itemID: createID,
            operation: .create,
            draft: .init(title: "Needs review", timezoneName: "Europe/Madrid")
        )
        conflict.disposition = .conflicted
        conflict.diagnostic = "The server already has different content."

        let deleted = try decodeItem(id: UUID(), revision: 3, deleted: true)
        let presentation = CanonicalInboxPresentation.build(
            activeItems: [],
            pendingMutations: [conflict],
            trashEntries: [.init(item: deleted)]
        )

        #expect(presentation.inbox.count == 1)
        #expect(presentation.conflicts.map(\.itemID) == [createID])
        #expect(presentation.trash.count == 1)
        #expect(presentation.trash[0].source == .recentTrash)
        #expect(presentation.trash[0].isSensitive == deleted.isSensitive)
    }

    @Test("active and completed items remain reachable as read-only lifecycle rows")
    func activeAndCompletedSections() throws {
        let scheduled = try decodeItem(
            id: UUID(),
            revision: 2,
            deleted: false,
            status: "scheduled"
        )
        let paused = try decodeItem(
            id: UUID(),
            revision: 3,
            deleted: false,
            status: "paused"
        )
        let completed = try decodeItem(
            id: UUID(),
            revision: 4,
            deleted: false,
            status: "completed"
        )

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [scheduled, paused, completed],
            pendingMutations: [],
            trashEntries: []
        )

        #expect(Set(presentation.active.map(\.itemID)) == Set([scheduled.id, paused.id]))
        #expect(presentation.active.allSatisfy { $0.isReadOnly })
        #expect(presentation.completed.map(\.itemID) == [completed.id])
        #expect(presentation.completed.allSatisfy { $0.isReadOnly })
        #expect(presentation.inbox.isEmpty)
        #expect(presentation.planned.isEmpty)
    }

    @Test("an active cross-device restore conflict remains reviewable and discardable")
    func activeRestoreConflictPresentation() throws {
        let itemID = UUID()
        let deleted = try decodeItem(id: itemID, revision: 2, deleted: true)
        let active = try decodeItem(id: itemID, revision: 3, deleted: false)
        var restore = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .restore,
            expectedRevision: 2,
            baseItem: deleted
        )
        restore.disposition = .conflicted
        restore.diagnostic = "Restored elsewhere with different content."

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [active],
            pendingMutations: [restore],
            trashEntries: []
        )

        let row = try #require(presentation.conflicts.first)
        #expect(row.itemID == itemID)
        #expect(row.source == .activeRestore)
        #expect(row.mutationID == restore.id)
        #expect(row.isReadOnly)
    }

    @Test("replace conflicts expose both retained draft and latest canonical version")
    func replaceConflictPresentationIncludesCanonicalVersion() throws {
        let itemID = UUID()
        let base = try decodeItem(id: itemID, revision: 1, deleted: false)
        let active = try decodeItem(id: itemID, revision: 2, deleted: false)
        var draft = DayWeaveCanonicalItemDraft(item: base)
        draft.title = "Retained local draft"
        draft.notes = "Local notes that must remain recoverable"
        var replace = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .replace,
            draft: draft,
            expectedRevision: base.revision,
            baseItem: base
        )
        replace.disposition = .conflicted
        replace.diagnostic = "The canonical revision changed."

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [active],
            pendingMutations: [replace],
            trashEntries: []
        )

        let row = try #require(presentation.conflicts.first)
        #expect(row.source == .pendingReplace)
        #expect(row.title == "Retained local draft")
        #expect(row.activeCanonicalItem == active)
        #expect(row.isReadOnly)
    }

    @Test("a remotely deleted replace draft remains recoverable after an empty rebuild")
    func missingReplaceConflictRemainsReachable() throws {
        let itemID = UUID()
        let base = try decodeItem(id: itemID, revision: 1, deleted: false)
        let deleted = try decodeItem(id: itemID, revision: 2, deleted: true)
        var draft = DayWeaveCanonicalItemDraft(item: base)
        draft.title = "Recover this local edit"
        var replace = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .replace,
            draft: draft,
            expectedRevision: base.revision,
            baseItem: base
        )
        replace.disposition = .conflicted
        replace.diagnostic = "The item was deleted remotely."

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [],
            pendingMutations: [replace],
            trashEntries: [.init(item: deleted)]
        )

        let row = try #require(presentation.conflicts.first)
        #expect(row.source == .pendingReplace)
        #expect(row.title == "Recover this local edit")
        #expect(row.activeCanonicalItem == nil)
        #expect(row.mutationID == replace.id)
        #expect(presentation.trash.isEmpty)
    }

    @Test("Google Task deletion excludes provider-imported trash")
    func googleTaskDeletionEligibilityRequiresAppAuthoredConstraints() throws {
        let itemID = UUID()
        let authored = try decodeItem(
            id: itemID,
            revision: 4,
            deleted: true,
            flexibleConstraintsJSON: "{}"
        )
        let imported = try decodeItem(
            id: itemID,
            revision: 4,
            deleted: true,
            flexibleConstraintsJSON: #"{"google_sync":{"remote_id":"task-1"}}"#
        )
        let active = try decodeItem(
            id: itemID,
            revision: 4,
            deleted: false,
            flexibleConstraintsJSON: "{}"
        )

        #expect(authored.isEligibleForGoogleTaskPublication(deleted: true))
        #expect(!imported.isEligibleForGoogleTaskPublication(deleted: true))
        #expect(!active.isEligibleForGoogleTaskPublication(deleted: true))
        #expect(active.isEligibleForGoogleTaskPublication(deleted: false))
    }

    @Test("sensitivity is inherited through pending hierarchy and missing ancestry fails closed")
    func inheritedSensitivityPresentation() throws {
        let parentID = UUID()
        let childID = UUID()
        let missingChildID = UUID()
        let mutations = [
            DayWeavePendingCanonicalAuthoringMutation(
                itemID: parentID,
                operation: .create,
                draft: .init(isSensitive: true, title: "Private project", timezoneName: "UTC")
            ),
            DayWeavePendingCanonicalAuthoringMutation(
                itemID: childID,
                operation: .create,
                draft: .init(
                    title: "Inherited child",
                    timezoneName: "UTC",
                    parentID: parentID
                )
            ),
            DayWeavePendingCanonicalAuthoringMutation(
                itemID: missingChildID,
                operation: .create,
                draft: .init(
                    title: "Unavailable ancestry",
                    timezoneName: "UTC",
                    parentID: UUID()
                )
            ),
        ]

        let presentation = CanonicalInboxPresentation.build(
            activeItems: [],
            pendingMutations: mutations,
            trashEntries: []
        )
        let rows = Dictionary(uniqueKeysWithValues: presentation.inbox.map { ($0.itemID, $0) })

        #expect(try #require(rows[parentID]).sensitivityPresentation == .own)
        #expect(try #require(rows[childID]).sensitivityPresentation == .inherited)
        #expect(try #require(rows[childID]).isSensitive)
        #expect(try #require(rows[missingChildID]).sensitivityPresentation == .inherited)
        #expect(try #require(rows[missingChildID]).isSensitive)
    }

    private func decodeItem(
        id: UUID,
        revision: UInt64,
        deleted: Bool,
        status: String = "inbox",
        flexibleConstraintsJSON: String = "{}"
    ) throws -> DayWeaveCanonicalItem {
        let deletedAt = deleted
            ? ",\"deleted_at\":\"2026-08-30T10:00:00Z\""
            : ",\"deleted_at\":null"
        let completedAt = status == "completed"
            ? "\"2026-08-30T09:30:00Z\""
            : "null"
        let json = """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":true,"kind":"task",
        "status":"\(status)","title":"Lifecycle task","notes":null,"timezone_name":"Europe/Madrid",
        "duration_seconds":null,"deadline_at":null,"earliest_start_at":null,"recurrence":null,
        "flexible_constraints":\(flexibleConstraintsJSON),"split_policy":{"type":"indivisible"},"importance":50,
        "urgency":50,"parent_id":null,"sibling_order":0,"is_executable":false,"revision":\(revision),
        "created_at":"2026-08-30T09:00:00Z","updated_at":"2026-08-30T10:00:00Z",
        "completed_at":\(completedAt)\(deletedAt)}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
