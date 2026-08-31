import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical item editor mode")
struct CanonicalItemEditorModeTests {
    @Test("a Codex suggestion opens as an unchanged approvable create")
    func codexSuggestionCreateMode() {
        let suggestionID = UUID(uuidString: "aa000000-0000-4000-8000-000000000001")!
        let itemID = UUID(uuidString: "bb000000-0000-4000-8000-000000000001")!
        let draft = DayWeaveCanonicalItemDraft(
            isSensitive: true,
            kind: .habit,
            status: .inbox,
            title: "Walk after lunch",
            timezoneName: "Europe/Madrid",
            durationSeconds: 1_800,
            recurrence: .object([
                "type": .string("daily"),
                "times_per_day": .number(JSONNumber(UInt64(1))),
            ])
        )

        let mode = CanonicalItemEditorMode.createFromSuggestion(
            suggestionID: suggestionID,
            itemID: itemID,
            draft: draft
        )

        #expect(mode.itemID == itemID)
        #expect(mode.initialDraft == draft)
        #expect(mode.title == "Review Codex item draft")
        #expect(mode.actionTitle == "Create item")
        #expect(mode.subtitle.contains("Codex cannot create"))
        #expect(mode.allowsUnchangedDraft)
        #expect(mode.preservesSensitivePresentation)
    }

    @Test("replacement modes still require a changed draft")
    func replacementModesRequireChange() {
        let itemID = UUID(uuidString: "cc000000-0000-4000-8000-000000000001")!
        let mutationID = UUID(uuidString: "dd000000-0000-4000-8000-000000000001")!
        let draft = DayWeaveCanonicalItemDraft(
            title: "Existing item",
            timezoneName: "UTC"
        )

        #expect(!CanonicalItemEditorMode.replace(
            itemID: itemID,
            draft: draft
        ).allowsUnchangedDraft)
        #expect(!CanonicalItemEditorMode.updatePending(
            mutationID: mutationID,
            itemID: itemID,
            draft: draft
        ).allowsUnchangedDraft)
        #expect(CanonicalItemEditorMode.create(itemID: itemID).allowsUnchangedDraft)
        #expect(!CanonicalItemEditorMode.create(itemID: itemID).preservesSensitivePresentation)
    }

    @Test("suggestion review availability closes only terminal or invalid routes")
    func suggestionReviewAvailability() {
        let suggestionID = UUID(uuidString: "ee000000-0000-4000-8000-000000000001")!
        let itemID = UUID(uuidString: "ff000000-0000-4000-8000-000000000001")!
        let draft = DayWeaveCanonicalItemDraft(
            isSensitive: true,
            status: .inbox,
            title: "Private Inbox draft",
            timezoneName: "UTC"
        )
        let route = LocalCanonicalItemSuggestionReviewRoute(
            suggestionID: suggestionID,
            itemID: itemID,
            draft: draft
        )
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        var suggestion = PlanningSuggestion(
            id: suggestionID,
            title: draft.title,
            summary: "Review this draft",
            source: PlanningSuggestion.codexSource,
            createdAt: now,
            expiresAt: now.addingTimeInterval(60),
            state: .pending,
            payload: .canonicalItemDraft(.init(itemID: itemID, draft: draft))
        )

        #expect(localCanonicalItemSuggestionReviewAvailability(
            route: route,
            suggestions: [suggestion]
        ) == .pending)

        suggestion.state = .accepted
        suggestion.payload = .canonicalItemReference(itemID: itemID)
        suggestion.resultingItemID = itemID
        suggestion.resultingMutationID = UUID()
        #expect(localCanonicalItemSuggestionReviewAvailability(
            route: route,
            suggestions: [suggestion]
        ) == .accepted)

        suggestion.state = .expired
        suggestion.payload = .canonicalItemReference(itemID: itemID)
        suggestion.resultingItemID = nil
        suggestion.resultingMutationID = nil
        #expect(localCanonicalItemSuggestionReviewAvailability(
            route: route,
            suggestions: [suggestion]
        ) == .unavailable)

        suggestion.state = .pending
        suggestion.payload = .advisory
        #expect(localCanonicalItemSuggestionReviewAvailability(
            route: route,
            suggestions: [suggestion]
        ) == .unavailable)
    }
}
#endif
