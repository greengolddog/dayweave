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

    @Test("preferred local start requires a same-day duration")
    func preferredStartMinuteValidation() {
        let itemID = UUID()
        var draft = DayWeaveCanonicalItemDraft(
            title: "Afternoon focus",
            timezoneName: "Europe/Paris",
            durationSeconds: 3_600,
            flexibleConstraints: .object([
                "preferred_start_minute": .number(JSONNumber(UInt64(13 * 60))),
            ])
        )
        #expect(draft.validationIssue(itemID: itemID) == nil)

        draft.durationSeconds = nil
        #expect(draft.validationIssue(itemID: itemID)?.contains("requires a duration") == true)

        draft.durationSeconds = 7_201
        draft.flexibleConstraints = .object([
            "preferred_start_minute": .number(JSONNumber(UInt64(22 * 60))),
        ])
        #expect(draft.validationIssue(itemID: itemID)?.contains("same day") == true)

        draft.flexibleConstraints = .object([
            "preferred_start_minute": .number(JSONNumber(UInt64(1_440))),
        ])
        #expect(draft.validationIssue(itemID: itemID)?.contains("read-only") == true)
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
            start: "2026-09-01T10:00:00.000001+02:00",
            end: "2026-09-01T08:30:00.000002Z"
        )
        let valid = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Planning call",
            timezoneName: "Europe/Madrid",
            flexibleConstraints: validConstraints
        )
        #expect(valid.validationIssue(itemID: UUID()) == nil)

        var fractionalDuration = valid
        fractionalDuration.durationSeconds = 1_800
        #expect(
            fractionalDuration.validationIssue(itemID: UUID())
                == "Event duration must equal its timing interval."
        )

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
        #expect(!calendarEvent(
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
                == "Calendar event metadata is only valid for event items."
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
            fractionalMidnight.validationIssue(itemID: UUID())?.contains("read-only") == true
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
            "weekdays": .array([]),
            "minimum_spacing": .number(JSONNumber(UInt64(90))),
            "anchor": .string("2026-08-30T12:00:00Z"),
        ])
        let excessiveInterval = JSONValue.object([
            "type": .string("every_interval"),
            "interval": .number(JSONNumber(UInt64(
                DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes + 1
            ))),
        ])

        #expect(!tooLarge.supportsCanonicalAuthoringRecurrence)
        #expect(!invalidWeekday.supportsCanonicalAuthoringRecurrence)
        #expect(!invalidFrequency.supportsCanonicalAuthoringRecurrence)
        #expect(validFrequency.supportsCanonicalAuthoringRecurrence)
        #expect(!excessiveInterval.supportsCanonicalAuthoringRecurrence)
        #expect(!JSONValue.object([
            "type": .string("custom"),
            "rrule": .string("FREQ=DAILY"),
        ]).supportsCanonicalAuthoringRecurrence)
    }

    @Test("qualified scheduling constraints validate strict composer shapes")
    func richSchedulingConstraintValidation() {
        let constraints = JSONValue.object([
            "constraints": .object([
                "earliest_start": .object([
                    "value": .string("2026-09-03T08:00:00+02:00"),
                    "strength": .object(["level": .string("hard")]),
                ]),
                "latest_finish": .object([
                    "value": .string("2026-09-30T18:00:00+02:00"),
                    "strength": .object([
                        "level": .string("soft"),
                        "weight": .number(JSONNumber(UInt64(250))),
                    ]),
                ]),
                "allowed_weekdays": .object([
                    "value": .array([.string("monday"), .string("friday")]),
                    "strength": .object(["level": .string("hard")]),
                ]),
                "preferred_daily_windows": .array([.object([
                    "value": .object([
                        "weekdays": .array([.string("monday")]),
                        "start_minute": .number(JSONNumber(UInt64(540))),
                        "end_minute": .number(JSONNumber(UInt64(720))),
                    ]),
                    "strength": .object([
                        "level": .string("soft"),
                        "weight": .number(JSONNumber(UInt64(75))),
                    ]),
                ])]),
                "buffers": .object([
                    "before": .number(JSONNumber(UInt64(10))),
                    "after": .number(JSONNumber(UInt64(15))),
                    "strength": .object(["level": .string("hard")]),
                ]),
                "occurrence_window": .null,
            ]),
            "energy": .object([
                "value": .string("deep"),
                "strength": .object(["level": .string("hard")]),
            ]),
            "tags": .array([.string("focus"), .string("writing")]),
        ])
        #expect(constraints.supportsCanonicalAuthoringConstraints)

        guard case var .object(root) = constraints,
              case var .object(nested)? = root["constraints"] else {
            Issue.record("Expected constraint fixture")
            return
        }
        nested["dependencies"] = .array([.object([
            "item_id": .string("00000000-0000-0000-0000-000000000199"),
            "relation": .string("finish_to_start"),
            "minimum_lag": .number(JSONNumber(UInt64(15))),
            "strength": .object(["level": .string("hard")]),
        ])])
        root["constraints"] = .object(nested)
        #expect(JSONValue.object(root).supportsCanonicalAuthoringConstraints)

        nested["dependencies"] = .array([
            .object([
                "item_id": .string("00000000-0000-0000-0000-000000000199"),
                "relation": .string("finish_to_start"),
                "minimum_lag": .number(JSONNumber(UInt64(15))),
                "strength": .object(["level": .string("hard")]),
            ]),
            .object([
                "item_id": .string("00000000-0000-0000-0000-000000000199"),
                "relation": .string("start_to_start"),
                "minimum_lag": .number(JSONNumber(UInt64(0))),
                "strength": .object(["level": .string("hard")]),
            ]),
        ])
        root["constraints"] = .object(nested)
        #expect(!JSONValue.object(root).supportsCanonicalAuthoringConstraints)

        nested.removeValue(forKey: "dependencies")
        nested["earliest_start"] = .object([
            "value": .string("2026-09-30T19:00:00+02:00"),
            "strength": .object(["level": .string("hard")]),
        ])
        root["constraints"] = .object(nested)
        #expect(!JSONValue.object(root).supportsCanonicalAuthoringConstraints)
    }

    @Test("dependency normalization mirrors the authoritative graph projection")
    func dependencyProjectionNormalization() {
        let earlier = UUID(uuidString: "00000000-0000-4000-8000-000000000199")!
        let later = UUID(uuidString: "00000000-0000-4000-8000-000000000200")!
        let draft = DayWeaveCanonicalItemDraft(
            title: "Ordered dependencies",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([
                        CanonicalDependencyEdge(
                            predecessorID: later,
                            relation: .startToStart,
                            minimumLagMinutes: 5,
                            strength: .soft(weight: 25)
                        ).jsonValue,
                        CanonicalDependencyEdge(
                            predecessorID: earlier,
                            relation: .finishToStart,
                            minimumLagMinutes: 0,
                            strength: .hard
                        ).jsonValue,
                    ]),
                ]),
            ])
        ).normalized

        #expect(CanonicalDependencyEdge.decode(
            fromFlexibleConstraints: draft.flexibleConstraints
        )?.map(\.predecessorID) == [earlier, later])

        let cleared = DayWeaveCanonicalItemDraft(
            title: "No dependencies",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object(["dependencies": .array([])]),
            ])
        ).normalized
        #expect(cleared.flexibleConstraints == .object([:]))
    }

    @Test("Rust default spellings and inactive buffers remain writable")
    func rustDefaultConstraintSpellings() {
        let defaults = JSONValue.object([
            "constraints": .object([
                "earliest_start": .null,
                "latest_finish": .null,
                "minimum_notice": .null,
                "allowed_weekdays": .null,
                "preferred_daily_windows": .array([]),
                "preferred_absolute_windows": .array([]),
                "forbidden_windows": .array([]),
                "required_contexts": .array([]),
                "required_location": .null,
                "dependencies": .array([]),
                "maximum_daily_work": .null,
                "maximum_weekly_work": .null,
                "buffers": .object([
                    "before": .number(JSONNumber(UInt64(0))),
                    "after": .number(JSONNumber(UInt64(0))),
                    "strength": .null,
                ]),
                "occurrence_window": .null,
            ]),
            "goal_ids": .array([]),
        ])
        let firmDefaults = JSONValue.object([
            "dayweave_firm_block": .object([
                "owned": .bool(true),
                "starts_at": .string("2026-09-01T10:00:00Z"),
                "ends_at": .string("2026-09-01T11:00:00Z"),
            ]),
        ])

        #expect(defaults.supportsCanonicalAuthoringConstraints)
        #expect(firmDefaults.supportsCanonicalAuthoringConstraints)

        let wrongKindNullEvent = DayWeaveCanonicalItemDraft(
            kind: .task,
            title: "Not an event",
            timezoneName: "UTC",
            flexibleConstraints: .object(["calendar_event": .null])
        )
        #expect(
            wrongKindNullEvent.validationIssue(itemID: UUID())
                == "Calendar event metadata is only valid for event items."
        )
    }

    @Test("canonical JSON byte limits do not count escaped slashes")
    func canonicalJSONByteCountUsesUnescapedSlashes() throws {
        let slashTag = String(repeating: "/", count: 20_000)
        let constraints = JSONValue.object([
            "tags": .array([.string(slashTag)]),
        ])
        let ordinaryCount = try JSONEncoder().encode(constraints).count
        let canonicalEncoder = JSONEncoder()
        canonicalEncoder.outputFormatting = [.withoutEscapingSlashes]
        let canonicalCount = try canonicalEncoder.encode(constraints).count
        let draft = DayWeaveCanonicalItemDraft(
            title: "Slash-preserving tag",
            timezoneName: "UTC",
            flexibleConstraints: constraints
        )

        #expect(ordinaryCount > DayWeaveCanonicalItemDraft.maximumConstraintBytes)
        #expect(canonicalCount <= DayWeaveCanonicalItemDraft.maximumConstraintBytes)
        #expect(draft.validationIssue(itemID: UUID()) == nil)
    }

    @Test("partial Inbox event timing fails closed while empty capture stays valid")
    func partialInboxEventTimingFailsClosed() {
        let itemID = UUID()
        var event = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Appointment details later",
            timezoneName: "UTC"
        )
        #expect(event.validationIssue(itemID: itemID) == nil)

        event.durationSeconds = 900
        #expect(event.validationIssue(itemID: itemID)?.contains("Incomplete Inbox") == true)
        event.durationSeconds = nil
        event.earliestStartAt = Date(timeIntervalSince1970: 1_788_278_400)
        #expect(event.validationIssue(itemID: itemID)?.contains("Incomplete Inbox") == true)
    }

    @Test("Planned unknown-duration work remains valid but does not create demand")
    func plannedUnknownDurationItemsRemainValid() {
        let itemID = UUID()
        let recurrence = JSONValue.object([
            "type": .string("daily"),
            "times_per_day": .number(JSONNumber(UInt64(1))),
        ])
        let drafts = [
            DayWeaveCanonicalItemDraft(
                kind: .task,
                status: .planned,
                title: "Estimate later",
                timezoneName: "UTC"
            ),
            DayWeaveCanonicalItemDraft(
                kind: .habit,
                status: .planned,
                title: "Habit estimate later",
                timezoneName: "UTC",
                recurrence: recurrence
            ),
            DayWeaveCanonicalItemDraft(
                kind: .breakTime,
                status: .planned,
                title: "Open-ended break",
                timezoneName: "UTC"
            ),
            DayWeaveCanonicalItemDraft(
                kind: .goal,
                status: .planned,
                title: "Goal effort estimate later",
                timezoneName: "UTC",
                flexibleConstraints: .object(["has_own_effort": .bool(true)])
            ),
            DayWeaveCanonicalItemDraft(
                kind: .routine,
                status: .planned,
                title: "Routine effort estimate later",
                timezoneName: "UTC",
                flexibleConstraints: .object(["has_own_effort": .bool(true)])
            ),
        ]

        for draft in drafts {
            #expect(draft.validationIssue(itemID: itemID) == nil)
            #expect(!draft.createsPlanningDemand(itemID: itemID))
        }
    }

    @Test("RFC3339 normalization retains every PostgreSQL microsecond")
    func exactMicrosecondInstantNormalization() throws {
        let instant = try #require(CanonicalRFC3339Instant(
            "2026-09-01T10:00:00.123456+02:00"
        ))
        #expect(instant.canonicalUTCString == "2026-09-01T08:00:00.123456Z")
        #expect(CanonicalRFC3339Instant("2026-09-01t08:00:00.123456z") == nil)
        #expect(CanonicalRFC3339Instant("2026-09-01T08:00:00+18:01") == nil)
        #expect(CanonicalRFC3339Instant("2026-09-01T08:00:00-18:00") != nil)
        #expect(CanonicalRFC3339Instant("2026-09-01T08:00:00.1234560000Z") == nil)
        #expect(
            CanonicalRFC3339Instant(date: instant.dateAtMicrosecondPrecision)?.canonicalUTCString
                == "2026-09-01T08:00:00.123456Z"
        )
        #expect(
            CanonicalRFC3339Instant("9999-12-30T10:00:00.000001Z")?.exactlyRepresentableDate
                == nil
        )
    }

    @Test("Inbox may retain incomplete semantics while supplied kind metadata is strict")
    func inboxCompletenessAndKindScope() {
        let habit = DayWeaveCanonicalItemDraft(
            kind: .habit,
            status: .inbox,
            title: "Maybe start a habit",
            timezoneName: "UTC"
        )
        let event = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Appointment details later",
            timezoneName: "UTC"
        )
        #expect(habit.validationIssue(itemID: UUID()) == nil)
        #expect(event.validationIssue(itemID: UUID()) == nil)

        var misplaced = habit
        misplaced.kind = .task
        misplaced.flexibleConstraints = .object([
            "habit_target": .object([
                "amount": .number(JSONNumber(UInt64(1))),
                "unit": .string("times"),
            ]),
        ])
        #expect(misplaced.validationIssue(itemID: UUID())?.contains("Habit metadata") == true)
    }

    @Test("every shared valid metadata fixture has an explicit native authoring classification")
    func sharedValidFixtureClassifications() throws {
        let fixtures = try Self.fixtureCases(named: "valid-rich-items.json")
        let expectedNames: Set<String> = [
            "frequency_task_with_rich_constraints_and_split_policy",
            "twice_daily_habit",
            "completion_relative_ordered_routine",
            "measured_goal_with_weekly_allocation",
            "prompted_movement_break",
            "open_ended_active_break",
            "blocking_imported_event_with_matching_canonical_bounds",
            "dst_fall_back_all_day_calendar_context",
            "owned_legacy_firm_block",
            "legacy_daily_default_count",
            "legacy_weekly_default_count",
            "legacy_monthly_default_count",
            "inbox_habit_may_be_incomplete",
            "inbox_event_may_lack_timing",
            "indivisible_explicit_default_split_extensions",
        ]
        let intentionallyReadOnly: Set<String> = [
            // Structural goal/dependency authority needs graph-aware item pickers.
            "frequency_task_with_rich_constraints_and_split_policy",
            // Active/scheduled rows are visible lifecycle records, not editable drafts.
            "open_ended_active_break",
            "blocking_imported_event_with_matching_canonical_bounds",
            "dst_fall_back_all_day_calendar_context",
            "owned_legacy_firm_block",
        ]

        #expect(fixtures.count == 15)
        #expect(Set(fixtures.map(\.name)) == expectedNames)
        for fixture in fixtures {
            let (itemID, draft) = try Self.draft(from: fixture)
            let state = CanonicalItemEditorState(itemID: itemID, draft: draft)
            if intentionallyReadOnly.contains(fixture.name) {
                #expect(state.readOnlyDiagnostic != nil, "Expected read-only: \(fixture.name)")
                #expect(state.draft == draft, "Read-only fixture changed: \(fixture.name)")
            } else {
                #expect(
                    draft.validationIssue(itemID: itemID) == nil,
                    "Expected server-valid native draft: \(fixture.name)"
                )
                #expect(
                    state.readOnlyDiagnostic == nil,
                    "Unexpected read-only fixture: \(fixture.name)"
                )
                #expect(
                    state.draft.validationIssue(itemID: itemID) == nil,
                    "Editor emitted an invalid fixture: \(fixture.name)"
                )
            }
        }
    }

    @Test("every shared invalid metadata fixture fails closed in native authoring")
    func sharedInvalidFixtureClassifications() throws {
        let fixtures = try Self.fixtureCases(named: "invalid-items.json")
        let expectedNames: Set<String> = [
            "unknown_metadata_key", "zero_frequency_target", "invalid_soft_weight",
            "empty_allowed_weekdays", "self_goal_reference", "self_dependency",
            "habit_metadata_on_task", "goal_with_recurrence",
            "planned_habit_without_recurrence", "planned_event_without_timing",
            "event_recurrence_must_be_expanded", "calendar_context_with_extra_metadata",
            "firm_block_without_ownership", "event_duration_mismatch",
            "all_day_bounds_not_local_midnight", "event_cannot_be_split",
            "split_extensions_on_indivisible_item", "maximum_sessions_cannot_fit_duration",
            "invalid_goal_allocation", "zero_habit_target",
            "duplicate_canonical_and_metadata_earliest_start",
            "preferred_start_without_duration", "reversed_occurrence_window",
            "calendar_frequency_with_rolling_anchor",
            "rolling_frequency_with_calendar_weekdays",
            "frequency_anchor_exceeds_database_precision", "duplicate_recurrence_weekday",
            "duplicate_goal_id", "duplicate_tag", "duplicate_allowed_weekday",
            "duplicate_daily_window_weekday", "semantic_duplicate_goal_uuid",
            "semantic_duplicate_dependency_item_id",
            "canonical_earliest_after_metadata_latest",
            "metadata_earliest_after_canonical_deadline",
            "canonical_timestamp_exceeds_database_precision",
            "fractional_event_duration_cannot_match_integral_seconds",
            "custom_rrule_rejects_ordinal_byday",
            "minimum_notice_exceeds_safe_offset", "frequency_spacing_exceeds_safe_offset",
            "zero_maximum_chunk_is_rejected_without_panicking",
            "metadata_timestamp_rejects_noncanonical_separator",
            "recurrence_anchor_rejects_excess_fractional_digits",
            "metadata_timestamp_rejects_leap_second",
            "metadata_timestamp_rejects_offset_beyond_eighteen_hours",
        ]

        #expect(fixtures.count == 45)
        #expect(Set(fixtures.map(\.name)) == expectedNames)
        for fixture in fixtures {
            do {
                let (itemID, draft) = try Self.draft(from: fixture)
                #expect(
                    draft.validationIssue(itemID: itemID) != nil,
                    "Invalid fixture was accepted: \(fixture.name)"
                )
            } catch FixtureError.unrepresentableInstant {
                #expect(
                    fixture.name == "canonical_timestamp_exceeds_database_precision",
                    "Unexpected unrepresentable instant: \(fixture.name)"
                )
            }
        }
    }

    @Test("rich pending mutations retain their version-one Codable wire shape")
    func richPendingMutationCodableCompatibility() throws {
        let draft = DayWeaveCanonicalItemDraft(
            kind: .habit,
            status: .planned,
            title: "Hydrate",
            timezoneName: "Europe/Paris",
            durationSeconds: 900,
            recurrence: .object([
                "type": .string("after_completion"),
                "interval": .number(JSONNumber(UInt64(240))),
            ]),
            flexibleConstraints: .object([
                "habit_target": .object([
                    "amount": .number(JSONNumber(UInt64(8))),
                    "unit": .string("glasses"),
                ]),
                "preserves_streak_when_paused": .bool(false),
                "tags": .array([.string("health")]),
            ])
        )
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: UUID(),
            operation: .create,
            draft: draft
        )
        let decoded = try JSONDecoder().decode(
            DayWeavePendingCanonicalAuthoringMutation.self,
            from: JSONEncoder().encode(mutation)
        )
        #expect(decoded == mutation)
        #expect(decoded.version == 1)
        #expect(decoded.isValid)
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

    private struct SchedulingFixtureCase {
        let name: String
        let fields: [String: JSONValue]
    }

    private enum FixtureError: Error {
        case invalidDocument
        case invalidField(String)
        case unrepresentableInstant
    }

    private static func fixtureCases(named fileName: String) throws
        -> [SchedulingFixtureCase] {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let data = try Data(contentsOf: repository
            .appendingPathComponent("fixtures/scheduling-metadata")
            .appendingPathComponent(fileName))
        let root = try JSONDecoder().decode(JSONValue.self, from: data)
        guard case let .object(document) = root,
              document["schema"] == .string("dayweave.scheduling-metadata-fixtures/1"),
              case let .array(entries)? = document["cases"] else {
            throw FixtureError.invalidDocument
        }
        return try entries.map { entry in
            guard case let .object(value) = entry,
                  case let .string(name)? = value["name"],
                  case let .object(fields)? = value["fields"] else {
                throw FixtureError.invalidDocument
            }
            return .init(name: name, fields: fields)
        }
    }

    private static func draft(from fixture: SchedulingFixtureCase) throws
        -> (UUID, DayWeaveCanonicalItemDraft) {
        let fields = fixture.fields
        guard case let .string(itemIDRaw)? = fields["item_id"],
              let itemID = UUID(uuidString: itemIDRaw),
              case let .string(kindRaw)? = fields["kind"],
              case let .string(statusRaw)? = fields["status"],
              case let .string(timezoneName)? = fields["timezone_name"],
              let kind = canonicalKind(kindRaw),
              let status = canonicalStatus(statusRaw),
              let duration = try optionalUnsigned(fields["duration_seconds"]),
              let deadline = try optionalInstant(fields["deadline_at"]),
              let earliestStart = try optionalInstant(fields["earliest_start_at"]),
              let recurrence = optionalJSON(fields["recurrence"]),
              let flexibleConstraints = fields["flexible_constraints"],
              let splitPolicy = splitPolicy(fields["split_policy"]),
              let parentID = try optionalUUID(fields["parent_id"]) else {
            throw FixtureError.invalidField(fixture.name)
        }
        return (itemID, DayWeaveCanonicalItemDraft(
            kind: kind,
            status: status,
            title: fixture.name,
            timezoneName: timezoneName,
            durationSeconds: duration,
            deadlineAt: deadline,
            earliestStartAt: earliestStart,
            recurrence: recurrence,
            flexibleConstraints: flexibleConstraints,
            splitPolicy: splitPolicy,
            parentID: parentID
        ))
    }

    private static func canonicalKind(_ value: String) -> DayWeaveCanonicalItemKind? {
        switch value {
        case "event": .event
        case "task": .task
        case "habit": .habit
        case "routine": .routine
        case "goal": .goal
        case "break": .breakTime
        default: nil
        }
    }

    private static func canonicalStatus(_ value: String) -> DayWeaveCanonicalItemStatus? {
        switch value {
        case "inbox": .inbox
        case "planned": .planned
        case "scheduled": .scheduled
        case "in_progress": .inProgress
        case "paused": .paused
        case "completed": .completed
        case "skipped": .skipped
        case "cancelled": .cancelled
        default: nil
        }
    }

    private static func optionalUnsigned(_ value: JSONValue?) throws -> UInt32?? {
        guard let value, value != .null else { return .some(nil) }
        guard case let .number(number) = value,
              let result = number.exactUInt32 else { return nil }
        return .some(result)
    }

    private static func optionalInstant(_ value: JSONValue?) throws -> Date?? {
        guard let value, value != .null else { return .some(nil) }
        guard case let .string(raw) = value,
              let instant = CanonicalRFC3339Instant(raw),
              instant.hasPostgresPrecision else {
            throw FixtureError.unrepresentableInstant
        }
        guard let date = instant.exactlyRepresentableDate else {
            throw FixtureError.unrepresentableInstant
        }
        return .some(date)
    }

    private static func optionalUUID(_ value: JSONValue?) throws -> UUID?? {
        guard let value, value != .null else { return .some(nil) }
        guard case let .string(raw) = value, let result = UUID(uuidString: raw) else {
            return nil
        }
        return .some(result)
    }

    private static func optionalJSON(_ value: JSONValue?) -> JSONValue?? {
        guard let value, value != .null else { return .some(nil) }
        return .some(value)
    }

    private static func splitPolicy(_ value: JSONValue?) -> DayWeaveSplitPolicy? {
        guard case let .object(fields)? = value,
              case let .string(type)? = fields["type"] else { return nil }
        switch type {
        case "indivisible":
            return Set(fields.keys) == ["type"] ? .indivisible : nil
        case "splittable":
            guard Set(fields.keys) == [
                "type", "minimum_chunk_seconds", "maximum_chunk_seconds",
            ],
            case let .number(minimum)? = fields["minimum_chunk_seconds"],
            case let .number(maximum)? = fields["maximum_chunk_seconds"],
            let minimumValue = minimum.exactUInt32,
            let maximumValue = maximum.exactUInt32 else { return nil }
            return .splittable(
                minimumChunkSeconds: minimumValue,
                maximumChunkSeconds: maximumValue
            )
        default:
            return nil
        }
    }
}

@Suite("Canonical Inbox presentation")
struct CanonicalInboxPresentationTests {
    @Test("partial Inbox events are visible but cannot be replaced lossily")
    func partialInboxEventIsReadOnly() throws {
        let partial = try decodeItem(
            id: UUID(),
            revision: 1,
            deleted: false,
            kind: "event",
            durationJSON: "900"
        )
        let empty = try decodeItem(
            id: UUID(),
            revision: 1,
            deleted: false,
            kind: "event"
        )
        let presentation = CanonicalInboxPresentation.build(
            activeItems: [partial, empty],
            pendingMutations: [],
            trashEntries: []
        )
        let rows = Dictionary(uniqueKeysWithValues: presentation.inbox.map { ($0.itemID, $0) })
        let partialRow = try #require(rows[partial.id])
        let emptyRow = try #require(rows[empty.id])

        #expect(!partial.supportsCanonicalAuthoringReplacement)
        #expect(empty.supportsCanonicalAuthoringReplacement)
        #expect(partialRow.isReadOnly)
        #expect(!emptyRow.isReadOnly)
    }

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

    @Test("dependency causes resolve multiple titles and redact cross-privacy blockers")
    func dependencyCausePresentation() throws {
        let ordinaryID = UUID()
        let sensitiveID = UUID()
        let completedID = UUID()
        let compatibilityBlockerID = UUID()
        let draft = DayWeaveCanonicalItemDraft(
            title: "Dependent work",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([
                        CanonicalDependencyEdge(
                            predecessorID: ordinaryID,
                            relation: .finishToStart,
                            minimumLagMinutes: 15,
                            strength: .hard
                        ).jsonValue,
                        CanonicalDependencyEdge(
                            predecessorID: sensitiveID,
                            relation: .startToStart,
                            minimumLagMinutes: 0,
                            strength: .hard
                        ).jsonValue,
                        CanonicalDependencyEdge(
                            predecessorID: completedID,
                            relation: .finishToFinish,
                            minimumLagMinutes: 5,
                            strength: .hard
                        ).jsonValue,
                    ]),
                ]),
            ])
        )
        let references = [
            CanonicalDependencyReference(
                id: ordinaryID,
                title: "Write release notes",
                kind: .task,
                status: .planned,
                isSensitive: false,
                isAvailable: true,
                hasOpaqueDependencies: false
            ),
            CanonicalDependencyReference(
                id: sensitiveID,
                title: "Secret medical appointment",
                kind: .event,
                status: .blocked,
                isSensitive: true,
                isAvailable: true,
                hasOpaqueDependencies: false
            ),
            CanonicalDependencyReference(
                id: completedID,
                title: "Approve design",
                kind: .task,
                status: .completed,
                isSensitive: false,
                isAvailable: true,
                hasOpaqueDependencies: false
            ),
            CanonicalDependencyReference(
                id: compatibilityBlockerID,
                title: "Legacy vendor approval",
                kind: .task,
                status: .planned,
                isSensitive: false,
                isAvailable: true,
                hasOpaqueDependencies: false
            ),
        ]

        let causes = CanonicalDependencyCatalog.causes(
            for: draft,
            ownerIsSensitive: false,
            references: references
        )
        #expect(causes.count == 3)
        #expect(causes.filter(\.isBlocking).count == 2)
        #expect(causes[0].title == "Write release notes")
        #expect(causes[0].requirementDescription.contains("15m lag"))
        #expect(causes[1].isTitleRedacted)
        #expect(causes[1].title.hasPrefix("Sensitive item"))
        #expect(!causes[1].title.contains("medical"))
        #expect(causes[2].isSatisfied)

        let privateCauses = CanonicalDependencyCatalog.causes(
            for: draft,
            ownerIsSensitive: true,
            references: references
        )
        #expect(privateCauses[1].title == "Secret medical appointment")
        #expect(!privateCauses[1].isTitleRedacted)

        let causesWithCompatibilityBlocker = CanonicalDependencyCatalog.causes(
            for: draft,
            ownerIsSensitive: false,
            references: references,
            reportedBlockerID: compatibilityBlockerID
        )
        #expect(causesWithCompatibilityBlocker.count == 4)
        let compatibilityCause = try #require(causesWithCompatibilityBlocker.last)
        #expect(compatibilityCause.predecessorID == compatibilityBlockerID)
        #expect(compatibilityCause.title == "Legacy vendor approval")
        #expect(compatibilityCause.relation == nil)
        #expect(compatibilityCause.isReportedBlocker)
        #expect(compatibilityCause.isBlocking)

        let opaqueDraft = DayWeaveCanonicalItemDraft(
            title: "Opaque dependency owner",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object(["dependencies": .string("newer-format")]),
            ])
        )
        let opaqueCauses = CanonicalDependencyCatalog.causes(
            for: opaqueDraft,
            ownerIsSensitive: false,
            references: references,
            reportedBlockerID: compatibilityBlockerID
        )
        #expect(opaqueCauses.count == 1)
        #expect(opaqueCauses[0].predecessorID == compatibilityBlockerID)
        #expect(opaqueCauses[0].isReportedBlocker)
    }

    @Test("recurring dependency ownership projects canonical, live, and pending hierarchy")
    func recurringDependencyOwnership() throws {
        let recurringRootID = UUID()
        let otherRecurringRootID = UUID()
        let predecessorID = UUID()
        let successorID = UUID()
        let ordinaryID = UUID()
        let pendingRootID = UUID()
        let pendingChildID = UUID()
        let recurrenceJSON = #"{"type":"daily","times_per_day":1}"#
        let recurringRoot = try decodeItem(
            id: recurringRootID,
            revision: 1,
            deleted: false,
            kind: "routine",
            recurrenceJSON: recurrenceJSON
        )
        let otherRecurringRoot = try decodeItem(
            id: otherRecurringRootID,
            revision: 1,
            deleted: false,
            kind: "routine",
            recurrenceJSON: recurrenceJSON
        )
        // A nested recurrence retains the highest recurring ancestor as its
        // materialization owner, matching the server's root-to-leaf projection.
        let recurringPredecessor = try decodeItem(
            id: predecessorID,
            revision: 1,
            deleted: false,
            recurrenceJSON: recurrenceJSON,
            parentID: recurringRootID
        )
        let ordinaryPredecessor = try decodeItem(
            id: ordinaryID,
            revision: 1,
            deleted: false
        )
        let dependencyJSON = """
        {"constraints":{"dependencies":[{"item_id":"\(predecessorID.uuidString.lowercased())",\
        "relation":"finish_to_start","minimum_lag":0,"strength":{"level":"hard"}}]}}
        """
        let canonicalSuccessor = try decodeItem(
            id: successorID,
            revision: 1,
            deleted: false,
            flexibleConstraintsJSON: dependencyJSON,
            parentID: recurringRootID
        )
        let canonicalItems = [
            recurringRoot,
            otherRecurringRoot,
            recurringPredecessor,
            ordinaryPredecessor,
            canonicalSuccessor,
        ]
        let sameOwnerDraft = DayWeaveCanonicalItemDraft(
            title: "Successor",
            timezoneName: "UTC",
            parentID: recurringRootID
        )

        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [],
            replacing: successorID,
            with: sameOwnerDraft,
            predecessorID: predecessorID
        ) == nil)
        let externalDraft = DayWeaveCanonicalItemDraft(
            title: "External successor",
            timezoneName: "UTC"
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [],
            replacing: successorID,
            with: externalDraft,
            predecessorID: predecessorID
        ) != nil)
        let otherOwnerDraft = DayWeaveCanonicalItemDraft(
            title: "Cross-series successor",
            timezoneName: "UTC",
            parentID: otherRecurringRootID
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [],
            replacing: successorID,
            with: otherOwnerDraft,
            predecessorID: predecessorID
        ) != nil)
        let recurringSuccessor = DayWeaveCanonicalItemDraft(
            title: "Recurring successor",
            timezoneName: "UTC",
            recurrence: .object([
                "type": .string("daily"),
                "times_per_day": .number(JSONNumber(UInt64(1))),
            ])
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [],
            replacing: successorID,
            with: recurringSuccessor,
            predecessorID: ordinaryID
        ) == nil)
        let unresolvedPredecessor = try decodeItem(
            id: UUID(),
            revision: 1,
            deleted: false,
            parentID: UUID()
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems + [unresolvedPredecessor],
            pendingMutations: [],
            replacing: successorID,
            with: externalDraft,
            predecessorID: unresolvedPredecessor.id
        ) == "Dependency recurrence safety cannot be verified because related hierarchy metadata is unavailable.")

        let retainedCrossBoundaryDraft = DayWeaveCanonicalItemDraft(
            title: "Moved successor",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([
                        CanonicalDependencyEdge(
                            predecessorID: predecessorID,
                            relation: .finishToStart,
                            minimumLagMinutes: 0,
                            strength: .hard
                        ).jsonValue,
                    ]),
                ]),
            ])
        )
        #expect(CanonicalDependencyCatalog.cycleWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [],
            replacing: successorID,
            with: retainedCrossBoundaryDraft
        ) == "A recurring predecessor can only be linked from within the same recurring subtree.")

        var reparentedPredecessor = DayWeaveCanonicalItemDraft(item: ordinaryPredecessor)
        reparentedPredecessor.parentID = recurringRootID
        reparentedPredecessor.siblingOrder = 9
        let pendingReparent = DayWeavePendingCanonicalAuthoringMutation(
            itemID: ordinaryID,
            operation: .replace,
            draft: reparentedPredecessor,
            expectedRevision: ordinaryPredecessor.revision,
            baseItem: ordinaryPredecessor
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: canonicalItems,
            pendingMutations: [pendingReparent],
            replacing: successorID,
            with: externalDraft,
            predecessorID: ordinaryID
        ) != nil)

        let pendingRoot = DayWeavePendingCanonicalAuthoringMutation(
            itemID: pendingRootID,
            operation: .create,
            draft: .init(
                kind: .routine,
                title: "Pending recurring routine",
                timezoneName: "UTC",
                recurrence: .object([
                    "type": .string("daily"),
                    "times_per_day": .number(JSONNumber(UInt64(1))),
                ])
            )
        )
        let pendingChild = DayWeavePendingCanonicalAuthoringMutation(
            itemID: pendingChildID,
            operation: .create,
            draft: .init(
                title: "Pending recurring step",
                timezoneName: "UTC",
                parentID: pendingRootID,
                siblingOrder: 4
            )
        )
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: [],
            pendingMutations: [pendingRoot, pendingChild],
            replacing: successorID,
            with: .init(
                title: "Matching pending step",
                timezoneName: "UTC",
                parentID: pendingRootID
            ),
            predecessorID: pendingChildID
        ) == nil)
        #expect(CanonicalDependencyCatalog.recurringBoundaryCandidateWarning(
            canonicalItems: [],
            pendingMutations: [pendingRoot, pendingChild],
            replacing: successorID,
            with: externalDraft,
            predecessorID: pendingChildID
        ) != nil)
    }

    private func decodeItem(
        id: UUID,
        revision: UInt64,
        deleted: Bool,
        status: String = "inbox",
        flexibleConstraintsJSON: String = "{}",
        kind: String = "task",
        recurrenceJSON: String = "null",
        parentID: UUID? = nil,
        siblingOrder: UInt32 = 0,
        durationJSON: String = "null",
        deadlineJSON: String = "null",
        earliestStartJSON: String = "null"
    ) throws -> DayWeaveCanonicalItem {
        let deletedAt = deleted
            ? ",\"deleted_at\":\"2026-08-30T10:00:00Z\""
            : ",\"deleted_at\":null"
        let completedAt = status == "completed"
            ? "\"2026-08-30T09:30:00Z\""
            : "null"
        let json = """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":true,"kind":"\(kind)",
        "status":"\(status)","title":"Lifecycle task","notes":null,"timezone_name":"Europe/Madrid",
        "duration_seconds":\(durationJSON),"deadline_at":\(deadlineJSON),
        "earliest_start_at":\(earliestStartJSON),"recurrence":\(recurrenceJSON),
        "flexible_constraints":\(flexibleConstraintsJSON),"split_policy":{"type":"indivisible"},"importance":50,
        "urgency":50,"parent_id":\(parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"),
        "sibling_order":\(siblingOrder),"is_executable":false,"revision":\(revision),
        "created_at":"2026-08-30T09:00:00Z","updated_at":"2026-08-30T10:00:00Z",
        "completed_at":\(completedAt)\(deletedAt)}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
