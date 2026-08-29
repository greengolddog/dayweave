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
            syncOrigin: .canonicalPreview,
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
}
#endif
