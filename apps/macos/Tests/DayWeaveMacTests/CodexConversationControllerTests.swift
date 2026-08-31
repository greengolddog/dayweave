import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@MainActor
@Suite("Codex planner conversation boundary")
struct CodexConversationControllerTests {
    @Test("planner context is explicit, bounded, and omits private planner fields")
    func testPlannerContextRedaction() throws {
        let sourceID = UUID()
        let source = try Self.canonicalItem(
            id: sourceID,
            parentID: nil,
            isSensitive: false,
            title: "Prepare launch plan",
            kind: "task",
            isExecutable: true
        )
        let block = ScheduleBlock(
            id: UUID(),
            title: "Prepare launch plan",
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: "Launch",
            notes: "PRIVATE-NOTE-DO-NOT-SEND",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: sourceID,
            sourceItemRevision: 1,
            occurrenceID: UUID(),
            sessionIndex: 2,
            syncOrigin: .local,
            placementReason: "PRIVATE-PLACEMENT-DIAGNOSTIC",
            previewKind: "planned",
            occurrenceFullyScheduled: false
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [source],
            restoreFromPersistence: false
        )

        let input = try CodexPlannerContextSerializer.turnInput(
            snapshot: store.codexPlannerContextSnapshot(),
            userMessage: "Can this move later?"
        )

        #expect(input.contains("Prepare launch plan"))
        #expect(input.contains("Can this move later?"))
        #expect(input.contains("block-1"))
        #expect(!input.contains("PRIVATE-NOTE-DO-NOT-SEND"))
        #expect(!input.contains("PRIVATE-PLACEMENT-DIAGNOSTIC"))
        #expect(!input.localizedCaseInsensitiveContains(sourceID.uuidString))
        #expect(!input.contains("\"sourceItemRevision\""))
        #expect(input.utf8.count < 96 * 1_024)
    }

    @Test("sensitive canonical content becomes only an unreferenced busy span")
    func testSensitivePlannerContextIsOccupancyOnly() throws {
        let parentID = UUID(uuidString: "90000000-0000-4000-8000-000000000001")!
        let childID = UUID(uuidString: "90000000-0000-4000-8000-000000000002")!
        let privateTitle = "SYNTHETIC-CODEX-SENSITIVE-CANARY"
        let parent = try Self.canonicalItem(
            id: parentID,
            parentID: nil,
            isSensitive: true,
            title: "SYNTHETIC-PRIVATE-PARENT-CANARY",
            kind: "goal",
            isExecutable: false
        )
        let child = try Self.canonicalItem(
            id: childID,
            parentID: parentID,
            isSensitive: false,
            title: privateTitle,
            kind: "task",
            isExecutable: true
        )
        let block = ScheduleBlock(
            id: UUID(uuidString: "90000000-0000-4000-8000-000000000003")!,
            isSensitive: true,
            title: privateTitle,
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: "SYNTHETIC-PRIVATE-PROJECT-CANARY",
            notes: "SYNTHETIC-PRIVATE-NOTES-CANARY",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: childID,
            sourceItemRevision: 1,
            syncOrigin: .canonicalPreview,
            placementReason: "SYNTHETIC-PRIVATE-EXPLANATION-CANARY",
            previewKind: "planned"
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [parent, child],
            restoreFromPersistence: false
        )
        let snapshot = store.codexPlannerContextSnapshot()
        #expect(snapshot.scheduledBlocks.isEmpty)
        #expect(snapshot.plannerItems.isEmpty)
        #expect(snapshot.totalPlannerItemCount == 0)
        #expect(snapshot.privateBusySpans.count == 1)
        #expect(snapshot.privateBusySpans[0].startsAt == block.start)
        #expect(snapshot.privateBusySpans[0].endsAt == block.end)

        let input = try CodexPlannerContextSerializer.turnInput(
            snapshot: snapshot,
            userMessage: "Where is my free time?"
        )
        for forbidden in [
            privateTitle,
            "SYNTHETIC-PRIVATE-PARENT-CANARY",
            "SYNTHETIC-PRIVATE-PROJECT-CANARY",
            "SYNTHETIC-PRIVATE-NOTES-CANARY",
            "SYNTHETIC-PRIVATE-EXPLANATION-CANARY",
            parentID.uuidString,
            childID.uuidString,
            "block-1",
        ] {
            #expect(!input.localizedCaseInsensitiveContains(forbidden))
        }
        #expect(input.contains("\"privateBusySpans\""))
    }

    @Test("a locally queued privacy mark redacts content before any network round trip")
    func testPendingSensitivityMarkRedactsImmediately() throws {
        let itemID = UUID(uuidString: "90500000-0000-4000-8000-000000000001")!
        let canary = "SYNTHETIC-PENDING-PRIVACY-CANARY"
        let item = try Self.canonicalItem(
            id: itemID,
            parentID: nil,
            isSensitive: false,
            title: canary,
            kind: "task",
            isExecutable: true
        )
        let block = ScheduleBlock(
            id: UUID(uuidString: "90500000-0000-4000-8000-000000000002")!,
            isSensitive: false,
            title: canary,
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: "SYNTHETIC-PENDING-PRIVATE-PROJECT",
            notes: "SYNTHETIC-PENDING-PRIVATE-NOTES",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: 1,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            restoreFromPersistence: false
        )

        #expect(store.setCanonicalItemSensitivity(itemID, isSensitive: true))
        #expect(store.blocks[0].isSensitive)
        let snapshot = store.codexPlannerContextSnapshot()
        #expect(snapshot.scheduledBlocks.isEmpty)
        #expect(snapshot.plannerItems.isEmpty)
        #expect(snapshot.privateBusySpans.count == 1)

        let input = try CodexPlannerContextSerializer.turnInput(
            snapshot: snapshot,
            userMessage: "What is available?"
        )
        for forbidden in [
            canary,
            "SYNTHETIC-PENDING-PRIVATE-PROJECT",
            "SYNTHETIC-PENDING-PRIVATE-NOTES",
            itemID.uuidString,
        ] {
            #expect(!input.localizedCaseInsensitiveContains(forbidden))
        }
    }

    @Test("a submitted privacy mark stays redacted through a queued unmark")
    func testAmbiguousMarkWithRemovalFollowUpStaysRedacted() throws {
        let itemID = UUID(uuidString: "90500000-0000-4000-8000-000000000003")!
        let canary = "SYNTHETIC-AMBIGUOUS-MARK-CANARY"
        let item = try Self.canonicalItem(
            id: itemID,
            parentID: nil,
            isSensitive: false,
            title: canary,
            kind: "task",
            isExecutable: true
        )
        let block = ScheduleBlock(
            id: UUID(uuidString: "90500000-0000-4000-8000-000000000004")!,
            isSensitive: false,
            title: canary,
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: "SYNTHETIC-AMBIGUOUS-MARK-PROJECT",
            notes: "SYNTHETIC-AMBIGUOUS-MARK-NOTES",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: 1,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            restoreFromPersistence: false
        )

        #expect(store.setCanonicalItemSensitivity(itemID, isSensitive: true))
        let submitted = try #require(store.pendingCanonicalSensitivityMutations.first)
        #expect(store.markCanonicalSensitivityMutationSubmitted(submitted.id))
        #expect(store.setCanonicalItemSensitivity(itemID, isSensitive: false))
        let mutation = try #require(store.pendingCanonicalSensitivityMutations.first)
        #expect(mutation.desiredIsSensitive)
        #expect(mutation.followUpIsSensitive == false)
        #expect(mutation.requestedIsSensitive == false)
        #expect(mutation.requiresSensitivePresentation)

        let snapshot = store.codexPlannerContextSnapshot()
        #expect(snapshot.scheduledBlocks.isEmpty)
        #expect(snapshot.plannerItems.isEmpty)
        #expect(snapshot.privateBusySpans.count == 1)
        let input = try CodexPlannerContextSerializer.turnInput(
            snapshot: snapshot,
            userMessage: "What is available?"
        )
        for forbidden in [
            canary,
            "SYNTHETIC-AMBIGUOUS-MARK-PROJECT",
            "SYNTHETIC-AMBIGUOUS-MARK-NOTES",
            itemID.uuidString,
        ] {
            #expect(!input.localizedCaseInsensitiveContains(forbidden))
        }
    }

    @Test("private block metadata cannot perturb public references or serialized context")
    func testSensitiveMetadataHasNoObservableOrderingSideChannel() throws {
        let instant = Date(timeIntervalSince1970: 1_788_033_600)
        let publicBlock = ScheduleBlock(
            id: UUID(uuidString: "91000000-0000-4000-8000-000000000001")!,
            isSensitive: false,
            title: "MIDDLE-PUBLIC-CANARY",
            kind: .task,
            start: instant,
            end: instant.addingTimeInterval(1_800),
            status: .scheduled,
            project: "Public project",
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            syncOrigin: .local
        )
        func privateBlock(
            id: UUID,
            title: String,
            kind: PlannerItemKind,
            status: PlannerItemStatus,
            project: String,
            explanation: String
        ) -> ScheduleBlock {
            ScheduleBlock(
                id: id,
                isSensitive: true,
                title: title,
                kind: kind,
                start: instant,
                end: instant.addingTimeInterval(1_800),
                status: status,
                project: project,
                notes: "SYNTHETIC-PRIVATE-INVARIANCE-NOTES",
                energy: .deep,
                isFlexible: false,
                isHardConstraint: true,
                actualMinutes: 17,
                syncOrigin: .local,
                placementReason: explanation,
                previewKind: "SYNTHETIC-PRIVATE-KIND"
            )
        }
        let firstPrivate = privateBlock(
            id: UUID(uuidString: "91000000-0000-4000-8000-000000000002")!,
            title: "AAA-SYNTHETIC-PRIVATE-ORDER-CANARY",
            kind: .event,
            status: .active,
            project: "SYNTHETIC-PRIVATE-PROJECT-A",
            explanation: "SYNTHETIC-PRIVATE-EXPLANATION-A"
        )
        let secondPrivate = privateBlock(
            id: UUID(uuidString: "91000000-0000-4000-8000-000000000003")!,
            title: "ZZZ-SYNTHETIC-PRIVATE-ORDER-CANARY",
            kind: .breakTime,
            status: .paused,
            project: "SYNTHETIC-PRIVATE-PROJECT-Z",
            explanation: "SYNTHETIC-PRIVATE-EXPLANATION-Z"
        )
        let first = PlannerStore(
            blocks: [publicBlock, firstPrivate],
            restoreFromPersistence: false
        ).codexPlannerContextSnapshot()
        let second = PlannerStore(
            blocks: [publicBlock, secondPrivate],
            restoreFromPersistence: false
        ).codexPlannerContextSnapshot()
        func stableTimestamp(_ snapshot: CodexPlannerContextSnapshot) -> CodexPlannerContextSnapshot {
            CodexPlannerContextSnapshot(
                generatedAt: Date(timeIntervalSince1970: 0),
                timezone: snapshot.timezone,
                scheduledBlocks: snapshot.scheduledBlocks,
                privateBusySpans: snapshot.privateBusySpans,
                totalScheduledBlockCount: snapshot.totalScheduledBlockCount,
                plannerItems: snapshot.plannerItems,
                totalPlannerItemCount: snapshot.totalPlannerItemCount,
                pendingSuggestionCount: snapshot.pendingSuggestionCount,
                omittedFields: snapshot.omittedFields
            )
        }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]

        #expect(first.scheduledBlocks.map(\.reference) == ["block-1"])
        #expect(first.privateBusySpans.count == 1)
        #expect(try encoder.encode(stableTimestamp(first)) == encoder.encode(stableTimestamp(second)))
    }

    @Test("planner context uses the persisted schedule profile timezone")
    func testPlannerContextUsesProfileTimezone() throws {
        let profile = try ScheduleProfile.legacyDefault(
            timezoneName: "America/New_York",
            protectedFreeMinutes: 90
        )
        let store = PlannerStore(
            scheduleProfile: profile,
            restoreFromPersistence: false
        )

        #expect(store.codexPlannerContextSnapshot().timezone == "America/New_York")
    }

    @Test("oversized user messages never cross the app-server boundary")
    func testOversizedUserMessageIsRejected() {
        let store = PlannerStore(restoreFromPersistence: false)
        let oversized = String(
            repeating: "x",
            count: CodexPlannerContextSerializer.maximumUserMessageBytes + 1
        )

        #expect(throws: CodexPlannerContextError.invalidUserMessage) {
            try CodexPlannerContextSerializer.turnInput(
                snapshot: store.codexPlannerContextSnapshot(),
                userMessage: oversized
            )
        }
    }

    @Test("a strict item-draft envelope becomes an app-owned editable canonical draft")
    func testCanonicalItemDraftEnvelope() throws {
        let item = Self.itemJSON(
            title: "  Move launch prep  ",
            deadline: "\"2026-09-02T17:30:00+02:00\"",
            earliestStart: "\"2026-09-01T09:00:00+02:00\"",
            recurrence: "{\"type\":\"daily\",\"times_per_day\":1}",
            constraints: "{\"energy\":\"deep\"}",
            splitPolicy: "{\"type\":\"splittable\",\"minimum_chunk_seconds\":900,\"maximum_chunk_seconds\":1800}"
        )
        let raw = Self.envelope(
            [Self.suggestionJSON(
                summary: "Move the flexible launch-prep block to after lunch.",
                item: item
            )],
            visibleText: "Moving the task after lunch protects your focus block."
        )
        let parsed = CodexProposalEnvelopeParser.parse(raw)
        #expect(parsed.visibleText == "Moving the task after lunch protects your focus block.")
        #expect(!parsed.containedInvalidEnvelope)
        let suggestion = try #require(parsed.drafts.first)
        #expect(suggestion.summary == "Move the flexible launch-prep block to after lunch.")
        #expect(suggestion.canonicalDraft.isSensitive)
        #expect(suggestion.canonicalDraft.status == .inbox)
        #expect(suggestion.canonicalDraft.parentID == nil)
        #expect(suggestion.canonicalDraft.siblingOrder == 0)
        #expect(suggestion.canonicalDraft.title == "Move launch prep")
        #expect(suggestion.canonicalDraft.kind == .task)
        #expect(suggestion.canonicalDraft.durationSeconds == 1_800)
        #expect(suggestion.canonicalDraft.importance == 50)
        #expect(suggestion.canonicalDraft.urgency == 50)
        #expect(suggestion.canonicalDraft.validationIssue(itemID: UUID()) == nil)
        let editor = CanonicalItemEditorState(
            itemID: UUID(),
            draft: suggestion.canonicalDraft
        )
        #expect(editor.readOnlyDiagnostic == nil)
        #expect(editor.validationIssue == nil)
    }

    @Test("all supported editable item kinds cross the Codex boundary")
    func testAllEditableItemKinds() {
        let eventConstraints = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"2026-09-01T09:00:00Z","ends_at":"2026-09-01T10:00:00Z","all_day":false,"tentative":false,"busy":true}}
        """
        let items = [
            Self.itemJSON(kind: "event", duration: "3600", constraints: eventConstraints),
            Self.itemJSON(kind: "task"),
            Self.itemJSON(
                kind: "habit",
                recurrence: "{\"type\":\"weekly\",\"times_per_week\":3,\"weekdays\":[\"monday\",\"wednesday\",\"friday\"]}"
            ),
            Self.itemJSON(
                kind: "routine",
                constraints: "{\"has_own_effort\":false}"
            ),
            Self.itemJSON(
                kind: "goal",
                constraints: "{\"has_own_effort\":false}"
            ),
            Self.itemJSON(kind: "break"),
        ]
        for item in items {
            let single = CodexProposalEnvelopeParser.parse(
                Self.envelope([Self.suggestionJSON(summary: "Editable", item: item)])
            )
            #expect(!single.containedInvalidEnvelope)
            #expect(single.drafts.count == 1)
        }
    }

    @Test("exact root, draft, and item keys are mandatory")
    func testExactEnvelopeKeys() {
        let validItem = Self.itemJSON()
        let validDraft = Self.suggestionJSON(summary: "Valid", item: validItem)
        let invalidJSON = [
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[\(validDraft)],\"extra\":true}",
            "{\"schema\":\"dayweave.item-drafts/2\",\"drafts\":[\(validDraft)]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"Valid\",\"item\":\(validItem),\"extra\":true}]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"Valid\",\"item\":\(Self.itemJSON(extraField: ",\"id\":\"01234567-89ab-cdef-0123-456789abcdef\""))}]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"Valid\",\"item\":\(Self.itemJSON(omitUrgency: true))}]}",
        ]

        for json in invalidJSON {
            let parsed = CodexProposalEnvelopeParser.parse(Self.wrapped(json))
            #expect(parsed.drafts.isEmpty)
            #expect(parsed.containedInvalidEnvelope)
        }
    }

    @Test("duplicate JSON keys at every depth reject the whole envelope")
    func testDuplicateKeysFailClosedAtEveryDepth() {
        let validItem = Self.itemJSON()
        let duplicateJSON = [
            "{\"schema\":\"dayweave.item-drafts/1\",\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"One\",\"summary\":\"Two\",\"item\":\(validItem)}]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"One\",\"item\":\(Self.itemJSON(extraField: ",\"title\":\"Duplicate\""))}]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"One\",\"item\":\(Self.itemJSON(kind: "habit", recurrence: "{\"type\":\"daily\",\"type\":\"daily\",\"times_per_day\":1}"))}]}",
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[{\"summary\":\"One\",\"item\":\(Self.itemJSON(constraints: "{\"energy\":\"low\",\"energy\":\"deep\"}"))}]}",
        ]

        for json in duplicateJSON {
            let parsed = CodexProposalEnvelopeParser.parse(Self.wrapped(json))
            #expect(parsed.drafts.isEmpty)
            #expect(parsed.containedInvalidEnvelope)
        }
    }

    @Test("one invalid draft rejects every otherwise-valid member")
    func testEnvelopeIsAtomicAtIngress() {
        let valid = Self.suggestionJSON(summary: "Valid", item: Self.itemJSON())
        let invalid = Self.suggestionJSON(
            summary: "Invalid",
            item: Self.itemJSON(kind: "unsupported")
        )
        let parsed = CodexProposalEnvelopeParser.parse(Self.envelope([valid, invalid]))

        #expect(parsed.drafts.isEmpty)
        #expect(parsed.containedInvalidEnvelope)
    }

    @Test("unsupported values, non-integers, unsafe text, and read-only forms are denied")
    func testUnsafeOrUnsupportedDraftValuesAreDenied() {
        let invalidItems = [
            Self.itemJSON(timezone: "PST"),
            Self.itemJSON(deadline: "\"2026-02-30T12:00:00Z\""),
            Self.itemJSON(deadline: "\"2026-09-01T12:00:00+0200\""),
            Self.itemJSON(duration: "1800.0"),
            Self.itemJSON(importance: "true"),
            Self.itemJSON(title: "Unsafe\\u202Etitle"),
            Self.itemJSON(notes: "\"Unsafe\\nnotes\""),
            Self.itemJSON(
                recurrence: "{\"type\":\"frequency\",\"target\":2,\"period\":\"week\",\"semantics\":\"calendar\"}"
            ),
            Self.itemJSON(
                kind: "event",
                constraints: "{\"calendar_event\":{\"start\":\"2026-09-01T09:00:00Z\",\"end\":\"2026-09-01T10:00:00Z\",\"immutable\":true,\"all_day\":false}}"
            ),
        ]

        for item in invalidItems {
            let parsed = CodexProposalEnvelopeParser.parse(Self.envelope([
                Self.suggestionJSON(summary: "Unsafe", item: item),
            ]))
            #expect(parsed.drafts.isEmpty)
            #expect(parsed.containedInvalidEnvelope)
        }
    }

    @Test("Codex drafts cannot hide semantics outside the typed review surface")
    func testHiddenReviewFieldsAreDenied() {
        let ordinaryFirmBlock = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"2026-09-01T09:00:00Z","ends_at":"2026-09-01T10:00:00Z","all_day":false,"tentative":false,"busy":true}}
        """
        let fractionalFirmBlock = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"2026-09-01T09:00:00.000000001Z","ends_at":"2026-09-01T10:00:00Z","all_day":false,"tentative":false,"busy":true}}
        """
        let historicalFirmBlock = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"1890-01-01T09:00:00Z","ends_at":"1890-01-01T10:00:00Z","all_day":false,"tentative":false,"busy":true}}
        """
        let nonMidnightAllDayFirmBlock = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"2026-09-01T12:00:00Z","ends_at":"2026-09-02T12:00:00Z","all_day":true,"tentative":false,"busy":true}}
        """
        let ambiguousFallBackFirmBlock = """
        {"dayweave_firm_block":{"owned":true,"starts_at":"2026-11-01T01:30:00-04:00","ends_at":"2026-11-01T01:30:00-05:00","all_day":false,"tentative":false,"busy":true}}
        """
        let hiddenConstraints = [
            "{\"routine_ordered\":true}",
            "{\"preserves_streak_when_paused\":true}",
            "{\"break_category\":\"rest\"}",
            "{\"break_mandatory\":true}",
            "{\"break_prompt_to_resume\":true}",
            "{\"has_own_effort\":true}",
        ]
        var invalidItems = hiddenConstraints.map { Self.itemJSON(constraints: $0) }
        invalidItems.append(contentsOf: [
            Self.itemJSON(kind: "goal"),
            Self.itemJSON(deadline: "\"2026-09-01T12:00:01Z\""),
            Self.itemJSON(
                kind: "event",
                duration: "1800",
                constraints: ordinaryFirmBlock
            ),
            Self.itemJSON(
                kind: "event",
                constraints: ordinaryFirmBlock,
                splitPolicy: "{\"type\":\"splittable\",\"minimum_chunk_seconds\":900,\"maximum_chunk_seconds\":1800}"
            ),
            Self.itemJSON(
                kind: "event",
                constraints: fractionalFirmBlock
            ),
            Self.itemJSON(
                kind: "event",
                timezone: "Europe/Paris",
                constraints: historicalFirmBlock
            ),
            Self.itemJSON(
                kind: "event",
                duration: "86400",
                constraints: nonMidnightAllDayFirmBlock
            ),
            Self.itemJSON(
                kind: "event",
                timezone: "America/New_York",
                duration: "3600",
                constraints: ambiguousFallBackFirmBlock
            ),
            Self.itemJSON(
                timezone: "America/New_York",
                deadline: "\"2026-11-01T01:30:00-04:00\""
            ),
        ])

        for item in invalidItems {
            let parsed = CodexProposalEnvelopeParser.parse(Self.envelope([
                Self.suggestionJSON(summary: "Must be visible", item: item),
            ]))
            #expect(parsed.drafts.isEmpty)
            #expect(parsed.containedInvalidEnvelope)
        }

        let nilDurationEvent = CodexProposalEnvelopeParser.parse(Self.envelope([
            Self.suggestionJSON(
                summary: "Visible fixed range",
                item: Self.itemJSON(
                    kind: "event",
                    duration: "null",
                    constraints: ordinaryFirmBlock
                )
            ),
        ]))
        #expect(!nilDurationEvent.containedInvalidEnvelope)
        #expect(nilDurationEvent.drafts.count == 1)
    }

    @Test("the item-draft envelope is bounded, trailing, and hidden while streaming")
    func testEnvelopeBoundsAndStreamingVisibility() {
        let draft = Self.suggestionJSON(summary: "Bounded", item: Self.itemJSON())
        let fiveDrafts = CodexProposalEnvelopeParser.parse(
            Self.envelope(Array(repeating: draft, count: 5))
        )
        #expect(!fiveDrafts.containedInvalidEnvelope)
        #expect(fiveDrafts.drafts.count == 5)

        let sixDrafts = CodexProposalEnvelopeParser.parse(
            Self.envelope(Array(repeating: draft, count: 6))
        )
        #expect(sixDrafts.drafts.isEmpty)
        #expect(sixDrafts.containedInvalidEnvelope)

        let oversizedItem = Self.itemJSON(notes: "\"\(String(repeating: "x", count: 66_000))\"")
        let oversized = CodexProposalEnvelopeParser.parse(Self.envelope([
            Self.suggestionJSON(summary: "Large", item: oversizedItem),
        ]))
        #expect(oversized.drafts.isEmpty)
        #expect(oversized.containedInvalidEnvelope)

        let trailing = CodexProposalEnvelopeParser.parse(
            Self.envelope([draft]) + " trailing"
        )
        #expect(trailing.drafts.isEmpty)
        #expect(trailing.containedInvalidEnvelope)

        #expect(CodexProposalEnvelopeParser.visibleStreamingText(
            "Reply<dayweave-item-draf"
        ) == "Reply")
        let malformed = CodexProposalEnvelopeParser.parse(
            "Reply<dayweave-item-drafts-v1>{}"
        )
        #expect(malformed.visibleText == "Reply")
        #expect(malformed.containedInvalidEnvelope)
    }

    @Test("the developer prompt exposes only the strict draft contract")
    func testDeveloperPromptUsesStrictDraftContract() {
        let prompt = CodexConversationController.developerInstructions
        #expect(prompt.contains(CodexProposalEnvelopeParser.startMarker))
        #expect(prompt.contains("dayweave.item-drafts/1"))
        #expect(prompt.contains("never emit IDs, status, sensitivity"))
        #expect(prompt.contains("unambiguous whole minute"))
        #expect(prompt.contains("start and end at local midnight"))
        #expect(!prompt.contains("dayweave-proposals-v1"))
    }

    private static func itemJSON(
        kind: String = "task",
        title: String = "Draft item",
        notes: String = "null",
        timezone: String = "UTC",
        duration: String = "1800",
        deadline: String = "null",
        earliestStart: String = "null",
        recurrence: String = "null",
        constraints: String = "{}",
        splitPolicy: String = "{\"type\":\"indivisible\"}",
        importance: String = "50",
        urgency: String = "50",
        extraField: String = "",
        omitUrgency: Bool = false
    ) -> String {
        let urgencyField = omitUrgency ? "" : ",\"urgency\":\(urgency)"
        return """
        {"kind":"\(kind)","title":"\(title)","notes":\(notes),"timezone_name":"\(timezone)","duration_seconds":\(duration),"deadline_at":\(deadline),"earliest_start_at":\(earliestStart),"recurrence":\(recurrence),"flexible_constraints":\(constraints),"split_policy":\(splitPolicy),"importance":\(importance)\(urgencyField)\(extraField)}
        """
    }

    private static func suggestionJSON(summary: String, item: String) -> String {
        "{\"summary\":\"\(summary)\",\"item\":\(item)}"
    }

    private static func envelope(
        _ suggestions: [String],
        visibleText: String = "Reply"
    ) -> String {
        wrapped(
            "{\"schema\":\"dayweave.item-drafts/1\",\"drafts\":[\(suggestions.joined(separator: ","))]}",
            visibleText: visibleText
        )
    }

    private static func wrapped(_ json: String, visibleText: String = "Reply") -> String {
        "\(visibleText)\n\(CodexProposalEnvelopeParser.startMarker)\(json)\(CodexProposalEnvelopeParser.endMarker)"
    }

    private static func canonicalItem(
        id: UUID,
        parentID: UUID?,
        isSensitive: Bool,
        title: String,
        kind: String,
        isExecutable: Bool
    ) throws -> DayWeaveCanonicalItem {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        let json = """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":\(isSensitive),
         "kind":"\(kind)","status":"planned","title":"\(title)","notes":null,
         "timezone_name":"UTC","duration_seconds":3600,"deadline_at":null,
         "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
         "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
         "parent_id":\(parent),"sibling_order":0,"is_executable":\(isExecutable),
         "revision":1,"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":null}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
