import Foundation

@MainActor
protocol ProposalApplicationJournaling: AnyObject {
    var pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? { get }
    var proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt] { get }

    func persistPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws
    func commitPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation,
        receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws
    func clearPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws
    func recordProposalApplicationReceipt(
        _ receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws
    func proposalApplicationReceipt(
        applicationID: UUID,
        configurationIdentifier: String
    ) -> DayWeaveStoredProposalApplicationReceipt?
}

extension PlannerStore: ProposalApplicationJournaling {}

enum ProposalApplicationWorkflowStatus: Equatable, Sendable {
    case idle
    case previewing(UUID)
    case ready(UUID, canApply: Bool)
    case applying(UUID)
    case undoing(UUID)
    case recovering
    case completed(String)
    case failed(String)

    var message: String {
        switch self {
        case .idle:
            "Review a typed proposal before it can change canonical items."
        case .previewing:
            "Simulating the exact proposal without changing canonical items…"
        case let .ready(_, canApply):
            canApply
                ? "Review complete. Explicit confirmation is required to apply these changes."
                : "Review complete. Conflicts block this proposal until it is refreshed or revised."
        case .applying:
            "Applying the exact reviewed changes atomically…"
        case .undoing:
            "Undoing the application atomically…"
        case .recovering:
            "Recovering the outcome of an interrupted proposal operation…"
        case let .completed(message), let .failed(message):
            message
        }
    }

    var isWorking: Bool {
        switch self {
        case .previewing, .applying, .undoing, .recovering: true
        case .idle, .ready, .completed, .failed: false
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

private enum ProposalApplicationWorkflowError: LocalizedError {
    case anotherOperationPending
    case confirmationRequired
    case expiredProposal
    case invalidPreview
    case previewExpired
    case previewBlocked
    case invalidReceipt
    case privacyBoundary
    case unsupportedProposal
    case newerProtectedSchema

    var errorDescription: String? {
        switch self {
        case .anotherOperationPending:
            "Recover the pending proposal application before starting another one."
        case .confirmationRequired:
            "Confirm the reviewed changes before applying them."
        case .expiredProposal:
            "This proposal has expired. Refresh the Suggestions Inbox."
        case .invalidPreview:
            "The server returned a review that did not exactly match this proposal. Nothing was applied."
        case .previewExpired:
            "This review expired. Generate a fresh review before applying it."
        case .previewBlocked:
            "The reviewed changes contain conflicts and cannot be applied."
        case .invalidReceipt:
            "The server returned a proposal application receipt that did not match the approved operation. Recovery remains available."
        case .privacyBoundary:
            "The review was discarded when DayWeave became private or inactive. Open it again to continue."
        case .unsupportedProposal:
            "This proposal is advisory and does not contain an executable typed change set."
        case .newerProtectedSchema:
            "This proposal uses a newer protected change-set format. Update DayWeave before reviewing it."
        }
    }
}

@MainActor
final class ProposalApplicationStore: ObservableObject {
    @Published private(set) var status: ProposalApplicationWorkflowStatus = .idle
    @Published private(set) var activeProposalID: UUID?
    @Published private(set) var activeApplicationID: UUID?

    private let suggestions: SuggestionSyncStore
    private let journal: any ProposalApplicationJournaling
    private let now: @Sendable () -> Date
    private struct BoundPreview {
        let configurationIdentifier: String
        let review: DayWeaveProposalApplicationPreview
    }
    private var previews: [UUID: BoundPreview] = [:]
    private var privacyGeneration: UInt64 = 0

    init(
        suggestions: SuggestionSyncStore,
        journal: any ProposalApplicationJournaling,
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.suggestions = suggestions
        self.journal = journal
        self.now = now
        if journal.pendingProposalApplicationMutation != nil {
            status = .completed("An interrupted proposal operation is ready for safe recovery.")
        }
        suggestions.installApplicationConfigurationChangeHandler { [weak self] in
            self?.applicationConfigurationDidChange()
        }
    }

    var hasPendingRecovery: Bool {
        journal.pendingProposalApplicationMutation != nil
    }

    var recentReceipts: [DayWeaveStoredProposalApplicationReceipt] {
        guard let identifier = suggestions.currentApplicationConfigurationIdentifier else {
            return []
        }
        return journal.proposalApplicationReceipts
            .filter { $0.configurationIdentifier == identifier }
            .sorted { left, right in
                if left.application.appliedAt != right.application.appliedAt {
                    return left.application.appliedAt > right.application.appliedAt
                }
                return left.application.applicationID.uuidString
                    > right.application.applicationID.uuidString
            }
    }

    func preview(for proposal: DayWeaveProposal) -> DayWeaveProposalApplicationPreview? {
        guard let currentIdentifier = suggestions.currentApplicationConfigurationIdentifier else {
            return nil
        }
        return previews[proposal.id].flatMap { bound in
            bound.configurationIdentifier == currentIdentifier
                && bound.review.matches(proposal) ? bound.review : nil
        }
    }

    func approval(for proposal: DayWeaveProposal) -> DayWeaveProposalReviewApproval? {
        preview(for: proposal).map { review in
            Self.approval(for: review, proposal: proposal)
        }
    }

    func prepareReview(for proposal: DayWeaveProposal) async {
        guard !status.isWorking else { return }
        do {
            try validateProposalForReview(proposal)
            guard journal.pendingProposalApplicationMutation == nil else {
                throw ProposalApplicationWorkflowError.anotherOperationPending
            }
            guard let client = suggestions.beginApplicationOperation(for: proposal) else { return }
            let generation = privacyGeneration
            activeProposalID = proposal.id
            status = .previewing(proposal.id)
            defer {
                suggestions.finishApplicationOperation(proposalIDs: [proposal.id])
                activeProposalID = nil
            }

            let review = try await client.previewSuggestionApplication(.init(proposals: [
                .init(proposalID: proposal.id, expectedRevision: proposal.revision),
            ]))
            guard generation == privacyGeneration else {
                throw ProposalApplicationWorkflowError.privacyBoundary
            }
            try validate(review: review, for: proposal, at: now())
            previews = [proposal.id: BoundPreview(
                configurationIdentifier: client.configurationIdentifier,
                review: review
            )]
            status = .ready(proposal.id, canApply: review.canApply)
        } catch {
            previews.removeValue(forKey: proposal.id)
            status = .failed(error.localizedDescription)
        }
    }

    @discardableResult
    func apply(
        _ proposal: DayWeaveProposal,
        approval: DayWeaveProposalReviewApproval?
    ) async -> Bool {
        guard !status.isWorking else { return false }
        do {
            try validateProposalForReview(proposal)
            guard journal.pendingProposalApplicationMutation == nil else {
                throw ProposalApplicationWorkflowError.anotherOperationPending
            }
            guard let review = preview(for: proposal) else {
                throw ProposalApplicationWorkflowError.invalidPreview
            }
            guard approval == Self.approval(for: review, proposal: proposal) else {
                throw ProposalApplicationWorkflowError.confirmationRequired
            }
            try validate(review: review, for: proposal, at: now())
            guard review.canApply else {
                throw ProposalApplicationWorkflowError.previewBlocked
            }
            guard let client = suggestions.beginApplicationOperation(for: proposal) else {
                return false
            }
            activeProposalID = proposal.id
            status = .applying(proposal.id)
            defer {
                suggestions.finishApplicationOperation(proposalIDs: [proposal.id])
                activeProposalID = nil
            }

            let requestBody = try client.prepareSuggestionApplicationApplyBody(
                expectedReviewHash: review.reviewHash
            )
            let mutation = DayWeavePendingProposalApplicationMutation.apply(
                configurationIdentifier: client.configurationIdentifier,
                proposalIDs: [proposal.id],
                proposalRevisions: [proposal.revision],
                expectedCommandIDs: review.commandIDs,
                previewID: review.previewID,
                expectedReviewHash: review.reviewHash,
                requestBody: requestBody,
                idempotencyKey: "macos-apply-\(UUID().uuidString.lowercased())",
                createdAt: now()
            )
            guard mutation.hasValidShape else {
                throw DayWeaveAPIError.requestEncodingFailed
            }
            try journal.persistPendingProposalApplicationMutation(mutation)

            do {
                let response = try await client.applySuggestionApplication(
                    previewID: review.previewID,
                    expectedReviewHash: review.reviewHash,
                    requestBody: requestBody,
                    idempotencyKey: mutation.idempotencyKey
                )
                try validateAppliedReceipt(
                    response.application,
                    for: mutation,
                    allowAlreadyUndone: false
                )
                return try finish(
                    mutation: mutation,
                    receipt: response.application,
                    message: "Proposal applied; refreshing canonical items and schedule"
                )
            } catch {
                return await recoverApply(
                    mutation,
                    using: client
                )
            }
        } catch {
            if Self.invalidatesCachedReview(error) {
                previews.removeValue(forKey: proposal.id)
            }
            status = .failed(error.localizedDescription)
            return false
        }
    }

    @discardableResult
    func undo(_ storedReceipt: DayWeaveStoredProposalApplicationReceipt) async -> Bool {
        guard !status.isWorking else { return false }
        do {
            guard journal.pendingProposalApplicationMutation == nil else {
                throw ProposalApplicationWorkflowError.anotherOperationPending
            }
            let receipt = storedReceipt.application
            guard storedReceipt.hasValidShape,
                  receipt.status == .applied,
                  receipt.applicationRevision == 1,
                  now() < receipt.undoExpiresAt else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
            let proposalIDs = receipt.proposals.map(\.proposalID)
            guard let client = suggestions.beginApplicationRecovery(
                proposalIDs: proposalIDs,
                configurationIdentifier: storedReceipt.configurationIdentifier
            ) else {
                return false
            }
            activeApplicationID = receipt.applicationID
            status = .undoing(receipt.applicationID)
            defer {
                suggestions.finishApplicationOperation(proposalIDs: proposalIDs)
                activeApplicationID = nil
            }

            let requestBody = try client.prepareSuggestionApplicationUndoBody(
                expectedApplicationRevision: receipt.applicationRevision
            )
            let mutation = DayWeavePendingProposalApplicationMutation.undo(
                configurationIdentifier: storedReceipt.configurationIdentifier,
                proposalIDs: proposalIDs,
                proposalRevisions: receipt.proposals.map(\.appliedRevision),
                expectedCommandIDs: receipt.commandIDs,
                applicationID: receipt.applicationID,
                expectedApplicationRevision: receipt.applicationRevision,
                requestBody: requestBody,
                idempotencyKey: "macos-undo-\(UUID().uuidString.lowercased())",
                createdAt: now()
            )
            guard mutation.hasValidShape else {
                throw DayWeaveAPIError.requestEncodingFailed
            }
            try journal.persistPendingProposalApplicationMutation(mutation)

            do {
                let response = try await client.undoSuggestionApplication(
                    applicationID: receipt.applicationID,
                    expectedApplicationRevision: receipt.applicationRevision,
                    requestBody: requestBody,
                    idempotencyKey: mutation.idempotencyKey
                )
                try validateUndoneReceipt(response.application, previous: receipt, mutation: mutation)
                return try finish(
                    mutation: mutation,
                    receipt: response.application,
                    message: "Proposal application undone; refreshing canonical items and schedule"
                )
            } catch {
                return await recoverUndo(
                    mutation,
                    previous: receipt,
                    using: client
                )
            }
        } catch {
            status = .failed(error.localizedDescription)
            return false
        }
    }

    @discardableResult
    func recoverPendingMutation() async -> Bool {
        guard !status.isWorking,
              let mutation = journal.pendingProposalApplicationMutation else {
            return false
        }
        guard mutation.hasValidShape else {
            status = .failed("The encrypted proposal recovery record is invalid. No network request was made.")
            return false
        }
        guard let client = suggestions.beginApplicationRecovery(
            proposalIDs: mutation.proposalIDs,
            configurationIdentifier: mutation.configurationIdentifier
        ) else {
            return false
        }
        status = .recovering
        activeProposalID = mutation.proposalIDs.count == 1 ? mutation.proposalIDs[0] : nil
        activeApplicationID = mutation.applicationID
        defer {
            suggestions.finishApplicationOperation(proposalIDs: mutation.proposalIDs)
            activeProposalID = nil
            activeApplicationID = nil
        }

        switch mutation.operation {
        case .apply:
            return await recoverApply(mutation, using: client)
        case .undo:
            guard let applicationID = mutation.applicationID,
                  let previous = journal.proposalApplicationReceipt(
                      applicationID: applicationID,
                      configurationIdentifier: mutation.configurationIdentifier
                  )?.application else {
                status = .failed("The retained applied receipt required to recover this undo is unavailable.")
                return false
            }
            return await recoverUndo(
                mutation,
                previous: previous,
                using: client
            )
        }
    }

    func discardPreview(for proposalID: UUID) {
        previews.removeValue(forKey: proposalID)
        if case .ready(proposalID, _) = status {
            status = .idle
        }
    }

    func suspendForPrivacyBoundary() {
        privacyGeneration &+= 1
        previews.removeAll(keepingCapacity: false)
        switch status {
        case .ready, .previewing:
            status = .idle
        case .idle, .applying, .undoing, .recovering, .completed, .failed:
            break
        }
    }

    private func recoverApply(
        _ mutation: DayWeavePendingProposalApplicationMutation,
        using client: DayWeaveAPIClient
    ) async -> Bool {
        status = .recovering
        var lookupWasNotFound = false
        do {
            guard let proposalID = mutation.proposalIDs.first else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
            let receipt = try await client.suggestionApplication(forProposalID: proposalID)
            try validateAppliedReceipt(receipt, for: mutation, allowAlreadyUndone: true)
            return try finish(
                mutation: mutation,
                receipt: receipt,
                message: receipt.status == .undone
                    ? "Recovered an application that has already been undone; refreshing canonical state"
                    : "Recovered the applied proposal; refreshing canonical items and schedule"
            )
        } catch {
            lookupWasNotFound = Self.isTrustedApplicationAbsent(error)
        }

        do {
            guard let previewID = mutation.previewID,
                  let reviewHash = mutation.expectedReviewHash else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
            let response = try await client.applySuggestionApplication(
                previewID: previewID,
                expectedReviewHash: reviewHash,
                requestBody: mutation.requestBody,
                idempotencyKey: mutation.idempotencyKey
            )
            try validateAppliedReceipt(
                response.application,
                for: mutation,
                allowAlreadyUndone: false
            )
            return try finish(
                mutation: mutation,
                receipt: response.application,
                message: "Proposal applied safely after recovery; refreshing canonical state"
            )
        } catch {
            if lookupWasNotFound,
               Self.isTrustedNoMutation(error) {
                do {
                    try journal.clearPendingProposalApplicationMutation(mutation)
                } catch {
                    status = .failed(error.localizedDescription)
                    return false
                }
                status = .failed(
                    "The server definitively rejected the reviewed apply request. No proposal changes were committed; generate a fresh review."
                )
                for proposalID in mutation.proposalIDs {
                    previews.removeValue(forKey: proposalID)
                }
                return false
            }
            status = .failed(
                "The apply outcome is still unresolved. DayWeave retained the exact encrypted request and will recover it without creating a duplicate. \(error.localizedDescription)"
            )
            return false
        }
    }

    private func recoverUndo(
        _ mutation: DayWeavePendingProposalApplicationMutation,
        previous: DayWeaveProposalApplicationReceipt,
        using client: DayWeaveAPIClient
    ) async -> Bool {
        status = .recovering
        var lookupWasNotFound = false
        do {
            guard let applicationID = mutation.applicationID else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
            let receipt = try await client.suggestionApplication(applicationID: applicationID)
            if receipt.status == .undone {
                try validateUndoneReceipt(receipt, previous: previous, mutation: mutation)
                return try finish(
                    mutation: mutation,
                    receipt: receipt,
                    message: "Recovered the completed undo; refreshing canonical state"
                )
            }
            guard receipt == previous else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
        } catch {
            lookupWasNotFound = Self.isTrustedApplicationAbsent(error)
        }

        do {
            guard let applicationID = mutation.applicationID,
                  let expectedRevision = mutation.expectedApplicationRevision else {
                throw ProposalApplicationWorkflowError.invalidReceipt
            }
            let response = try await client.undoSuggestionApplication(
                applicationID: applicationID,
                expectedApplicationRevision: expectedRevision,
                requestBody: mutation.requestBody,
                idempotencyKey: mutation.idempotencyKey
            )
            try validateUndoneReceipt(
                response.application,
                previous: previous,
                mutation: mutation
            )
            return try finish(
                mutation: mutation,
                receipt: response.application,
                message: "Proposal application undone safely; refreshing canonical state"
            )
        } catch {
            if lookupWasNotFound,
               Self.isTrustedNoMutation(error) {
                do {
                    try journal.clearPendingProposalApplicationMutation(mutation)
                } catch {
                    status = .failed(error.localizedDescription)
                    return false
                }
                status = .failed(
                    "The server definitively rejected the undo request. The prior applied state was not changed by this request."
                )
                return false
            }
            status = .failed(
                "The undo outcome is still unresolved. DayWeave retained the exact encrypted request and will recover it without duplicating the operation. \(error.localizedDescription)"
            )
            return false
        }
    }

    private func finish(
        mutation: DayWeavePendingProposalApplicationMutation,
        receipt: DayWeaveProposalApplicationReceipt,
        message: String
    ) throws -> Bool {
        let stored = DayWeaveStoredProposalApplicationReceipt(
            configurationIdentifier: mutation.configurationIdentifier,
            application: receipt
        )
        guard stored.hasValidShape else {
            throw ProposalApplicationWorkflowError.invalidReceipt
        }
        try journal.commitPendingProposalApplicationMutation(
            mutation,
            receipt: stored
        )
        for proposalID in mutation.proposalIDs {
            previews.removeValue(forKey: proposalID)
        }
        suggestions.applicationDidCommit(
            proposalIDs: mutation.proposalIDs,
            configurationIdentifier: mutation.configurationIdentifier,
            message: message
        )
        status = .completed(message)
        return true
    }

    private func validateProposalForReview(_ proposal: DayWeaveProposal) throws {
        if proposal.advertisesApplicationReadyChangeSet {
            guard proposal.status == .pending else {
                throw ProposalApplicationWorkflowError.invalidPreview
            }
            guard proposal.expiresAt > now() else {
                throw ProposalApplicationWorkflowError.expiredProposal
            }
            return
        }
        if proposal.advertisesReservedChangeSetSchema {
            throw ProposalApplicationWorkflowError.newerProtectedSchema
        }
        throw ProposalApplicationWorkflowError.unsupportedProposal
    }

    private func validate(
        review: DayWeaveProposalApplicationPreview,
        for proposal: DayWeaveProposal,
        at date: Date
    ) throws {
        guard review.hasSupportedContract,
              review.matches(proposal),
              review.expiresAt > date,
              review.expiresAt <= proposal.expiresAt,
              Self.hasValidReviewContents(review) else {
            throw review.expiresAt <= date
                ? ProposalApplicationWorkflowError.previewExpired
                : ProposalApplicationWorkflowError.invalidPreview
        }
    }

    private func validateAppliedReceipt(
        _ receipt: DayWeaveProposalApplicationReceipt,
        for mutation: DayWeavePendingProposalApplicationMutation,
        allowAlreadyUndone: Bool
    ) throws {
        guard mutation.operation == .apply,
              receipt.hasValidShape,
              receipt.commandIDs == mutation.expectedCommandIDs,
              receipt.proposals.map(\.proposalID) == mutation.proposalIDs,
              receipt.proposals.count == mutation.proposalRevisions.count,
              zip(receipt.proposals, mutation.proposalRevisions).allSatisfy({ member, revision in
                  let incremented = revision.addingReportingOverflow(1)
                  return !incremented.overflow && member.appliedRevision == incremented.partialValue
              }),
              receipt.status == .applied || allowAlreadyUndone else {
            throw ProposalApplicationWorkflowError.invalidReceipt
        }
    }

    private func validateUndoneReceipt(
        _ receipt: DayWeaveProposalApplicationReceipt,
        previous: DayWeaveProposalApplicationReceipt,
        mutation: DayWeavePendingProposalApplicationMutation
    ) throws {
        let nextRevision = previous.applicationRevision.addingReportingOverflow(1)
        guard mutation.operation == .undo,
              receipt.hasValidShape,
              !nextRevision.overflow,
              receipt.applicationID == previous.applicationID,
              receipt.applicationRevision == nextRevision.partialValue,
              receipt.status == .undone,
              receipt.proposals == previous.proposals,
              receipt.proposals.map(\.proposalID) == mutation.proposalIDs,
              receipt.proposals.map(\.appliedRevision) == mutation.proposalRevisions,
              receipt.commandIDs == previous.commandIDs,
              receipt.commandIDs == mutation.expectedCommandIDs,
              receipt.affectedItemIDs == previous.affectedItemIDs,
              receipt.appliedAt == previous.appliedAt,
              receipt.undoExpiresAt == previous.undoExpiresAt,
              receipt.undoneAt != nil else {
            throw ProposalApplicationWorkflowError.invalidReceipt
        }
    }

    private static func hasValidReviewContents(
        _ review: DayWeaveProposalApplicationPreview
    ) -> Bool {
        let commandIDs = Set(review.commandIDs)
        let directItemIDs = review.diffs.map(\.itemID)
        let implicitItemIDs = review.implicitDiffs.map(\.itemID)
        let knownOperations = Set(["create_item", "replace_item", "trash_item", "restore_item"])
        let knownFields = Set([
            "is_sensitive", "kind", "status", "title", "notes", "timezone_name",
            "duration_seconds", "deadline_at", "earliest_start_at", "recurrence",
            "flexible_constraints", "split_policy", "importance", "urgency", "parent_id",
            "sibling_order", "is_executable", "revision", "completed_at", "deleted_at",
        ])
        guard review.diffs.count <= 100,
              review.implicitDiffs.count <= 10_000,
              review.risks.count <= 1_000,
              review.conflicts.count <= 100,
              Set(review.risks.map(\.id)).count == review.risks.count,
              Set(review.conflicts.map(\.id)).count == review.conflicts.count,
              Set(directItemIDs).count == directItemIDs.count,
              Set(implicitItemIDs).isDisjoint(with: directItemIDs),
              review.diffs.allSatisfy({ diff in
                  commandIDs.contains(diff.commandID)
                      && knownOperations.contains(diff.operation)
                      && (1...20).contains(diff.changedFields.count)
                      && Set(diff.changedFields).count == diff.changedFields.count
                      && Set(diff.changedFields).isSubset(of: knownFields)
                      && (diff.before?.id == nil || diff.before?.id == diff.itemID)
                      && (diff.after?.id == nil || diff.after?.id == diff.itemID)
                      && (diff.before != nil || diff.after != nil)
                      && Self.hasValidDirectDiffShape(diff)
                      && diff.before.map(Self.hasSupportedReviewItem) ?? true
                      && diff.after.map(Self.hasSupportedReviewItem) ?? true
                      && Set(diff.changedFields)
                          == Self.changedProposalFields(before: diff.before, after: diff.after)
              }), review.implicitDiffs.allSatisfy({ diff in
                  diff.reason == "hierarchy_refresh"
                      && diff.before.id == diff.itemID
                      && diff.after.id == diff.itemID
                      && (1...20).contains(diff.changedFields.count)
                      && Set(diff.changedFields).count == diff.changedFields.count
                      && Set(diff.changedFields).isSubset(of: knownFields)
                      && Self.hasSupportedReviewItem(diff.before)
                      && Self.hasSupportedReviewItem(diff.after)
                      && Set(diff.changedFields)
                          == Self.changedProposalFields(before: diff.before, after: diff.after)
              }) else {
            return false
        }

        let riskLevels = ["low": 0, "medium": 1, "high": 2, "critical": 3]
        let knownRiskCodes = Set([
            "creates_item", "replaces_item", "trashes_item", "restores_item",
            "changes_deadline", "relaxes_deadline", "changes_hierarchy",
            "changes_sensitivity", "changes_recurrence", "changes_execution_state",
            "sensitive_content", "bulk_change",
        ])
        guard review.risks.allSatisfy({ risk in
                  knownRiskCodes.contains(risk.code)
                      && riskLevels[risk.level] != nil
                      && risk.commandID.map(commandIDs.contains) ?? true
                      && Self.hasBoundedSummary(risk.summary)
              }) else {
            return false
        }
        let actualMaximum = review.risks.compactMap { riskLevels[$0.level] }.max() ?? 0
        guard riskLevels[review.maximumRisk] == actualMaximum,
              review.requiresExplicitApproval
                  == review.risks.contains(where: \.requiresExplicitApproval) else {
            return false
        }

        let knownConflictCodes = Set([
            "proposal_not_pending", "proposal_expired", "proposal_revision_mismatch",
            "item_already_exists", "item_not_found", "item_revision_mismatch",
            "parent_not_found", "hierarchy_cycle", "invalid_parent_state",
            "non_leaf_executable", "has_children", "deleted_parent", "invalid_item",
            "provider_managed_item", "preview_expired", "preview_mismatch",
            "preview_not_applicable", "already_applied", "undo_expired", "undo_diverged",
        ])
        return review.conflicts.allSatisfy { conflict in
            knownConflictCodes.contains(conflict.code)
                && conflict.commandID.map(commandIDs.contains) ?? true
                && Self.hasBoundedSummary(conflict.summary)
        }
    }

    private static func hasBoundedSummary(_ value: String) -> Bool {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return !trimmed.isEmpty && trimmed.unicodeScalars.count <= 1_000
    }

    private static func hasValidDirectDiffShape(_ diff: DayWeaveProposalItemDiff) -> Bool {
        switch diff.operation {
        case "create_item":
            diff.before == nil && diff.after != nil
        case "replace_item", "trash_item", "restore_item":
            diff.before != nil && diff.after != nil
        default:
            false
        }
    }

    private static func hasSupportedReviewItem(_ item: DayWeaveCanonicalItem) -> Bool {
        guard item.unsupportedFields.isEmpty else { return false }
        if case .unknown = item.kind { return false }
        if case .unknown = item.status { return false }
        if case .unknown = item.splitPolicy { return false }
        return true
    }

    private static func changedProposalFields(
        before: DayWeaveCanonicalItem?,
        after: DayWeaveCanonicalItem?
    ) -> Set<String> {
        let everyField = Set([
            "is_sensitive", "kind", "status", "title", "notes", "timezone_name",
            "duration_seconds", "deadline_at", "earliest_start_at", "recurrence",
            "flexible_constraints", "split_policy", "importance", "urgency", "parent_id",
            "sibling_order", "is_executable", "revision", "completed_at", "deleted_at",
        ])
        guard let before else { return after == nil ? [] : everyField }
        guard let after else { return ["deleted_at"] }
        var changed = Set<String>()
        if before.isSensitive != after.isSensitive { changed.insert("is_sensitive") }
        if before.kind != after.kind { changed.insert("kind") }
        if before.status != after.status { changed.insert("status") }
        if before.title != after.title { changed.insert("title") }
        if before.notes != after.notes { changed.insert("notes") }
        if before.timezoneName != after.timezoneName { changed.insert("timezone_name") }
        if before.durationSeconds != after.durationSeconds { changed.insert("duration_seconds") }
        if before.deadlineAt != after.deadlineAt { changed.insert("deadline_at") }
        if before.earliestStartAt != after.earliestStartAt {
            changed.insert("earliest_start_at")
        }
        if before.recurrence != after.recurrence { changed.insert("recurrence") }
        if before.flexibleConstraints != after.flexibleConstraints {
            changed.insert("flexible_constraints")
        }
        if before.splitPolicy != after.splitPolicy { changed.insert("split_policy") }
        if before.importance != after.importance { changed.insert("importance") }
        if before.urgency != after.urgency { changed.insert("urgency") }
        if before.parentID != after.parentID { changed.insert("parent_id") }
        if before.siblingOrder != after.siblingOrder { changed.insert("sibling_order") }
        if before.isExecutable != after.isExecutable { changed.insert("is_executable") }
        if before.revision != after.revision { changed.insert("revision") }
        if before.completedAt != after.completedAt { changed.insert("completed_at") }
        if before.deletedAt != after.deletedAt { changed.insert("deleted_at") }
        return changed
    }

    private static func approval(
        for review: DayWeaveProposalApplicationPreview,
        proposal: DayWeaveProposal
    ) -> DayWeaveProposalReviewApproval {
        DayWeaveProposalReviewApproval(
            proposalID: proposal.id,
            proposalRevision: proposal.revision,
            previewID: review.previewID,
            reviewHash: review.reviewHash
        )
    }

    private func applicationConfigurationDidChange() {
        previews.removeAll(keepingCapacity: false)
        status = journal.pendingProposalApplicationMutation == nil
            ? .idle
            : .completed("An interrupted proposal operation is ready for safe recovery.")
    }

    private static func invalidatesCachedReview(_ error: Error) -> Bool {
        guard let error = error as? ProposalApplicationWorkflowError else { return false }
        return switch error {
        case .expiredProposal, .invalidPreview, .previewExpired:
            true
        case .anotherOperationPending, .confirmationRequired, .previewBlocked,
             .invalidReceipt, .privacyBoundary, .unsupportedProposal, .newerProtectedSchema:
            false
        }
    }

    private static func isTrustedApplicationAbsent(_ error: Error) -> Bool {
        if case DayWeaveAPIError.trustedProposalApplicationAbsent = error { return true }
        return false
    }

    private static func isTrustedNoMutation(_ error: Error) -> Bool {
        switch error {
        case DayWeaveAPIError.trustedProposalApplicationAbsent,
             DayWeaveAPIError.trustedProposalApplicationNoEffect:
            true
        default:
            false
        }
    }
}
