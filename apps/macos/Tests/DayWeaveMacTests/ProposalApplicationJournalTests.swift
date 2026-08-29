import Foundation
#if canImport(XCTest)
import XCTest
#elseif canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

private struct ProposalApplicationJournalScenarioFailure: Error, CustomStringConvertible {
    let description: String
}

@MainActor
private enum ProposalApplicationJournalScenarios {
    static let configurationIdentifier =
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"
    static let reviewHash = "sha256:\(String(repeating: "b", count: 64))"

    static func exactApplyAndUndoSurviveRelaunch() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proposalID = UUID(uuidString: "a1000000-0000-4000-8000-000000000001")!
        let commandID = UUID(uuidString: "a2000000-0000-4000-8000-000000000002")!
        let previewID = UUID(uuidString: "a3000000-0000-4000-8000-000000000003")!
        let appliedAt = Date(timeIntervalSince1970: 1_700_000_000)
        let applyBody = try encode(DayWeaveProposalApplyRequest(
            expectedReviewHash: reviewHash
        ))
        let pendingApply = DayWeavePendingProposalApplicationMutation.apply(
            configurationIdentifier: configurationIdentifier,
            proposalIDs: [proposalID],
            proposalRevisions: [4],
            expectedCommandIDs: [commandID],
            previewID: previewID,
            expectedReviewHash: reviewHash,
            requestBody: applyBody,
            idempotencyKey: "proposal-apply-retry-key-0001",
            createdAt: appliedAt.addingTimeInterval(-30)
        )
        let first = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )

        try first.persistPendingProposalApplicationMutation(pendingApply)
        try require(
            first.persistenceError == nil,
            "Persistence-before-network reported a local failure"
        )

        let afterApplyPreparation = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            afterApplyPreparation.pendingProposalApplicationMutation == pendingApply,
            "Relaunch did not recover the exact apply body, revisions, command IDs, and retry key"
        )
        let appliedReceipt = makeAppliedReceipt(
            proposalID: proposalID,
            commandID: commandID,
            appliedAt: appliedAt
        )
        try afterApplyPreparation.commitPendingProposalApplicationMutation(
            pendingApply,
            receipt: appliedReceipt
        )

        let afterApplyCommit = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            afterApplyCommit.pendingProposalApplicationMutation == nil,
            "Committed apply journal remained pending after relaunch"
        )
        try require(
            afterApplyCommit.proposalApplicationReceipt(
                for: proposalID,
                configurationIdentifier: configurationIdentifier
            ) == appliedReceipt,
            "Applied receipt was not retained for proposal recovery"
        )

        let undoBody = try encode(DayWeaveProposalUndoRequest(
            expectedApplicationRevision: 1
        ))
        let pendingUndo = DayWeavePendingProposalApplicationMutation.undo(
            configurationIdentifier: configurationIdentifier,
            proposalIDs: [proposalID],
            proposalRevisions: [5],
            expectedCommandIDs: [commandID],
            applicationID: appliedReceipt.application.applicationID,
            expectedApplicationRevision: 1,
            requestBody: undoBody,
            idempotencyKey: "proposal-undo-retry-key-0001",
            createdAt: appliedAt.addingTimeInterval(60)
        )
        try afterApplyCommit.persistPendingProposalApplicationMutation(pendingUndo)

        let afterUndoPreparation = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            afterUndoPreparation.pendingProposalApplicationMutation == pendingUndo,
            "Relaunch did not recover the exact undo body and application revision"
        )
        let undoneReceipt = makeUndoneReceipt(
            from: appliedReceipt,
            undoneAt: appliedAt.addingTimeInterval(120)
        )
        try afterUndoPreparation.commitPendingProposalApplicationMutation(
            pendingUndo,
            receipt: undoneReceipt
        )

        let final = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            final.pendingProposalApplicationMutation == nil,
            "Committed undo journal remained pending after relaunch"
        )
        try require(
            final.proposalApplicationReceipt(
                applicationID: appliedReceipt.id,
                configurationIdentifier: configurationIdentifier
            ) == undoneReceipt,
            "Undo did not monotonically replace the applied receipt"
        )
    }

    static func definitiveNoEffectCanClearExactJournal() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let mutation = try makeApplyMutation(createdAt: Date(timeIntervalSince1970: 1_700_000_000))
        let store = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )
        try store.persistPendingProposalApplicationMutation(mutation)

        let recovered = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try recovered.clearPendingProposalApplicationMutation(mutation)
        let final = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            final.pendingProposalApplicationMutation == nil,
            "A definitively cleared exact request reappeared after relaunch"
        )
    }

    static func proposalJournalSharesCanonicalMutationFence() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let mutation = try makeApplyMutation(
            createdAt: Date(timeIntervalSince1970: 1_700_000_000)
        )
        let store = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )

        try require(store.beginCanonicalSync(), "Canonical mutation fence was unavailable")
        do {
            try store.persistPendingProposalApplicationMutation(mutation)
            throw ProposalApplicationJournalScenarioFailure(
                description: "Proposal apply overlapped an active canonical mutation"
            )
        } catch PlannerProposalApplicationJournalError.remoteCanonicalMutationInProgress {
            // Expected: the exact proposal request was never staged or sent.
        }
        store.endCanonicalSync()

        try store.persistPendingProposalApplicationMutation(mutation)
        try require(
            !store.beginCanonicalSync(),
            "Canonical sync started while an exact proposal mutation remained pending"
        )
    }

    static func ambiguousApplyCanResolveToAlreadyUndoneReceipt() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proposalID = UUID()
        let commandID = UUID()
        let appliedAt = Date(timeIntervalSince1970: 1_700_000_000)
        let mutation = DayWeavePendingProposalApplicationMutation.apply(
            configurationIdentifier: configurationIdentifier,
            proposalIDs: [proposalID],
            proposalRevisions: [4],
            expectedCommandIDs: [commandID],
            previewID: UUID(),
            expectedReviewHash: reviewHash,
            requestBody: try encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: reviewHash
            )),
            idempotencyKey: "proposal-apply-retry-key-0004",
            createdAt: appliedAt.addingTimeInterval(-30)
        )
        let store = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )
        try store.persistPendingProposalApplicationMutation(mutation)
        let applied = makeAppliedReceipt(
            proposalID: proposalID,
            commandID: commandID,
            appliedAt: appliedAt
        )
        let alreadyUndone = makeUndoneReceipt(
            from: applied,
            undoneAt: appliedAt.addingTimeInterval(60)
        )

        try store.commitPendingProposalApplicationMutation(
            mutation,
            receipt: alreadyUndone
        )
        try require(
            store.pendingProposalApplicationMutation == nil,
            "Resolved cross-device apply remained pending"
        )
        try require(
            store.proposalApplicationReceipt(
                applicationID: alreadyUndone.id,
                configurationIdentifier: configurationIdentifier
            ) == alreadyUndone,
            "Already-undone recovery receipt was not retained"
        )
    }

    static func mismatchedRequestBodyFailsBeforePersistence() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proposalID = UUID()
        let commandID = UUID()
        let mismatched = DayWeavePendingProposalApplicationMutation.apply(
            configurationIdentifier: configurationIdentifier,
            proposalIDs: [proposalID],
            proposalRevisions: [7],
            expectedCommandIDs: [commandID],
            previewID: UUID(),
            expectedReviewHash: reviewHash,
            requestBody: try encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: "sha256:\(String(repeating: "c", count: 64))"
            )),
            idempotencyKey: "proposal-apply-retry-key-0002",
            createdAt: Date(timeIntervalSince1970: 1_700_000_000)
        )
        let store = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )

        do {
            try store.persistPendingProposalApplicationMutation(mismatched)
            throw ProposalApplicationJournalScenarioFailure(
                description: "Mismatched apply bytes were persisted"
            )
        } catch let error as PlannerProposalApplicationJournalError {
            try require(error == .invalidMutation, "Unexpected journal validation error: \(error)")
        }
        try require(
            store.pendingProposalApplicationMutation == nil,
            "Invalid request became live pending state"
        )
        try require(
            !FileManager.default.fileExists(atPath: context.fileURL.path),
            "Invalid request reached encrypted persistence"
        )
    }

    static func receiptRetentionIsBounded() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = Date(timeIntervalSince1970: 1_700_000_000)
        let original = (0..<PlannerProposalApplicationJournalValidator.maximumStoredReceipts)
            .map { offset in
                makeAppliedReceipt(
                    proposalID: UUID(),
                    commandID: UUID(),
                    applicationID: UUID(),
                    appliedAt: base.addingTimeInterval(TimeInterval(offset))
                )
            }
        let oldestID = try requireValue(original.first?.id, "Missing oldest receipt")
        let store = PlannerStore(
            canonicalConfigurationIdentifier: configurationIdentifier,
            proposalApplicationReceipts: original,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )
        store.flushPersistence()
        let newest = makeAppliedReceipt(
            proposalID: UUID(),
            commandID: UUID(),
            applicationID: UUID(),
            appliedAt: base.addingTimeInterval(10_000)
        )

        try store.recordProposalApplicationReceipt(newest)
        try require(
            store.proposalApplicationReceipts.count
                == PlannerProposalApplicationJournalValidator.maximumStoredReceipts,
            "Receipt history exceeded its encrypted retention bound"
        )
        try require(
            store.proposalApplicationReceipts.first == newest,
            "Newest receipt was not retained first"
        )
        try require(
            !store.proposalApplicationReceipts.contains { $0.id == oldestID },
            "Oldest receipt was not pruned deterministically"
        )

        let restored = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            restored.proposalApplicationReceipts == store.proposalApplicationReceipts,
            "Bounded receipt order changed after relaunch"
        )
    }

    private static func makeApplyMutation(
        createdAt: Date
    ) throws -> DayWeavePendingProposalApplicationMutation {
        DayWeavePendingProposalApplicationMutation.apply(
            configurationIdentifier: configurationIdentifier,
            proposalIDs: [UUID()],
            proposalRevisions: [3],
            expectedCommandIDs: [UUID()],
            previewID: UUID(),
            expectedReviewHash: reviewHash,
            requestBody: try encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: reviewHash
            )),
            idempotencyKey: "proposal-apply-retry-key-0003",
            createdAt: createdAt
        )
    }

    private static func makeAppliedReceipt(
        proposalID: UUID,
        commandID: UUID,
        applicationID: UUID = UUID(uuidString: "a4000000-0000-4000-8000-000000000004")!,
        appliedAt: Date
    ) -> DayWeaveStoredProposalApplicationReceipt {
        DayWeaveStoredProposalApplicationReceipt(
            configurationIdentifier: configurationIdentifier,
            application: DayWeaveProposalApplicationReceipt(
                applicationID: applicationID,
                proposals: [DayWeaveProposalAppliedMember(
                    proposalID: proposalID,
                    appliedRevision: 5
                )],
                applicationRevision: 1,
                status: .applied,
                commandIDs: [commandID],
                affectedItemIDs: [UUID()],
                appliedAt: appliedAt,
                undoExpiresAt: appliedAt.addingTimeInterval(86_400),
                undoneAt: nil
            )
        )
    }

    private static func makeUndoneReceipt(
        from applied: DayWeaveStoredProposalApplicationReceipt,
        undoneAt: Date
    ) -> DayWeaveStoredProposalApplicationReceipt {
        let application = applied.application
        return DayWeaveStoredProposalApplicationReceipt(
            configurationIdentifier: applied.configurationIdentifier,
            application: DayWeaveProposalApplicationReceipt(
                applicationID: application.applicationID,
                proposals: application.proposals,
                applicationRevision: 2,
                status: .undone,
                commandIDs: application.commandIDs,
                affectedItemIDs: application.affectedItemIDs,
                appliedAt: application.appliedAt,
                undoExpiresAt: application.undoExpiresAt,
                undoneAt: undoneAt
            )
        )
    }

    private static func encode<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(value)
    }

    private static func makeContext() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeaveProposalJournalTests-\(UUID().uuidString)", isDirectory: true)
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data(repeating: 11, count: 32))
        return (
            directory,
            fileURL,
            EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        )
    }

    private static func require(
        _ condition: @autoclosure () -> Bool,
        _ message: String
    ) throws {
        guard condition() else {
            throw ProposalApplicationJournalScenarioFailure(description: message)
        }
    }

    private static func requireValue<T>(_ value: T?, _ message: String) throws -> T {
        guard let value else {
            throw ProposalApplicationJournalScenarioFailure(description: message)
        }
        return value
    }
}

#if canImport(XCTest)
@MainActor
final class ProposalApplicationJournalTests: XCTestCase {
    func testExactApplyAndUndoSurviveRelaunch() throws {
        try ProposalApplicationJournalScenarios.exactApplyAndUndoSurviveRelaunch()
    }

    func testDefinitiveNoEffectCanClearExactJournal() throws {
        try ProposalApplicationJournalScenarios.definitiveNoEffectCanClearExactJournal()
    }

    func testAmbiguousApplyCanResolveToAlreadyUndoneReceipt() throws {
        try ProposalApplicationJournalScenarios.ambiguousApplyCanResolveToAlreadyUndoneReceipt()
    }

    func testMismatchedRequestBodyFailsBeforePersistence() throws {
        try ProposalApplicationJournalScenarios.mismatchedRequestBodyFailsBeforePersistence()
    }


    func testProposalJournalSharesCanonicalMutationFence() throws {
        try ProposalApplicationJournalScenarios.proposalJournalSharesCanonicalMutationFence()
    }

    func testReceiptRetentionIsBounded() throws {
        try ProposalApplicationJournalScenarios.receiptRetentionIsBounded()
    }
}
#elseif canImport(Testing)
@Suite("Transactional proposal application journal")
@MainActor
struct ProposalApplicationJournalTests {
    @Test("Exact apply and undo requests survive relaunch before transport")
    func exactApplyAndUndoSurviveRelaunch() throws {
        try ProposalApplicationJournalScenarios.exactApplyAndUndoSurviveRelaunch()
    }

    @Test("Authoritative no-effect recovery clears only the exact journal")
    func definitiveNoEffectCanClearExactJournal() throws {
        try ProposalApplicationJournalScenarios.definitiveNoEffectCanClearExactJournal()
    }

    @Test("Ambiguous apply can resolve to an already-undone cross-device receipt")
    func ambiguousApplyCanResolveToAlreadyUndoneReceipt() throws {
        try ProposalApplicationJournalScenarios.ambiguousApplyCanResolveToAlreadyUndoneReceipt()
    }

    @Test("Mismatched request bytes fail before encrypted persistence")
    func mismatchedRequestBodyFailsBeforePersistence() throws {
        try ProposalApplicationJournalScenarios.mismatchedRequestBodyFailsBeforePersistence()
    }


    @Test("Proposal application and canonical writes share one exclusion fence")
    func proposalJournalSharesCanonicalMutationFence() throws {
        try ProposalApplicationJournalScenarios.proposalJournalSharesCanonicalMutationFence()
    }

    @Test("Application receipts remain deterministically bounded")
    func receiptRetentionIsBounded() throws {
        try ProposalApplicationJournalScenarios.receiptRetentionIsBounded()
    }
}
#endif
