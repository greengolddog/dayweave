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
            sourceItemRevision: 91,
            occurrenceID: UUID(),
            sessionIndex: 2,
            syncOrigin: .local,
            placementReason: "PRIVATE-PLACEMENT-DIAGNOSTIC",
            previewKind: "planned",
            occurrenceFullyScheduled: false
        )
        let store = PlannerStore(
            blocks: [block],
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

    @Test("only strict bounded proposal metadata becomes a reviewable Inbox entry")
    func testProposalEnvelopeAndApprovalBoundary() {
        let raw = """
        Moving the task after lunch protects your focus block.
        <dayweave-proposals-v1>{"suggestions":[{"title":"Move launch prep","summary":"Move the flexible launch-prep block to after lunch."}]}</dayweave-proposals-v1>
        """
        let parsed = CodexProposalEnvelopeParser.parse(raw)
        #expect(parsed.visibleText == "Moving the task after lunch protects your focus block.")
        #expect(parsed.drafts == [CodexSuggestionDraft(
            title: "Move launch prep",
            summary: "Move the flexible launch-prep block to after lunch."
        )])
        #expect(!parsed.containedInvalidEnvelope)

        let block = ScheduleBlock(
            id: UUID(),
            title: "Launch prep",
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil
        )
        let store = PlannerStore(blocks: [block], restoreFromPersistence: false)
        let router = CodexSuggestionInboxRouter(planner: store)
        let originalBlocks = store.blocks
        let createdAt = Date(timeIntervalSince1970: 1_787_986_845)

        #expect(router.routeCodexSuggestionsToInbox(parsed.drafts, createdAt: createdAt) == 1)
        let suggestion = store.suggestions[0]
        #expect(suggestion.state == .pending)
        #expect(suggestion.source == "Codex · requires approval")
        #expect(suggestion.expiresAt == createdAt.addingTimeInterval(7 * 24 * 60 * 60))
        #expect(store.blocks == originalBlocks)

        store.acceptSuggestion(suggestion.id)
        #expect(store.suggestions[0].state == .accepted)
        #expect(store.blocks == originalBlocks)
        #expect(router.routeCodexSuggestionsToInbox(parsed.drafts, createdAt: createdAt) == 0)
        #expect(store.blocks == originalBlocks)
    }

    @Test("malformed or partial proposal metadata remains hidden and never routes")
    func testMalformedProposalEnvelopeIsRejected() {
        let malformed = """
        Human reply
        <dayweave-proposals-v1>{"suggestions":[{"title":"Unsafe","summary":"No","extra":true}]}</dayweave-proposals-v1>
        """
        let parsed = CodexProposalEnvelopeParser.parse(malformed)
        #expect(parsed.visibleText == "Human reply")
        #expect(parsed.drafts.isEmpty)
        #expect(parsed.containedInvalidEnvelope)

        #expect(CodexProposalEnvelopeParser.visibleStreamingText("Reply<dayweave-prop") == "Reply")
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
