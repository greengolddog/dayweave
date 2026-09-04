import Foundation
import SwiftUI

enum PlannerLoadState: Equatable, Sendable {
    case ready
    case persistenceFailed
}

enum PlannerCanonicalConfigurationError: LocalizedError, Equatable, Sendable {
    case unboundExistingState
    case configurationMismatch

    var errorDescription: String? {
        switch self {
        case .unboundExistingState:
            "The encrypted canonical cache predates server binding. Reset that cache explicitly before syncing so it cannot be sent to the wrong server."
        case .configurationMismatch:
            "This encrypted canonical cache belongs to a different API configuration. Restore that configuration or explicitly reset the canonical cache."
        }
    }
}

enum PlannerExecutionStateError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case invalidDurableState
    case configurationMismatch
    case credentialReplacementBlocked
    case breakNotificationCancellationUnavailable

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Encrypted planner persistence is required before cross-device execution can run."
        case .invalidDurableState:
            "The encrypted execution recovery state is invalid; no server action was attempted."
        case .configurationMismatch:
            "Execution recovery state belongs to another API credential binding."
        case .credentialReplacementBlocked:
            "Reconcile the pending execution, canonical projection, or exact schedule publication before replacing credentials."
        case .breakNotificationCancellationUnavailable:
            "DayWeave could not verify removal of its break reminder. Your encrypted execution state and credentials were preserved; retry when Notification Center is available."
        }
    }
}

enum PlannerSchedulePublicationError: LocalizedError, Equatable, Sendable {
    case invalidJournal
    case publicationAlreadyPending
    case publicationDoesNotMatchJournal
    case replayedReceiptCannotAuthorize

    var errorDescription: String? {
        switch self {
        case .invalidJournal:
            "The exact schedule publication journal is invalid or exceeds its encrypted size limit."
        case .publicationAlreadyPending:
            "An earlier schedule publication has an ambiguous result. Restore its API configuration and sync to recover it exactly."
        case .publicationDoesNotMatchJournal:
            "The schedule publication response does not match the encrypted request awaiting recovery."
        case .replayedReceiptCannotAuthorize:
            "An idempotent publication replay cannot prove that its historical revision is still current. Compose and publish a fresh schedule."
        }
    }
}

enum PlannerGoogleSchedulePublicationRecoveryError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case invalidJournal
    case journalConflict
    case currentPublishedScheduleRequired

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Healthy encrypted planner persistence is required before publishing a schedule to Google."
        case .invalidJournal:
            "The encrypted generated-schedule Google publication recovery is invalid."
        case .journalConflict:
            "The generated-schedule Google publication recovery changed concurrently."
        case .currentPublishedScheduleRequired:
            "Publish and retain the current server-generated schedule before reviewing Google Calendar changes."
        }
    }
}

enum PlannerScheduleReplicaError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case mutationFenceUnavailable
    case publicationRecoveryPending
    case configurationMismatch
    case invalidPublication

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Healthy encrypted planner persistence is required before recovering a published schedule."
        case .mutationFenceUnavailable:
            "Another canonical or execution operation is active; the published schedule was not installed."
        case .publicationRecoveryPending:
            "Recover the exact pending schedule publication before replacing the local projection."
        case .configurationMismatch:
            "The published schedule belongs to another API credential binding."
        case .invalidPublication:
            "The current published schedule did not match the encrypted canonical cache."
        }
    }
}

enum PlannerLocalCompositionError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case mutationFenceUnavailable
    case invalidProvenance

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Healthy encrypted planner persistence is required before installing an on-device schedule."
        case .mutationFenceUnavailable:
            "Another canonical or execution operation is active; the on-device schedule was not installed."
        case .invalidProvenance:
            "The on-device schedule evidence is invalid; the prior plan was preserved."
        }
    }
}

enum PlannerScheduleProfileError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case invalidProfile
    case staleBaseline
    case mutationFenceActive
    case recoveryInProgress
    case activeExecution

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Healthy encrypted planner persistence is required before changing the schedule profile."
        case .invalidProfile:
            "The schedule profile is invalid or disagrees with its protected-time compatibility value."
        case .staleBaseline:
            "The schedule profile changed after this edit began. Reload it before saving."
        case .mutationFenceActive:
            "Wait for canonical synchronization or on-device composition to finish before changing the schedule profile."
        case .recoveryInProgress:
            "Recover pending schedule, proposal, Google Calendar, or status work before changing the schedule profile."
        case .activeExecution:
            "Finish the active cross-device execution before changing the schedule profile."
        }
    }
}

enum PlannerProposalApplicationJournalError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case invalidMutation
    case remoteCanonicalMutationInProgress
    case operationAlreadyPending
    case mutationDoesNotMatchJournal
    case invalidReceipt
    case receiptConflict

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Encrypted planner persistence is required before a proposal can be applied or undone."
        case .invalidMutation:
            "The exact proposal application request is invalid or belongs to another API configuration."
        case .remoteCanonicalMutationInProgress:
            "Wait for canonical synchronization or execution to finish before applying or undoing a proposal."
        case .operationAlreadyPending:
            "An earlier proposal application or undo has an ambiguous result and must be recovered first."
        case .mutationDoesNotMatchJournal:
            "The proposal application result does not match the exact encrypted request awaiting recovery."
        case .invalidReceipt:
            "The proposal application receipt is invalid or does not match the pending operation."
        case .receiptConflict:
            "The proposal application receipt conflicts with retained application history."
        }
    }
}

enum PlannerCanonicalAuthoringError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case mutationFenceActive
    case activeExecution
    case invalidDraft
    case itemNotFound
    case trashEntryNotFound
    case unsupportedReplacement
    case duplicateItemOperation
    case mutationNotFound
    case submittedMutationIsImmutable
    case invalidConfiguration
    case invalidMutation
    case journalCapacityReached
    case invalidRemoteResponse
    case suggestionNotFound
    case suggestionNotPending
    case suggestionExpired
    case suggestionIdentityMismatch

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Encrypted planner persistence is required before canonical items can be authored."
        case .mutationFenceActive:
            "Wait for the active canonical or proposal mutation to finish."
        case .activeExecution:
            "Finish or pause the active cross-device execution before changing canonical items."
        case .invalidDraft:
            "The item draft violates the canonical item contract or hierarchy."
        case .itemNotFound:
            "The canonical item is no longer available."
        case .trashEntryNotFound:
            "The deleted item is no longer available to restore."
        case .unsupportedReplacement:
            "This item contains fields that this version of DayWeave cannot replace safely."
        case .duplicateItemOperation:
            "Another authoring operation already targets this item."
        case .mutationNotFound:
            "The canonical authoring operation is no longer pending."
        case .submittedMutationIsImmutable:
            "A submitted authoring operation is immutable until it is reconciled or conflicted."
        case .invalidConfiguration:
            "The authoring operation belongs to another API configuration or credential binding."
        case .invalidMutation:
            "The encrypted canonical authoring journal is invalid."
        case .journalCapacityReached:
            "The offline authoring queue is full. Sync or remove a queued item before adding more content."
        case .invalidRemoteResponse:
            "The server response does not prove the pending authoring operation was applied."
        case .suggestionNotFound:
            "The Codex item draft is no longer available."
        case .suggestionNotPending:
            "The Codex item draft has already been decided."
        case .suggestionExpired:
            "The Codex item draft expired before it was accepted."
        case .suggestionIdentityMismatch:
            "The reviewed Codex item draft does not match the durable Inbox record."
        }
    }
}

enum PlannerRecurrenceMoveError: LocalizedError, Equatable, Sendable {
    case encryptedPersistenceRequired
    case recoveryInProgress
    case invalidOccurrence
    case partiallyResolvedOccurrence
    case capacityReached

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Healthy encrypted planner persistence is required before moving a recurring occurrence."
        case .recoveryInProgress:
            "Recover the active canonical, publication, or execution operation before moving this occurrence."
        case .invalidOccurrence:
            "Only a current, flexible recurring occurrence can be moved to a later whole-second time."
        case .partiallyResolvedOccurrence:
            "This occurrence already has finished sessions and cannot be moved as a whole."
        case .capacityReached:
            "The encrypted occurrence-move ledger is full. Finish or skip an older moved occurrence before adding another."
        }
    }
}

enum CanonicalSensitivityPresentation: Equatable, Sendable {
    case standard
    case own
    case inherited
}

enum PlannerGoogleOutboundRecoveryError: Error, Equatable, Sendable, LocalizedError {
    case encryptedPersistenceRequired
    case invalidJournal
    case journalConflict

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Google publication recovery requires the healthy encrypted planner store."
        case .invalidJournal:
            "The Google publication recovery transition is invalid."
        case .journalConflict:
            "Another Google publication recovery transition changed the encrypted record."
        }
    }
}

private struct CanonicalSessionKey: Hashable {
    let itemID: UUID
    let occurrenceID: UUID?
    let sessionIndex: UInt16
}

private struct ExecutionProjectionKey: Hashable {
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?
    let sessionIndex: UInt16
}

struct CanonicalDeltaCommitResult: Equatable, Sendable {
    let schedulingInputsChanged: Bool
    let cursorChanged: Bool
}

enum PlannerCanonicalDeltaCommitError: Error, LocalizedError {
    case encryptedPersistenceRequired
    case mutationFenceUnavailable

    var errorDescription: String? {
        switch self {
        case .encryptedPersistenceRequired:
            "Canonical item catch-up requires healthy encrypted persistence."
        case .mutationFenceUnavailable:
            "Canonical item catch-up could not acquire the shared mutation fence."
        }
    }
}

@MainActor
final class PlannerStore: ObservableObject {
    static let maximumCanonicalTitleScalars = 500
    static let maximumRecurrenceSessionOutcomes = 10_000
    static let maximumCanonicalTrashEntries = 500
    static let maximumCanonicalTrashItemBytes = 256 * 1_024
    static let maximumCanonicalTrashRetainedItemBytes = 4 * 1_024 * 1_024
    static let canonicalTrashRetentionInterval: TimeInterval = 30 * 24 * 60 * 60
    static let maximumLocalSuggestions = 500
    static let maximumCodexSuggestionsPerTurn = 5
    static let localSuggestionLifetime: TimeInterval = 7 * 24 * 60 * 60
    static let localSuggestionFutureSkewTolerance: TimeInterval = 5 * 60
    static let localSuggestionHighWaterCheckpointInterval: TimeInterval = 5 * 60
    @Published var destination: SidebarDestination? = .today {
        didSet { scheduleAutosave() }
    }
    @Published var selectedBlockID: UUID? {
        didSet { scheduleAutosave() }
    }
    @Published var selectedCanonicalItemID: UUID? = nil {
        didSet { scheduleAutosave() }
    }
    @Published var blocks: [ScheduleBlock] {
        didSet { scheduleAutosave() }
    }
    @Published var suggestions: [PlanningSuggestion] {
        didSet { scheduleAutosave() }
    }
    @Published var assistantMessages: [AssistantMessage] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var canonicalItems: [DayWeaveCanonicalItem] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var canonicalDeltaCursor: String? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var canonicalTombstoneRevisions: [UUID: UInt64] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var completedOccurrenceIDs: Set<UUID> {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingCanonicalMutations: [PendingCanonicalMutation] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var recurrenceSessionOutcomes: [RecurrenceSessionOutcome] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingExecutionDeferIntent:
        DayWeavePendingExecutionDeferIntent? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var deferredExecutionPublicationSessionIDs: Set<UUID> {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingPublicationDeferredSessionIDs: Set<UUID> {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var canonicalConfigurationIdentifier: String? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var schedulePreviewProvenance: SchedulePreviewProvenance? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var publishedScheduleProof: DayWeavePublishedScheduleProof? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var onboardingFirstItemAnchor:
        DayWeaveOnboardingFirstItemAnchor? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var localScheduleCompositionProvenance:
        LocalScheduleCompositionProvenance? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingSchedulePublication: PendingSchedulePublication? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingProposalApplicationMutation:
        DayWeavePendingProposalApplicationMutation? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var proposalApplicationReceipts:
        [DayWeaveStoredProposalApplicationReceipt] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var pendingCanonicalAuthoringMutations:
        [DayWeavePendingCanonicalAuthoringMutation] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var canonicalTrash: [DayWeaveCanonicalTrashEntry] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var googleOutboundRecoveryJournal:
        GoogleOutboundRecoveryJournal? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var googleSchedulePublicationRecoveryJournal:
        GoogleSchedulePublicationRecoveryJournal? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var localCaptureDiagnostics: [UUID: String] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var executionState: DayWeaveExecutionDurableState {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var scheduleProfile: ScheduleProfile {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var isCanonicalSyncLocked = false
    @Published var isQuickAddPresented = false
    @Published var lastScheduleMessage: String {
        didSet { scheduleAutosave() }
    }
    /// Schema-12 compatibility value derived from the profile's common daily
    /// terminal protected interval. It is never a second mutation source.
    var protectedFreeMinutes: Int {
        scheduleProfile.protectedFreeMinutes
    }
    @Published var freezeHours = 2 {
        didSet { scheduleAutosave() }
    }
    @Published var showCompleted = true {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var persistenceError: PlannerPersistenceError?
    @Published private(set) var loadState: PlannerLoadState

    private let persistence: EncryptedPlannerPersistence?
    private let autosaveDelay: Duration
    private let now: @Sendable () -> Date
    private let localSuggestionCheckpointSleep:
        @Sendable (Duration) async throws -> Void
    private var autosaveTask: Task<Void, Never>?
    private var canonicalTrashRetentionTask: Task<Void, Never>?
    private struct LocalSuggestionExpirationIdentity: Hashable, Sendable {
        let id: UUID
        let createdAt: Date
        let expiresAt: Date
    }

    private var localSuggestionExpirationTasks:
        [LocalSuggestionExpirationIdentity: Task<Void, Never>] = [:]
    private var localSuggestionHighWaterCheckpointTask: Task<Void, Never>?
    private var localSuggestionHighWaterCheckpointIdentity: Date?
    private var localSuggestionDateHighWater: Date?
    private var persistedLocalSuggestionDateHighWater: Date?
    private var scheduleProfileCommitObservers: [@MainActor () -> Void] = []
    private var isCanonicalPreviewValidatedForCurrentLaunch = false
    private var persistenceRevision: PlannerPersistenceRevision = .missing

    /// Complete in-memory transaction preimage for authoritative item delta
    /// application. Several reconciliation helpers deliberately touch more
    /// than the item array and cursor (privacy presentation, recovery journals,
    /// recurrence history and execution projection diagnostics), so rollback
    /// must restore their whole mutation surface after a failed encrypted save.
    private struct CanonicalDeltaMutationPreimage {
        let blocks: [ScheduleBlock]
        let canonicalItems: [DayWeaveCanonicalItem]
        let canonicalDeltaCursor: String?
        let canonicalTombstoneRevisions: [UUID: UInt64]
        let completedOccurrenceIDs: Set<UUID>
        let pendingCanonicalMutations: [PendingCanonicalMutation]
        let pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation]
        let recurrenceSessionOutcomes: [RecurrenceSessionOutcome]
        let recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove]
        let schedulePreviewProvenance: SchedulePreviewProvenance?
        let publishedScheduleProof: DayWeavePublishedScheduleProof?
        let onboardingFirstItemAnchor: DayWeaveOnboardingFirstItemAnchor?
        let localScheduleCompositionProvenance: LocalScheduleCompositionProvenance?
        let pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation]
        let canonicalTrash: [DayWeaveCanonicalTrashEntry]
        let executionState: DayWeaveExecutionDurableState
        let selectedCanonicalItemID: UUID?
        let lastScheduleMessage: String
        let previewValidatedForCurrentLaunch: Bool
    }

    init(
        blocks: [ScheduleBlock] = [],
        suggestions: [PlanningSuggestion] = [],
        assistantMessages: [AssistantMessage] = [],
        canonicalItems: [DayWeaveCanonicalItem] = [],
        canonicalDeltaCursor: String? = nil,
        canonicalTombstoneRevisions: [UUID: UInt64] = [:],
        completedOccurrenceIDs: Set<UUID> = [],
        pendingCanonicalMutations: [PendingCanonicalMutation] = [],
        pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation] = [],
        recurrenceSessionOutcomes: [RecurrenceSessionOutcome] = [],
        recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove] = [],
        pendingExecutionDeferIntent: DayWeavePendingExecutionDeferIntent? = nil,
        deferredExecutionPublicationSessionIDs: Set<UUID> = [],
        pendingPublicationDeferredSessionIDs: Set<UUID> = [],
        canonicalConfigurationIdentifier: String? = nil,
        schedulePreviewProvenance: SchedulePreviewProvenance? = nil,
        publishedScheduleProof: DayWeavePublishedScheduleProof? = nil,
        onboardingFirstItemAnchor: DayWeaveOnboardingFirstItemAnchor? = nil,
        localScheduleCompositionProvenance: LocalScheduleCompositionProvenance? = nil,
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? = nil,
        proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt] = [],
        pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = [],
        canonicalTrash: [DayWeaveCanonicalTrashEntry] = [],
        googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal? = nil,
        googleSchedulePublicationRecoveryJournal:
            GoogleSchedulePublicationRecoveryJournal? = nil,
        selectedCanonicalItemID: UUID? = nil,
        localCaptureDiagnostics: [UUID: String] = [:],
        executionState: DayWeaveExecutionDurableState = .empty,
        scheduleProfile: ScheduleProfile? = nil,
        previewValidatedForCurrentLaunch: Bool = false,
        lastScheduleMessage: String = "No schedule yet — add an item when you’re ready",
        persistence: EncryptedPlannerPersistence? = nil,
        restoreFromPersistence: Bool = true,
        autosaveDelay: Duration = .milliseconds(250),
        now: @escaping @Sendable () -> Date = Date.init,
        localSuggestionCheckpointSleep: @escaping @Sendable (Duration) async throws -> Void = {
            try await Task.sleep(for: $0)
        }
    ) {
        self.persistence = persistence
        self.autosaveDelay = autosaveDelay
        self.now = now
        self.localSuggestionCheckpointSleep = localSuggestionCheckpointSleep

        var restoredSnapshot: PlannerSnapshot?
        var restorationError: PlannerPersistenceError?
        var restoredRevision = PlannerPersistenceRevision.missing
        if restoreFromPersistence, let persistence {
            do {
                let loaded = try persistence.loadRevisioned()
                restoredSnapshot = loaded.snapshot
                restoredRevision = loaded.revision
            } catch {
                restorationError = error
            }
        }
        let initialBlocks = restoredSnapshot?.blocks ?? blocks
        let initialCanonicalItems = restoredSnapshot?.canonicalItems ?? canonicalItems
        let initialCanonicalTombstoneRevisions = restoredSnapshot?.canonicalTombstoneRevisions
            ?? canonicalTombstoneRevisions
        let initialCanonicalConfigurationIdentifier = restoredSnapshot?.canonicalConfigurationIdentifier
            ?? canonicalConfigurationIdentifier
        self.blocks = initialBlocks
        self.suggestions = restoredSnapshot?.suggestions ?? suggestions
        self.assistantMessages = restoredSnapshot?.assistantMessages ?? assistantMessages
        self.canonicalItems = initialCanonicalItems
        self.canonicalDeltaCursor = restoredSnapshot?.canonicalDeltaCursor ?? canonicalDeltaCursor
        self.canonicalTombstoneRevisions = initialCanonicalTombstoneRevisions
        self.completedOccurrenceIDs = restoredSnapshot?.completedOccurrenceIDs ?? completedOccurrenceIDs
        self.pendingCanonicalMutations = restoredSnapshot?.pendingCanonicalMutations ?? pendingCanonicalMutations
        self.pendingCanonicalSensitivityMutations = restoredSnapshot?.pendingCanonicalSensitivityMutations
            ?? pendingCanonicalSensitivityMutations
        self.recurrenceSessionOutcomes = restoredSnapshot?.recurrenceSessionOutcomes ?? recurrenceSessionOutcomes
        let initialRecurrenceOccurrenceMoves = restoredSnapshot?.recurrenceOccurrenceMoves
            ?? recurrenceOccurrenceMoves
        self.recurrenceOccurrenceMoves = initialRecurrenceOccurrenceMoves
        let initialPendingExecutionDeferIntent = restoredSnapshot?.pendingExecutionDeferIntent
            ?? pendingExecutionDeferIntent
        self.pendingExecutionDeferIntent = initialPendingExecutionDeferIntent
        let initialDeferredExecutionPublicationSessionIDs =
            restoredSnapshot?.deferredExecutionPublicationSessionIDs
                ?? deferredExecutionPublicationSessionIDs
        self.deferredExecutionPublicationSessionIDs = initialDeferredExecutionPublicationSessionIDs
        let initialPendingPublicationDeferredSessionIDs =
            restoredSnapshot?.pendingPublicationDeferredSessionIDs
                ?? pendingPublicationDeferredSessionIDs
        self.pendingPublicationDeferredSessionIDs = initialPendingPublicationDeferredSessionIDs
        self.canonicalConfigurationIdentifier = initialCanonicalConfigurationIdentifier
        let initialSchedulePreviewProvenance = restoredSnapshot?.schedulePreviewProvenance
            ?? schedulePreviewProvenance
        let initialLocalScheduleCompositionProvenance = restoredSnapshot == nil
            ? localScheduleCompositionProvenance
            : restoredSnapshot?.localScheduleCompositionProvenance
        self.schedulePreviewProvenance = initialSchedulePreviewProvenance
        let initialPublishedScheduleProof = restoredSnapshot == nil
            ? publishedScheduleProof
            : restoredSnapshot?.publishedScheduleProof
        self.publishedScheduleProof = initialPublishedScheduleProof
        let initialOnboardingFirstItemAnchor = restoredSnapshot == nil
            ? onboardingFirstItemAnchor
            : restoredSnapshot?.onboardingFirstItemAnchor
        self.onboardingFirstItemAnchor = initialOnboardingFirstItemAnchor
        self.localScheduleCompositionProvenance = initialLocalScheduleCompositionProvenance
        if initialLocalScheduleCompositionProvenance?.hasValidShape == false
            || (initialSchedulePreviewProvenance != nil
                && initialLocalScheduleCompositionProvenance != nil)
            || initialLocalScheduleCompositionProvenance.map({
                $0.configurationIdentifier != initialCanonicalConfigurationIdentifier
                    || initialBlocks.contains {
                        $0.syncOrigin == .canonicalPreview
                            || $0.syncOrigin == .externalPreview
                    }
            }) == true
            || (initialSchedulePreviewProvenance != nil
                && initialBlocks.contains { $0.syncOrigin == .localComposition })
            || (initialLocalScheduleCompositionProvenance == nil
                && initialBlocks.contains { $0.syncOrigin == .localComposition }) {
            restorationError = .snapshotDecodingFailed
        }
        let initialPendingSchedulePublication = restoredSnapshot == nil
            ? pendingSchedulePublication
            : restoredSnapshot?.pendingSchedulePublication
        self.pendingSchedulePublication = initialPendingSchedulePublication
        if initialPublishedScheduleProof.map({ proof in
            !proof.hasValidShape
                || proof.configurationIdentifier
                    != initialCanonicalConfigurationIdentifier
                || initialSchedulePreviewProvenance.map(proof.matches) != true
                || initialLocalScheduleCompositionProvenance != nil
                || !proof.matchesPublishedPlan(initialBlocks)
        }) == true {
            restorationError = .snapshotDecodingFailed
        }
        let initialPendingProposalApplicationMutation = restoredSnapshot == nil
            ? pendingProposalApplicationMutation
            : restoredSnapshot?.pendingProposalApplicationMutation
        let initialProposalApplicationReceipts = restoredSnapshot?.proposalApplicationReceipts
            ?? proposalApplicationReceipts
        let sortedProposalApplicationReceipts = Self.sortedProposalApplicationReceipts(
            initialProposalApplicationReceipts
        )
        self.pendingProposalApplicationMutation = initialPendingProposalApplicationMutation
        self.proposalApplicationReceipts = sortedProposalApplicationReceipts
        if !PlannerProposalApplicationJournalValidator.isValidState(
            pending: initialPendingProposalApplicationMutation,
            receipts: sortedProposalApplicationReceipts
        ) {
            restorationError = .snapshotDecodingFailed
        }
        let initialCanonicalAuthoringMutations = restoredSnapshot?.pendingCanonicalAuthoringMutations
            ?? pendingCanonicalAuthoringMutations
        let initialCanonicalTrash = restoredSnapshot?.canonicalTrash ?? canonicalTrash
        let retentionReferenceDate = now()
        let boundedCanonicalAuthoringMutations = Self.boundedCanonicalAuthoringMutations(
            initialCanonicalAuthoringMutations,
            referenceDate: retentionReferenceDate
        )
        let boundedCanonicalTrash = Self.boundedCanonicalTrash(
            initialCanonicalTrash,
            referenceDate: retentionReferenceDate,
            pinnedItemIDs: Self.canonicalRecoveryPinnedItemIDs(
                boundedCanonicalAuthoringMutations
            )
        )
        let restoredCanonicalRetentionNeedsRewrite = restoredSnapshot != nil
            && (initialCanonicalTrash != boundedCanonicalTrash
                || initialCanonicalAuthoringMutations != boundedCanonicalAuthoringMutations)
        self.pendingCanonicalAuthoringMutations = boundedCanonicalAuthoringMutations
        self.canonicalTrash = boundedCanonicalTrash
        if !PlannerCanonicalAuthoringJournalValidator.isValidState(
            mutations: boundedCanonicalAuthoringMutations,
            trash: boundedCanonicalTrash,
            canonicalItems: initialCanonicalItems,
            tombstoneRevisions: initialCanonicalTombstoneRevisions,
            configurationIdentifier: initialCanonicalConfigurationIdentifier
        ) {
            restorationError = .snapshotDecodingFailed
        }
        if initialOnboardingFirstItemAnchor.map({ anchor in
            guard anchor.hasValidShape else { return true }
            if let revision = anchor.canonicalRevision {
                return !initialCanonicalItems.contains {
                    $0.id == anchor.itemID
                        && $0.revision == revision
                        && $0.deletedAt == nil
                }
            }
            return !boundedCanonicalAuthoringMutations.contains { mutation in
                mutation.itemID == anchor.itemID
                    && mutation.operation == .create
                    && mutation.draft.map {
                        $0.createsPlanningDemand(itemID: anchor.itemID)
                    } == true
            }
        }) == true {
            restorationError = .snapshotDecodingFailed
        }
        let initialGoogleOutboundRecoveryJournal = restoredSnapshot == nil
            ? googleOutboundRecoveryJournal
            : restoredSnapshot?.googleOutboundRecoveryJournal
        self.googleOutboundRecoveryJournal = initialGoogleOutboundRecoveryJournal
        if initialGoogleOutboundRecoveryJournal?.hasValidShape == false {
            restorationError = .snapshotDecodingFailed
        }
        let initialGoogleSchedulePublicationRecoveryJournal = restoredSnapshot == nil
            ? googleSchedulePublicationRecoveryJournal
            : restoredSnapshot?.googleSchedulePublicationRecoveryJournal
        self.googleSchedulePublicationRecoveryJournal =
            initialGoogleSchedulePublicationRecoveryJournal
        if initialGoogleSchedulePublicationRecoveryJournal?.hasValidShape == false {
            restorationError = .snapshotDecodingFailed
        }
        self.localCaptureDiagnostics = restoredSnapshot?.localCaptureDiagnostics
            ?? localCaptureDiagnostics
        let initialExecutionState = restoredSnapshot?.executionState ?? executionState
        self.executionState = initialExecutionState
        if !Self.validateExecutionState(initialExecutionState) {
            restorationError = .snapshotDecodingFailed
        }
        if !RecurrenceOccurrenceMove.collectionIsValid(
            initialRecurrenceOccurrenceMoves,
            canonicalItemIDs: Set(initialCanonicalItems.map(\.id))
        )
            || initialPendingExecutionDeferIntent?.hasValidShape == false
            || (initialPendingExecutionDeferIntent.map { intent in
                initialExecutionState.activeSession.map(intent.identity.matches) == true
                    || initialExecutionState.terminalOutcomes[intent.identity.sessionID]
                        .map { intent.identity.matches($0.session) } == true
                    || initialExecutionState.pendingCommand
                        .map { $0.identity == intent.identity } == true
            } == false)
            || initialDeferredExecutionPublicationSessionIDs.count > 10_000
            || !initialDeferredExecutionPublicationSessionIDs.allSatisfy({
                initialExecutionState.terminalOutcomes[$0]?.session.status == .deferred
            })
            || !initialPendingPublicationDeferredSessionIDs.isSubset(
                of: initialDeferredExecutionPublicationSessionIDs
            )
            || (initialPendingSchedulePublication == nil
                && !initialPendingPublicationDeferredSessionIDs.isEmpty) {
            restorationError = .snapshotDecodingFailed
        }
        let restoredProtectedFreeMinutes = restoredSnapshot?.protectedFreeMinutes
        let initialScheduleProfile = restoredSnapshot?.scheduleProfile
            ?? scheduleProfile
            ?? Self.defaultScheduleProfile(
                protectedFreeMinutes: restoredProtectedFreeMinutes ?? 90,
                timezoneName: initialSchedulePreviewProvenance?.timezoneName
                    ?? initialLocalScheduleCompositionProvenance?.timezoneName
            )
        self.scheduleProfile = initialScheduleProfile
        if !initialScheduleProfile.hasValidShape
            || initialScheduleProfile.protectedFreeMinutes
                != (restoredProtectedFreeMinutes ?? initialScheduleProfile.protectedFreeMinutes)
            || (initialSchedulePreviewProvenance.map { provenance in
                provenance.timezoneName != initialScheduleProfile.timezoneName
                    && initialPublishedScheduleProof.map { proof in
                        proof.hasCurrentImmutablePlanSeal
                            && proof.configurationIdentifier
                                == initialCanonicalConfigurationIdentifier
                            && proof.matches(provenance)
                            && proof.matchesPublishedPlan(initialBlocks)
                    } != true
            } == true)
            || initialLocalScheduleCompositionProvenance.map({
                $0.timezoneName != initialScheduleProfile.timezoneName
            }) == true {
            restorationError = .snapshotDecodingFailed
        }
        isCanonicalPreviewValidatedForCurrentLaunch = previewValidatedForCurrentLaunch
        destination = restoredSnapshot?.destination ?? .today
        if let restoredSelection = restoredSnapshot?.selectedBlockID,
           initialBlocks.contains(where: { $0.id == restoredSelection }) {
            selectedBlockID = restoredSelection
        } else {
            selectedBlockID = initialBlocks.first?.id
        }
        let requestedCanonicalSelection = restoredSnapshot == nil
            ? selectedCanonicalItemID
            : restoredSnapshot?.selectedCanonicalItemID
        let selectableCanonicalIDs = Set(initialCanonicalItems.map(\.id))
            .union(boundedCanonicalTrash.map(\.id))
            .union(boundedCanonicalAuthoringMutations.map(\.itemID))
        self.selectedCanonicalItemID = requestedCanonicalSelection.flatMap {
            selectableCanonicalIDs.contains($0) ? $0 : nil
        }
        self.lastScheduleMessage = restoredSnapshot?.lastScheduleMessage ?? lastScheduleMessage
        freezeHours = restoredSnapshot?.freezeHours ?? 2
        showCompleted = restoredSnapshot?.showCompleted ?? true
        persistenceError = restorationError
        loadState = restorationError == nil ? .ready : .persistenceFailed
        persistenceRevision = restoredRevision

        persistedLocalSuggestionDateHighWater = restoredSnapshot?
            .localSuggestionDateHighWater
        localSuggestionDateHighWater = persistedLocalSuggestionDateHighWater
            ?? restoredSnapshot?.savedAt
        let localSuggestionObservation = observeLocalSuggestionDate()
        let localSuggestionHighWaterNeedsRewrite = restoredSnapshot != nil
            && restoredSnapshot?.localSuggestionDateHighWater
                != localSuggestionDateHighWater
        let recurrenceHistoryNeedsRewrite = pruneRecurrenceHistory()
        let localSuggestionsNeedRewrite = expireLocalSuggestionsInMemory(
            referenceDate: localSuggestionObservation.referenceDate,
            forcePendingExpiration: localSuggestionObservation.rollbackDetected
        )
        hardenPendingSensitivityPresentation()

        if persistence != nil, restorationError == nil {
            if restoreFromPersistence, restoredSnapshot == nil {
                scheduleAutosave()
            } else if restoredCanonicalRetentionNeedsRewrite
                        || recurrenceHistoryNeedsRewrite
                        || localSuggestionsNeedRewrite
                        || localSuggestionHighWaterNeedsRewrite {
                // Retention is a durable privacy boundary. Rewrite an old
                // snapshot before exposing an indefinitely quiet restored app.
                flushPersistence()
            } else {
                scheduleCanonicalTrashRetention()
                scheduleLocalSuggestionExpiration()
            }
        }
    }

    deinit {
        autosaveTask?.cancel()
        canonicalTrashRetentionTask?.cancel()
        localSuggestionExpirationTasks.values.forEach { $0.cancel() }
        localSuggestionHighWaterCheckpointTask?.cancel()
    }

    func flushPersistence() {
        autosaveTask?.cancel()
        autosaveTask = nil
        canonicalTrashRetentionTask?.cancel()
        canonicalTrashRetentionTask = nil
        guard loadState == .ready, let persistence else { return }
        let retentionReferenceDate = now()
        let localSuggestionObservation = observeLocalSuggestionDate(
            candidate: retentionReferenceDate
        )
        let priorSuggestions = suggestions
        let localSuggestionRetentionChanged = expireLocalSuggestionsInMemory(
            referenceDate: localSuggestionObservation.referenceDate,
            forcePendingExpiration: localSuggestionObservation.rollbackDetected
        )
        let boundedMutations = Self.boundedCanonicalAuthoringMutations(
            pendingCanonicalAuthoringMutations,
            referenceDate: retentionReferenceDate
        )
        let boundedTrash = Self.boundedCanonicalTrash(
            canonicalTrash,
            referenceDate: retentionReferenceDate,
            pinnedItemIDs: Self.canonicalRecoveryPinnedItemIDs(boundedMutations)
        )

        do {
            let snapshot = makeSnapshot(
                canonicalTrashOverride: boundedTrash,
                canonicalAuthoringMutationsOverride: boundedMutations
            )
            persistenceRevision = try persistence.save(
                snapshot,
                expectedRevision: persistenceRevision
            )
            persistedLocalSuggestionDateHighWater = snapshot
                .localSuggestionDateHighWater
            // Install retention changes only after the exact bounded snapshot
            // commits. A failed CAS/write leaves user transactions able to
            // restore their complete in-memory preimage.
            if boundedMutations != pendingCanonicalAuthoringMutations {
                pendingCanonicalAuthoringMutations = boundedMutations
                hardenPendingSensitivityPresentation()
            }
            if boundedTrash != canonicalTrash { canonicalTrash = boundedTrash }
            autosaveTask?.cancel()
            autosaveTask = nil
            persistenceError = nil
            scheduleCanonicalTrashRetention()
            scheduleLocalSuggestionExpiration()
        } catch {
            if !localSuggestionRetentionChanged {
                suggestions = priorSuggestions
            }
            persistenceError = error
            loadState = .persistenceFailed
            cancelLocalSuggestionHighWaterCheckpoint()
        }
    }

    var canPersistPlan: Bool {
        loadState == .ready
    }

    var canMutatePlan: Bool {
        canPersistPlan
            && !isCanonicalSyncLocked
            && pendingExecutionDeferIntent == nil
            && !hasGoogleSchedulePublicationAuthorityFence
    }

    var hasGoogleSchedulePublicationAuthorityFence: Bool {
        googleSchedulePublicationRecoveryJournal.map { $0.stage != .accepted } == true
    }

    var hasEncryptedPersistence: Bool {
        persistence != nil
    }

    /// Commit-only observation: unlike `$scheduleProfile`, this never fires
    /// for an in-memory candidate that is subsequently rolled back after a
    /// persistence/CAS failure.
    func observeCommittedScheduleProfileChanges(
        _ observer: @escaping @MainActor () -> Void
    ) {
        scheduleProfileCommitObservers.append(observer)
    }

    /// Replaces the complete value-semantic profile as one encrypted
    /// transaction. The optional baseline lets an editor reject a stale save
    /// without mutating either the profile or the visible schedule.
    func updateScheduleProfile(
        _ replacement: ScheduleProfile,
        expectedCurrentProfile: ScheduleProfile? = nil
    ) throws {
        try commitScheduleProfile(
            replacement,
            expectedCurrentProfile: expectedCurrentProfile
        )
    }

    private func commitScheduleProfile(
        _ replacement: ScheduleProfile,
        expectedCurrentProfile: ScheduleProfile?
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerScheduleProfileError.encryptedPersistenceRequired
        }
        guard replacement.hasValidShape else {
            throw PlannerScheduleProfileError.invalidProfile
        }
        if let expectedCurrentProfile, expectedCurrentProfile != scheduleProfile {
            throw PlannerScheduleProfileError.staleBaseline
        }
        guard !isCanonicalSyncLocked else {
            throw PlannerScheduleProfileError.mutationFenceActive
        }
        guard pendingSchedulePublication == nil,
              pendingProposalApplicationMutation == nil,
              googleOutboundRecoveryJournal == nil,
              !hasGoogleSchedulePublicationAuthorityFence,
              pendingCanonicalMutations.isEmpty,
              pendingCanonicalSensitivityMutations.isEmpty,
              pendingCanonicalAuthoringMutations.isEmpty else {
            throw PlannerScheduleProfileError.recoveryInProgress
        }
        guard executionState.activeSession == nil,
              executionState.pendingCommand == nil,
              !executionState.hasCredentialReplacementBlocker,
              !blocks.contains(where: { $0.syncOrigin == .remoteExecutionLease }) else {
            throw PlannerScheduleProfileError.activeExecution
        }
        guard replacement != scheduleProfile else { return }

        let priorProfile = scheduleProfile
        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorServerProvenance = schedulePreviewProvenance
        let priorPublishedScheduleProof = publishedScheduleProof
        let priorLocalProvenance = localScheduleCompositionProvenance
        let priorMessage = lastScheduleMessage
        let priorLaunchValidation = isCanonicalPreviewValidatedForCurrentLaunch
        let preservesAuthoritativeReplica = schedulePreviewProvenance.map { provenance in
            publishedScheduleProof.map { proof in
                proof.hasCurrentImmutablePlanSeal
                    && proof.configurationIdentifier == canonicalConfigurationIdentifier
                    && proof.matches(provenance)
                    && proof.matchesPublishedPlan(blocks)
            } == true
        } == true

        scheduleProfile = replacement
        if preservesAuthoritativeReplica {
            // Device-local profile settings govern the next composition. They
            // cannot erase another device's immutable authoritative head.
            blocks.removeAll { $0.syncOrigin == .localComposition }
            localScheduleCompositionProvenance = nil
            isCanonicalPreviewValidatedForCurrentLaunch = priorLaunchValidation
            lastScheduleMessage =
                "Schedule profile changed · current published schedule retained"
        } else {
            blocks.removeAll {
                $0.syncOrigin == .canonicalPreview
                    || $0.syncOrigin == .externalPreview
                    || $0.syncOrigin == .localComposition
            }
            schedulePreviewProvenance = nil
            publishedScheduleProof = nil
            localScheduleCompositionProvenance = nil
            isCanonicalPreviewValidatedForCurrentLaunch = false
            selectedBlockID = blocks.first?.id
            lastScheduleMessage = "Schedule profile changed · compose a fresh schedule"
        }

        flushPersistence()
        if let persistenceError {
            scheduleProfile = priorProfile
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorServerProvenance
            publishedScheduleProof = priorPublishedScheduleProof
            localScheduleCompositionProvenance = priorLocalProvenance
            lastScheduleMessage = priorMessage
            isCanonicalPreviewValidatedForCurrentLaunch = priorLaunchValidation
            throw persistenceError
        }
        for observer in scheduleProfileCommitObservers { observer() }
    }

    @discardableResult
    func beginCanonicalSync() -> Bool {
        guard canPersistPlan,
              !isCanonicalSyncLocked,
              pendingExecutionDeferIntent == nil,
              pendingProposalApplicationMutation == nil,
              googleOutboundRecoveryJournal == nil,
              !hasGoogleSchedulePublicationAuthorityFence else { return false }
        isCanonicalSyncLocked = true
        return true
    }

    /// Execution recovery shares the same exclusive mutation lock but must be
    /// able to acquire it while the saved Pause -> Defer intent is precisely
    /// the state it is reconciling.
    @discardableResult
    func beginExecutionSync() -> Bool {
        guard canPersistPlan,
              !isCanonicalSyncLocked,
              pendingProposalApplicationMutation == nil,
              googleOutboundRecoveryJournal == nil,
              !hasGoogleSchedulePublicationAuthorityFence else { return false }
        isCanonicalSyncLocked = true
        return true
    }

    func endCanonicalSync() {
        isCanonicalSyncLocked = false
    }

    func prepareCanonicalSync(configurationIdentifier: String) throws {
        guard canPersistPlan else {
            throw persistenceError ?? PlannerPersistenceError.snapshotEncodingFailed
        }
        // Invalidate before inspecting the identifier so an out-of-process
        // defaults change cannot leave an old server's preview actionable.
        invalidateCanonicalPreview()
        try prepareCanonicalBinding(configurationIdentifier: configurationIdentifier)
    }

    /// Establishes or canonicalizes the durable API/auth binding for a
    /// read-only schedule bootstrap without invalidating the currently
    /// installed projection. Only a validated current resource, an exact typed
    /// absence, or a subsequently recovered local write may change that plan.
    func prepareCanonicalReplicaRead(configurationIdentifier: String) throws {
        guard canPersistPlan else {
            throw persistenceError ?? PlannerPersistenceError.snapshotEncodingFailed
        }
        try prepareCanonicalBinding(configurationIdentifier: configurationIdentifier)
    }

    private func prepareCanonicalBinding(configurationIdentifier: String) throws {
        guard let requestedIdentifier = Self.canonicalConfigurationIdentifier(
            configurationIdentifier
        ) else {
            throw PlannerCanonicalConfigurationError.configurationMismatch
        }
        if let savedIdentifier = canonicalConfigurationIdentifier,
           Self.canonicalConfigurationIdentifier(savedIdentifier) == requestedIdentifier {
            canonicalConfigurationIdentifier = requestedIdentifier
            for index in pendingCanonicalAuthoringMutations.indices {
                if let saved = pendingCanonicalAuthoringMutations[index].configurationIdentifier,
                   Self.canonicalConfigurationIdentifier(saved) == requestedIdentifier {
                    pendingCanonicalAuthoringMutations[index].configurationIdentifier = requestedIdentifier
                }
            }
            if let provenance = schedulePreviewProvenance,
               Self.canonicalConfigurationIdentifier(provenance.configurationIdentifier)
                == requestedIdentifier,
               provenance.configurationIdentifier != requestedIdentifier {
                schedulePreviewProvenance = .init(
                    configurationIdentifier: requestedIdentifier,
                    generatedAt: provenance.generatedAt,
                    asOf: provenance.asOf,
                    horizonStart: provenance.horizonStart,
                    horizonEnd: provenance.horizonEnd,
                    timezoneName: provenance.timezoneName
                )
            }
            if let proof = publishedScheduleProof,
               Self.canonicalConfigurationIdentifier(proof.configurationIdentifier)
                == requestedIdentifier,
               proof.configurationIdentifier != requestedIdentifier {
                publishedScheduleProof = proof.rebindingConfigurationIdentifier(
                    requestedIdentifier
                )
            }
            flushPersistence()
            if let persistenceError { throw persistenceError }
            return
        }
        guard !hasCanonicalRemoteState else {
            throw canonicalConfigurationIdentifier == nil
                ? PlannerCanonicalConfigurationError.unboundExistingState
                : PlannerCanonicalConfigurationError.configurationMismatch
        }
        canonicalConfigurationIdentifier = requestedIdentifier
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    func invalidateCanonicalPreview() {
        isCanonicalPreviewValidatedForCurrentLaunch = false
    }

    func resetCanonicalSyncState() {
        guard canMutatePlan,
              !executionState.hasCredentialReplacementBlocker,
              pendingCanonicalMutations.isEmpty,
              pendingCanonicalSensitivityMutations.isEmpty,
              pendingSchedulePublication == nil,
              pendingProposalApplicationMutation == nil,
              !pendingCanonicalAuthoringMutations.contains(where: \.hasBeenSubmitted) else {
            return
        }
        let preservedCreates = localCreatesPreservedAcrossConfigurationReset()
        blocks.removeAll {
            $0.sourceItemID != nil
                || $0.syncOrigin == .canonicalPreview
                || $0.syncOrigin == .externalPreview
                || $0.syncOrigin == .localComposition
                || $0.syncOrigin == .remoteExecutionLease
        }
        canonicalItems = []
        canonicalTrash = []
        canonicalDeltaCursor = nil
        canonicalTombstoneRevisions = [:]
        completedOccurrenceIDs = []
        pendingCanonicalMutations = []
        pendingCanonicalSensitivityMutations = []
        recurrenceSessionOutcomes = []
        recurrenceOccurrenceMoves = []
        pendingExecutionDeferIntent = nil
        deferredExecutionPublicationSessionIDs = []
        pendingPublicationDeferredSessionIDs = []
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        publishedScheduleProof = nil
        localScheduleCompositionProvenance = nil
        pendingSchedulePublication = nil
        pendingProposalApplicationMutation = nil
        proposalApplicationReceipts = []
        pendingCanonicalAuthoringMutations = preservedCreates
        if let anchor = onboardingFirstItemAnchor,
           preservedCreates.contains(where: {
               $0.itemID == anchor.itemID && $0.operation == .create
           }) {
            onboardingFirstItemAnchor = .init(
                itemID: anchor.itemID,
                canonicalRevision: nil
            )
        } else {
            onboardingFirstItemAnchor = nil
        }
        localCaptureDiagnostics = localCaptureDiagnostics.filter { id, _ in
            blocks.contains { $0.id == id && $0.isLocallyAuthored && $0.sourceItemID == nil }
        }
        var resetExecution = DayWeaveExecutionDurableState.empty
        resetExecution.deviceID = executionState.deviceID
        executionState = resetExecution
        isCanonicalPreviewValidatedForCurrentLaunch = false
        selectedBlockID = blocks.first?.id
        reconcileSelectedCanonicalItem()
        lastScheduleMessage = "Canonical cache reset locally; no server data was changed"
        flushPersistence()
    }

    var canonicalPreviewFreshnessIssue: String? {
        guard isCanonicalPreviewValidatedForCurrentLaunch else {
            return "Sync or compose on this device in this app session before changing canonical schedule blocks."
        }
        let generatedAt: Date
        let asOf: Date
        let horizonStart: Date
        let horizonEnd: Date
        let timezoneName: String
        let requiresLocalProfileTimezone: Bool
        if let provenance = localScheduleCompositionProvenance {
            guard schedulePreviewProvenance == nil,
                  provenance.hasValidShape,
                  provenance.configurationIdentifier == canonicalConfigurationIdentifier else {
                return "The visible on-device schedule has invalid composition evidence."
            }
            let currentRevisions = Dictionary(
                uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
            )
            guard provenance.sourceItemRevisions == currentRevisions else {
                return "Canonical item revisions changed after on-device composition. Compose again."
            }
            guard !canonicalItems.contains(where: {
                $0.kind == .habit && $0.deletedAt == nil
            }) || provenance.habitCheckpointFingerprint != nil else {
                return "The visible on-device schedule has no complete habit-history evidence."
            }
            generatedAt = provenance.generatedAt
            asOf = provenance.asOf
            horizonStart = provenance.horizonStart
            horizonEnd = provenance.horizonEnd
            timezoneName = provenance.timezoneName
            requiresLocalProfileTimezone = true
        } else if let provenance = schedulePreviewProvenance {
            guard provenance.configurationIdentifier == canonicalConfigurationIdentifier else {
                return "The visible preview is not bound to the active API configuration."
            }
            generatedAt = provenance.generatedAt
            asOf = provenance.asOf
            horizonStart = provenance.horizonStart
            horizonEnd = provenance.horizonEnd
            timezoneName = provenance.timezoneName
            requiresLocalProfileTimezone = publishedScheduleProof.map { proof in
                proof.hasCurrentImmutablePlanSeal
                    && proof.configurationIdentifier == canonicalConfigurationIdentifier
                    && proof.matches(provenance)
                    && proof.matchesPublishedPlan(blocks)
            } != true
        } else {
            return "The visible canonical schedule has no trusted composition evidence."
        }
        let currentTime = now()
        guard generatedAt <= currentTime.addingTimeInterval(5 * 60),
              currentTime.timeIntervalSince(generatedAt) <= 6 * 3_600 else {
            return "The visible schedule is older than the six-hour execution safety window. Sync or compose again."
        }
        guard let timezone = DayWeaveCanonicalItemDraft.supportedTimeZone(
            identifier: timezoneName
        ), !requiresLocalProfileTimezone || timezoneName == scheduleProfile.timezoneName else {
            return "The planning timezone changed. Sync or compose again before changing canonical blocks."
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timezone
        guard calendar.isDate(asOf, inSameDayAs: currentTime),
              currentTime >= horizonStart,
              currentTime < horizonEnd else {
            return "The visible schedule is outside its generated day or planning horizon. Sync or compose again."
        }
        return nil
    }

    func canMutate(_ block: ScheduleBlock) -> Bool {
        guard canMutatePlan else { return false }
        guard block.syncOrigin != .remoteExecutionLease else { return false }
        if let itemID = block.sourceItemID,
           canonicalAuthoringMutation(itemID: itemID) != nil { return false }
        guard block.syncOrigin == .canonicalPreview
                || block.syncOrigin == .externalPreview
                || block.syncOrigin == .localComposition else {
            return true
        }
        guard canonicalPreviewFreshnessIssue == nil,
              !block.isHardConstraint,
              block.previewKind == "planned" || block.previewKind == "pinned" else { return false }
        if let itemID = block.sourceItemID {
            guard let item = canonicalItem(id: itemID),
                  block.sourceItemRevision == item.revision else { return false }
        }
        return true
    }

    var canRecomposeSchedule: Bool {
        canMutatePlan && blocks.allSatisfy { block in
            if block.syncOrigin == .remoteExecutionLease { return false }
            guard block.syncOrigin == .canonicalPreview
                    || block.syncOrigin == .externalPreview
                    || block.syncOrigin == .localComposition else {
                return true
            }
            return canMutate(block)
        }
    }

    /// A proven immutable replica carries its own planning timezone. The local
    /// profile remains the input for the next composition, but cannot reinterpret
    /// the calendar day of the currently authoritative published plan.
    var schedulePresentationTimezoneName: String {
        guard let provenance = schedulePreviewProvenance,
              let proof = publishedScheduleProof,
              proof.hasCurrentImmutablePlanSeal,
              proof.configurationIdentifier == canonicalConfigurationIdentifier,
              proof.matches(provenance),
              proof.matchesPublishedPlan(blocks),
              DayWeaveCanonicalItemDraft.supportedTimeZone(
                  identifier: provenance.timezoneName
              ) != nil else {
            return scheduleProfile.timezoneName
        }
        return provenance.timezoneName
    }

    /// Checked while the shared mutation lock is held, so it deliberately does
    /// not consult `canMutatePlan`. Execution authority comes only from the
    /// exact encrypted publication receipt for the unchanged server block.
    func canonicalScheduleBlockActionabilityIssue(_ block: ScheduleBlock) -> String? {
        if block.syncOrigin == .localComposition {
            return "The on-device helper schedule is a visible draft. Publish a canonical server schedule before starting it."
        }
        guard block.syncOrigin == .canonicalPreview else {
            return "Only an exactly published canonical schedule block can be started."
        }
        guard pendingSchedulePublication == nil else {
            return "Finish recovering the pending schedule publication before starting work."
        }
        guard deferredExecutionPublicationSessionIDs.isEmpty else {
            return "Publish the deferred work's replacement placement before starting another session."
        }
        guard let provenance = schedulePreviewProvenance,
              let proof = publishedScheduleProof,
              proof.configurationIdentifier == canonicalConfigurationIdentifier,
              proof.matches(provenance),
              proof.matches(block) else {
            return "This block has no durable exact publication proof. Sync and publish the schedule again."
        }
        if let issue = canonicalPreviewFreshnessIssue { return issue }
        guard !block.isHardConstraint,
              block.previewKind == "planned" || block.previewKind == "pinned",
              let itemID = block.sourceItemID,
              let revision = block.sourceItemRevision,
              let item = canonicalItem(id: itemID),
              revision == item.revision,
              item.isExecutable else {
            return "The scheduled block no longer matches its canonical item revision."
        }
        return nil
    }

    /// Exact placement warnings are only complete for the local day whose
    /// published schedule is currently loaded. A future-day target may contain
    /// calendar or protected blocks that this process has not attested, so an
    /// active lease must stay paused for a fresh day-specific review instead of
    /// silently accepting that placement.
    func exactMoveWindowCoverageIssue(
        for block: ScheduleBlock,
        start: Date,
        end: Date
    ) -> String? {
        guard start.timeIntervalSinceReferenceDate.isFinite,
              end.timeIntervalSinceReferenceDate.isFinite,
              start < end,
              let timezone = DayWeaveCanonicalItemDraft.supportedTimeZone(
                  identifier: schedulePresentationTimezoneName
              ) else {
            return "The exact target window is invalid."
        }

        let loadedDayReference: Date
        if block.sourceItemID != nil {
            guard let provenance = schedulePreviewProvenance,
                  let proof = publishedScheduleProof,
                  block.syncOrigin == .canonicalPreview,
                  pendingSchedulePublication == nil,
                  deferredExecutionPublicationSessionIDs.isEmpty,
                  proof.configurationIdentifier == canonicalConfigurationIdentifier,
                  proof.matches(provenance),
                  proof.matches(block),
                  proof.matchesPublishedPlan(blocks),
                  start >= provenance.horizonStart,
                  end <= provenance.horizonEnd else {
                return "The exact target is outside the fully verified published schedule. Sync and review it again."
            }
            loadedDayReference = provenance.asOf
        } else {
            loadedDayReference = now()
        }

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timezone
        let loadedDayStart = calendar.startOfDay(for: loadedDayReference)
        guard let loadedDayEnd = calendar.date(
            byAdding: .day,
            value: 1,
            to: loadedDayStart
        ),
        start >= loadedDayStart,
        end <= loadedDayEnd else {
            return "Exact moves are limited to the currently loaded schedule day. Sync on the target day before moving active or local work there."
        }
        return nil
    }

    private var hasCanonicalRemoteState: Bool {
        !canonicalItems.isEmpty
            || !canonicalTrash.isEmpty
            || canonicalDeltaCursor != nil
            || !canonicalTombstoneRevisions.isEmpty
            || !completedOccurrenceIDs.isEmpty
            || !pendingCanonicalMutations.isEmpty
            || !pendingCanonicalSensitivityMutations.isEmpty
            || !recurrenceSessionOutcomes.isEmpty
            || !recurrenceOccurrenceMoves.isEmpty
            || pendingExecutionDeferIntent != nil
            || !deferredExecutionPublicationSessionIDs.isEmpty
            || !pendingPublicationDeferredSessionIDs.isEmpty
            || schedulePreviewProvenance != nil
            || publishedScheduleProof != nil
            || localScheduleCompositionProvenance != nil
            || pendingSchedulePublication != nil
            || pendingProposalApplicationMutation != nil
            || !proposalApplicationReceipts.isEmpty
            || pendingCanonicalAuthoringMutations.contains {
                $0.operation != .create
                    || $0.hasBeenSubmitted
                    || $0.configurationIdentifier != nil
            }
            || blocks.contains {
                $0.syncOrigin == .canonicalPreview
                    || $0.syncOrigin == .externalPreview
                    || $0.syncOrigin == .localComposition
                    || $0.syncOrigin == .remoteExecutionLease
            }
    }

    private static func canonicalConfigurationIdentifier(_ value: String) -> String? {
        let separator = "|auth="
        let baseValue: String
        let authBinding: String?
        if let range = value.range(of: separator) {
            baseValue = String(value[..<range.lowerBound])
            let binding = String(value[range.upperBound...])
            guard !binding.isEmpty,
                  binding.utf8.count <= 2_048,
                  binding.range(of: separator) == nil,
                  isValidAuthBinding(binding) else { return nil }
            authBinding = binding
        } else {
            baseValue = value
            authBinding = nil
        }
        guard let baseURL = try? DayWeaveAPIBaseURL(baseValue) else { return nil }
        let identifier = baseURL.canonicalConfigurationIdentifier
        guard !identifier.isEmpty else { return nil }
        return authBinding.map { "\(identifier)\(separator)\($0)" } ?? identifier
    }

    private static func isValidAuthBinding(_ value: String) -> Bool {
        let components = value.split(separator: ":", omittingEmptySubsequences: false)
        switch components.first {
        case "static-v1", "legacy-v1":
            guard components.count == 2 else { return false }
            let digest = components[1]
            return digest.utf8.count == 64 && digest.utf8.allSatisfy {
                (48...57).contains($0) || (97...102).contains($0)
            }
        case "device-v1":
            guard components.count == 3,
                  let clientID = UUID(uuidString: String(components[1])),
                  let sessionID = UUID(uuidString: String(components[2])) else { return false }
            return components[1] == Substring(clientID.uuidString.lowercased())
                && components[2] == Substring(sessionID.uuidString.lowercased())
        default:
            return false
        }
    }

    var selectedBlock: ScheduleBlock? {
        blocks.first(where: { $0.id == selectedBlockID })
    }

    var activeItem: ScheduleBlock? {
        blocks.first(where: { $0.status == .active })
    }

    var completedCount: Int {
        blocks.count(where: { $0.status == .completed })
    }

    var visibleBlocks: [ScheduleBlock] {
        todaysBlocks
            .filter { showCompleted || $0.status != .completed }
            .sorted { $0.start < $1.start }
    }

    var todaysBlocks: [ScheduleBlock] {
        var calendar = Calendar(identifier: .gregorian)
        if let timezone = DayWeaveCanonicalItemDraft.supportedTimeZone(
            identifier: schedulePresentationTimezoneName
        ) {
            calendar.timeZone = timezone
        }
        let start = calendar.startOfDay(for: now())
        let end = calendar.date(byAdding: .day, value: 1, to: start)
            ?? start.addingTimeInterval(86_400)
        return blocks.filter { $0.end > start && $0.start < end }
    }

    func select(_ block: ScheduleBlock) {
        selectedBlockID = block.id
    }

    func start(_ id: UUID) {
        guard let target = blocks.first(where: { $0.id == id }),
              canMutate(target),
              blocks.filter({ $0.status == .active }).allSatisfy(canMutate) else { return }
        for index in blocks.indices {
            if blocks[index].status == .active {
                updateStatus(at: index, to: .paused)
            }
            if blocks[index].id == id {
                updateStatus(at: index, to: .active)
            }
        }
        flushPersistence()
    }

    func pauseActive() {
        guard let index = blocks.firstIndex(where: { $0.status == .active }),
              canMutate(blocks[index]) else { return }
        updateStatus(at: index, to: .paused)
        lastScheduleMessage = "Paused — remaining work is held tentatively"
        flushPersistence()
    }

    func complete(_ id: UUID) {
        guard let index = blocks.firstIndex(where: { $0.id == id }),
              canMutate(blocks[index]) else { return }
        blocks[index].actualMinutes = blocks[index].durationMinutes
        updateStatus(at: index, to: .completed)
        lastScheduleMessage = "Completed — later flexible work was checked"
        flushPersistence()
    }

    func skip(_ id: UUID) {
        guard let index = blocks.firstIndex(where: { $0.id == id }),
              canMutate(blocks[index]) else { return }
        updateStatus(at: index, to: .skipped)
        lastScheduleMessage = "Skipped — recurrence policy will decide the next occurrence"
        flushPersistence()
    }

    func doLater(_ id: UUID) {
        guard let block = blocks.first(where: { $0.id == id }) else { return }
        doLater(id, moveStart: block.start.addingTimeInterval(60 * 60))
    }

    func doLater(_ id: UUID, moveStart: Date) {
        guard let index = blocks.firstIndex(where: { $0.id == id }),
              canMutate(blocks[index]),
              blocks[index].sourceItemID == nil,
              blocks[index].isFlexible,
              !blocks[index].isHardConstraint,
              moveStart.timeIntervalSinceReferenceDate.isFinite,
              moveStart > now() else { return }
        let duration = blocks[index].end.timeIntervalSince(blocks[index].start)
        guard duration > 0, duration.isFinite else { return }
        let moveEnd = moveStart.addingTimeInterval(duration)
        guard exactMoveWindowCoverageIssue(
            for: blocks[index],
            start: moveStart,
            end: moveEnd
        ) == nil else {
            lastScheduleMessage = "Exact local moves are limited to the currently loaded schedule day"
            return
        }
        blocks[index].start = moveStart
        blocks[index].end = moveEnd
        if blocks[index].status == .active || blocks[index].status == .paused {
            blocks[index].status = .scheduled
            blocks[index].actualMinutes = nil
        }
        blocks.sort { $0.start < $1.start }
        selectedBlockID = id
        lastScheduleMessage = "Moved local work to \(PlannerTimeZone.dateTimeLabel(moveStart, timezoneName: scheduleProfile.timezoneName))"
        flushPersistence()
    }

    func recomposeSchedule() {
        guard canRecomposeSchedule else { return }
        let frozenUntil = now().addingTimeInterval(TimeInterval(freezeHours * 3_600))
        var cursor: Date?
        var shiftedPublishedBlock = false

        for index in blocks.indices where blocks[index].isFlexible && blocks[index].start > frozenUntil {
            if let cursor, blocks[index].start < cursor {
                let duration = blocks[index].end.timeIntervalSince(blocks[index].start)
                blocks[index].start = cursor
                blocks[index].end = cursor.addingTimeInterval(duration)
                if blocks[index].syncOrigin == .canonicalPreview {
                    shiftedPublishedBlock = true
                }
            }
            cursor = blocks[index].end.addingTimeInterval(10 * 60)
        }
        blocks.sort { $0.start < $1.start }
        if shiftedPublishedBlock {
            publishedScheduleProof = nil
            isCanonicalPreviewValidatedForCurrentLaunch = false
        }
        lastScheduleMessage = "Locally reordered beyond the \(freezeHours)-hour freeze; sync to validate constraints"
    }

    func applyCanonicalDelta(
        _ changes: [DayWeaveItemDeltaChange],
        nextCursor: String,
        flushPrunedRecurrenceHistory: Bool = true
    ) {
        guard canPersistPlan else { return }
        var indexed: [UUID: DayWeaveCanonicalItem] = [:]
        for item in canonicalItems {
            if indexed[item.id] == nil || item.revision > (indexed[item.id]?.revision ?? 0) {
                indexed[item.id] = item
            }
        }
        for change in changes {
            switch change {
            case let .upsert(item):
                let tombstoneRevision = canonicalTombstoneRevisions[item.id] ?? 0
                if item.revision > tombstoneRevision,
                   (indexed[item.id] == nil || item.revision > (indexed[item.id]?.revision ?? 0)) {
                    indexed[item.id] = item
                    canonicalTombstoneRevisions.removeValue(forKey: item.id)
                    canonicalTrash.removeAll {
                        $0.id == item.id && $0.revision < item.revision
                    }
                    if let mutation = pendingCanonicalMutations.first(where: {
                        $0.itemID == item.id && $0.baseRevision != item.revision
                    }) {
                        markCanonicalMutationConflicted(
                            itemID: item.id,
                            diagnostic: "Remote revision \(item.revision) differs from local base revision \(mutation.baseRevision)."
                        )
                    }
                    if let mutation = pendingCanonicalSensitivityMutations.first(where: {
                        $0.itemID == item.id && $0.baseRevision != item.revision
                    }) {
                        if item.isSensitive == mutation.desiredIsSensitive {
                            reconcileCanonicalSensitivityObservation(item)
                        } else {
                            markCanonicalSensitivityMutationConflicted(
                                itemID: item.id,
                                diagnostic: "Remote revision \(item.revision) differs from local privacy-edit base revision \(mutation.baseRevision)."
                            )
                        }
                    }
                }
            case let .tombstone(tombstone):
                if tombstone.revision >= (indexed[tombstone.id]?.revision ?? 0),
                   tombstone.revision >= (canonicalTombstoneRevisions[tombstone.id] ?? 0) {
                    let lastKnownItem = indexed[tombstone.id]
                        ?? canonicalTrash.first(where: { $0.id == tombstone.id })?.lastKnownItem
                    indexed.removeValue(forKey: tombstone.id)
                    canonicalTombstoneRevisions[tombstone.id] = tombstone.revision
                    upsertCanonicalTrash(.init(
                        tombstone: tombstone,
                        lastKnownItem: lastKnownItem
                    ))
                    markCanonicalMutationConflicted(
                        itemID: tombstone.id,
                        diagnostic: "The item was deleted remotely at revision \(tombstone.revision)."
                    )
                    markCanonicalSensitivityMutationConflicted(
                        itemID: tombstone.id,
                        diagnostic: "The item was deleted remotely at revision \(tombstone.revision)."
                    )
                }
            }
        }
        // Reconcile restore intent only against the final state of the whole
        // delta batch. A later tombstone in this same page must win over an
        // earlier matching upsert without silently clearing local intent.
        for mutation in pendingCanonicalAuthoringMutations
            where mutation.operation == .restore {
            if let finalActiveItem = indexed[mutation.itemID] {
                reconcileCanonicalRestoreObservation(finalActiveItem)
            }
        }
        pendingCanonicalAuthoringMutations.removeAll { mutation in
            guard mutation.operation == .trash,
                  !mutation.hasBeenSubmitted,
                  let expectedRevision = mutation.expectedRevision,
                  indexed[mutation.itemID] == nil else { return false }
            // A final newer tombstone proves the requested end state even if
            // another device performed the delete. Clear the full local base
            // body instead of retaining stale deleted content indefinitely.
            return (canonicalTombstoneRevisions[mutation.itemID] ?? 0)
                > expectedRevision
        }
        canonicalItems = Self.hierarchicallySorted(Array(indexed.values))
        reconcileOnboardingFirstItemAnchor()
        canonicalTrash = Self.boundedCanonicalTrash(
            canonicalTrash,
            referenceDate: now(),
            pinnedItemIDs: pendingCanonicalRecoveryItemIDs
        )
        canonicalDeltaCursor = nextCursor
        reconcileSelectedCanonicalItem()
        hardenPendingSensitivityPresentation()
        if pruneRecurrenceHistory(retainingItemIDs: Set(indexed.keys)),
           flushPrunedRecurrenceHistory {
            flushPersistence()
        }
    }

    /// Applies a fully buffered authoritative delta and its opaque cursor as
    /// one encrypted boundary. A semantic canonical change invalidates the
    /// current schedule before this synchronous MainActor method returns;
    /// cursor-only own echoes preserve an otherwise exact publication proof.
    func applyCanonicalDeltaDurably(
        _ changes: [DayWeaveItemDeltaChange],
        nextCursor: String
    ) throws -> CanonicalDeltaCommitResult {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerCanonicalDeltaCommitError.encryptedPersistenceRequired
        }
        guard isCanonicalSyncLocked else {
            throw PlannerCanonicalDeltaCommitError.mutationFenceUnavailable
        }
        let preimage = canonicalDeltaMutationPreimage()
        applyCanonicalDelta(
            changes,
            nextCursor: nextCursor,
            flushPrunedRecurrenceHistory: false
        )
        let result = CanonicalDeltaCommitResult(
            schedulingInputsChanged: canonicalItems != preimage.canonicalItems
                || completedOccurrenceIDs != preimage.completedOccurrenceIDs
                || recurrenceSessionOutcomes != preimage.recurrenceSessionOutcomes
                || recurrenceOccurrenceMoves != preimage.recurrenceOccurrenceMoves,
            cursorChanged: canonicalDeltaCursor != preimage.canonicalDeltaCursor
        )
        if result.schedulingInputsChanged {
            invalidateCanonicalPreview()
            publishedScheduleProof = nil
        }
        flushPersistence()
        if let persistenceError {
            restoreCanonicalDeltaMutationPreimage(preimage)
            throw persistenceError
        }
        return result
    }

    /// A newer active upsert is cross-device evidence for a queued restore.
    /// Resolve an exact semantic match or retain a body-backed conflict before
    /// removing trash, keeping every intermediate snapshot schema-valid.
    private func reconcileCanonicalRestoreObservation(
        _ item: DayWeaveCanonicalItem
    ) {
        guard item.deletedAt == nil,
              let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
                  $0.itemID == item.id && $0.operation == .restore
              }),
              let expectedRevision = pendingCanonicalAuthoringMutations[index]
                  .expectedRevision,
              item.revision > expectedRevision else { return }
        let mutation = pendingCanonicalAuthoringMutations[index]
        if mutation.baseItem.map({ $0.hasSameAuthoredContent(as: item) }) ?? true {
            pendingCanonicalAuthoringMutations.remove(at: index)
        } else {
            pendingCanonicalAuthoringMutations[index].disposition = .conflicted
            pendingCanonicalAuthoringMutations[index].diagnostic =
                "The item was restored elsewhere with different content. Review the retained deleted version and the active revision."
        }
    }

    func replaceCanonicalState(
        changes: [DayWeaveItemDeltaChange],
        nextCursor: String,
        flushPrunedRecurrenceHistory: Bool = true
    ) {
        guard canPersistPlan else { return }
        let recoveryItemIDs = pendingCanonicalRecoveryItemIDs
        var retainedRestoreTrashByID = Dictionary(
            uniqueKeysWithValues: canonicalTrash
                .filter { recoveryItemIDs.contains($0.id) }
                .map { ($0.id, $0) }
        )
        var retainedRestoreTombstones = canonicalTombstoneRevisions.filter {
            recoveryItemIDs.contains($0.key)
        }
        for mutation in pendingCanonicalAuthoringMutations
            where mutation.operation == .restore {
            guard let expectedRevision = mutation.expectedRevision else { continue }
            retainedRestoreTombstones[mutation.itemID] = max(
                retainedRestoreTombstones[mutation.itemID] ?? 0,
                expectedRevision
            )
            if retainedRestoreTrashByID[mutation.itemID] == nil {
                // A conflicted restore can be represented by newer active
                // evidence with no trash. Normalize its exact pre-restore
                // revision back into a recovery record before clearing the
                // old cursor scope, so an empty rebuilt stream cannot erase
                // the only validator evidence for local intent.
                retainedRestoreTrashByID[mutation.itemID] = .init(
                    id: mutation.itemID,
                    revision: expectedRevision,
                    deletedAt: mutation.baseItem?.deletedAt ?? mutation.createdAt,
                    parentID: mutation.baseItem?.parentID,
                    lastKnownItem: mutation.baseItem
                )
            }
        }
        canonicalItems = []
        canonicalDeltaCursor = nil
        // A cursor-scope rebuild may omit deleted history. Preserve the
        // minimum evidence pinned by a local restore journal until the rebuilt
        // final stream supplies a newer active item or tombstone.
        canonicalTombstoneRevisions = retainedRestoreTombstones
        canonicalTrash = Array(retainedRestoreTrashByID.values)
        applyCanonicalDelta(
            changes,
            nextCursor: nextCursor,
            flushPrunedRecurrenceHistory: flushPrunedRecurrenceHistory
        )

        let finalActiveByID = Dictionary(
            canonicalItems.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        for mutation in pendingCanonicalAuthoringMutations
            where mutation.operation == .trash
                && mutation.hasBeenSubmitted
                && finalActiveByID[mutation.itemID] == nil
                && canonicalTrashEntry(id: mutation.itemID) == nil {
            guard let expectedRevision = mutation.expectedRevision else { continue }
            let nextRevision = expectedRevision.addingReportingOverflow(1)
            guard !nextRevision.overflow else { continue }
            canonicalTombstoneRevisions[mutation.itemID] = max(
                canonicalTombstoneRevisions[mutation.itemID] ?? 0,
                nextRevision.partialValue
            )
            canonicalTrash.append(.init(
                id: mutation.itemID,
                revision: nextRevision.partialValue,
                deletedAt: now(),
                parentID: mutation.baseItem?.parentID,
                lastKnownItem: mutation.baseItem
            ))
        }
        pendingCanonicalAuthoringMutations.removeAll { mutation in
            mutation.operation == .trash
                && !mutation.hasBeenSubmitted
                && finalActiveByID[mutation.itemID] == nil
        }
        for index in pendingCanonicalAuthoringMutations.indices {
            let mutation = pendingCanonicalAuthoringMutations[index]
            switch mutation.operation {
            case .replace where finalActiveByID[mutation.itemID] == nil:
                pendingCanonicalAuthoringMutations[index].disposition = .conflicted
                pendingCanonicalAuthoringMutations[index].diagnostic =
                    "The item is absent from the authoritative canonical state. Copy the retained draft or keep the server deletion."
            case .replace, .trash:
                guard let expectedRevision = mutation.expectedRevision,
                      let active = finalActiveByID[mutation.itemID],
                      active.revision != expectedRevision else { continue }
                pendingCanonicalAuthoringMutations[index].disposition = .conflicted
                pendingCanonicalAuthoringMutations[index].diagnostic =
                    "The authoritative item is now revision \(active.revision), not the retained base revision \(expectedRevision)."
            default:
                continue
            }
        }
        canonicalTrash = Self.boundedCanonicalTrash(
            canonicalTrash,
            referenceDate: now(),
            pinnedItemIDs: pendingCanonicalRecoveryItemIDs
        )
        reconcileOnboardingFirstItemAnchor(authoritativeMissing: true)
        reconcileSelectedCanonicalItem()
        hardenPendingSensitivityPresentation()
    }

    /// Durable cursor-scope recovery counterpart to
    /// `applyCanonicalDeltaDurably`. The complete replacement is built by the
    /// caller before the encrypted cache is changed.
    func replaceCanonicalStateDurably(
        changes: [DayWeaveItemDeltaChange],
        nextCursor: String
    ) throws -> CanonicalDeltaCommitResult {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerCanonicalDeltaCommitError.encryptedPersistenceRequired
        }
        guard isCanonicalSyncLocked else {
            throw PlannerCanonicalDeltaCommitError.mutationFenceUnavailable
        }
        let preimage = canonicalDeltaMutationPreimage()
        replaceCanonicalState(
            changes: changes,
            nextCursor: nextCursor,
            flushPrunedRecurrenceHistory: false
        )
        let result = CanonicalDeltaCommitResult(
            schedulingInputsChanged: canonicalItems != preimage.canonicalItems
                || completedOccurrenceIDs != preimage.completedOccurrenceIDs
                || recurrenceSessionOutcomes != preimage.recurrenceSessionOutcomes
                || recurrenceOccurrenceMoves != preimage.recurrenceOccurrenceMoves,
            cursorChanged: canonicalDeltaCursor != preimage.canonicalDeltaCursor
        )
        if result.schedulingInputsChanged {
            invalidateCanonicalPreview()
            publishedScheduleProof = nil
        }
        flushPersistence()
        if let persistenceError {
            restoreCanonicalDeltaMutationPreimage(preimage)
            throw persistenceError
        }
        return result
    }

    private func canonicalDeltaMutationPreimage() -> CanonicalDeltaMutationPreimage {
        CanonicalDeltaMutationPreimage(
            blocks: blocks,
            canonicalItems: canonicalItems,
            canonicalDeltaCursor: canonicalDeltaCursor,
            canonicalTombstoneRevisions: canonicalTombstoneRevisions,
            completedOccurrenceIDs: completedOccurrenceIDs,
            pendingCanonicalMutations: pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
            recurrenceSessionOutcomes: recurrenceSessionOutcomes,
            recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
            schedulePreviewProvenance: schedulePreviewProvenance,
            publishedScheduleProof: publishedScheduleProof,
            onboardingFirstItemAnchor: onboardingFirstItemAnchor,
            localScheduleCompositionProvenance: localScheduleCompositionProvenance,
            pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
            canonicalTrash: canonicalTrash,
            executionState: executionState,
            selectedCanonicalItemID: selectedCanonicalItemID,
            lastScheduleMessage: lastScheduleMessage,
            previewValidatedForCurrentLaunch: isCanonicalPreviewValidatedForCurrentLaunch
        )
    }

    private func restoreCanonicalDeltaMutationPreimage(
        _ preimage: CanonicalDeltaMutationPreimage
    ) {
        blocks = preimage.blocks
        canonicalItems = preimage.canonicalItems
        canonicalDeltaCursor = preimage.canonicalDeltaCursor
        canonicalTombstoneRevisions = preimage.canonicalTombstoneRevisions
        completedOccurrenceIDs = preimage.completedOccurrenceIDs
        pendingCanonicalMutations = preimage.pendingCanonicalMutations
        pendingCanonicalSensitivityMutations = preimage.pendingCanonicalSensitivityMutations
        recurrenceSessionOutcomes = preimage.recurrenceSessionOutcomes
        recurrenceOccurrenceMoves = preimage.recurrenceOccurrenceMoves
        schedulePreviewProvenance = preimage.schedulePreviewProvenance
        publishedScheduleProof = preimage.publishedScheduleProof
        onboardingFirstItemAnchor = preimage.onboardingFirstItemAnchor
        localScheduleCompositionProvenance = preimage.localScheduleCompositionProvenance
        pendingCanonicalAuthoringMutations = preimage.pendingCanonicalAuthoringMutations
        canonicalTrash = preimage.canonicalTrash
        executionState = preimage.executionState
        selectedCanonicalItemID = preimage.selectedCanonicalItemID
        lastScheduleMessage = preimage.lastScheduleMessage
        isCanonicalPreviewValidatedForCurrentLaunch =
            preimage.previewValidatedForCurrentLaunch
    }

    func upsertCanonicalItem(_ item: DayWeaveCanonicalItem) {
        guard canPersistPlan else { return }
        guard item.revision > (canonicalTombstoneRevisions[item.id] ?? 0) else { return }
        if let index = canonicalItems.firstIndex(where: { $0.id == item.id }) {
            guard canonicalItems[index].revision < item.revision else { return }
            canonicalItems[index] = item
        } else {
            canonicalItems.append(item)
        }
        canonicalTombstoneRevisions.removeValue(forKey: item.id)
        canonicalTrash.removeAll { $0.id == item.id && $0.revision < item.revision }
        canonicalItems = Self.hierarchicallySorted(canonicalItems)
        reconcileOnboardingFirstItemAnchor()
    }

    func bindLocalBlock(_ blockID: UUID, to item: DayWeaveCanonicalItem) {
        guard canPersistPlan,
              let index = blocks.firstIndex(where: { $0.id == blockID }) else { return }
        blocks[index].sourceItemID = item.id
        blocks[index].sourceItemRevision = item.revision
        blocks[index].syncOrigin = .canonicalPreview
        publishedScheduleProof = nil
        isCanonicalPreviewValidatedForCurrentLaunch = false
        localCaptureDiagnostics.removeValue(forKey: blockID)
        upsertCanonicalItem(item)
    }

    var sortedPendingCanonicalAuthoringMutations:
        [DayWeavePendingCanonicalAuthoringMutation] {
        pendingCanonicalAuthoringMutations.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.id.uuidString < $1.id.uuidString
        }
    }

    func canonicalAuthoringMutation(
        id: UUID
    ) -> DayWeavePendingCanonicalAuthoringMutation? {
        pendingCanonicalAuthoringMutations.first { $0.id == id }
    }

    func canonicalAuthoringMutation(
        itemID: UUID
    ) -> DayWeavePendingCanonicalAuthoringMutation? {
        pendingCanonicalAuthoringMutations.first { $0.itemID == itemID }
    }

    func canonicalTrashEntry(id: UUID) -> DayWeaveCanonicalTrashEntry? {
        canonicalTrash.first { $0.id == id }
    }

    func selectCanonicalItem(_ itemID: UUID?) {
        guard let itemID else {
            selectedCanonicalItemID = nil
            return
        }
        let exists = canonicalItems.contains { $0.id == itemID }
            || canonicalTrash.contains { $0.id == itemID }
            || pendingCanonicalAuthoringMutations.contains { $0.itemID == itemID }
        if exists { selectedCanonicalItemID = itemID }
    }

    @discardableResult
    func enqueueCanonicalCreate(
        itemID: UUID = UUID(),
        draft: DayWeaveCanonicalItemDraft
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        // Capturing a new, unrelated Inbox identity is safe while another
        // canonical item is executing. Existing-item edits keep the stricter
        // lease fence below.
        try requireCanonicalAuthoringUserFence(allowDuringExecution: true)
        guard canonicalItem(id: itemID) == nil,
              canonicalTrashEntry(id: itemID) == nil,
              canonicalTombstoneRevisions[itemID] == nil else {
            throw PlannerCanonicalAuthoringError.duplicateItemOperation
        }
        try validateCanonicalAuthoringDraft(draft, itemID: itemID)
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: draft,
            createdAt: now()
        )
        return try appendCanonicalAuthoringMutation(mutation)
    }

    /// Atomically designates and queues the exact reviewed onboarding item.
    /// The content remains solely in the encrypted authoring journal; the
    /// anchor carries only its opaque UUID until canonical reconciliation.
    @discardableResult
    func enqueueOnboardingFirstItemCreate(
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        guard draft.createsPlanningDemand(itemID: itemID),
              onboardingFirstItemAnchor == nil
                || onboardingFirstItemAnchor == .init(
                    itemID: itemID,
                    canonicalRevision: nil
                ) else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let priorAnchor = onboardingFirstItemAnchor
        onboardingFirstItemAnchor = .init(itemID: itemID, canonicalRevision: nil)
        do {
            return try enqueueCanonicalCreate(itemID: itemID, draft: draft)
        } catch {
            onboardingFirstItemAnchor = priorAnchor
            throw error
        }
    }

    @discardableResult
    func enqueueCanonicalReplace(
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringUserFence()
        guard let item = canonicalItem(id: itemID), item.deletedAt == nil else {
            throw PlannerCanonicalAuthoringError.itemNotFound
        }
        guard item.supportsCanonicalAuthoringReplacement else {
            throw PlannerCanonicalAuthoringError.unsupportedReplacement
        }
        try validateCanonicalAuthoringDraft(draft, itemID: itemID)
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .replace,
            draft: draft,
            expectedRevision: item.revision,
            baseItem: item,
            createdAt: now()
        )
        return try appendCanonicalAuthoringMutation(mutation)
    }

    @discardableResult
    func enqueueCanonicalMoveLater(
        blockID: UUID,
        earliestStart: Date,
        relaxCanonicalDeadlineTo: Date? = nil
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        guard earliestStart.timeIntervalSinceReferenceDate.isFinite,
              earliestStart > now(),
              let block = blocks.first(where: { $0.id == blockID }),
              block.status == .scheduled,
              block.isFlexible,
              !block.isHardConstraint,
              block.occurrenceID == nil,
              earliestStart > block.start,
              block.previewKind != "pinned",
              block.previewKind != "external_fixed",
              let itemID = block.sourceItemID,
              let itemRevision = block.sourceItemRevision,
              let item = canonicalItem(id: itemID),
              item.revision == itemRevision else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        var draft = DayWeaveCanonicalItemDraft(item: item)
        draft.earliestStartAt = max(draft.earliestStartAt ?? earliestStart, earliestStart)
        if let relaxCanonicalDeadlineTo {
            guard case let .valid(.some(boundary)) = item.moveLaterDeadlineAssessment,
                  boundary.isCanonicalField,
                  boundary.isHard,
                  boundary.date == item.deadlineAt,
                  relaxCanonicalDeadlineTo > boundary.date,
                  relaxCanonicalDeadlineTo > draft.earliestStartAt! else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            draft.deadlineAt = relaxCanonicalDeadlineTo
        }
        return try enqueueCanonicalReplace(itemID: itemID, draft: draft.normalized)
    }

    /// Durably records one recurring occurrence's shifted outer window. The
    /// visible blocks stay in place until the server validates, composes, and
    /// publishes the exception; no series-level item field is changed.
    @discardableResult
    func enqueueCanonicalOccurrenceMove(
        blockID: UUID,
        moveStart: Date
    ) throws -> RecurrenceOccurrenceMove {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerRecurrenceMoveError.encryptedPersistenceRequired
        }
        guard canMutatePlan,
              pendingSchedulePublication == nil,
              pendingProposalApplicationMutation == nil,
              googleOutboundRecoveryJournal == nil,
              !hasGoogleSchedulePublicationAuthorityFence,
              executionState.activeSession == nil,
              executionState.pendingCommand == nil else {
            throw PlannerRecurrenceMoveError.recoveryInProgress
        }
        guard let focused = blocks.first(where: { $0.id == blockID }),
              focused.status == .scheduled,
              focused.isFlexible,
              !focused.isHardConstraint,
              focused.previewKind != "pinned",
              focused.previewKind != "external_fixed",
              let itemID = focused.sourceItemID,
              let itemRevision = focused.sourceItemRevision,
              let occurrenceID = focused.occurrenceID,
              let seriesItemID = focused.recurrenceSeriesItemID ?? focused.sourceItemID,
              let source = focused.recurrenceMoveSource,
              let seriesItem = canonicalItem(id: seriesItemID),
              seriesItem.revision == source.itemRevision,
              source.canAuthorizeOccurrenceMove,
              let item = canonicalItem(id: itemID),
              item.revision == itemRevision,
              seriesItem.recurrence != nil,
              canonicalItem(itemID, belongsToSeries: seriesItemID),
              canonicalAuthoringMutation(itemID: seriesItemID) == nil,
              canMutate(focused),
              moveStart.timeIntervalSinceReferenceDate.isFinite,
              moveStart > now(),
              let shiftSeconds = Self.exactPositiveWholeSeconds(
                  from: focused.start,
                  to: moveStart
              ) else {
            throw PlannerRecurrenceMoveError.invalidOccurrence
        }
        let occurrenceBlocks = blocks.filter { $0.occurrenceID == occurrenceID }
        guard !occurrenceBlocks.isEmpty,
              occurrenceBlocks.allSatisfy({ block in
                  guard let blockItemID = block.sourceItemID,
                        let blockRevision = block.sourceItemRevision,
                        canonicalItem(id: blockItemID)?.revision == blockRevision else {
                      return false
                  }
                  return (block.recurrenceSeriesItemID ?? blockItemID) == seriesItemID
                      && block.recurrenceMoveSource == source
                      && canonicalItem(blockItemID, belongsToSeries: seriesItemID)
                      && canonicalAuthoringMutation(itemID: blockItemID) == nil
                      && block.status != .completed
                      && block.status != .skipped
                      && block.status != .canceled
              }) else {
            throw PlannerRecurrenceMoveError.partiallyResolvedOccurrence
        }
        guard occurrenceBlocks.allSatisfy({
            $0.status == .scheduled
                && $0.isFlexible
                && !$0.isHardConstraint
                && $0.previewKind != "pinned"
                && $0.previewKind != "external_fixed"
                && $0.occurrenceFullyScheduled
        }), let earliest = occurrenceBlocks.map(\.start).min(),
           let latest = occurrenceBlocks.map(\.end).max() else {
            throw PlannerRecurrenceMoveError.invalidOccurrence
        }
        let delta = TimeInterval(shiftSeconds)
        let move = RecurrenceOccurrenceMove(
            itemID: seriesItemID,
            occurrenceID: occurrenceID,
            startAt: earliest.addingTimeInterval(delta),
            endAt: latest.addingTimeInterval(delta),
            movedAt: Date(timeIntervalSince1970: now().timeIntervalSince1970.rounded(.down)),
            source: source
        )
        guard move.hasValidShape,
              let targetHorizon = try? scheduleProfile.expanded(asOf: move.startAt),
              move.startAt >= targetHorizon.horizonStart,
              move.endAt <= targetHorizon.horizonEnd else {
            throw PlannerRecurrenceMoveError.invalidOccurrence
        }
        guard recurrenceOccurrenceMoves.contains(where: {
            $0.occurrenceID == occurrenceID
        }) || recurrenceOccurrenceMoves.count < RecurrenceOccurrenceMove.maximumStoredCount else {
            throw PlannerRecurrenceMoveError.capacityReached
        }

        let priorMoves = recurrenceOccurrenceMoves
        let priorProof = publishedScheduleProof
        let priorValidation = isCanonicalPreviewValidatedForCurrentLaunch
        let priorMessage = lastScheduleMessage
        recurrenceOccurrenceMoves.removeAll { $0.occurrenceID == occurrenceID }
        recurrenceOccurrenceMoves.append(move)
        pruneRecurrenceHistory()
        guard recurrenceOccurrenceMoves.contains(move) else {
            recurrenceOccurrenceMoves = priorMoves
            throw PlannerRecurrenceMoveError.capacityReached
        }
        publishedScheduleProof = nil
        isCanonicalPreviewValidatedForCurrentLaunch = false
        lastScheduleMessage =
            "Move requested · the previous occurrence remains visible until server validation"
        flushPersistence()
        if let persistenceError {
            recurrenceOccurrenceMoves = priorMoves
            publishedScheduleProof = priorProof
            isCanonicalPreviewValidatedForCurrentLaunch = priorValidation
            lastScheduleMessage = priorMessage
            throw persistenceError
        }
        return move
    }

    @discardableResult
    func enqueueCanonicalTrash(
        itemID: UUID
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringUserFence()
        guard let item = canonicalItem(id: itemID), item.deletedAt == nil else {
            throw PlannerCanonicalAuthoringError.itemNotFound
        }
        guard !canonicalItems.contains(where: { $0.parentID == itemID && $0.deletedAt == nil }),
              !pendingCanonicalAuthoringMutations.contains(where: {
                  guard $0.disposition == .pending else { return false }
                  if $0.draft?.parentID == itemID { return true }
                  guard $0.operation == .restore else { return false }
                  return canonicalTrashEntry(id: $0.itemID)?.parentID == itemID
                      || $0.baseItem?.parentID == itemID
              }) else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .trash,
            expectedRevision: item.revision,
            baseItem: item,
            createdAt: now()
        )
        return try appendCanonicalAuthoringMutation(mutation)
    }

    @discardableResult
    func enqueueCanonicalRestore(
        itemID: UUID
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringUserFence()
        guard let entry = canonicalTrashEntry(id: itemID) else {
            throw PlannerCanonicalAuthoringError.trashEntryNotFound
        }
        if let parentID = entry.parentID,
           (!canonicalItems.contains(where: { $0.id == parentID && $0.deletedAt == nil })
            || canonicalAuthoringMutation(itemID: parentID)?.operation == .trash) {
                throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let deletedBase = entry.lastKnownItem.flatMap { $0.deletedAt == nil ? nil : $0 }
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .restore,
            expectedRevision: entry.revision,
            baseItem: deletedBase,
            createdAt: now()
        )
        return try appendCanonicalAuthoringMutation(mutation)
    }

    @discardableResult
    func updateCanonicalAuthoringDraft(
        _ mutationID: UUID,
        draft: DayWeaveCanonicalItemDraft
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringUserFence()
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        let prior = pendingCanonicalAuthoringMutations[index]
        guard !prior.hasBeenSubmitted,
              prior.operation == .create || prior.operation == .replace else {
            throw PlannerCanonicalAuthoringError.submittedMutationIsImmutable
        }
        if prior.operation == .create,
           onboardingFirstItemAnchor?.itemID == prior.itemID,
           onboardingFirstItemAnchor?.canonicalRevision == nil,
           !draft.createsPlanningDemand(itemID: prior.itemID) {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        try validateCanonicalAuthoringDraft(draft, itemID: prior.itemID)
        let replacement = DayWeavePendingCanonicalAuthoringMutation(
            id: prior.id,
            itemID: prior.itemID,
            operation: prior.operation,
            draft: draft,
            expectedRevision: prior.expectedRevision,
            baseItem: prior.baseItem,
            createdAt: prior.createdAt
        )
        guard PlannerCanonicalAuthoringJournalValidator.isValid(replacement) else {
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        pendingCanonicalAuthoringMutations[index] = replacement
        hardenPendingSensitivityPresentation()
        guard currentCanonicalAuthoringStateIsValid else {
            pendingCanonicalAuthoringMutations[index] = prior
            hardenPendingSensitivityPresentation()
            throw PlannerCanonicalAuthoringError.journalCapacityReached
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations[index] = prior
            throw error
        }
        return replacement
    }

    @discardableResult
    func bindCanonicalAuthoringMutation(
        _ mutationID: UUID,
        configurationIdentifier requestedIdentifier: String
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringSyncFence()
        guard let normalized = Self.canonicalConfigurationIdentifier(requestedIdentifier),
              normalized == requestedIdentifier else {
            throw PlannerCanonicalAuthoringError.invalidConfiguration
        }
        guard canonicalConfigurationIdentifier == nil
                || Self.canonicalConfigurationIdentifier(canonicalConfigurationIdentifier ?? "")
                    == normalized else {
            throw PlannerCanonicalAuthoringError.invalidConfiguration
        }
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        let priorMutation = pendingCanonicalAuthoringMutations[index]
        guard !priorMutation.hasBeenSubmitted,
              priorMutation.disposition == .pending else {
            throw PlannerCanonicalAuthoringError.submittedMutationIsImmutable
        }
        if let existing = priorMutation.configurationIdentifier {
            guard existing == normalized else {
                throw PlannerCanonicalAuthoringError.invalidConfiguration
            }
            return priorMutation
        }

        let priorConfiguration = canonicalConfigurationIdentifier
        canonicalConfigurationIdentifier = normalized
        pendingCanonicalAuthoringMutations[index].configurationIdentifier = normalized
        guard currentCanonicalAuthoringStateIsValid else {
            canonicalConfigurationIdentifier = priorConfiguration
            pendingCanonicalAuthoringMutations[index] = priorMutation
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            canonicalConfigurationIdentifier = priorConfiguration
            pendingCanonicalAuthoringMutations[index] = priorMutation
            throw error
        }
        return pendingCanonicalAuthoringMutations[index]
    }

    @discardableResult
    func markCanonicalAuthoringMutationSubmitted(
        _ mutationID: UUID
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringSyncFence()
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        let prior = pendingCanonicalAuthoringMutations[index]
        guard !prior.hasBeenSubmitted,
              prior.disposition == .pending else {
            throw PlannerCanonicalAuthoringError.submittedMutationIsImmutable
        }
        guard let binding = prior.configurationIdentifier,
              binding == canonicalConfigurationIdentifier else {
            throw PlannerCanonicalAuthoringError.invalidConfiguration
        }
        pendingCanonicalAuthoringMutations[index].hasBeenSubmitted = true
        guard currentCanonicalAuthoringStateIsValid else {
            pendingCanonicalAuthoringMutations[index] = prior
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations[index] = prior
            throw error
        }
        return pendingCanonicalAuthoringMutations[index]
    }

    @discardableResult
    func markCanonicalAuthoringMutationConflicted(
        _ mutationID: UUID,
        diagnostic: String
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringSyncFence()
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        guard let bounded = Self.boundedCanonicalAuthoringDiagnostic(diagnostic) else {
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        let prior = pendingCanonicalAuthoringMutations[index]
        pendingCanonicalAuthoringMutations[index].disposition = .conflicted
        pendingCanonicalAuthoringMutations[index].diagnostic = bounded
        guard currentCanonicalAuthoringStateIsValid else {
            pendingCanonicalAuthoringMutations[index] = prior
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations[index] = prior
            throw error
        }
        return pendingCanonicalAuthoringMutations[index]
    }

    func discardCanonicalAuthoringMutation(_ mutationID: UUID) throws {
        try requireCanonicalAuthoringUserFence()
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        let mutation = pendingCanonicalAuthoringMutations[index]
        guard !mutation.hasBeenSubmitted || mutation.disposition == .conflicted else {
            throw PlannerCanonicalAuthoringError.submittedMutationIsImmutable
        }
        let priorSelection = selectedCanonicalItemID
        let priorOnboardingFirstItemAnchor = onboardingFirstItemAnchor
        pendingCanonicalAuthoringMutations.remove(at: index)
        if mutation.operation == .create,
           onboardingFirstItemAnchor?.itemID == mutation.itemID,
           onboardingFirstItemAnchor?.canonicalRevision == nil {
            onboardingFirstItemAnchor = nil
        }
        reconcileSelectedCanonicalItem()
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations.insert(mutation, at: index)
            onboardingFirstItemAnchor = priorOnboardingFirstItemAnchor
            selectedCanonicalItemID = priorSelection
            throw error
        }
    }

    /// Preserves a conflicted submitted body while creating a fresh, editable
    /// standalone Inbox copy with a new item identity and idempotency key. The
    /// original conflict remains available until the user explicitly discards
    /// it, so recovery never destroys the only retained draft.
    @discardableResult
    func duplicateConflictedCanonicalDraft(
        _ mutationID: UUID,
        as newItemID: UUID = UUID()
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        try requireCanonicalAuthoringUserFence()
        guard let source = canonicalAuthoringMutation(id: mutationID),
              source.disposition == .conflicted,
              source.operation == .create || source.operation == .replace,
              var draft = source.draft else {
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        if canonicalItemRequiresSensitivePresentation(itemID: source.itemID) {
            // Detaching the copy from its ancestry must never downgrade an
            // inherited privacy boundary into an ordinary standalone item.
            draft.isSensitive = true
        }
        draft.status = .inbox
        draft.parentID = nil
        draft.siblingOrder = 0
        return try enqueueCanonicalCreate(itemID: newItemID, draft: draft)
    }

    func applyCanonicalAuthoringResponse(
        _ mutationID: UUID,
        item response: DayWeaveCanonicalItem
    ) throws {
        try requireCanonicalAuthoringSyncFence()
        guard let index = pendingCanonicalAuthoringMutations.firstIndex(where: {
            $0.id == mutationID
        }) else { throw PlannerCanonicalAuthoringError.mutationNotFound }
        let mutation = pendingCanonicalAuthoringMutations[index]
        guard mutation.hasBeenSubmitted,
              mutation.disposition == .pending,
              response.id == mutation.itemID,
              response.revision > 0 else {
            throw PlannerCanonicalAuthoringError.invalidRemoteResponse
        }
        let minimumRevision: UInt64
        switch mutation.operation {
        case .create:
            minimumRevision = 1
            guard response.deletedAt == nil,
                  response.supportsCanonicalAuthoringReplacement,
                  mutation.draft?.matches(response) == true else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
        case .replace:
            guard let expected = mutation.expectedRevision,
                  let draft = mutation.draft,
                  response.deletedAt == nil,
                  response.supportsCanonicalAuthoringReplacement,
                  draft.matches(response) else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            let next = expected.addingReportingOverflow(1)
            guard !next.overflow else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            minimumRevision = next.partialValue
        case .trash:
            guard let expected = mutation.expectedRevision,
                  response.deletedAt != nil,
                  mutation.baseItem.map({
                      $0.hasSameAuthoredContent(as: response)
                  }) ?? true else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            let next = expected.addingReportingOverflow(1)
            guard !next.overflow else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            minimumRevision = next.partialValue
        case .restore:
            guard let expected = mutation.expectedRevision,
                  response.deletedAt == nil,
                  mutation.baseItem.map({
                      $0.hasSameAuthoredContent(as: response)
                  }) ?? true else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            let next = expected.addingReportingOverflow(1)
            guard !next.overflow else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            minimumRevision = next.partialValue
        }
        guard response.revision >= minimumRevision else {
            throw PlannerCanonicalAuthoringError.invalidRemoteResponse
        }

        let priorItems = canonicalItems
        let priorTrash = canonicalTrash
        let priorTombstones = canonicalTombstoneRevisions
        let priorMutations = pendingCanonicalAuthoringMutations
        let priorOnboardingFirstItemAnchor = onboardingFirstItemAnchor
        let priorSelection = selectedCanonicalItemID

        switch mutation.operation {
        case .trash:
            guard (canonicalItems.first(where: { $0.id == response.id })?.revision ?? 0)
                    <= response.revision else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            canonicalItems.removeAll { $0.id == response.id }
            canonicalTombstoneRevisions[response.id] = max(
                canonicalTombstoneRevisions[response.id] ?? 0,
                response.revision
            )
            upsertCanonicalTrash(DayWeaveCanonicalTrashEntry(item: response))
        case .create, .replace, .restore:
            guard response.revision > (canonicalTombstoneRevisions[response.id] ?? 0) else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
            if let current = canonicalItems.first(where: { $0.id == response.id }),
               current.revision > response.revision {
                guard mutation.operation == .restore
                        || mutation.draft?.matches(current) == true else {
                    throw PlannerCanonicalAuthoringError.invalidRemoteResponse
                }
            } else {
                canonicalItems.removeAll { $0.id == response.id }
                canonicalItems.append(response)
                canonicalItems = Self.hierarchicallySorted(canonicalItems)
            }
            canonicalTombstoneRevisions.removeValue(forKey: response.id)
            canonicalTrash.removeAll { $0.id == response.id }
        }
        pendingCanonicalAuthoringMutations.remove(at: index)
        if onboardingFirstItemAnchor?.itemID == response.id {
            switch mutation.operation {
            case .create, .replace, .restore:
                onboardingFirstItemAnchor = .init(
                    itemID: response.id,
                    canonicalRevision: canonicalItem(id: response.id)?.revision
                        ?? response.revision
                )
            case .trash:
                onboardingFirstItemAnchor = nil
            }
        }
        selectedCanonicalItemID = response.id
        guard currentCanonicalAuthoringStateIsValid else {
            canonicalItems = priorItems
            canonicalTrash = priorTrash
            canonicalTombstoneRevisions = priorTombstones
            pendingCanonicalAuthoringMutations = priorMutations
            onboardingFirstItemAnchor = priorOnboardingFirstItemAnchor
            selectedCanonicalItemID = priorSelection
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            canonicalItems = priorItems
            canonicalTrash = priorTrash
            canonicalTombstoneRevisions = priorTombstones
            pendingCanonicalAuthoringMutations = priorMutations
            onboardingFirstItemAnchor = priorOnboardingFirstItemAnchor
            selectedCanonicalItemID = priorSelection
            throw error
        }
    }

    private func requireCanonicalAuthoringPersistence(
        allowDuringExecution: Bool = false
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerCanonicalAuthoringError.encryptedPersistenceRequired
        }
        guard pendingProposalApplicationMutation == nil,
              pendingSchedulePublication == nil,
              pendingExecutionDeferIntent == nil else {
            throw PlannerCanonicalAuthoringError.mutationFenceActive
        }
        if !allowDuringExecution {
            guard executionState.activeSession == nil,
                  executionState.pendingCommand == nil else {
                throw PlannerCanonicalAuthoringError.activeExecution
            }
        }
    }

    private func requireCanonicalAuthoringUserFence(
        allowDuringExecution: Bool = false
    ) throws {
        try requireCanonicalAuthoringPersistence(
            allowDuringExecution: allowDuringExecution
        )
        guard canMutatePlan else {
            throw PlannerCanonicalAuthoringError.mutationFenceActive
        }
    }

    private func requireCanonicalAuthoringSyncFence() throws {
        try requireCanonicalAuthoringPersistence()
        guard isCanonicalSyncLocked else {
            throw PlannerCanonicalAuthoringError.mutationFenceActive
        }
    }

    private func validateCanonicalAuthoringDraft(
        _ draft: DayWeaveCanonicalItemDraft,
        itemID: UUID
    ) throws {
        guard draft.validationIssue(itemID: itemID) == nil,
              canonicalAuthoringDraftHierarchyIsCurrent(
                  draft,
                  itemID: itemID,
                  requiresCommittedParent: false
              ) else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
    }

    func canonicalAuthoringDraftHierarchyIsCurrent(
        _ draft: DayWeaveCanonicalItemDraft,
        itemID: UUID,
        requiresCommittedParent: Bool
    ) -> Bool {
        guard !wouldCreateCanonicalHierarchyCycle(
            itemID: itemID,
            parentID: draft.parentID
        ) else { return false }
        guard let parentID = draft.parentID else { return true }

        let parentStatus: DayWeaveCanonicalItemStatus?
        if requiresCommittedParent {
            parentStatus = canonicalItem(id: parentID)?.status
        } else if let mutation = canonicalAuthoringMutation(itemID: parentID) {
            guard mutation.disposition == .pending else { return false }
            switch mutation.operation {
            case .create, .replace:
                parentStatus = mutation.draft?.status
            case .restore:
                parentStatus = mutation.baseItem?.status
                    ?? canonicalTrashEntry(id: parentID)?.lastKnownItem?.status
            case .trash:
                return false
            }
        } else {
            parentStatus = canonicalItem(id: parentID)?.status
        }
        return parentStatus == .inbox || parentStatus == .planned
    }

    private func wouldCreateCanonicalHierarchyCycle(
        itemID: UUID,
        parentID: UUID?
    ) -> Bool {
        guard let parentID else { return false }
        var parentByID = Dictionary(uniqueKeysWithValues: canonicalItems.compactMap { item in
            item.parentID.map { (item.id, $0) }
        })
        var knownIDs = Set(canonicalItems.map(\.id))
        for mutation in pendingCanonicalAuthoringMutations {
            knownIDs.insert(mutation.itemID)
            if let parent = mutation.draft?.parentID {
                parentByID[mutation.itemID] = parent
            } else if mutation.draft != nil {
                parentByID.removeValue(forKey: mutation.itemID)
            }
        }
        knownIDs.insert(itemID)
        parentByID[itemID] = parentID
        guard knownIDs.contains(parentID) else { return true }

        var visited = Set([itemID])
        var current: UUID? = parentID
        while let candidate = current {
            guard visited.insert(candidate).inserted else { return true }
            current = parentByID[candidate]
        }
        return false
    }

    private func appendCanonicalAuthoringMutation(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        guard canonicalAuthoringMutation(itemID: mutation.itemID) == nil,
              pendingCanonicalMutations.allSatisfy({ $0.itemID != mutation.itemID }),
              pendingCanonicalSensitivityMutations.allSatisfy({
                  $0.itemID != mutation.itemID
              }) else {
            throw PlannerCanonicalAuthoringError.duplicateItemOperation
        }
        guard PlannerCanonicalAuthoringJournalValidator.isValid(mutation) else {
            throw PlannerCanonicalAuthoringError.invalidMutation
        }
        let priorSelection = selectedCanonicalItemID
        pendingCanonicalAuthoringMutations.append(mutation)
        selectedCanonicalItemID = mutation.itemID
        hardenPendingSensitivityPresentation()
        guard currentCanonicalAuthoringStateIsValid else {
            pendingCanonicalAuthoringMutations.removeAll { $0.id == mutation.id }
            selectedCanonicalItemID = priorSelection
            throw PlannerCanonicalAuthoringError.journalCapacityReached
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations.removeAll { $0.id == mutation.id }
            selectedCanonicalItemID = priorSelection
            throw error
        }
        return mutation
    }

    private var currentCanonicalAuthoringStateIsValid: Bool {
        PlannerCanonicalAuthoringJournalValidator.isValidState(
            mutations: pendingCanonicalAuthoringMutations,
            trash: canonicalTrash,
            canonicalItems: canonicalItems,
            tombstoneRevisions: canonicalTombstoneRevisions,
            configurationIdentifier: canonicalConfigurationIdentifier
        )
    }

    private func flushCanonicalAuthoringTransition() throws {
        if let persistence {
            let retentionReferenceDate = now()
            let boundedMutations = Self.boundedCanonicalAuthoringMutations(
                pendingCanonicalAuthoringMutations,
                referenceDate: retentionReferenceDate
            )
            let boundedTrash = Self.boundedCanonicalTrash(
                canonicalTrash,
                referenceDate: retentionReferenceDate,
                pinnedItemIDs: Self.canonicalRecoveryPinnedItemIDs(boundedMutations)
            )
            try persistence.preflightSave(makeSnapshot(
                canonicalTrashOverride: boundedTrash,
                canonicalAuthoringMutationsOverride: boundedMutations
            ))
        }
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    private static func boundedCanonicalAuthoringDiagnostic(
        _ diagnostic: String
    ) -> String? {
        let source = diagnostic.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !source.isEmpty else { return nil }
        var result = ""
        var usedBytes = 0
        for scalar in source.unicodeScalars {
            let value = scalar.value
            let byteCount: Int = switch value {
            case 0...0x7F: 1
            case 0x80...0x7FF: 2
            case 0x800...0xFFFF: 3
            default: 4
            }
            guard usedBytes + byteCount
                    <= PlannerCanonicalAuthoringJournalValidator.maximumDiagnosticBytes else {
                break
            }
            result.unicodeScalars.append(scalar)
            usedBytes += byteCount
        }
        return result.isEmpty ? nil : result
    }

    private func upsertCanonicalTrash(_ entry: DayWeaveCanonicalTrashEntry) {
        if let index = canonicalTrash.firstIndex(where: { $0.id == entry.id }) {
            guard canonicalTrash[index].revision <= entry.revision else { return }
            canonicalTrash[index] = entry
        } else {
            canonicalTrash.append(entry)
        }
        canonicalTrash = Self.boundedCanonicalTrash(
            canonicalTrash,
            referenceDate: now(),
            pinnedItemIDs: pendingCanonicalRecoveryItemIDs
        )
    }

    private static func sortedCanonicalTrash(
        _ entries: [DayWeaveCanonicalTrashEntry]
    ) -> [DayWeaveCanonicalTrashEntry] {
        entries.sorted { left, right in
            if left.deletedAt != right.deletedAt { return left.deletedAt > right.deletedAt }
            return left.id.uuidString < right.id.uuidString
        }
    }

    /// Trash and restore requests need only item identity, expected revision,
    /// and their stable idempotency key. A full base body improves short-term
    /// response validation, but must not outlive the same thirty-day privacy
    /// boundary as Recently Deleted. Rebuilding with the same mutation ID
    /// preserves the exact request identity and every sync/conflict fence.
    private static func boundedCanonicalAuthoringMutations(
        _ mutations: [DayWeavePendingCanonicalAuthoringMutation],
        referenceDate: Date
    ) -> [DayWeavePendingCanonicalAuthoringMutation] {
        let cutoff = referenceDate.addingTimeInterval(-canonicalTrashRetentionInterval)
        return mutations.map { mutation in
            guard let baseItem = mutation.baseItem else { return mutation }
            let retentionAnchor: Date
            switch mutation.operation {
            case .trash:
                retentionAnchor = mutation.createdAt
            case .restore:
                guard let deletedAt = baseItem.deletedAt else { return mutation }
                // A remote future timestamp cannot postpone local expiry.
                retentionAnchor = min(deletedAt, mutation.createdAt)
            case .create, .replace:
                return mutation
            }
            guard retentionAnchor <= cutoff else { return mutation }
            return DayWeavePendingCanonicalAuthoringMutation(
                id: mutation.id,
                itemID: mutation.itemID,
                operation: mutation.operation,
                draft: nil,
                expectedRevision: mutation.expectedRevision,
                baseItem: nil,
                createdAt: mutation.createdAt,
                configurationIdentifier: mutation.configurationIdentifier,
                hasBeenSubmitted: mutation.hasBeenSubmitted,
                disposition: mutation.disposition,
                diagnostic: mutation.diagnostic
            )
        }
    }

    /// Recently deleted metadata is useful for recovery, but retaining every
    /// full item body forever lets ordinary deletion history exhaust the
    /// encrypted snapshot. Keep thirty days of bounded metadata and retain full
    /// bodies newest-first within a separate byte budget. Tombstone revision
    /// watermarks remain independent, so pruning can never resurrect an item.
    private static func boundedCanonicalTrash(
        _ entries: [DayWeaveCanonicalTrashEntry],
        referenceDate: Date,
        pinnedItemIDs: Set<UUID>
    ) -> [DayWeaveCanonicalTrashEntry] {
        let cutoff = referenceDate.addingTimeInterval(-canonicalTrashRetentionInterval)
        let encoder = JSONEncoder()
        var seen = Set<UUID>()
        let locallyAnchored = entries.map {
            $0.clampingDeletedAt(to: referenceDate)
        }
        let sorted = sortedCanonicalTrash(locallyAnchored).filter {
            seen.insert($0.id).inserted
        }
        let pinned = sorted.filter { pinnedItemIDs.contains($0.id) }
            .prefix(maximumCanonicalTrashEntries)
        let availableUnpinnedSlots = maximumCanonicalTrashEntries - pinned.count
        let recentUnpinned = sorted.filter {
            !pinnedItemIDs.contains($0.id) && $0.deletedAt > cutoff
        }.prefix(availableUnpinnedSlots)
        let candidates = sortedCanonicalTrash(Array(pinned) + Array(recentUnpinned))
        var retainedBodyBytes = 0
        var result: [DayWeaveCanonicalTrashEntry] = []
        result.reserveCapacity(candidates.count)

        for source in candidates {
            guard source.deletedAt > cutoff,
                  let item = source.lastKnownItem,
                  let bodyBytes = try? encoder.encode(item).count,
                  bodyBytes <= maximumCanonicalTrashItemBytes,
                  bodyBytes <= maximumCanonicalTrashRetainedItemBytes - retainedBodyBytes else {
                result.append(source.withoutRetainedItemBody)
                continue
            }
            result.append(source)
            retainedBodyBytes += bodyBytes
        }
        return result
    }

    private static func canonicalRecoveryPinnedItemIDs(
        _ mutations: [DayWeavePendingCanonicalAuthoringMutation]
    ) -> Set<UUID> {
        Set(mutations.compactMap { mutation in
            if mutation.operation == .restore
                || (mutation.operation == .trash && mutation.hasBeenSubmitted) {
                return mutation.itemID
            }
            return nil
        })
    }

    private var pendingCanonicalRecoveryItemIDs: Set<UUID> {
        Self.canonicalRecoveryPinnedItemIDs(pendingCanonicalAuthoringMutations)
    }

    private func reconcileSelectedCanonicalItem() {
        guard let selectedCanonicalItemID else { return }
        let stillExists = canonicalItems.contains { $0.id == selectedCanonicalItemID }
            || canonicalTrash.contains { $0.id == selectedCanonicalItemID }
            || pendingCanonicalAuthoringMutations.contains {
                $0.itemID == selectedCanonicalItemID
            }
        if !stillExists { self.selectedCanonicalItemID = nil }
    }

    func persistPendingSchedulePublication(
        _ publication: PendingSchedulePublication
    ) throws {
        guard canPersistPlan,
              publication.version == PendingSchedulePublication.currentVersion,
              publication.isWithinEncodedSizeLimit,
              publication.configurationIdentifier == canonicalConfigurationIdentifier,
              publication.provenance.configurationIdentifier
                == publication.configurationIdentifier else {
            throw PlannerSchedulePublicationError.invalidJournal
        }
        guard pendingSchedulePublication == nil else {
            throw PlannerSchedulePublicationError.publicationAlreadyPending
        }
        pendingSchedulePublication = publication
        pendingPublicationDeferredSessionIDs = deferredExecutionPublicationSessionIDs
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    func commitPendingSchedulePublication(
        _ publication: PendingSchedulePublication,
        blocks newBlocks: [ScheduleBlock],
        response: DayWeaveSchedulePublishResponse
    ) throws {
        guard pendingSchedulePublication == publication else {
            throw PlannerSchedulePublicationError.publicationDoesNotMatchJournal
        }
        guard !response.replayed else {
            throw PlannerSchedulePublicationError.replayedReceiptCannotAuthorize
        }
        guard let publicationProof = DayWeavePublishedScheduleProof(
            publication: publication,
            revision: response.revision,
            renderedBlocks: newBlocks
        ), publicationProof.hasCurrentImmutablePlanSeal,
           publicationProof.matchesPublishedPlan(newBlocks) else {
            throw PlannerSchedulePublicationError.invalidJournal
        }
        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorProvenance = schedulePreviewProvenance
        let priorPublishedScheduleProof = publishedScheduleProof
        let priorLocalProvenance = localScheduleCompositionProvenance
        let priorExecutionState = executionState
        let priorDeferredPublicationSessionIDs = deferredExecutionPublicationSessionIDs
        let priorPendingDeferredSessionIDs = pendingPublicationDeferredSessionIDs
        let priorMessage = lastScheduleMessage

        applySchedulePreviewInMemory(
            blocks: newBlocks,
            message: publication.message,
            provenance: publication.provenance
        )
        publishedScheduleProof = publicationProof
        reconcileOutstandingDeferredPublicationProof(
            authorizedSessionIDs: pendingPublicationDeferredSessionIDs
        )
        pendingPublicationDeferredSessionIDs = []
        pendingSchedulePublication = nil
        flushPersistence()
        if let persistenceError {
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorProvenance
            publishedScheduleProof = priorPublishedScheduleProof
            localScheduleCompositionProvenance = priorLocalProvenance
            executionState = priorExecutionState
            deferredExecutionPublicationSessionIDs = priorDeferredPublicationSessionIDs
            pendingPublicationDeferredSessionIDs = priorPendingDeferredSessionIDs
            lastScheduleMessage = priorMessage
            pendingSchedulePublication = publication
            // A failed atomic local commit must never leave either the prior
            // or newly published projection actionable in this process.
            isCanonicalPreviewValidatedForCurrentLaunch = false
            throw persistenceError
        }
    }

    /// Atomically installs an authoritative native replica into the encrypted
    /// planner snapshot. The caller must hold the shared canonical mutation
    /// fence and must have validated the complete public schedule against the
    /// current canonical cache before entering this transaction.
    func installCurrentPublishedSchedule(
        _ publication: DayWeaveCurrentPublishedSchedule,
        blocks newBlocks: [ScheduleBlock],
        configurationIdentifier: String,
        message: String
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerScheduleReplicaError.encryptedPersistenceRequired
        }
        guard isCanonicalSyncLocked else {
            throw PlannerScheduleReplicaError.mutationFenceUnavailable
        }
        guard pendingSchedulePublication == nil else {
            throw PlannerScheduleReplicaError.publicationRecoveryPending
        }
        guard canonicalConfigurationIdentifier == configurationIdentifier else {
            throw PlannerScheduleReplicaError.configurationMismatch
        }
        let currentRevisions = Dictionary(
            uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
        )
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configurationIdentifier,
            // Poll observation time must never renew execution authority for
            // an old immutable publication. `as_of` is the composition clock
            // sealed by the current resource and durable publication proof.
            generatedAt: publication.schedule.plan.asOf,
            asOf: publication.schedule.plan.asOf,
            horizonStart: publication.schedule.plan.horizonStart,
            horizonEnd: publication.schedule.plan.horizonEnd,
            timezoneName: publication.revision.timezoneName
        )
        guard publication.schedule.sourceItemRevisions == currentRevisions,
              let proof = DayWeavePublishedScheduleProof(
                  current: publication,
                  configurationIdentifier: configurationIdentifier,
                  renderedBlocks: newBlocks
              ),
              proof.hasCurrentImmutablePlanSeal,
              proof.matches(provenance),
              proof.matchesPublishedPlan(newBlocks) else {
            throw PlannerScheduleReplicaError.invalidPublication
        }

        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorProvenance = schedulePreviewProvenance
        let priorProof = publishedScheduleProof
        let priorLocalProvenance = localScheduleCompositionProvenance
        let priorExecutionState = executionState
        let priorDeferredSessionIDs = deferredExecutionPublicationSessionIDs
        let priorMessage = lastScheduleMessage

        applySchedulePreviewInMemory(
            blocks: newBlocks,
            message: message,
            provenance: provenance
        )
        publishedScheduleProof = proof
        reconcileOutstandingDeferredPublicationProof(
            authorizedSessionIDs: deferredExecutionPublicationSessionIDs
        )
        flushPersistence()
        if let persistenceError {
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorProvenance
            publishedScheduleProof = priorProof
            localScheduleCompositionProvenance = priorLocalProvenance
            executionState = priorExecutionState
            deferredExecutionPublicationSessionIDs = priorDeferredSessionIDs
            lastScheduleMessage = priorMessage
            // A failed CAS cannot leave either projection actionable.
            isCanonicalPreviewValidatedForCurrentLaunch = false
            throw persistenceError
        }
    }

    /// Applies the current endpoint's exact typed absence without disturbing
    /// local-only captures or an on-device composition. Generic 404 responses
    /// never reach this method.
    func clearCurrentPublishedSchedule(
        configurationIdentifier: String
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerScheduleReplicaError.encryptedPersistenceRequired
        }
        guard isCanonicalSyncLocked else {
            throw PlannerScheduleReplicaError.mutationFenceUnavailable
        }
        guard pendingSchedulePublication == nil else {
            throw PlannerScheduleReplicaError.publicationRecoveryPending
        }
        guard canonicalConfigurationIdentifier == configurationIdentifier else {
            throw PlannerScheduleReplicaError.configurationMismatch
        }
        guard schedulePreviewProvenance != nil || publishedScheduleProof != nil else { return }

        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorProvenance = schedulePreviewProvenance
        let priorProof = publishedScheduleProof
        let priorMessage = lastScheduleMessage

        blocks.removeAll {
            $0.syncOrigin == .canonicalPreview || $0.syncOrigin == .externalPreview
        }
        schedulePreviewProvenance = nil
        publishedScheduleProof = nil
        isCanonicalPreviewValidatedForCurrentLaunch = localScheduleCompositionProvenance != nil
        selectedBlockID = blocks.first(where: { $0.id == priorSelection })?.id ?? blocks.first?.id
        lastScheduleMessage = "No schedule is currently published on this workspace"
        flushPersistence()
        if let persistenceError {
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorProvenance
            publishedScheduleProof = priorProof
            lastScheduleMessage = priorMessage
            isCanonicalPreviewValidatedForCurrentLaunch = false
            throw persistenceError
        }
    }

    /// Installs a helper-produced plan and its local evidence as one encrypted
    /// transition. It never creates, clears, or recovers the server publication
    /// journal.
    func commitLocalScheduleComposition(
        blocks newBlocks: [ScheduleBlock],
        message: String,
        provenance: LocalScheduleCompositionProvenance
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerLocalCompositionError.encryptedPersistenceRequired
        }
        guard isCanonicalSyncLocked, pendingSchedulePublication == nil else {
            throw PlannerLocalCompositionError.mutationFenceUnavailable
        }
        let currentRevisions = Dictionary(
            uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
        )
        guard provenance.hasValidShape,
              provenance.configurationIdentifier == canonicalConfigurationIdentifier,
              provenance.sourceItemRevisions == currentRevisions,
              newBlocks.allSatisfy({ block in
                  guard block.syncOrigin == .localComposition else { return false }
                  guard let itemID = block.sourceItemID else { return true }
                  return block.sourceItemRevision == currentRevisions[itemID]
              }) else {
            throw PlannerLocalCompositionError.invalidProvenance
        }

        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorServerProvenance = schedulePreviewProvenance
        let priorPublishedScheduleProof = publishedScheduleProof
        let priorLocalProvenance = localScheduleCompositionProvenance
        let priorExecutionState = executionState
        let priorMessage = lastScheduleMessage

        applyLocalScheduleCompositionInMemory(
            blocks: newBlocks,
            message: message,
            provenance: provenance
        )
        flushPersistence()
        if let persistenceError {
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorServerProvenance
            publishedScheduleProof = priorPublishedScheduleProof
            localScheduleCompositionProvenance = priorLocalProvenance
            executionState = priorExecutionState
            lastScheduleMessage = priorMessage
            isCanonicalPreviewValidatedForCurrentLaunch = false
            throw persistenceError
        }
    }

    /// Resolves an exact publication journal after a response proves that its
    /// candidate must not become the actionable local plan. This is used for a
    /// possibly-superseded idempotent receipt and for the server's explicit
    /// no-side-effect stale-composition result. The prior projection remains
    /// visible but launch-invalid until a fresh publication succeeds.
    func clearPendingSchedulePublicationWithoutApplying(
        _ publication: PendingSchedulePublication
    ) throws {
        guard pendingSchedulePublication == publication else {
            throw PlannerSchedulePublicationError.publicationDoesNotMatchJournal
        }
        let priorPublishedScheduleProof = publishedScheduleProof
        let priorPendingDeferredSessionIDs = pendingPublicationDeferredSessionIDs
        pendingSchedulePublication = nil
        pendingPublicationDeferredSessionIDs = []
        publishedScheduleProof = nil
        isCanonicalPreviewValidatedForCurrentLaunch = false
        flushPersistence()
        if let persistenceError {
            pendingSchedulePublication = publication
            pendingPublicationDeferredSessionIDs = priorPendingDeferredSessionIDs
            publishedScheduleProof = priorPublishedScheduleProof
            // Never make the prior or candidate projection actionable when the
            // atomic acknowledgement could not be persisted.
            isCanonicalPreviewValidatedForCurrentLaunch = false
            throw persistenceError
        }
    }

    /// Flushes the complete apply/undo request and its retry key before the
    /// caller may send any bytes. Exactly one ambiguous proposal mutation is
    /// retained because application and undo share one canonical write fence.
    func persistPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerProposalApplicationJournalError.encryptedPersistenceRequired
        }
        guard canMutatePlan else {
            throw PlannerProposalApplicationJournalError.remoteCanonicalMutationInProgress
        }
        guard PlannerProposalApplicationJournalValidator.isValid(mutation),
              proposalApplicationConfigurationMatches(mutation.configurationIdentifier),
              PlannerProposalApplicationJournalValidator.isValidState(
                  pending: mutation,
                  receipts: proposalApplicationReceipts
              ) else {
            throw PlannerProposalApplicationJournalError.invalidMutation
        }
        guard pendingProposalApplicationMutation == nil else {
            throw PlannerProposalApplicationJournalError.operationAlreadyPending
        }

        pendingProposalApplicationMutation = mutation
        flushPersistence()
        if let persistenceError {
            pendingProposalApplicationMutation = nil
            throw persistenceError
        }
    }

    /// Atomically replaces an exact pending request with its content-free
    /// receipt. A lost response can therefore be recovered with the same
    /// mutation and committed through this path after a receipt lookup.
    func commitPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation,
        receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerProposalApplicationJournalError.encryptedPersistenceRequired
        }
        guard pendingProposalApplicationMutation == mutation else {
            throw PlannerProposalApplicationJournalError.mutationDoesNotMatchJournal
        }
        guard proposalApplicationReceipt(receipt, matches: mutation) else {
            throw PlannerProposalApplicationJournalError.invalidReceipt
        }

        let priorReceipts = proposalApplicationReceipts
        let nextReceipts = try Self.receiptsByRecording(
            receipt,
            in: proposalApplicationReceipts
        )
        guard PlannerProposalApplicationJournalValidator.isValidState(
            pending: nil,
            receipts: nextReceipts
        ) else {
            throw PlannerProposalApplicationJournalError.invalidReceipt
        }

        pendingProposalApplicationMutation = nil
        proposalApplicationReceipts = nextReceipts
        flushPersistence()
        if let persistenceError {
            pendingProposalApplicationMutation = mutation
            proposalApplicationReceipts = priorReceipts
            throw persistenceError
        }
    }

    /// Clears a journal only after the caller has authoritative evidence that
    /// the exact request had no effect. Transport failure alone is not enough.
    func clearPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerProposalApplicationJournalError.encryptedPersistenceRequired
        }
        guard pendingProposalApplicationMutation == mutation else {
            throw PlannerProposalApplicationJournalError.mutationDoesNotMatchJournal
        }

        pendingProposalApplicationMutation = nil
        flushPersistence()
        if let persistenceError {
            pendingProposalApplicationMutation = mutation
            throw persistenceError
        }
    }

    /// Retains a recovered application receipt when no exact local mutation is
    /// outstanding. Receipts are monotonic and bounded newest-first.
    func recordProposalApplicationReceipt(
        _ receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerProposalApplicationJournalError.encryptedPersistenceRequired
        }
        guard PlannerProposalApplicationJournalValidator.isValid(receipt),
              proposalApplicationConfigurationMatches(receipt.configurationIdentifier) else {
            throw PlannerProposalApplicationJournalError.invalidReceipt
        }

        let priorReceipts = proposalApplicationReceipts
        let nextReceipts = try Self.receiptsByRecording(
            receipt,
            in: proposalApplicationReceipts
        )
        guard PlannerProposalApplicationJournalValidator.isValidState(
            pending: pendingProposalApplicationMutation,
            receipts: nextReceipts
        ) else {
            throw PlannerProposalApplicationJournalError.receiptConflict
        }
        guard nextReceipts != priorReceipts else { return }

        proposalApplicationReceipts = nextReceipts
        flushPersistence()
        if let persistenceError {
            proposalApplicationReceipts = priorReceipts
            throw persistenceError
        }
    }

    func proposalApplicationReceipt(
        for proposalID: UUID,
        configurationIdentifier: String
    ) -> DayWeaveStoredProposalApplicationReceipt? {
        proposalApplicationReceipts.first {
            $0.configurationIdentifier == configurationIdentifier
                && $0.application.contains(proposalID: proposalID)
        }
    }

    func proposalApplicationReceipt(
        applicationID: UUID,
        configurationIdentifier: String
    ) -> DayWeaveStoredProposalApplicationReceipt? {
        proposalApplicationReceipts.first {
            $0.configurationIdentifier == configurationIdentifier
                && $0.application.applicationID == applicationID
        }
    }

    private func proposalApplicationConfigurationMatches(_ value: String) -> Bool {
        guard let normalized = Self.canonicalConfigurationIdentifier(value),
              normalized == value else {
            return false
        }
        if let canonicalConfigurationIdentifier,
           Self.canonicalConfigurationIdentifier(canonicalConfigurationIdentifier)
            != normalized {
            return false
        }
        let retained = Set(proposalApplicationReceipts.map(\.configurationIdentifier))
            .union(pendingProposalApplicationMutation.map { [$0.configurationIdentifier] } ?? [])
        return retained.isEmpty || retained == [value]
    }

    private func proposalApplicationReceipt(
        _ receipt: DayWeaveStoredProposalApplicationReceipt,
        matches mutation: DayWeavePendingProposalApplicationMutation
    ) -> Bool {
        guard PlannerProposalApplicationJournalValidator.isValid(receipt),
              receipt.configurationIdentifier == mutation.configurationIdentifier,
              receipt.application.proposals.map(\.proposalID) == mutation.proposalIDs,
              receipt.application.commandIDs == mutation.expectedCommandIDs else {
            return false
        }
        switch mutation.operation {
        case .apply:
            let appliedRevisions = mutation.proposalRevisions.map {
                $0.addingReportingOverflow(1)
            }
            let isResolvedApplication =
                (receipt.application.status == .applied
                    && receipt.application.applicationRevision == 1)
                || (receipt.application.status == .undone
                    && receipt.application.applicationRevision == 2)
            return isResolvedApplication
                && appliedRevisions.allSatisfy { !$0.overflow }
                && receipt.application.proposals.map(\.appliedRevision)
                    == appliedRevisions.map(\.partialValue)
        case .undo:
            guard let applicationID = mutation.applicationID,
                  let expectedRevision = mutation.expectedApplicationRevision else {
                return false
            }
            let nextRevision = expectedRevision.addingReportingOverflow(1)
            return receipt.application.applicationID == applicationID
                && receipt.application.status == .undone
                && !nextRevision.overflow
                && receipt.application.applicationRevision == nextRevision.partialValue
                && receipt.application.proposals.map(\.appliedRevision)
                    == mutation.proposalRevisions
        }
    }

    private static func receiptsByRecording(
        _ receipt: DayWeaveStoredProposalApplicationReceipt,
        in current: [DayWeaveStoredProposalApplicationReceipt]
    ) throws -> [DayWeaveStoredProposalApplicationReceipt] {
        guard PlannerProposalApplicationJournalValidator.isValid(receipt) else {
            throw PlannerProposalApplicationJournalError.invalidReceipt
        }
        var next = current
        if let index = next.firstIndex(where: { $0.id == receipt.id }) {
            let prior = next[index]
            guard prior.configurationIdentifier == receipt.configurationIdentifier else {
                throw PlannerProposalApplicationJournalError.receiptConflict
            }
            if prior == receipt { return next }
            guard prior.application.status == .applied,
                  prior.application.applicationRevision == 1,
                  receipt.application.status == .undone,
                  receipt.application.applicationRevision == 2,
                  prior.application.proposals == receipt.application.proposals,
                  prior.application.commandIDs == receipt.application.commandIDs,
                  prior.application.affectedItemIDs == receipt.application.affectedItemIDs,
                  prior.application.appliedAt == receipt.application.appliedAt,
                  prior.application.undoExpiresAt == receipt.application.undoExpiresAt else {
                throw PlannerProposalApplicationJournalError.receiptConflict
            }
            next[index] = receipt
        } else {
            let proposalIDs = Set(receipt.application.proposals.map(\.proposalID))
            guard current.allSatisfy({ stored in
                proposalIDs.isDisjoint(with: stored.application.proposals.map(\.proposalID))
            }) else {
                throw PlannerProposalApplicationJournalError.receiptConflict
            }
            next.append(receipt)
        }
        return Array(
            sortedProposalApplicationReceipts(next)
                .prefix(PlannerProposalApplicationJournalValidator.maximumStoredReceipts)
        )
    }

    private static func sortedProposalApplicationReceipts(
        _ receipts: [DayWeaveStoredProposalApplicationReceipt]
    ) -> [DayWeaveStoredProposalApplicationReceipt] {
        receipts.sorted { left, right in
            let leftDate = left.application.undoneAt ?? left.application.appliedAt
            let rightDate = right.application.undoneAt ?? right.application.appliedAt
            if leftDate != rightDate { return leftDate > rightDate }
            return left.id.uuidString < right.id.uuidString
        }
    }

    private func applySchedulePreviewInMemory(
        blocks newBlocks: [ScheduleBlock],
        message: String,
        provenance: SchedulePreviewProvenance
    ) {
        applyComposedScheduleBlocksInMemory(blocks: newBlocks, message: message)
        for index in blocks.indices
            where blocks[index].sourceItemID != nil
                && blocks[index].syncOrigin != .remoteExecutionLease {
            blocks[index].syncOrigin = .canonicalPreview
        }
        schedulePreviewProvenance = provenance
        publishedScheduleProof = nil
        localScheduleCompositionProvenance = nil
        isCanonicalPreviewValidatedForCurrentLaunch = true
    }

    private func applyLocalScheduleCompositionInMemory(
        blocks newBlocks: [ScheduleBlock],
        message: String,
        provenance: LocalScheduleCompositionProvenance
    ) {
        applyComposedScheduleBlocksInMemory(blocks: newBlocks, message: message)
        for index in blocks.indices
            where blocks[index].sourceItemID != nil
                && blocks[index].syncOrigin != .remoteExecutionLease {
            blocks[index].syncOrigin = .localComposition
        }
        schedulePreviewProvenance = nil
        publishedScheduleProof = nil
        localScheduleCompositionProvenance = provenance
        isCanonicalPreviewValidatedForCurrentLaunch = true
    }

    private func applyComposedScheduleBlocksInMemory(
        blocks newBlocks: [ScheduleBlock],
        message: String
    ) {
        guard canPersistPlan else { return }
        let previousSelection = selectedBlockID
        let selectedSourceID = selectedBlock?.sourceItemID
        let pendingLocalBlocks = blocks.filter { $0.isLocallyAuthored && $0.sourceItemID == nil }
        var mutationBySession: [CanonicalSessionKey: PendingCanonicalMutation] = [:]
        let executionWinners = currentExecutionProjectionWinners()
        for mutation in pendingCanonicalMutations
            where canonicalMutationAffectsSchedulePresentation(
                mutation,
                executionWinners: executionWinners
            ) {
            let key = CanonicalSessionKey(
                itemID: mutation.itemID,
                occurrenceID: mutation.occurrenceID,
                sessionIndex: mutation.sessionIndex ?? 0
            )
            if mutationBySession[key] == nil { mutationBySession[key] = mutation }
        }
        func sessionKey(for block: ScheduleBlock) -> CanonicalSessionKey? {
            guard let itemID = block.sourceItemID else { return nil }
            return .init(
                itemID: itemID,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0
            )
        }
        var merged = newBlocks
        for index in merged.indices {
            guard let key = sessionKey(for: merged[index]),
                  let mutation = mutationBySession[key] else { continue }
            merged[index].status = mutation.desiredStatus
        }
        let mergedSessionKeys = Set(merged.compactMap(sessionKey(for:)))
        let retainedEditedBlocks = blocks.filter { existing in
            guard let key = sessionKey(for: existing), mutationBySession[key] != nil else {
                return false
            }
            return !mergedSessionKeys.contains(key)
        }
        let outcomeSessionKeys = Set(recurrenceSessionOutcomes.map {
            CanonicalSessionKey(
                itemID: $0.itemID,
                occurrenceID: $0.occurrenceID,
                sessionIndex: $0.sessionIndex
            )
        })
        let retainedOutcomeBlocks = blocks.filter { existing in
            guard let key = sessionKey(for: existing),
                  outcomeSessionKeys.contains(key) else { return false }
            return !mergedSessionKeys.contains(key)
        }
        let retainedIDs = Set(retainedEditedBlocks.map(\.id))
        let uniqueOutcomeBlocks = retainedOutcomeBlocks.filter { !retainedIDs.contains($0.id) }
        blocks = (merged + retainedEditedBlocks + uniqueOutcomeBlocks + pendingLocalBlocks)
            .sorted { $0.start < $1.start }
        // A pending or conflicted local privacy mark is a one-way hardening
        // boundary. A server preview cannot visually or contextually lower it
        // before that intent is explicitly resolved.
        hardenPendingSensitivityPresentation()
        selectedBlockID = blocks.first(where: { $0.id == previousSelection })?.id
            ?? blocks.first(where: { $0.sourceItemID == selectedSourceID })?.id
            ?? blocks.first?.id
        var presentedExecutionState = executionState
        applyExecutionPresentation(to: &presentedExecutionState)
        executionState = presentedExecutionState
        lastScheduleMessage = message
    }

    private func canonicalMutationAffectsSchedulePresentation(
        _ mutation: PendingCanonicalMutation
    ) -> Bool {
        canonicalMutationAffectsSchedulePresentation(
            mutation,
            executionWinners: currentExecutionProjectionWinners()
        )
    }

    private func canonicalMutationAffectsSchedulePresentation(
        _ mutation: PendingCanonicalMutation,
        executionWinners: [ExecutionProjectionKey: DayWeaveExecutionSession]
    ) -> Bool {
        guard let sessionID = mutation.executionSessionID,
              let outcome = executionState.terminalOutcomes[sessionID],
              executionProjectionWinner(
                  for: sessionID,
                  executionWinners: executionWinners
              )?.id == sessionID else {
            return mutation.executionSessionID == nil
        }
        switch outcome.projection {
        case .pending, .conflicted, .retryAuthorized:
            return true
        case .notRequired, .applied, .keptLatest:
            return false
        }
    }

    private func executionProjectionWinner(
        for sessionID: UUID
    ) -> DayWeaveExecutionSession? {
        executionProjectionWinner(
            for: sessionID,
            executionWinners: currentExecutionProjectionWinners()
        )
    }

    private func executionProjectionWinner(
        for sessionID: UUID,
        executionWinners: [ExecutionProjectionKey: DayWeaveExecutionSession]
    ) -> DayWeaveExecutionSession? {
        guard let linked = executionState.terminalOutcomes[sessionID]?.session else { return nil }
        return executionWinners[Self.executionProjectionKey(for: linked)]
    }

    private func currentExecutionProjectionWinners()
        -> [ExecutionProjectionKey: DayWeaveExecutionSession] {
        Self.newestExecutionSessionsByProjectionKey(
            terminalSessions: executionState.terminalOutcomes.values.map(\.session),
            activeSession: executionState.activeSession
        )
    }

    func canonicalItem(id: UUID) -> DayWeaveCanonicalItem? {
        canonicalItems.first(where: { $0.id == id })
    }

    var hasExactOnboardingFirstPlanProof: Bool {
        onboardingFirstItemAnchor?.hasExactPublishedPlanProof(
            canonicalItems: canonicalItems,
            pendingAuthoringMutations: pendingCanonicalAuthoringMutations,
            publishedScheduleProof: publishedScheduleProof
        ) == true
    }

    private func reconcileOnboardingFirstItemAnchor(
        authoritativeMissing: Bool = false
    ) {
        guard let anchor = onboardingFirstItemAnchor else { return }
        if let item = canonicalItem(id: anchor.itemID), item.deletedAt == nil {
            if anchor.canonicalRevision == item.revision { return }
            let exactReviewedMutation = pendingCanonicalAuthoringMutations.contains {
                mutation in
                mutation.itemID == anchor.itemID
                    && item.supportsCanonicalAuthoringReplacement
                    && (anchor.canonicalRevision == nil
                        ? mutation.operation == .create
                        : mutation.operation == .create || mutation.operation == .replace)
                    && mutation.draft.map {
                        $0.createsPlanningDemand(itemID: anchor.itemID)
                            && $0.matches(item)
                    } == true
            }
            if exactReviewedMutation {
                let replacement = DayWeaveOnboardingFirstItemAnchor(
                    itemID: item.id,
                    canonicalRevision: item.revision
                )
                if replacement != anchor { onboardingFirstItemAnchor = replacement }
            } else if anchor.canonicalRevision != nil {
                // A newer cross-device revision has not crossed this Mac's
                // review boundary. Drop the designation instead of silently
                // calling that revision the reviewed onboarding item.
                onboardingFirstItemAnchor = nil
            }
            return
        }
        let wasDeleted = canonicalTombstoneRevisions[anchor.itemID] != nil
            || canonicalTrash.contains { $0.id == anchor.itemID }
        if wasDeleted {
            onboardingFirstItemAnchor = nil
            return
        }
        guard authoritativeMissing else { return }
        if pendingCanonicalAuthoringMutations.contains(where: { mutation in
            mutation.itemID == anchor.itemID
                && mutation.operation == .create
                && mutation.draft.map {
                    $0.createsPlanningDemand(itemID: anchor.itemID)
                } == true
        }) {
            let replacement = DayWeaveOnboardingFirstItemAnchor(
                itemID: anchor.itemID,
                canonicalRevision: nil
            )
            if replacement != anchor { onboardingFirstItemAnchor = replacement }
        } else {
            onboardingFirstItemAnchor = nil
        }
    }

    /// Resolves inherited sensitivity and fails closed for a missing or cyclic ancestor.
    private func effectiveSensitivity(
        itemID: UUID,
        includingPendingMarks: Bool = true
    ) -> Bool {
        var ownSensitivity = Dictionary(uniqueKeysWithValues: canonicalItems.map {
            ($0.id, $0.isSensitive)
        })
        var parents = Dictionary(uniqueKeysWithValues: canonicalItems.map { item in
            (item.id, Set([item.parentID].compactMap { $0 }))
        })

        if includingPendingMarks {
            for mutation in pendingCanonicalSensitivityMutations
                where mutation.requiresSensitivePresentation {
                ownSensitivity[mutation.itemID] = true
            }
            for mutation in pendingCanonicalAuthoringMutations {
                if let baseItem = mutation.baseItem {
                    // Restore/trash/replace recovery may retain an older body
                    // or ancestry that is more sensitive than the active
                    // revision. Keep that one-way boundary until the exact
                    // journal is resolved or its bounded body expires.
                    ownSensitivity[mutation.itemID] =
                        (ownSensitivity[mutation.itemID] ?? false) || baseItem.isSensitive
                    parents[mutation.itemID, default: []].formUnion(
                        [baseItem.parentID].compactMap { $0 }
                    )
                }
                guard mutation.operation == .create || mutation.operation == .replace,
                      let draft = mutation.draft else { continue }
                // Pending authoring is a one-way privacy boundary: a new mark
                // or sensitive parent applies immediately, while the old own
                // mark and old ancestry remain protective until confirmation.
                ownSensitivity[mutation.itemID] =
                    (ownSensitivity[mutation.itemID] ?? false) || draft.isSensitive
                parents[mutation.itemID, default: []].formUnion(
                    [draft.parentID].compactMap { $0 }
                )
            }
        }

        var visiting = Set<UUID>()
        var completed = Set<UUID>()
        var stack: [(id: UUID, isExit: Bool)] = [(itemID, false)]
        while let frame = stack.popLast() {
            if frame.isExit {
                visiting.remove(frame.id)
                completed.insert(frame.id)
                continue
            }
            if completed.contains(frame.id) { continue }
            guard let isSensitive = ownSensitivity[frame.id] else { return true }
            if isSensitive { return true }
            guard visiting.insert(frame.id).inserted else { return true }
            stack.append((frame.id, true))
            for parentID in parents[frame.id] ?? [] {
                if visiting.contains(parentID) { return true }
                stack.append((parentID, false))
            }
        }
        return false
    }

    func canonicalItemRequiresSensitivePresentation(itemID: UUID) -> Bool {
        effectiveSensitivity(itemID: itemID, includingPendingMarks: true)
    }

    func canonicalSensitivityPresentation(itemID: UUID) -> CanonicalSensitivityPresentation {
        let item = canonicalItem(id: itemID)
        let pendingOwnMark = pendingCanonicalSensitivityMutations.contains {
            $0.itemID == itemID && $0.requiresSensitivePresentation
        } || pendingCanonicalAuthoringMutations.contains {
            $0.itemID == itemID
                && ($0.draft?.isSensitive == true || $0.baseItem?.isSensitive == true)
        }
        let retainedTrashOwnMark = canonicalTrash.first { $0.id == itemID }?.isSensitive == true
        if item?.isSensitive == true || retainedTrashOwnMark || pendingOwnMark { return .own }
        return effectiveSensitivity(itemID: itemID, includingPendingMarks: true)
            ? .inherited
            : .standard
    }

    func canonicalSensitivityMutation(
        itemID: UUID
    ) -> PendingCanonicalSensitivityMutation? {
        pendingCanonicalSensitivityMutations.first { $0.itemID == itemID }
    }

    func canEditCanonicalSensitivity(itemID: UUID) -> Bool {
        guard canMutatePlan,
              let item = canonicalItem(id: itemID),
              item.deletedAt == nil,
              item.supportsLosslessReplacement else { return false }
        return executionState.activeSession?.itemID != itemID
            && executionState.pendingCommand == nil
    }

    @discardableResult
    func setCanonicalItemSensitivity(_ itemID: UUID, isSensitive: Bool) -> Bool {
        guard canEditCanonicalSensitivity(itemID: itemID),
              let item = canonicalItem(id: itemID) else { return false }
        if let index = pendingCanonicalSensitivityMutations.firstIndex(where: {
            $0.itemID == itemID
        }), pendingCanonicalSensitivityMutations[index].hasBeenSubmitted {
            // The server may already have applied the submitted replacement.
            // Preserve its exact bytes/idempotency identity and queue the
            // user's new classification only as a follow-up.
            pendingCanonicalSensitivityMutations[index].followUpIsSensitive =
                isSensitive == pendingCanonicalSensitivityMutations[index].desiredIsSensitive
                    ? nil
                    : isSensitive
            lastScheduleMessage = "Saved the final privacy choice; sync will reconcile the submitted change before applying it"
        } else if isSensitive == item.isSensitive {
            pendingCanonicalSensitivityMutations.removeAll { $0.itemID == itemID }
            lastScheduleMessage = "Discarded the unsubmitted privacy change; canonical state was not changed"
        } else {
            pendingCanonicalSensitivityMutations.removeAll { $0.itemID == itemID }
            pendingCanonicalSensitivityMutations.append(.init(
                id: UUID(),
                itemID: itemID,
                desiredIsSensitive: isSensitive,
                baseRevision: item.revision,
                createdAt: now(),
                disposition: .pending,
                diagnostic: nil
            ))
            lastScheduleMessage = isSensitive
                ? "Privacy mark saved locally; sync to publish it"
                : "Privacy removal saved locally; content stays redacted until sync confirms it"
        }
        hardenPendingSensitivityPresentation()
        flushPersistence()
        return true
    }

    func retryConflictedCanonicalSensitivityMutation(_ mutationID: UUID) {
        guard let index = pendingCanonicalSensitivityMutations.firstIndex(where: {
            $0.id == mutationID
        }),
              pendingCanonicalSensitivityMutations[index].disposition == .conflicted,
              canEditCanonicalSensitivity(
                itemID: pendingCanonicalSensitivityMutations[index].itemID
              ),
              let item = canonicalItem(
                id: pendingCanonicalSensitivityMutations[index].itemID
              ) else { return }
        if item.isSensitive == pendingCanonicalSensitivityMutations[index].desiredIsSensitive {
            let next = advanceCanonicalSensitivityMutation(
                itemID: item.id,
                observedIsSensitive: item.isSensitive,
                observedRevision: item.revision
            )
            lastScheduleMessage = next == nil
                ? "The latest canonical item already has the requested privacy setting"
                : "The submitted privacy change was reconciled; sync to apply the saved final choice"
        } else {
            pendingCanonicalSensitivityMutations[index].baseRevision = item.revision
            pendingCanonicalSensitivityMutations[index].disposition = .pending
            pendingCanonicalSensitivityMutations[index].diagnostic = nil
            pendingCanonicalSensitivityMutations[index].hasBeenSubmitted = false
            lastScheduleMessage = "Privacy conflict rebased locally; sync to retry against revision \(item.revision)"
        }
        hardenPendingSensitivityPresentation()
        flushPersistence()
    }

    func keepLatestCanonicalSensitivity(_ mutationID: UUID) {
        guard canMutatePlan,
              pendingCanonicalSensitivityMutations.contains(where: {
                  $0.id == mutationID && $0.disposition == .conflicted
              }) else { return }
        pendingCanonicalSensitivityMutations.removeAll { $0.id == mutationID }
        lastScheduleMessage = "Kept the latest canonical privacy setting"
        flushPersistence()
    }

    func markCanonicalSensitivityMutationConflicted(
        itemID: UUID,
        diagnostic: String
    ) {
        guard canPersistPlan,
              let index = pendingCanonicalSensitivityMutations.firstIndex(where: {
                  $0.itemID == itemID
              }) else { return }
        pendingCanonicalSensitivityMutations[index].disposition = .conflicted
        pendingCanonicalSensitivityMutations[index].diagnostic = diagnostic
    }

    func clearCanonicalSensitivityMutation(itemID: UUID) {
        guard canPersistPlan else { return }
        pendingCanonicalSensitivityMutations.removeAll { $0.itemID == itemID }
    }

    @discardableResult
    func markCanonicalSensitivityMutationSubmitted(_ mutationID: UUID) -> Bool {
        guard canPersistPlan,
              let index = pendingCanonicalSensitivityMutations.firstIndex(where: {
                  $0.id == mutationID && $0.disposition == .pending
              }) else { return false }
        if !pendingCanonicalSensitivityMutations[index].hasBeenSubmitted {
            pendingCanonicalSensitivityMutations[index].hasBeenSubmitted = true
            flushPersistence()
        }
        return persistenceError == nil
    }

    @discardableResult
    func reconcileCanonicalSensitivityObservation(
        _ item: DayWeaveCanonicalItem
    ) -> PendingCanonicalSensitivityMutation? {
        guard canPersistPlan,
              let mutation = canonicalSensitivityMutation(itemID: item.id),
              mutation.desiredIsSensitive == item.isSensitive else { return nil }
        return advanceCanonicalSensitivityMutation(
            itemID: item.id,
            observedIsSensitive: item.isSensitive,
            observedRevision: item.revision
        )
    }

    @discardableResult
    func applyCanonicalSensitivityMutationResponse(
        _ item: DayWeaveCanonicalItem,
        replacingBaseRevision baseRevision: UInt64
    ) -> PendingCanonicalSensitivityMutation? {
        guard canPersistPlan else { return nil }
        upsertCanonicalItem(item)
        let next = advanceCanonicalSensitivityMutation(
            itemID: item.id,
            observedIsSensitive: item.isSensitive,
            observedRevision: item.revision
        )
        for index in pendingCanonicalMutations.indices
            where pendingCanonicalMutations[index].itemID == item.id
                && pendingCanonicalMutations[index].baseRevision == baseRevision
                && pendingCanonicalMutations[index].disposition == .pending {
            pendingCanonicalMutations[index].baseRevision = item.revision
        }
        for index in blocks.indices
            where blocks[index].sourceItemID == item.id
                && blocks[index].sourceItemRevision == baseRevision {
            blocks[index].sourceItemRevision = item.revision
            if item.isSensitive { blocks[index].isSensitive = true }
        }
        hardenPendingSensitivityPresentation()
        return next
    }

    private func advanceCanonicalSensitivityMutation(
        itemID: UUID,
        observedIsSensitive: Bool,
        observedRevision: UInt64
    ) -> PendingCanonicalSensitivityMutation? {
        guard let index = pendingCanonicalSensitivityMutations.firstIndex(where: {
            $0.itemID == itemID && $0.desiredIsSensitive == observedIsSensitive
        }) else { return nil }
        let followUp = pendingCanonicalSensitivityMutations[index].followUpIsSensitive
        pendingCanonicalSensitivityMutations.remove(at: index)
        guard let followUp, followUp != observedIsSensitive else { return nil }
        let next = PendingCanonicalSensitivityMutation(
            id: UUID(),
            itemID: itemID,
            desiredIsSensitive: followUp,
            baseRevision: observedRevision,
            createdAt: now(),
            disposition: .pending,
            diagnostic: nil
        )
        pendingCanonicalSensitivityMutations.append(next)
        return next
    }

    private func hardenPendingSensitivityPresentation() {
        for index in blocks.indices {
            guard let itemID = blocks[index].sourceItemID else { continue }
            if canonicalItemRequiresSensitivePresentation(itemID: itemID) {
                blocks[index].isSensitive = true
            }
        }
    }

    @discardableResult
    func quickAdd(
        title: String,
        kind: PlannerItemKind,
        minutes: Int,
        isSensitive: Bool = false
    ) -> Bool {
        guard canMutatePlan,
              let title = Self.normalizedCanonicalTitle(title),
              minutes > 0 else { return false }
        let currentTime = now()
        let lastEnd = blocks.map(\.end).max() ?? currentTime
        let start = max(lastEnd.addingTimeInterval(10 * 60), currentTime)
        let block = ScheduleBlock(
            id: UUID(),
            isSensitive: isSensitive,
            title: title,
            kind: kind,
            start: start,
            end: start.addingTimeInterval(TimeInterval(minutes * 60)),
            status: .scheduled,
            project: nil,
            notes: "Captured with Quick Add",
            energy: .medium,
            isFlexible: kind != .event,
            isHardConstraint: kind == .event,
            actualMinutes: nil
        )
        blocks.append(block)
        blocks.sort { $0.start < $1.start }
        selectedBlockID = block.id
        lastScheduleMessage = "Added \"\(title)\" locally; sync will validate its placement"
        return true
    }

    static func normalizedCanonicalTitle(_ title: String) -> String? {
        let normalized = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty,
              normalized.unicodeScalars.count <= maximumCanonicalTitleScalars else { return nil }
        return normalized
    }

    @discardableResult
    func updateLocalCapture(_ id: UUID, title: String) -> Bool {
        updateLocalCapture(id, title: title, isSensitive: nil)
    }

    @discardableResult
    func updateLocalCapture(
        _ id: UUID,
        title: String,
        isSensitive: Bool?
    ) -> Bool {
        guard canMutatePlan,
              let title = Self.normalizedCanonicalTitle(title),
              let index = blocks.firstIndex(where: {
                  $0.id == id && $0.isLocallyAuthored && $0.sourceItemID == nil
              }) else { return false }
        blocks[index].title = title
        if let isSensitive { blocks[index].isSensitive = isSensitive }
        localCaptureDiagnostics.removeValue(forKey: id)
        selectedBlockID = id
        lastScheduleMessage = "Updated local capture; sync will validate its placement"
        flushPersistence()
        return true
    }

    func deleteLocalCapture(_ id: UUID) {
        guard canMutatePlan,
              blocks.contains(where: {
                  $0.id == id && $0.isLocallyAuthored && $0.sourceItemID == nil
              }) else { return }
        blocks.removeAll { $0.id == id }
        localCaptureDiagnostics.removeValue(forKey: id)
        selectedBlockID = blocks.first?.id
        lastScheduleMessage = "Deleted the local capture; no server data was changed"
        flushPersistence()
    }

    func normalizeLocalCaptureForSync(_ id: UUID, title: String) {
        guard canPersistPlan,
              let index = blocks.firstIndex(where: {
                  $0.id == id && $0.isLocallyAuthored && $0.sourceItemID == nil
              }) else { return }
        blocks[index].title = title
        localCaptureDiagnostics.removeValue(forKey: id)
    }

    func quarantineLocalCapture(_ id: UUID, diagnostic: String) {
        guard canPersistPlan else { return }
        localCaptureDiagnostics[id] = diagnostic
    }

    func clearLocalCaptureDiagnostic(_ id: UUID) {
        guard canPersistPlan else { return }
        localCaptureDiagnostics.removeValue(forKey: id)
    }

    func sendAssistantMessage(_ text: String) {
        guard canMutatePlan else { return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let sentAt = now()
        assistantMessages.append(.init(id: UUID(), role: .user, text: trimmed, createdAt: sentAt))
        assistantMessages.append(.init(
            id: UUID(),
            role: .assistant,
            text: "Saved as a local chat note. This screen is not yet applying Codex changes to the planner; use Sync & compose to validate the current plan.",
            createdAt: sentAt
        ))
    }

    /// Stores a completed Codex turn as bounded, encrypted, typed review
    /// records. Model output is still untrusted here: the parser's contract is
    /// revalidated, item identities are minted locally, and either the whole
    /// resulting suggestion transition is durable or none of it is exposed.
    @discardableResult
    func storeCodexItemDraftSuggestions(
        _ drafts: [CodexSuggestionDraft],
        createdAt: Date
    ) throws -> Int {
        guard !drafts.isEmpty else { return 0 }
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerCanonicalAuthoringError.encryptedPersistenceRequired
        }
        guard drafts.count <= Self.maximumCodexSuggestionsPerTurn,
              createdAt.timeIntervalSinceReferenceDate.isFinite else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let localSuggestionObservation = observeLocalSuggestionDate()
        if localSuggestionObservation.rollbackDetected {
            expireLocalSuggestions()
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        guard createdAt >= localSuggestionObservation.referenceDate.addingTimeInterval(
                  -Self.localSuggestionFutureSkewTolerance
              ),
              createdAt <= localSuggestionObservation.referenceDate.addingTimeInterval(
                  Self.localSuggestionFutureSkewTolerance
              ) else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let expiresAt = createdAt.addingTimeInterval(Self.localSuggestionLifetime)
        guard expiresAt.timeIntervalSinceReferenceDate.isFinite else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        let retentionReferenceDate = max(
            localSuggestionObservation.referenceDate,
            createdAt
        )
        if expireLocalSuggestionsInMemory(referenceDate: retentionReferenceDate) {
            // Commit privacy retention before any later validation error can
            // return control while an expired body is only scrubbed in RAM.
            try flushLocalSuggestionPrivacyTransition()
        }

        var reservedItemIDs = Set(canonicalItems.map(\.id))
            .union(canonicalTrash.map(\.id))
            .union(canonicalTombstoneRevisions.keys)
            .union(pendingCanonicalAuthoringMutations.map(\.itemID))
            .union(pendingCanonicalMutations.map(\.itemID))
            .union(pendingCanonicalSensitivityMutations.map(\.itemID))
            .union(suggestions.compactMap(\.resultingItemID))
        var reservedSuggestionIDs = Set(suggestions.map(\.id))
        for suggestion in suggestions {
            if case let .canonicalItemDraft(itemDraft) = suggestion.payload {
                reservedItemIDs.insert(itemDraft.itemID)
            } else if case let .canonicalItemReference(itemID) = suggestion.payload {
                reservedItemIDs.insert(itemID)
            }
        }

        var prepared: [(summary: String, item: PlanningSuggestionItemDraft)] = []
        prepared.reserveCapacity(drafts.count)
        for proposal in drafts {
            let canonicalDraft = proposal.canonicalDraft.normalized
            let itemID = uniqueLocalIdentifier(excluding: &reservedItemIDs)
            guard canonicalDraft.validationIssue(itemID: itemID) == nil,
                  canonicalAuthoringDraftHierarchyIsCurrent(
                      canonicalDraft,
                      itemID: itemID,
                      requiresCommittedParent: false
                  ) else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            guard CodexCanonicalItemDraftReviewValidator.accepts(
                canonicalDraft,
                itemID: itemID,
                now: createdAt
            ) else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            let summary = proposal.summary.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !summary.isEmpty else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            let item = PlanningSuggestionItemDraft(
                itemID: itemID,
                draft: canonicalDraft
            )
            guard item.hasValidShape else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            let isDuplicate = suggestions.contains { suggestion in
                switch suggestion.payload {
                case let .canonicalItemDraft(existing):
                    return suggestion.expiresAt > createdAt
                        && suggestion.state == .pending
                        && suggestion.summary == summary
                        && existing.version == PlanningSuggestionItemDraft.currentVersion
                        && existing.draft.normalized == canonicalDraft
                case .canonicalItemReference:
                    guard suggestion.state == .accepted,
                          let itemID = suggestion.resultingItemID else {
                        return false
                    }
                    if let mutationID = suggestion.resultingMutationID,
                       let mutation = canonicalAuthoringMutation(id: mutationID) {
                        return mutation.itemID == itemID
                            && mutation.operation == .create
                            && mutation.draft == canonicalDraft
                    }
                    return canonicalItem(id: itemID).map(canonicalDraft.matches) == true
                case .advisory:
                    return false
                }
            } || prepared.contains {
                $0.summary == summary && $0.item.draft.normalized == canonicalDraft
            }
            guard !isDuplicate else { continue }
            prepared.append((summary, item))
        }

        let priorSuggestions = suggestions
        let priorLocalSuggestionDateHighWater = localSuggestionDateHighWater
        pruneTerminalLocalSuggestionsToFit(additionalCount: prepared.count)
        guard suggestions.count <= Self.maximumLocalSuggestions - prepared.count else {
            restoreLocalSuggestionTransactionPreimage(
                priorSuggestions,
                dateHighWater: priorLocalSuggestionDateHighWater
            )
            throw PlannerCanonicalAuthoringError.journalCapacityReached
        }

        for proposal in prepared {
            let suggestion = PlanningSuggestion(
                id: uniqueLocalIdentifier(excluding: &reservedSuggestionIDs),
                title: proposal.item.draft.title,
                summary: proposal.summary,
                source: PlanningSuggestion.codexSource,
                createdAt: createdAt,
                expiresAt: expiresAt,
                state: .pending,
                payload: .canonicalItemDraft(proposal.item),
                resultingItemID: nil,
                resultingMutationID: nil
            )
            guard suggestion.hasValidShape else {
                restoreLocalSuggestionTransactionPreimage(
                    priorSuggestions,
                    dateHighWater: priorLocalSuggestionDateHighWater
                )
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            suggestions.append(suggestion)
        }
        if !prepared.isEmpty {
            localSuggestionDateHighWater = max(
                localSuggestionDateHighWater ?? createdAt,
                createdAt
            )
        }
        guard PlanningSuggestion.collectionIsValid(suggestions) else {
            restoreLocalSuggestionTransactionPreimage(
                priorSuggestions,
                dateHighWater: priorLocalSuggestionDateHighWater
            )
            throw PlannerCanonicalAuthoringError.journalCapacityReached
        }
        guard suggestions != priorSuggestions else { return 0 }
        do {
            try flushLocalSuggestionTransition()
        } catch {
            restoreLocalSuggestionTransactionPreimage(
                priorSuggestions,
                dateHighWater: priorLocalSuggestionDateHighWater
            )
            throw error
        }
        return prepared.count
    }

    /// One local commit records both facts established by explicit user
    /// approval: the suggestion was accepted and the exact canonical create is
    /// queued. No network request is made here. CanonicalSyncStore later binds,
    /// submits, and reconciles the immutable idempotent journal.
    func acceptCanonicalItemSuggestion(
        _ suggestionID: UUID,
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    ) throws {
        guard let suggestionIndex = suggestions.firstIndex(where: {
            $0.id == suggestionID
        }) else {
            throw PlannerCanonicalAuthoringError.suggestionNotFound
        }

        let initialSuggestion = suggestions[suggestionIndex]
        if initialSuggestion.state == .accepted {
            guard initialSuggestion.resultingItemID == itemID,
                  let resultingMutationID = initialSuggestion.resultingMutationID,
                  case let .canonicalItemReference(payloadItemID) = initialSuggestion.payload,
                  payloadItemID == itemID else {
                throw PlannerCanonicalAuthoringError.suggestionIdentityMismatch
            }
            if let mutation = canonicalAuthoringMutation(id: resultingMutationID) {
                guard mutation.itemID == itemID,
                      mutation.operation == .create,
                      mutation.draft == draft.normalized else {
                    throw PlannerCanonicalAuthoringError.suggestionIdentityMismatch
                }
            } else {
                let normalizedDraft = draft.normalized
                guard canonicalItem(id: itemID).map(normalizedDraft.matches) == true else {
                    throw PlannerCanonicalAuthoringError.suggestionIdentityMismatch
                }
            }
            return
        }
        try requireCanonicalAuthoringUserFence(allowDuringExecution: true)
        let localSuggestionObservation = observeLocalSuggestionDate()
        let localSuggestionRetentionChanged = expireLocalSuggestionsInMemory(
            referenceDate: localSuggestionObservation.referenceDate,
            forcePendingExpiration: localSuggestionObservation.rollbackDetected
        )
        if localSuggestionRetentionChanged {
            // Privacy expiry is its own durable transition. Persist it before
            // draft validation or approval can fail for an unrelated reason.
            try flushLocalSuggestionPrivacyTransition()
        }
        let suggestion = suggestions[suggestionIndex]
        if suggestion.state == .expired {
            throw PlannerCanonicalAuthoringError.suggestionExpired
        }
        guard suggestion.state == .pending else {
            throw PlannerCanonicalAuthoringError.suggestionNotPending
        }
        guard case let .canonicalItemDraft(itemDraft) = suggestion.payload,
              itemDraft.version == PlanningSuggestionItemDraft.currentVersion,
              itemDraft.itemID == itemID,
              itemDraft.hasValidShape else {
            throw PlannerCanonicalAuthoringError.suggestionIdentityMismatch
        }
        guard canonicalItem(id: itemID) == nil,
              canonicalTrashEntry(id: itemID) == nil,
              canonicalTombstoneRevisions[itemID] == nil else {
            throw PlannerCanonicalAuthoringError.duplicateItemOperation
        }
        guard CodexCanonicalItemDraftReviewValidator.acceptsReviewedDraft(
            draft,
            itemID: itemID,
            now: localSuggestionObservation.referenceDate
        ) else {
            throw PlannerCanonicalAuthoringError.invalidDraft
        }
        try validateCanonicalAuthoringDraft(draft, itemID: itemID)
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: draft,
            createdAt: now()
        )
        guard canonicalAuthoringMutation(itemID: itemID) == nil,
              pendingCanonicalMutations.allSatisfy({ $0.itemID != itemID }),
              pendingCanonicalSensitivityMutations.allSatisfy({
                  $0.itemID != itemID
              }) else {
            throw PlannerCanonicalAuthoringError.duplicateItemOperation
        }
        guard PlannerCanonicalAuthoringJournalValidator.isValid(mutation) else {
            throw PlannerCanonicalAuthoringError.invalidMutation
        }

        let priorSuggestions = suggestions
        let priorLocalSuggestionDateHighWater = localSuggestionDateHighWater
        let priorMutations = pendingCanonicalAuthoringMutations
        let priorSelection = selectedCanonicalItemID
        let priorBlocks = blocks
        suggestions[suggestionIndex].state = .accepted
        suggestions[suggestionIndex].payload = .canonicalItemReference(itemID: itemID)
        suggestions[suggestionIndex].resultingItemID = itemID
        suggestions[suggestionIndex].resultingMutationID = mutation.id
        scrubTerminalLocalSuggestion(at: suggestionIndex)
        pendingCanonicalAuthoringMutations.append(mutation)
        selectedCanonicalItemID = itemID
        hardenPendingSensitivityPresentation()

        guard currentCanonicalAuthoringStateIsValid else {
            restoreLocalSuggestionTransactionPreimage(
                priorSuggestions,
                dateHighWater: priorLocalSuggestionDateHighWater
            )
            pendingCanonicalAuthoringMutations = priorMutations
            selectedCanonicalItemID = priorSelection
            blocks = priorBlocks
            throw PlannerCanonicalAuthoringError.journalCapacityReached
        }
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            restoreLocalSuggestionTransactionPreimage(
                priorSuggestions,
                dateHighWater: priorLocalSuggestionDateHighWater
            )
            pendingCanonicalAuthoringMutations = priorMutations
            selectedCanonicalItemID = priorSelection
            blocks = priorBlocks
            throw error
        }
    }

    /// Advisory acceptance deliberately cannot create canonical state. This
    /// compatibility action is retained for local, non-actionable suggestions.
    func acceptSuggestion(_ id: UUID) {
        guard canMutatePlan,
              let index = suggestions.firstIndex(where: { $0.id == id }),
              suggestions[index].state == .pending else { return }
        let localSuggestionObservation = observeLocalSuggestionDate()
        let localSuggestionRetentionChanged = expireLocalSuggestionsInMemory(
            referenceDate: localSuggestionObservation.referenceDate,
            forcePendingExpiration: localSuggestionObservation.rollbackDetected
        )
        if localSuggestionRetentionChanged {
            do {
                try flushLocalSuggestionPrivacyTransition()
            } catch {
                return
            }
        }
        let retentionState = suggestions
        let retentionDateHighWater = localSuggestionDateHighWater
        if suggestions[index].state == .pending {
            guard case .advisory = suggestions[index].payload else { return }
            suggestions[index].state = .accepted
            suggestions[index].resultingItemID = nil
            suggestions[index].resultingMutationID = nil
        }
        do {
            try flushLocalSuggestionTransitionIfAvailable()
        } catch {
            restoreLocalSuggestionTransactionPreimage(
                retentionState,
                dateHighWater: retentionDateHighWater
            )
        }
    }

    func rejectSuggestion(_ id: UUID) {
        guard canMutatePlan,
              let index = suggestions.firstIndex(where: { $0.id == id }),
              suggestions[index].state == .pending else { return }
        let localSuggestionObservation = observeLocalSuggestionDate()
        let localSuggestionRetentionChanged = expireLocalSuggestionsInMemory(
            referenceDate: localSuggestionObservation.referenceDate,
            forcePendingExpiration: localSuggestionObservation.rollbackDetected
        )
        if localSuggestionRetentionChanged {
            do {
                try flushLocalSuggestionPrivacyTransition()
            } catch {
                return
            }
        }
        let retentionState = suggestions
        let retentionDateHighWater = localSuggestionDateHighWater
        if suggestions[index].state == .pending {
            suggestions[index].state = .rejected
            scrubTerminalLocalSuggestion(at: index)
        }
        do {
            try flushLocalSuggestionTransitionIfAvailable()
        } catch {
            restoreLocalSuggestionTransactionPreimage(
                retentionState,
                dateHighWater: retentionDateHighWater
            )
        }
    }

    /// Expiration is a privacy transition, not a user-authored canonical
    /// mutation. It may run while the canonical sync fence is held, but never
    /// while encrypted persistence is unhealthy.
    func expireLocalSuggestions() {
        guard canPersistPlan else { return }
        let observation = observeLocalSuggestionDate()
        guard expireLocalSuggestionsInMemory(
            referenceDate: observation.referenceDate,
            forcePendingExpiration: observation.rollbackDetected
        ) else {
            scheduleLocalSuggestionExpiration()
            return
        }
        do {
            try flushLocalSuggestionPrivacyTransition()
        } catch {
            // Keep privacy-expired bodies scrubbed in memory. Persistence is
            // now unhealthy and the planner is locked until a safe reload.
        }
    }

    /// Called by an identity-bound monotonic sleeper. The exact still-pending
    /// record expires even if wall time moved backwards while the process was
    /// asleep. Internal visibility keeps the seven-day boundary testable.
    func expireLocalSuggestionAtScheduledDeadline(
        _ suggestionID: UUID,
        createdAt: Date,
        expiresAt: Date
    ) {
        let identity = LocalSuggestionExpirationIdentity(
            id: suggestionID,
            createdAt: createdAt,
            expiresAt: expiresAt
        )
        localSuggestionExpirationTasks.removeValue(forKey: identity)
        guard canPersistPlan,
              let index = suggestions.firstIndex(where: {
                  $0.id == suggestionID
                      && $0.state == .pending
                      && $0.createdAt == createdAt
                      && $0.expiresAt == expiresAt
              }) else {
            scheduleLocalSuggestionExpiration()
            return
        }
        let observation = observeLocalSuggestionDate()
        let changed = expireLocalSuggestionsInMemory(
            referenceDate: observation.referenceDate,
            forcePendingExpiration: observation.rollbackDetected,
            forcedSuggestionIDs: [suggestions[index].id]
        )
        guard changed else {
            scheduleLocalSuggestionExpiration()
            return
        }
        do {
            try flushLocalSuggestionPrivacyTransition()
        } catch {
            // The process-local copy remains scrubbed on a failed privacy save.
        }
    }

    /// Rebuilds durable intent for snapshots written before mutation tracking
    /// existed, and for any status edit made by an older client build.
    func capturePendingCanonicalMutations() {
        guard canPersistPlan else { return }
        let itemByID = Dictionary(uniqueKeysWithValues: canonicalItems.map { ($0.id, $0) })
        let authoringItemIDs = Set(pendingCanonicalAuthoringMutations.map(\.itemID))
        pendingCanonicalMutations.removeAll {
            $0.executionSessionID == nil && authoringItemIDs.contains($0.itemID)
        }
        var existingKeys = Set(pendingCanonicalMutations.map {
            CanonicalSessionKey(
                itemID: $0.itemID,
                occurrenceID: $0.occurrenceID,
                sessionIndex: $0.sessionIndex ?? 0
            )
        })
        var keysToRemove = Set<CanonicalSessionKey>()
        var mismatchKeys = Set<CanonicalSessionKey>()
        var additions: [PendingCanonicalMutation] = []
        for block in blocks where block.sourceItemID != nil {
            if block.occurrenceID != nil
                && (block.status == .completed || block.status == .skipped) {
                continue
            }
            guard let itemID = block.sourceItemID,
                  !authoringItemIDs.contains(itemID),
                  let item = itemByID[itemID] else { continue }
            let key = CanonicalSessionKey(
                itemID: itemID,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0
            )
            if block.status == Self.plannerStatus(for: item.status) {
                if !mismatchKeys.contains(key) { keysToRemove.insert(key) }
                continue
            }
            mismatchKeys.insert(key)
            keysToRemove.remove(key)
            guard existingKeys.insert(key).inserted else { continue }
            additions.append(.init(
                id: UUID(),
                itemID: itemID,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0,
                desiredStatus: block.status,
                baseRevision: item.revision,
                createdAt: now(),
                disposition: .pending,
                diagnostic: nil
            ))
        }
        if !keysToRemove.isEmpty {
            pendingCanonicalMutations.removeAll {
                $0.executionSessionID == nil && keysToRemove.contains(.init(
                    itemID: $0.itemID,
                    occurrenceID: $0.occurrenceID,
                    sessionIndex: $0.sessionIndex ?? 0
                ))
            }
        }
        pendingCanonicalMutations.append(contentsOf: additions)
    }

    func markCanonicalMutationConflicted(
        itemID: UUID,
        diagnostic: String
    ) {
        guard canPersistPlan else { return }
        let executionWinners = currentExecutionProjectionWinners()
        for index in pendingCanonicalMutations.indices
            where pendingCanonicalMutations[index].itemID == itemID {
            pendingCanonicalMutations[index].disposition = .conflicted
            pendingCanonicalMutations[index].diagnostic = diagnostic
            if let sessionID = pendingCanonicalMutations[index].executionSessionID,
               var outcome = executionState.terminalOutcomes[sessionID] {
                switch outcome.projection {
                case .pending, .conflicted, .retryAuthorized:
                    outcome.projection = executionProjectionWinner(
                        for: sessionID,
                        executionWinners: executionWinners
                    )?.id == sessionID ? .conflicted(diagnostic) : .notRequired
                case .notRequired, .applied, .keptLatest:
                    break
                }
                executionState.terminalOutcomes[sessionID] = outcome
            }
        }
    }

    func clearCanonicalMutations(itemID: UUID) {
        guard canPersistPlan else { return }
        let executionSessionIDs = pendingCanonicalMutations.compactMap {
            $0.itemID == itemID ? $0.executionSessionID : nil
        }
        pendingCanonicalMutations.removeAll { $0.itemID == itemID }
        if let item = canonicalItem(id: itemID) {
            for sessionID in executionSessionIDs {
                guard var outcome = executionState.terminalOutcomes[sessionID] else { continue }
                let desired: PlannerItemStatus = outcome.session.status == .completed
                    ? .completed : .skipped
                if Self.plannerStatus(for: item.status) == desired,
                   item.revision > outcome.session.itemRevision {
                    outcome.projection = .applied(revision: item.revision)
                    executionState.terminalOutcomes[sessionID] = outcome
                }
            }
        }
    }

    func canonicalMutation(for block: ScheduleBlock) -> PendingCanonicalMutation? {
        guard let itemID = block.sourceItemID else { return nil }
        return pendingCanonicalMutations.first {
            $0.itemID == itemID
                && $0.occurrenceID == block.occurrenceID
                && ($0.sessionIndex ?? 0) == (block.sessionIndex ?? 0)
        }
    }

    func canRetryCanonicalMutation(_ mutation: PendingCanonicalMutation) -> Bool {
        guard canMutatePlan,
              canonicalPreviewFreshnessIssue == nil,
              mutation.disposition == .conflicted,
              canonicalMutationAffectsSchedulePresentation(mutation),
              mutation.occurrenceID == nil,
              let item = canonicalItem(id: mutation.itemID),
              item.recurrence == nil,
              item.supportsLosslessReplacement,
              Self.canonicalStatus(for: mutation.desiredStatus) != nil,
              case .indivisible = item.splitPolicy else { return false }
        let matchingBlocks = blocks.filter {
            $0.sourceItemID == mutation.itemID && $0.occurrenceID == nil
        }
        return matchingBlocks.count == 1 && matchingBlocks[0].occurrenceFullyScheduled
    }

    /// Execution-linked status intent is also a durable uncertainty journal.
    /// It may be replayed only while its originating terminal session remains
    /// the current actionable winner for that exact projection key. Generic
    /// user-authored status mutations are unaffected by execution history.
    func canPublishCanonicalMutation(_ mutation: PendingCanonicalMutation) -> Bool {
        guard mutation.executionSessionID != nil else { return true }
        return canonicalMutationAffectsSchedulePresentation(mutation)
    }

    func retryConflictedCanonicalMutation(_ mutationID: UUID) {
        guard let mutationIndex = pendingCanonicalMutations.firstIndex(where: {
            $0.id == mutationID
        }),
              canRetryCanonicalMutation(pendingCanonicalMutations[mutationIndex]),
              let item = canonicalItem(id: pendingCanonicalMutations[mutationIndex].itemID) else {
            return
        }
        let mutation = pendingCanonicalMutations[mutationIndex]
        for blockIndex in blocks.indices
            where blocks[blockIndex].sourceItemID == mutation.itemID
                && blocks[blockIndex].occurrenceID == mutation.occurrenceID
                && (blocks[blockIndex].sessionIndex ?? 0) == (mutation.sessionIndex ?? 0) {
            blocks[blockIndex].sourceItemRevision = item.revision
        }
        pendingCanonicalMutations[mutationIndex].baseRevision = item.revision
        pendingCanonicalMutations[mutationIndex].disposition = .pending
        pendingCanonicalMutations[mutationIndex].diagnostic = nil
        if let sessionID = mutation.executionSessionID,
           var outcome = executionState.terminalOutcomes[sessionID] {
            outcome.projection = .retryAuthorized
            executionState.terminalOutcomes[sessionID] = outcome
        }
        lastScheduleMessage = "Conflict rebased locally; sync to retry against revision \(item.revision)"
        flushPersistence()
    }

    var hasExecutionCredentialReplacementBlocker: Bool {
        executionState.hasCredentialReplacementBlocker
            || !pendingCanonicalMutations.isEmpty
            || !pendingCanonicalSensitivityMutations.isEmpty
            || pendingSchedulePublication != nil
            || pendingProposalApplicationMutation != nil
            || pendingCanonicalAuthoringMutations.contains {
                $0.operation != .create
                    || $0.hasBeenSubmitted
                    || $0.configurationIdentifier != nil
            }
    }

    /// Binds the encrypted execution cache to an opaque URL+credential digest.
    /// Unknown or rotated credentials never inherit canonical/execution state:
    /// it is either blocked by an unresolved write or quarantined durably first.
    func prepareExecutionBinding(
        _ bindingIdentifier: String,
        canonicalConfigurationIdentifier requestedCanonicalIdentifier: String
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerExecutionStateError.encryptedPersistenceRequired
        }
        guard !bindingIdentifier.isEmpty, bindingIdentifier.utf8.count <= 1_024,
              let canonicalIdentifier = Self.canonicalConfigurationIdentifier(
                  requestedCanonicalIdentifier
              ) else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        if executionState.bindingIdentifier == bindingIdentifier {
            guard canonicalConfigurationIdentifier == nil
                    || canonicalConfigurationIdentifier == canonicalIdentifier else {
                throw PlannerExecutionStateError.configurationMismatch
            }
            if canonicalConfigurationIdentifier == nil { canonicalConfigurationIdentifier = canonicalIdentifier }
            return
        }

        let hasAnyRemoteState = hasCanonicalRemoteState || executionState.hasCredentialBoundState
        if hasAnyRemoteState && hasExecutionCredentialReplacementBlocker {
            throw PlannerExecutionStateError.credentialReplacementBlocked
        }
        if hasAnyRemoteState { quarantineCredentialBoundState(preservingDeviceID: true) }
        var state = executionState
        if state.deviceID == nil { state.deviceID = UUID() }
        state.bindingIdentifier = bindingIdentifier
        executionState = state
        canonicalConfigurationIdentifier = canonicalIdentifier
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    /// Call before destroying/replacing a bearer credential. This is recoverable
    /// only when no exact execution/canonical write remains unresolved.
    func prepareForExecutionCredentialReplacement() throws {
        guard hasEncryptedPersistence, canMutatePlan else {
            throw PlannerExecutionStateError.encryptedPersistenceRequired
        }
        guard !hasExecutionCredentialReplacementBlocker else {
            throw PlannerExecutionStateError.credentialReplacementBlocked
        }
        quarantineCredentialBoundState(preservingDeviceID: true)
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    func persistExecutionDeferIntent(
        _ intent: DayWeavePendingExecutionDeferIntent
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan,
              intent.hasValidShape,
              pendingExecutionDeferIntent == nil
                || pendingExecutionDeferIntent?.isSameRequest(as: intent) == true,
              executionState.activeSession.map(intent.identity.matches) == true
                || executionState.pendingCommand.map({ $0.identity == intent.identity }) == true
                || executionState.terminalOutcomes[intent.identity.sessionID]
                    .map({ intent.identity.matches($0.session) }) == true else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        let prior = pendingExecutionDeferIntent
        pendingExecutionDeferIntent = intent
        flushPersistence()
        if let persistenceError {
            pendingExecutionDeferIntent = prior
            throw persistenceError
        }
    }

    func clearExecutionDeferIntent(
        _ intent: DayWeavePendingExecutionDeferIntent,
        message: String? = nil
    ) throws {
        guard pendingExecutionDeferIntent == intent else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        pendingExecutionDeferIntent = nil
        if let message { lastScheduleMessage = message }
        flushPersistence()
        if let persistenceError {
            pendingExecutionDeferIntent = intent
            throw persistenceError
        }
    }

    func cancelExecutionDeferIntent(
        _ intent: DayWeavePendingExecutionDeferIntent,
        message: String? = nil
    ) throws {
        guard executionState.pendingCommand == nil,
              pendingExecutionDeferIntent == intent else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        pendingExecutionDeferIntent = nil
        if let message { lastScheduleMessage = message }
        flushPersistence()
        if let persistenceError {
            pendingExecutionDeferIntent = intent
            throw persistenceError
        }
    }

    func persistExecutionState(
        _ state: DayWeaveExecutionDurableState,
        message: String? = nil,
        reconcilePresentation: Bool = false
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerExecutionStateError.encryptedPersistenceRequired
        }
        var next = state
        guard Self.validateExecutionState(next) else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        if reconcilePresentation { applyExecutionPresentation(to: &next) }
        guard Self.validateExecutionState(next) else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        deferredExecutionPublicationSessionIDs = deferredExecutionPublicationSessionIDs.filter {
            next.terminalOutcomes[$0]?.session.status == .deferred
        }
        pendingPublicationDeferredSessionIDs.formIntersection(
            deferredExecutionPublicationSessionIDs
        )
        let newlyObservedDeferred: [DayWeaveExecutionSession] = next.terminalOutcomes.values.compactMap { outcome in
            guard outcome.session.status == .deferred,
                  executionState.terminalOutcomes[outcome.session.id]?.session
                    != outcome.session else { return nil }
            return outcome.session
        }
        for deferred in newlyObservedDeferred {
            deferredExecutionPublicationSessionIDs.insert(deferred.id)
            // A proof that predates observation of this terminal Defer cannot
            // attest its replacement, even if an unrelated pinned sibling
            // happens to share the same window and a higher session index.
            publishedScheduleProof = nil
            isCanonicalPreviewValidatedForCurrentLaunch = false
        }
        executionState = next
        if let message { lastScheduleMessage = message }
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    /// Records only the exact still-current expired-pause acknowledgment. The
    /// caller removes and verifies the corresponding notification first, while
    /// this method makes the final local transition atomic. A failed write
    /// restores the unresolved in-memory state so the resolver cannot vanish
    /// on the strength of bytes that never reached encrypted storage.
    func persistExpiredPauseAcknowledgment(
        _ version: DayWeaveExecutionSessionVersion,
        message: String
    ) throws {
        guard let active = executionState.activeSession,
              active.status == .paused,
              active.id == version.sessionID,
              active.revision == version.revision,
              executionState.acknowledgedExpiredPause != version else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        let priorState = executionState
        let priorMessage = lastScheduleMessage
        var next = priorState
        next.acknowledgedExpiredPause = version
        do {
            try persistExecutionState(next, message: message)
        } catch {
            executionState = priorState
            lastScheduleMessage = priorMessage
            throw error
        }
    }

    func canonicalProjectionEligibleAtExecutionStart(_ block: ScheduleBlock) -> Bool {
        guard canonicalScheduleBlockActionabilityIssue(block) == nil,
              block.sourceItemID != nil,
              block.occurrenceID == nil,
              block.occurrenceFullyScheduled,
              let itemID = block.sourceItemID,
              let item = canonicalItem(id: itemID),
              block.sourceItemRevision == item.revision,
              item.isExecutable,
              item.recurrence == nil,
              item.supportsLosslessReplacement,
              case .indivisible = item.splitPolicy else { return false }
        return blocks.count(where: {
            $0.sourceItemID == itemID && $0.occurrenceID == nil
        }) == 1
    }

    var hasDeferredExecutionPublicationWork: Bool {
        !deferredExecutionPublicationSessionIDs.isEmpty
    }

    func keepLatestCanonicalItem(forExecutionSession sessionID: UUID) throws {
        guard canKeepLatestCanonicalItem(forExecutionSession: sessionID),
              var outcome = executionState.terminalOutcomes[sessionID] else {
            throw PlannerExecutionStateError.invalidDurableState
        }
        switch outcome.projection {
        case .pending, .conflicted, .retryAuthorized:
            outcome.projection = .keptLatest
        case .notRequired, .applied, .keptLatest:
            throw PlannerExecutionStateError.invalidDurableState
        }
        var next = executionState
        next.terminalOutcomes[sessionID] = outcome
        pendingCanonicalMutations.removeAll { $0.executionSessionID == sessionID }
        try persistExecutionState(
            next,
            message: "Kept the latest canonical item; the execution outcome remains in history",
            reconcilePresentation: true
        )
    }

    func canKeepLatestCanonicalItem(forExecutionSession sessionID: UUID) -> Bool {
        guard canMutatePlan,
              executionProjectionWinner(for: sessionID)?.id == sessionID,
              pendingCanonicalMutations.contains(where: {
                  $0.executionSessionID == sessionID
              }),
              let outcome = executionState.terminalOutcomes[sessionID] else { return false }
        switch outcome.projection {
        case .pending, .conflicted, .retryAuthorized:
            return true
        case .notRequired, .applied, .keptLatest:
            return false
        }
    }

    private func quarantineCredentialBoundState(preservingDeviceID: Bool) {
        let deviceID = preservingDeviceID ? executionState.deviceID : nil
        let preservedCreates = localCreatesPreservedAcrossConfigurationReset()
        blocks.removeAll {
            $0.sourceItemID != nil
                || $0.syncOrigin == .canonicalPreview
                || $0.syncOrigin == .externalPreview
                || $0.syncOrigin == .localComposition
                || $0.syncOrigin == .remoteExecutionLease
        }
        canonicalItems = []
        canonicalTrash = []
        canonicalDeltaCursor = nil
        canonicalTombstoneRevisions = [:]
        completedOccurrenceIDs = []
        pendingCanonicalMutations = []
        pendingCanonicalSensitivityMutations = []
        recurrenceSessionOutcomes = []
        recurrenceOccurrenceMoves = []
        pendingExecutionDeferIntent = nil
        deferredExecutionPublicationSessionIDs = []
        pendingPublicationDeferredSessionIDs = []
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        publishedScheduleProof = nil
        localScheduleCompositionProvenance = nil
        pendingSchedulePublication = nil
        pendingProposalApplicationMutation = nil
        proposalApplicationReceipts = []
        pendingCanonicalAuthoringMutations = preservedCreates
        if let anchor = onboardingFirstItemAnchor,
           preservedCreates.contains(where: {
               $0.itemID == anchor.itemID && $0.operation == .create
           }) {
            onboardingFirstItemAnchor = .init(
                itemID: anchor.itemID,
                canonicalRevision: nil
            )
        } else {
            onboardingFirstItemAnchor = nil
        }
        isCanonicalPreviewValidatedForCurrentLaunch = false
        var empty = DayWeaveExecutionDurableState.empty
        empty.deviceID = deviceID
        executionState = empty
        selectedBlockID = blocks.first?.id
        reconcileSelectedCanonicalItem()
        lastScheduleMessage = "Credential-bound canonical state was quarantined locally"
    }

    /// Unsubmitted local creates belong to the user rather than a server
    /// configuration. Their links to server-owned parents do not: carrying such
    /// UUIDs into another tenant could either strand the draft or silently bind
    /// it to an unrelated item. Keep only links within the preserved local set.
    private func localCreatesPreservedAcrossConfigurationReset()
        -> [DayWeavePendingCanonicalAuthoringMutation] {
        let candidates = pendingCanonicalAuthoringMutations.filter {
            $0.operation == .create && !$0.hasBeenSubmitted
        }
        let localItemIDs = Set(candidates.map(\.itemID))
        return candidates.compactMap { mutation in
            guard var draft = mutation.draft else { return nil }
            if let parentID = draft.parentID, !localItemIDs.contains(parentID) {
                if canonicalItemRequiresSensitivePresentation(itemID: mutation.itemID) {
                    // Once the server-owned ancestor is removed, preserve its
                    // privacy boundary as an explicit mark on the new root.
                    draft.isSensitive = true
                }
                draft.parentID = nil
                draft.siblingOrder = 0
            }
            return DayWeavePendingCanonicalAuthoringMutation(
                id: mutation.id,
                itemID: mutation.itemID,
                operation: .create,
                draft: draft,
                createdAt: mutation.createdAt,
                configurationIdentifier: nil,
                hasBeenSubmitted: false,
                disposition: mutation.disposition,
                diagnostic: mutation.diagnostic
            )
        }
    }

    private func applyExecutionPresentation(to state: inout DayWeaveExecutionDurableState) {
        blocks.removeAll { $0.syncOrigin == .remoteExecutionLease }
        let itemByID = Dictionary(uniqueKeysWithValues: canonicalItems.map { ($0.id, $0) })
        for index in blocks.indices where state.presentedBlockIDs.contains(blocks[index].id) {
            if let itemID = blocks[index].sourceItemID, let item = itemByID[itemID] {
                blocks[index].status = Self.plannerStatus(for: item.status)
                blocks[index].actualMinutes = nil
            }
        }
        state.presentedBlockIDs = []

        let newest = Self.newestExecutionSessionsByProjectionKey(
            terminalSessions: state.terminalOutcomes.values.map(\.session),
            activeSession: state.activeSession
        )

        suppressCanonicalEffectsSupersededByNonCanonicalSession(
            state: &state,
            newest: newest
        )

        for outcome in state.terminalOutcomes.values.sorted(by: {
            Self.executionNewestFirst($0.session, $1.session)
        }) {
            let session = outcome.session
            guard session.status.isCanonicalTerminal else { continue }
            let key = ExecutionProjectionKey(
                itemID: session.itemID,
                itemRevision: session.itemRevision,
                occurrenceID: session.occurrenceID,
                sessionIndex: session.sessionIndex
            )
            guard newest[key]?.id == session.id,
                  outcome.projection != .keptLatest,
                  let index = executionBlockIndex(matching: session) else { continue }
            blocks[index].status = session.status == .completed ? .completed : .skipped
            blocks[index].actualMinutes = session.actualSeconds.map(Self.roundedExecutionMinutes)
            state.presentedBlockIDs.insert(blocks[index].id)
            recordExecutionRecurrenceOutcome(for: blocks[index], session: session)
        }

        if let active = state.activeSession {
            let key = ExecutionProjectionKey(
                itemID: active.itemID,
                itemRevision: active.itemRevision,
                occurrenceID: active.occurrenceID,
                sessionIndex: active.sessionIndex
            )
            if newest[key]?.id == active.id {
                if let index = executionBlockIndex(matching: active) {
                    blocks[index].status = active.status == .active ? .active : .paused
                    blocks[index].actualMinutes = nil
                    state.presentedBlockIDs.insert(blocks[index].id)
                    removeExecutionRecurrenceOutcome(for: blocks[index])
                    selectedBlockID = blocks[index].id
                } else {
                    let item = itemByID[active.itemID]
                    let duration = max(60, TimeInterval(item?.durationSeconds ?? 60))
                    let placeholder = ScheduleBlock(
                        id: active.id,
                        isSensitive: item.map { effectiveSensitivity(itemID: $0.id) } ?? true,
                        title: item?.title ?? "Remote focus session",
                        kind: item.map { Self.plannerKind(for: $0.kind) } ?? .task,
                        start: active.startedAt,
                        end: active.startedAt.addingTimeInterval(duration),
                        status: active.status == .active ? .active : .paused,
                        project: nil,
                        notes: "Authoritative execution lease started on another device",
                        energy: .medium,
                        isFlexible: false,
                        isHardConstraint: true,
                        actualMinutes: nil,
                        sourceItemID: active.itemID,
                        sourceItemRevision: active.itemRevision,
                        occurrenceID: active.occurrenceID,
                        sessionIndex: active.sessionIndex,
                        syncOrigin: .remoteExecutionLease,
                        placementReason: "Cross-device execution lease",
                        previewKind: "remote_execution_lease",
                        occurrenceFullyScheduled: false
                    )
                    blocks.append(placeholder)
                    state.presentedBlockIDs.insert(placeholder.id)
                    selectedBlockID = placeholder.id
                }
            }
        }

        reconcileExecutionCanonicalProjections(state: &state)
        blocks.sort {
            if $0.start != $1.start { return $0.start < $1.start }
            return $0.id.uuidString.lowercased() < $1.id.uuidString.lowercased()
        }
    }

    private func suppressCanonicalEffectsSupersededByNonCanonicalSession(
        state: inout DayWeaveExecutionDurableState,
        newest: [ExecutionProjectionKey: DayWeaveExecutionSession]
    ) {
        var supersededRecurrenceKeys = Set<CanonicalSessionKey>()
        for (sessionID, storedOutcome) in state.terminalOutcomes {
            let session = storedOutcome.session
            guard session.status.isCanonicalTerminal else { continue }
            let key = ExecutionProjectionKey(
                itemID: session.itemID,
                itemRevision: session.itemRevision,
                occurrenceID: session.occurrenceID,
                sessionIndex: session.sessionIndex
            )
            guard let winner = newest[key],
                  winner.id != sessionID,
                  !winner.status.isCanonicalTerminal else { continue }
            if let occurrenceID = session.occurrenceID {
                supersededRecurrenceKeys.insert(.init(
                    itemID: session.itemID,
                    occurrenceID: occurrenceID,
                    sessionIndex: session.sessionIndex
                ))
            }
            switch storedOutcome.projection {
            case .pending, .conflicted, .retryAuthorized:
                var outcome = storedOutcome
                outcome.projection = .notRequired
                state.terminalOutcomes[sessionID] = outcome
            case .notRequired, .applied, .keptLatest:
                break
            }
        }
        // A linked canonical mutation is also the idempotency journal for a
        // request that may already have reached the server. A newer execution
        // session cannot prove otherwise, so only authoritative canonical
        // refresh may clear that mutation.
        guard !supersededRecurrenceKeys.isEmpty else { return }
        let supersededOccurrenceIDs = Set(supersededRecurrenceKeys.compactMap(\.occurrenceID))
        recurrenceSessionOutcomes.removeAll { outcome in
            supersededRecurrenceKeys.contains(.init(
                itemID: outcome.itemID,
                occurrenceID: outcome.occurrenceID,
                sessionIndex: outcome.sessionIndex
            ))
        }
        completedOccurrenceIDs.subtract(supersededOccurrenceIDs)
    }

    private func reconcileExecutionCanonicalProjections(
        state: inout DayWeaveExecutionDurableState
    ) {
        for sessionID in state.terminalOutcomes.keys.sorted(by: {
            $0.uuidString.lowercased() < $1.uuidString.lowercased()
        }) {
            guard var outcome = state.terminalOutcomes[sessionID] else { continue }
            guard outcome.session.status.isCanonicalTerminal else { continue }
            let desired: PlannerItemStatus = outcome.session.status == .completed
                ? .completed : .skipped
            if let item = canonicalItem(id: outcome.session.itemID),
               Self.plannerStatus(for: item.status) == desired,
               item.revision > outcome.session.itemRevision {
                outcome.projection = .applied(revision: item.revision)
                state.terminalOutcomes[sessionID] = outcome
                pendingCanonicalMutations.removeAll { $0.executionSessionID == sessionID }
                continue
            }
            guard outcome.projection == .pending || outcome.projection == .retryAuthorized,
                  !pendingCanonicalMutations.contains(where: {
                      $0.executionSessionID == sessionID
                  }) else {
                continue
            }
            guard let item = canonicalItem(id: outcome.session.itemID) else {
                let diagnostic = "The canonical item is no longer present in the local server cache."
                pendingCanonicalMutations.append(.init(
                    id: UUID(),
                    itemID: outcome.session.itemID,
                    occurrenceID: nil,
                    sessionIndex: outcome.session.sessionIndex,
                    desiredStatus: desired,
                    baseRevision: outcome.session.itemRevision,
                    createdAt: outcome.recordedAt,
                    disposition: .conflicted,
                    diagnostic: diagnostic,
                    executionSessionID: sessionID
                ))
                outcome.projection = .conflicted(diagnostic)
                state.terminalOutcomes[sessionID] = outcome
                continue
            }
            let retryAuthorized = outcome.projection == .retryAuthorized
            let baseRevision = retryAuthorized ? item.revision : outcome.session.itemRevision
            let exactIndex = executionBlockIndex(matching: outcome.session)
            let shapeIsSafe = exactIndex.map {
                canonicalProjectionEligibleAtExecutionStart(blocks[$0])
            } ?? false
            let exactBase = item.revision == baseRevision
            pendingCanonicalMutations.append(.init(
                id: UUID(),
                itemID: item.id,
                occurrenceID: nil,
                sessionIndex: outcome.session.sessionIndex,
                desiredStatus: desired,
                baseRevision: baseRevision,
                createdAt: outcome.recordedAt,
                disposition: shapeIsSafe && exactBase ? .pending : .conflicted,
                diagnostic: shapeIsSafe && exactBase
                    ? nil
                    : "The canonical item changed or is no longer a safe single-block projection.",
                executionSessionID: sessionID
            ))
            if !shapeIsSafe || !exactBase {
                outcome.projection = .conflicted(
                    "The canonical item changed or is no longer a safe single-block projection."
                )
                state.terminalOutcomes[sessionID] = outcome
            }
        }
    }

    private func executionBlockIndex(matching session: DayWeaveExecutionSession) -> Int? {
        func exact(_ block: ScheduleBlock) -> Bool {
            block.sourceItemID == session.itemID
                && block.sourceItemRevision == session.itemRevision
                && block.occurrenceID == session.occurrenceID
                && (block.sessionIndex ?? 0) == session.sessionIndex
        }
        if let plannedID = session.plannedBlockID,
           let index = blocks.firstIndex(where: { $0.id == plannedID && exact($0) }) {
            return index
        }
        let matches = blocks.indices.filter { exact(blocks[$0]) }
        return matches.count == 1 ? matches[0] : nil
    }

    private func recordExecutionRecurrenceOutcome(
        for block: ScheduleBlock,
        session: DayWeaveExecutionSession
    ) {
        guard session.status.isCanonicalTerminal else { return }
        guard let itemID = block.sourceItemID,
              let occurrenceID = block.occurrenceID else { return }
        let sessionIndex = block.sessionIndex ?? 0
        recurrenceSessionOutcomes.removeAll {
            $0.itemID == itemID && $0.occurrenceID == occurrenceID
                && $0.sessionIndex == sessionIndex
        }
        recurrenceSessionOutcomes.append(.init(
            itemID: itemID,
            occurrenceID: occurrenceID,
            sessionIndex: sessionIndex,
            disposition: session.status == .completed ? .completed : .skipped,
            occurredAt: session.endedAt ?? session.updatedAt,
            occurrenceFullyScheduled: block.occurrenceFullyScheduled
        ))
        rebuildOccurrenceRollup(occurrenceID)
        pruneRecurrenceHistory()
    }

    private func removeExecutionRecurrenceOutcome(for block: ScheduleBlock) {
        guard let itemID = block.sourceItemID,
              let occurrenceID = block.occurrenceID else { return }
        recurrenceSessionOutcomes.removeAll {
            $0.itemID == itemID && $0.occurrenceID == occurrenceID
                && $0.sessionIndex == (block.sessionIndex ?? 0)
        }
        rebuildOccurrenceRollup(occurrenceID)
    }

    private static func executionNewestFirst(
        _ left: DayWeaveExecutionSession,
        _ right: DayWeaveExecutionSession
    ) -> Bool {
        if left.updatedAt != right.updatedAt { return left.updatedAt > right.updatedAt }
        return left.id.uuidString.lowercased() > right.id.uuidString.lowercased()
    }

    private static func newestExecutionSessionsByProjectionKey(
        terminalSessions: [DayWeaveExecutionSession],
        activeSession: DayWeaveExecutionSession?
    ) -> [ExecutionProjectionKey: DayWeaveExecutionSession] {
        var newest: [ExecutionProjectionKey: DayWeaveExecutionSession] = [:]
        for session in terminalSessions {
            let key = executionProjectionKey(for: session)
            if let current = newest[key], !executionNewestFirst(session, current) {
                continue
            }
            newest[key] = session
        }
        if let activeSession {
            // The snapshot's one open lease is authoritative even when legacy
            // timestamps across separate sessions are not monotonic.
            newest[executionProjectionKey(for: activeSession)] = activeSession
        }
        return newest
    }

    private static func executionProjectionKey(
        for session: DayWeaveExecutionSession
    ) -> ExecutionProjectionKey {
        ExecutionProjectionKey(
            itemID: session.itemID,
            itemRevision: session.itemRevision,
            occurrenceID: session.occurrenceID,
            sessionIndex: session.sessionIndex
        )
    }

    private static func roundedExecutionMinutes(_ seconds: UInt64) -> Int {
        let rounded = seconds / 60 + (seconds % 60 == 0 ? 0 : 1)
        return Int(min(rounded, UInt64(Int.max)))
    }

    private static func plannerKind(for kind: DayWeaveCanonicalItemKind) -> PlannerItemKind {
        switch kind {
        case .event: .event
        case .task: .task
        case .habit: .habit
        case .routine: .routine
        case .goal: .goal
        case .project: .project
        case .breakTime: .breakTime
        case .unknown: .task
        }
    }

    private static func validateExecutionState(_ state: DayWeaveExecutionDurableState) -> Bool {
        let nilUUID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
        guard state.revision <= UInt64(Int64.max),
              state.historyWindow.count <= DayWeaveAPIClient.maximumExecutionHistoryLimit,
              state.deviceID != nil || !state.hasCredentialBoundState,
              state.deviceID != nilUUID,
              state.bindingIdentifier.map({ !$0.isEmpty && $0.utf8.count <= 1_024 }) ?? true,
              state.historyVerified == state.historyContinuityEstablished,
              !state.historyVerified || state.historyWindowRevision == state.revision,
              Set(state.historyWindow.map(\.id)).count == state.historyWindow.count,
              zip(state.historyWindow, state.historyWindow.dropFirst()).allSatisfy({
                  executionNewestFirst($0, $1)
              }) else { return false }
        if let active = state.activeSession {
            guard active.status.isOpen, active.revision <= state.revision,
                  state.terminalOutcomes[active.id] == nil else { return false }
        }
        for (sessionID, outcome) in state.terminalOutcomes {
            guard sessionID == outcome.session.id,
                  !outcome.session.status.isOpen,
                  outcome.session.revision <= state.revision else { return false }
            if outcome.session.status == .deferred,
               outcome.projection != .notRequired { return false }
            if case let .applied(revision) = outcome.projection,
               revision <= outcome.session.itemRevision { return false }
        }
        let known = Dictionary(uniqueKeysWithValues:
            state.terminalOutcomes.values.map { ($0.session.id, $0.session) }
                + (state.activeSession.map { [($0.id, $0)] } ?? [])
        )
        for session in state.historyWindow where known[session.id] != session { return false }
        if state.historyVerified {
            let sum = known.values.reduce(UInt64(0)) { partial, session in
                partial.addingReportingOverflow(session.revision).overflow
                    ? UInt64.max : partial + session.revision
            }
            guard sum == state.revision,
                  (state.revision == 0) == known.isEmpty else { return false }
        }
        if let pending = state.pendingCommand {
            guard pending.bindingIdentifier == state.bindingIdentifier,
                  pending.expectedRevision == state.revision,
                  pending.encodedRequest.count <= DayWeaveAPIClient.maximumRequestBytes,
                  pending.command.sessionID == pending.identity.sessionID,
                  pending.identity.sourceDeviceID == state.deviceID,
                  pending.focusedBlockID != nilUUID,
                  Self.validExecutionIdempotencyKey(pending.idempotencyKey),
                  let decoded = try? DayWeaveExecutionWireCodec.decode(pending.encodedRequest),
                  decoded.expectedRevision == pending.expectedRevision,
                  decoded.command == pending.command else { return false }
            switch pending.command {
            case let .start(sessionID, itemID, itemRevision, occurrenceID, sessionIndex, blockID, deviceID):
                guard pending.priorSession == nil,
                      pending.identity == .init(
                          sessionID: sessionID,
                          itemID: itemID,
                          itemRevision: itemRevision,
                          occurrenceID: occurrenceID,
                          sessionIndex: sessionIndex,
                          plannedBlockID: blockID,
                          sourceDeviceID: deviceID
                      ) else { return false }
            case .pause, .resume, .complete, .skip, .deferWork:
                guard let prior = pending.priorSession,
                      prior.status.isOpen,
                      pending.identity.matches(prior) else { return false }
            }
        }
        for outcome in state.terminalOutcomes.values {
            if case let .conflicted(diagnostic) = outcome.projection,
               diagnostic.unicodeScalars.count > 2_000 { return false }
        }
        return true
    }

    private static func validExecutionIdempotencyKey(_ value: String) -> Bool {
        let bytes = value.utf8
        return (8...128).contains(bytes.count) && bytes.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte)
                || (97...122).contains(byte) || [46, 95, 58, 45].contains(byte)
        }
    }

    func recurrenceCompletionAnchors() -> [UUID: Date] {
        Dictionary(grouping: recurrenceSessionOutcomes, by: \.itemID)
            .compactMapValues { outcomes in
                let completedOccurrences = Dictionary(grouping: outcomes, by: \.occurrenceID)
                    .values
                    .filter { group in
                        guard let occurrenceID = group.first?.occurrenceID,
                              completedOccurrenceIDs.contains(occurrenceID) else { return false }
                        return group.allSatisfy {
                            $0.disposition == .completed && $0.occurrenceFullyScheduled
                        }
                    }
                return completedOccurrences.flatMap { $0 }.map(\.occurredAt).max()
            }
    }

    var skippedOccurrenceIDs: Set<UUID> {
        Set(recurrenceSessionOutcomes.compactMap {
            $0.disposition == .skipped ? $0.occurrenceID : nil
        })
    }

    private func updateStatus(at index: Int, to status: PlannerItemStatus) {
        guard blocks.indices.contains(index), blocks[index].status != status else { return }
        blocks[index].status = status
        let block = blocks[index]
        if block.sourceItemID != nil {
            if block.occurrenceID != nil && (status == .completed || status == .skipped) {
                removePendingMutation(for: block)
            } else {
                recordPendingMutation(for: block)
            }
        }
        updateRecurrenceOutcome(for: block, status: status)
    }

    private func recordPendingMutation(for block: ScheduleBlock) {
        guard let itemID = block.sourceItemID,
              canonicalAuthoringMutation(itemID: itemID) == nil,
              let revision = canonicalItem(id: itemID)?.revision ?? block.sourceItemRevision else { return }
        let matches: (PendingCanonicalMutation) -> Bool = {
            $0.itemID == itemID
                && $0.occurrenceID == block.occurrenceID
                && ($0.sessionIndex ?? 0) == (block.sessionIndex ?? 0)
        }
        if let item = canonicalItem(id: itemID),
           block.status == Self.plannerStatus(for: item.status) {
            pendingCanonicalMutations.removeAll(where: matches)
            return
        }
        if let index = pendingCanonicalMutations.firstIndex(where: matches) {
            pendingCanonicalMutations[index].desiredStatus = block.status
            pendingCanonicalMutations[index].baseRevision = revision
            pendingCanonicalMutations[index].disposition = .pending
            pendingCanonicalMutations[index].diagnostic = nil
        } else {
            pendingCanonicalMutations.append(.init(
                id: UUID(),
                itemID: itemID,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0,
                desiredStatus: block.status,
                baseRevision: revision,
                createdAt: now(),
                disposition: .pending,
                diagnostic: nil
            ))
        }
    }

    private func removePendingMutation(for block: ScheduleBlock) {
        guard let itemID = block.sourceItemID else { return }
        pendingCanonicalMutations.removeAll {
            $0.itemID == itemID
                && $0.occurrenceID == block.occurrenceID
                && ($0.sessionIndex ?? 0) == (block.sessionIndex ?? 0)
        }
    }

    private func updateRecurrenceOutcome(
        for block: ScheduleBlock,
        status: PlannerItemStatus
    ) {
        guard let itemID = block.sourceItemID,
              let occurrenceID = block.occurrenceID else { return }
        let sessionIndex = block.sessionIndex ?? 0
        recurrenceSessionOutcomes.removeAll {
            $0.itemID == itemID
                && $0.occurrenceID == occurrenceID
                && $0.sessionIndex == sessionIndex
        }
        let disposition: RecurrenceSessionDisposition?
        switch status {
        case .completed: disposition = .completed
        case .skipped: disposition = .skipped
        default: disposition = nil
        }
        if let disposition {
            recurrenceSessionOutcomes.append(.init(
                itemID: itemID,
                occurrenceID: occurrenceID,
                sessionIndex: sessionIndex,
                disposition: disposition,
                occurredAt: now(),
                occurrenceFullyScheduled: block.occurrenceFullyScheduled
            ))
        }
        rebuildOccurrenceRollup(occurrenceID)
        pruneRecurrenceHistory()
    }

    private func rebuildOccurrenceRollup(_ occurrenceID: UUID) {
        completedOccurrenceIDs.remove(occurrenceID)
        let occurrenceBlocks = blocks.filter { $0.occurrenceID == occurrenceID }
        guard !occurrenceBlocks.isEmpty,
              occurrenceBlocks.allSatisfy({
                  $0.status == .completed && $0.occurrenceFullyScheduled
              }) else { return }
        completedOccurrenceIDs.insert(occurrenceID)
    }

    /// Keeps complete occurrence groups, newest first, so pruning cannot turn
    /// a partially retained split occurrence into a false completion anchor.
    @discardableResult
    private func pruneRecurrenceHistory(retainingItemIDs: Set<UUID>? = nil) -> Bool {
        let originalOutcomes = recurrenceSessionOutcomes
        let originalCompleted = completedOccurrenceIDs
        let originalMoves = recurrenceOccurrenceMoves
        var latestBySession: [CanonicalSessionKey: RecurrenceSessionOutcome] = [:]
        for outcome in recurrenceSessionOutcomes
            where retainingItemIDs?.contains(outcome.itemID) ?? true {
            let key = CanonicalSessionKey(
                itemID: outcome.itemID,
                occurrenceID: outcome.occurrenceID,
                sessionIndex: outcome.sessionIndex
            )
            if let existing = latestBySession[key],
               existing.occurredAt > outcome.occurredAt {
                continue
            }
            latestBySession[key] = outcome
        }
        let groups = Dictionary(grouping: latestBySession.values, by: \.occurrenceID)
            .map { occurrenceID, outcomes in
                (
                    occurrenceID,
                    outcomes.sorted {
                        if $0.sessionIndex != $1.sessionIndex {
                            return $0.sessionIndex < $1.sessionIndex
                        }
                        return $0.itemID.uuidString < $1.itemID.uuidString
                    },
                    outcomes.map(\.occurredAt).max() ?? .distantPast
                )
            }
            .sorted {
                if $0.2 != $1.2 { return $0.2 > $1.2 }
                return $0.0.uuidString < $1.0.uuidString
            }
        var retained: [RecurrenceSessionOutcome] = []
        retained.reserveCapacity(min(latestBySession.count, Self.maximumRecurrenceSessionOutcomes))
        var retainedOccurrenceIDs = Set<UUID>()
        for (occurrenceID, outcomes, _) in groups {
            guard outcomes.count <= Self.maximumRecurrenceSessionOutcomes - retained.count else {
                continue
            }
            retained.append(contentsOf: outcomes)
            retainedOccurrenceIDs.insert(occurrenceID)
        }
        if recurrenceSessionOutcomes != retained {
            recurrenceSessionOutcomes = retained
        }
        let knownOutcomeIDs = Set(latestBySession.values.map(\.occurrenceID))
        let legacyCompletedIDs: ArraySlice<UUID>
        if retainingItemIDs == nil {
            legacyCompletedIDs = completedOccurrenceIDs
                .subtracting(knownOutcomeIDs)
                .sorted { $0.uuidString < $1.uuidString }
                .prefix(max(0, Self.maximumRecurrenceSessionOutcomes - retainedOccurrenceIDs.count))
        } else {
            legacyCompletedIDs = []
        }
        let prunedCompleted = completedOccurrenceIDs
            .intersection(retainedOccurrenceIDs)
            .union(legacyCompletedIDs)
        if completedOccurrenceIDs != prunedCompleted {
            completedOccurrenceIDs = prunedCompleted
        }

        let terminalOccurrenceIDs = Set(recurrenceSessionOutcomes.map(\.occurrenceID))
        let currentHorizonStart = (try? scheduleProfile.expanded(asOf: now()))?.horizonStart
        let latestMoves = recurrenceOccurrenceMoves
            .filter { move in
                (retainingItemIDs?.contains(move.itemID) ?? true)
                    && !terminalOccurrenceIDs.contains(move.occurrenceID)
                    && move.hasValidShape
                    && canonicalItem(id: move.itemID)?.revision == move.source?.itemRevision
                    && currentHorizonStart.map({ move.endAt > $0 }) ?? true
            }
            .sorted {
                if $0.movedAt != $1.movedAt { return $0.movedAt > $1.movedAt }
                return $0.occurrenceID.uuidString < $1.occurrenceID.uuidString
            }
        var seenMoveOccurrences = Set<UUID>()
        let retainedMoves = Array(latestMoves.filter {
            seenMoveOccurrences.insert($0.occurrenceID).inserted
        }.prefix(RecurrenceOccurrenceMove.maximumStoredCount))
        if recurrenceOccurrenceMoves != retainedMoves {
            recurrenceOccurrenceMoves = retainedMoves
        }
        let movesChanged = originalMoves != recurrenceOccurrenceMoves
        if movesChanged {
            publishedScheduleProof = nil
            isCanonicalPreviewValidatedForCurrentLaunch = false
            lastScheduleMessage =
                "An obsolete recurring move was removed; sync will publish a fresh schedule"
        }
        return movesChanged
            || originalOutcomes != recurrenceSessionOutcomes
            || originalCompleted != completedOccurrenceIDs
    }

    private static func exactPositiveWholeSeconds(from start: Date, to end: Date) -> Int64? {
        guard let seconds = dayWeaveExactWholeSecondDelta(from: start, to: end),
              seconds <= UInt64(Int64.max) else { return nil }
        return Int64(seconds)
    }

    private func canonicalItem(
        _ itemID: UUID,
        belongsToSeries seriesItemID: UUID
    ) -> Bool {
        var currentID: UUID? = itemID
        var visited = Set<UUID>()
        while let identifier = currentID {
            guard visited.insert(identifier).inserted,
                  let item = canonicalItem(id: identifier) else { return false }
            if identifier == seriesItemID { return true }
            currentID = item.parentID
        }
        return false
    }

    private static func proofContainsReplacement(
        after deferred: DayWeaveExecutionSession,
        proof: DayWeavePublishedScheduleProof
    ) -> Bool {
        guard let moveStart = deferred.moveStart,
              let moveEnd = deferred.moveEnd,
              let moveStartMicros = dayWeavePostgresEpochMicroseconds(moveStart),
              let moveEndMicros = dayWeavePostgresEpochMicroseconds(moveEnd) else {
            return false
        }
        return proof.publishedBlocks.contains { block in
            block.itemID == deferred.itemID
                && block.itemRevision == deferred.itemRevision
                && block.occurrenceID == deferred.occurrenceID
                && block.sessionIndex > deferred.sessionIndex
                && block.kind == "pinned"
                && dayWeavePostgresEpochMicroseconds(block.start) == moveStartMicros
                && dayWeavePostgresEpochMicroseconds(block.end) == moveEndMicros
        }
    }

    private func reconcileOutstandingDeferredPublicationProof(
        authorizedSessionIDs: Set<UUID>
    ) {
        guard let proof = publishedScheduleProof else { return }
        deferredExecutionPublicationSessionIDs = deferredExecutionPublicationSessionIDs.filter {
            guard let deferred = executionState.terminalOutcomes[$0]?.session else {
                return false
            }
            return !authorizedSessionIDs.contains($0)
                || !Self.proofContainsReplacement(after: deferred, proof: proof)
        }
        if !deferredExecutionPublicationSessionIDs.isEmpty {
            publishedScheduleProof = nil
            isCanonicalPreviewValidatedForCurrentLaunch = false
        }
    }

    private static func plannerStatus(
        for status: DayWeaveCanonicalItemStatus
    ) -> PlannerItemStatus {
        switch status {
        case .inbox, .planned: .notStarted
        case .scheduled: .scheduled
        case .inProgress: .active
        case .paused: .paused
        case .blocked: .blocked
        case .completed: .completed
        case .skipped: .skipped
        case .cancelled: .canceled
        case .unknown: .blocked
        }
    }

    private static func canonicalStatus(
        for status: PlannerItemStatus
    ) -> DayWeaveCanonicalItemStatus? {
        switch status {
        case .notStarted: .planned
        case .scheduled: .scheduled
        case .active: .inProgress
        case .paused: .paused
        case .completed: .completed
        case .skipped: .skipped
        case .canceled: .cancelled
        case .blocked: nil
        }
    }

    private func uniqueLocalIdentifier(
        excluding reserved: inout Set<UUID>
    ) -> UUID {
        while true {
            let candidate = UUID()
            if reserved.insert(candidate).inserted { return candidate }
        }
    }

    private func expireLocalSuggestionsInMemory(
        referenceDate: Date,
        forcePendingExpiration: Bool = false,
        forcedSuggestionIDs: Set<UUID> = []
    ) -> Bool {
        var changed = false
        for index in suggestions.indices
            where suggestions[index].state == .pending
                && (forcePendingExpiration
                    || forcedSuggestionIDs.contains(suggestions[index].id)
                    || suggestions[index].expiresAt <= referenceDate
                    || suggestions[index].createdAt > referenceDate.addingTimeInterval(
                        Self.localSuggestionFutureSkewTolerance
                    )) {
            expireLocalSuggestion(at: index)
            changed = true
        }
        clearLocalSuggestionHighWaterIfNoBodiesRemain()
        return changed
    }

    private func observeLocalSuggestionDate(
        candidate: Date? = nil
    ) -> (referenceDate: Date, rollbackDetected: Bool) {
        let candidate = candidate ?? now()
        let pendingCreatedAt = suggestions.compactMap { suggestion -> Date? in
            guard suggestion.state == .pending,
                  case .canonicalItemDraft = suggestion.payload else {
                return nil
            }
            return suggestion.createdAt
        }
        guard let latestPendingCreatedAt = pendingCreatedAt.max() else {
            localSuggestionDateHighWater = nil
            return (candidate, false)
        }
        guard candidate.timeIntervalSinceReferenceDate.isFinite else {
            return (localSuggestionDateHighWater ?? .distantFuture, true)
        }
        guard let highWater = localSuggestionDateHighWater else {
            let initialHighWater = max(candidate, latestPendingCreatedAt)
            localSuggestionDateHighWater = initialHighWater
            return (initialHighWater, false)
        }
        let rollbackDetected = candidate.addingTimeInterval(
            Self.localSuggestionFutureSkewTolerance
        ) < highWater
        if candidate > highWater { localSuggestionDateHighWater = candidate }
        return (max(candidate, highWater), rollbackDetected)
    }

    private func clearLocalSuggestionHighWaterIfNoBodiesRemain() {
        let hasPendingBody = suggestions.contains { suggestion in
            guard suggestion.state == .pending,
                  case .canonicalItemDraft = suggestion.payload else {
                return false
            }
            return true
        }
        if !hasPendingBody { localSuggestionDateHighWater = nil }
    }

    /// Restores a failed user transaction without undoing a privacy expiry
    /// that happened inside the attempted persistence flush. The high-water
    /// checkpoint follows the same rule: a failed action may not lower an
    /// authenticated clock observation while any private draft remains.
    private func restoreLocalSuggestionTransactionPreimage(
        _ preimage: [PlanningSuggestion],
        dateHighWater preimageDateHighWater: Date?
    ) {
        let currentSuggestions = suggestions
        let currentDateHighWater = localSuggestionDateHighWater
        var newlyExpiredByID: [UUID: PlanningSuggestion] = [:]
        for suggestion in currentSuggestions where suggestion.state == .expired {
            newlyExpiredByID[suggestion.id] = suggestion
        }
        suggestions = preimage.map { suggestion in
            guard suggestion.state == .pending,
                  let expired = newlyExpiredByID[suggestion.id],
                  expired.createdAt == suggestion.createdAt,
                  expired.expiresAt == suggestion.expiresAt else {
                return suggestion
            }
            return expired
        }

        let retainedHighWaters = [preimageDateHighWater, currentDateHighWater]
            .compactMap { $0 }
            .filter { $0.timeIntervalSinceReferenceDate.isFinite }
        localSuggestionDateHighWater = retainedHighWaters.max()
        clearLocalSuggestionHighWaterIfNoBodiesRemain()
    }

    private func expireLocalSuggestion(at index: Int) {
        suggestions[index].state = .expired
        scrubTerminalLocalSuggestion(at: index)
    }

    private func scrubTerminalLocalSuggestion(at index: Int) {
        let wasCanonicalDraft: Bool
        if case let .canonicalItemDraft(itemDraft) = suggestions[index].payload {
            suggestions[index].payload = .canonicalItemReference(itemID: itemDraft.itemID)
            wasCanonicalDraft = true
        } else if case .canonicalItemReference = suggestions[index].payload {
            wasCanonicalDraft = true
        } else {
            wasCanonicalDraft = false
        }
        if wasCanonicalDraft {
            // Terminal rows are not presented in the Inbox. Keep only generic,
            // content-free decision metadata and the opaque item correlation;
            // an accepted exact body already lives in the canonical journal.
            suggestions[index].title = PlanningSuggestion.scrubbedCanonicalTitle
            suggestions[index].summary = PlanningSuggestion.scrubbedCanonicalSummary(
                for: suggestions[index].state
            ) ?? "Pending local review."
        }
        if suggestions[index].state != .accepted {
            suggestions[index].resultingItemID = nil
            suggestions[index].resultingMutationID = nil
        }
        clearLocalSuggestionHighWaterIfNoBodiesRemain()
    }

    private func pruneTerminalLocalSuggestionsToFit(additionalCount: Int) {
        guard additionalCount > 0,
              suggestions.count > Self.maximumLocalSuggestions - additionalCount else {
            return
        }
        let removable = suggestions
            .filter { $0.state != .pending }
            .sorted {
                if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
                return $0.id.uuidString < $1.id.uuidString
            }
        let removalCount = min(
            removable.count,
            suggestions.count + additionalCount - Self.maximumLocalSuggestions
        )
        let removedIDs = Set(removable.prefix(removalCount).map(\.id))
        suggestions.removeAll { removedIDs.contains($0.id) }
    }

    private func flushLocalSuggestionTransitionIfAvailable() throws {
        guard persistence != nil else { return }
        try flushLocalSuggestionTransition()
    }

    private func flushLocalSuggestionTransition() throws {
        guard let persistence else {
            throw PlannerCanonicalAuthoringError.encryptedPersistenceRequired
        }
        try persistence.preflightSave(makeSnapshot())
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    private func flushLocalSuggestionPrivacyTransition() throws {
        do {
            try flushLocalSuggestionTransitionIfAvailable()
        } catch {
            // A preflight failure occurs before flushPersistence can install
            // its usual fail-closed state. Privacy expiry is irreversible in
            // memory, so lock further persistence until a clean reload rather
            // than continuing with a scrub that exists only in RAM.
            if loadState == .ready, let persistenceError = error as? PlannerPersistenceError {
                self.persistenceError = persistenceError
                loadState = .persistenceFailed
                cancelLocalSuggestionHighWaterCheckpoint()
            }
            throw error
        }
    }

    private func scheduleAutosave() {
        guard loadState == .ready, persistence != nil else { return }
        autosaveTask?.cancel()
        let delay = autosaveDelay
        autosaveTask = Task { @MainActor [weak self] in
            do {
                try await Task.sleep(for: delay)
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            self?.flushPersistence()
        }
    }

    /// Schedules the next metadata/body expiry independently of UI activity,
    /// so a quiet process cannot retain deleted sensitive content indefinitely.
    private func scheduleCanonicalTrashRetention() {
        canonicalTrashRetentionTask?.cancel()
        canonicalTrashRetentionTask = nil
        guard loadState == .ready, persistence != nil else { return }

        let pinnedItemIDs = pendingCanonicalRecoveryItemIDs
        let currentDate = now()
        let trashExpirations = canonicalTrash.compactMap { entry -> Date? in
            if pinnedItemIDs.contains(entry.id), entry.lastKnownItem == nil {
                // Restore intent pins minimum tombstone metadata, not an old
                // full body. With no body left, this entry has no timed expiry.
                return nil
            }
            return entry.deletedAt.addingTimeInterval(
                Self.canonicalTrashRetentionInterval
            )
        }
        let retainedJournalBodyExpirations = pendingCanonicalAuthoringMutations.compactMap {
            mutation -> Date? in
            guard mutation.baseItem != nil else { return nil }
            let retentionAnchor: Date
            switch mutation.operation {
            case .trash:
                retentionAnchor = mutation.createdAt
            case .restore:
                guard let deletedAt = mutation.baseItem?.deletedAt else { return nil }
                retentionAnchor = min(deletedAt, mutation.createdAt)
            case .create, .replace:
                return nil
            }
            return retentionAnchor.addingTimeInterval(
                Self.canonicalTrashRetentionInterval
            )
        }
        let nextExpiration = (trashExpirations + retainedJournalBodyExpirations).min()
        guard let nextExpiration else { return }

        let seconds = min(
            Self.canonicalTrashRetentionInterval,
            max(0, nextExpiration.timeIntervalSince(currentDate))
        )
        let milliseconds = max(Int64(1), Int64((seconds * 1_000).rounded(.up)))
        canonicalTrashRetentionTask = Task { @MainActor [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(milliseconds))
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            self?.flushPersistence()
        }
    }

    private func cancelLocalSuggestionHighWaterCheckpoint() {
        localSuggestionHighWaterCheckpointTask?.cancel()
        localSuggestionHighWaterCheckpointTask = nil
        localSuggestionHighWaterCheckpointIdentity = nil
    }

    /// A monotonic process timer periodically commits the authenticated wall
    /// observation while private Codex bodies remain. Without this checkpoint,
    /// a quiet app could run for days, crash, and then relaunch after a wall
    /// rollback from the much older observation still on disk.
    private func reconcileLocalSuggestionHighWaterCheckpoint() {
        let hasPendingTypedBody = suggestions.contains { suggestion in
            guard suggestion.state == .pending,
                  case .canonicalItemDraft = suggestion.payload else {
                return false
            }
            return true
        }
        guard loadState == .ready,
              persistence != nil,
              hasPendingTypedBody,
              let identity = persistedLocalSuggestionDateHighWater else {
            cancelLocalSuggestionHighWaterCheckpoint()
            return
        }
        if localSuggestionHighWaterCheckpointTask != nil,
           localSuggestionHighWaterCheckpointIdentity == identity {
            return
        }

        cancelLocalSuggestionHighWaterCheckpoint()
        localSuggestionHighWaterCheckpointIdentity = identity
        let sleep = localSuggestionCheckpointSleep
        let milliseconds = Int64(
            (Self.localSuggestionHighWaterCheckpointInterval * 1_000).rounded(.up)
        )
        localSuggestionHighWaterCheckpointTask = Task { @MainActor [weak self] in
            do {
                try await sleep(.milliseconds(milliseconds))
            } catch {
                return
            }
            guard !Task.isCancelled,
                  let self,
                  self.localSuggestionHighWaterCheckpointIdentity == identity else {
                return
            }
            self.localSuggestionHighWaterCheckpointTask = nil
            self.localSuggestionHighWaterCheckpointIdentity = nil
            self.flushPersistence()
        }
    }

    private func scheduleLocalSuggestionExpiration() {
        guard loadState == .ready, persistence != nil else {
            localSuggestionExpirationTasks.values.forEach { $0.cancel() }
            localSuggestionExpirationTasks.removeAll()
            cancelLocalSuggestionHighWaterCheckpoint()
            return
        }
        let observation = observeLocalSuggestionDate()
        if observation.rollbackDetected {
            expireLocalSuggestions()
            return
        }
        let identities = Set(suggestions.compactMap { suggestion ->
            LocalSuggestionExpirationIdentity? in
            guard suggestion.state == .pending else { return nil }
            return .init(
                id: suggestion.id,
                createdAt: suggestion.createdAt,
                expiresAt: suggestion.expiresAt
            )
        })
        let staleIdentities = localSuggestionExpirationTasks.keys.filter {
            !identities.contains($0)
        }
        for identity in staleIdentities {
            localSuggestionExpirationTasks.removeValue(forKey: identity)?.cancel()
        }
        for identity in identities where localSuggestionExpirationTasks[identity] == nil {
            let seconds = min(
                Self.localSuggestionLifetime,
                max(0, identity.expiresAt.timeIntervalSince(observation.referenceDate))
            )
            let milliseconds = max(Int64(1), Int64((seconds * 1_000).rounded(.up)))
            localSuggestionExpirationTasks[identity] = Task { @MainActor [weak self] in
                do {
                    try await Task.sleep(for: .milliseconds(milliseconds))
                } catch {
                    return
                }
                guard !Task.isCancelled else { return }
                self?.expireLocalSuggestionAtScheduledDeadline(
                    identity.id,
                    createdAt: identity.createdAt,
                    expiresAt: identity.expiresAt
                )
            }
        }
        reconcileLocalSuggestionHighWaterCheckpoint()
    }

    private func makeSnapshot(
        canonicalTrashOverride: [DayWeaveCanonicalTrashEntry]? = nil,
        canonicalAuthoringMutationsOverride:
            [DayWeavePendingCanonicalAuthoringMutation]? = nil
    ) -> PlannerSnapshot {
        PlannerSnapshot(
            destination: destination,
            selectedBlockID: selectedBlockID,
            selectedCanonicalItemID: selectedCanonicalItemID,
            blocks: blocks,
            suggestions: suggestions,
            localSuggestionDateHighWater: localSuggestionDateHighWater,
            assistantMessages: assistantMessages,
            lastScheduleMessage: lastScheduleMessage,
            protectedFreeMinutes: protectedFreeMinutes,
            scheduleProfile: scheduleProfile,
            freezeHours: freezeHours,
            showCompleted: showCompleted,
            canonicalItems: canonicalItems,
            canonicalDeltaCursor: canonicalDeltaCursor,
            canonicalTombstoneRevisions: canonicalTombstoneRevisions,
            completedOccurrenceIDs: completedOccurrenceIDs,
            pendingCanonicalMutations: pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
            recurrenceSessionOutcomes: recurrenceSessionOutcomes,
            recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
            pendingExecutionDeferIntent: pendingExecutionDeferIntent,
            deferredExecutionPublicationSessionIDs:
                deferredExecutionPublicationSessionIDs,
            pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
            canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
            schedulePreviewProvenance: schedulePreviewProvenance,
            publishedScheduleProof: publishedScheduleProof,
            onboardingFirstItemAnchor: onboardingFirstItemAnchor,
            localScheduleCompositionProvenance: localScheduleCompositionProvenance,
            pendingSchedulePublication: pendingSchedulePublication,
            pendingProposalApplicationMutation: pendingProposalApplicationMutation,
            proposalApplicationReceipts: proposalApplicationReceipts,
            pendingCanonicalAuthoringMutations: canonicalAuthoringMutationsOverride
                ?? pendingCanonicalAuthoringMutations,
            canonicalTrash: canonicalTrashOverride ?? canonicalTrash,
            googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
            googleSchedulePublicationRecoveryJournal:
                googleSchedulePublicationRecoveryJournal,
            localCaptureDiagnostics: localCaptureDiagnostics,
            executionState: executionState
        )
    }

    private static func defaultScheduleProfile(
        protectedFreeMinutes: Int,
        timezoneName: String? = nil
    ) -> ScheduleProfile {
        let currentTimezone = ScheduleProfile.normalizedTimezoneName(
            TimeZone.autoupdatingCurrent.identifier
        )
        for candidate in [timezoneName, currentTimezone, "UTC"].compactMap({ $0 }) {
            if let profile = try? ScheduleProfile.legacyDefault(
                timezoneName: candidate,
                protectedFreeMinutes: protectedFreeMinutes
            ) {
                return profile
            }
        }
        // UTC and 90 minutes are compile-time contract constants accepted by
        // the strict model. This is reached only while fail-closing a corrupt
        // legacy value or an unsupported host timezone.
        do {
            return try ScheduleProfile.legacyDefault(
                timezoneName: "UTC",
                protectedFreeMinutes: 90
            )
        } catch {
            preconditionFailure("The built-in schedule profile is invalid: \(error)")
        }
    }

    private static func hierarchicallySorted(
        _ items: [DayWeaveCanonicalItem]
    ) -> [DayWeaveCanonicalItem] {
        let indexed = Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0) })
        func localOrder(_ left: DayWeaveCanonicalItem, _ right: DayWeaveCanonicalItem) -> Bool {
            if left.siblingOrder != right.siblingOrder {
                return left.siblingOrder < right.siblingOrder
            }
            return left.id.uuidString < right.id.uuidString
        }
        var children: [UUID: [DayWeaveCanonicalItem]] = [:]
        var roots: [DayWeaveCanonicalItem] = []
        for item in items {
            if let parentID = item.parentID, parentID != item.id, indexed[parentID] != nil {
                children[parentID, default: []].append(item)
            } else {
                roots.append(item)
            }
        }
        var visited = Set<UUID>()
        var result: [DayWeaveCanonicalItem] = []
        func visitIteratively(from start: DayWeaveCanonicalItem) {
            var stack = [start]
            while let item = stack.popLast() {
                guard visited.insert(item.id).inserted else { continue }
                result.append(item)
                // Reverse-push keeps the resulting preorder in ascending local
                // order without recursive calls on an unbounded hierarchy.
                for child in children[item.id, default: []]
                    .sorted(by: localOrder)
                    .reversed() {
                    stack.append(child)
                }
            }
        }
        for root in roots.sorted(by: localOrder) { visitIteratively(from: root) }
        // Cycles are invalid server data, but retain every item in a stable
        // order and break the cycle deterministically instead of violating the
        // comparator contract.
        for item in items.sorted(by: localOrder) { visitIteratively(from: item) }
        return result
    }

    static func live() -> PlannerStore {
        do {
            return live(persistence: try EncryptedPlannerPersistence.applicationDefault())
        } catch {
            let store = PlannerStore()
            store.persistenceError = error
            store.loadState = .persistenceFailed
            return store
        }
    }

    /// Production startup restores synchronously before the store is exposed.
    /// With no snapshot, it starts empty rather than presenting preview data
    /// that actions could accidentally target.
    static func live(persistence: EncryptedPlannerPersistence) -> PlannerStore {
        PlannerStore(persistence: persistence)
    }

    static func preview(now: Date = Date()) -> PlannerStore {
        let calendar = Calendar.current
        let day = calendar.startOfDay(for: now)
        func at(_ hour: Int, _ minute: Int = 0) -> Date {
            calendar.date(byAdding: .minute, value: hour * 60 + minute, to: day) ?? day
        }

        let blocks: [ScheduleBlock] = [
            .init(id: UUID(), title: "Morning reset", kind: .routine, start: at(7, 30), end: at(8), status: .completed, project: nil, notes: "Water, plan, and prepare", energy: .low, isFlexible: true, isHardConstraint: false, actualMinutes: 27),
            .init(id: UUID(), title: "Walk outside", kind: .habit, start: at(8, 10), end: at(8, 40), status: .completed, project: "Health", notes: "Habit target: 30 minutes", energy: .low, isFlexible: true, isHardConstraint: false, actualMinutes: 31),
            .init(id: UUID(), title: "Architecture deep work", kind: .task, start: at(9), end: at(10, 30), status: .active, project: "DayWeave", notes: "Finish sync boundary and review the scheduler contract.", energy: .deep, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
            .init(id: UUID(), title: "Coffee & reset", kind: .breakTime, start: at(10, 30), end: at(10, 45), status: .scheduled, project: nil, notes: "Protected break", energy: .low, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Weekly planning call", kind: .event, start: at(11), end: at(11, 45), status: .scheduled, project: "DayWeave", notes: "Google Calendar · attendee event", energy: .medium, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Review scheduler tests", kind: .task, start: at(12), end: at(12, 45), status: .scheduled, project: "DayWeave", notes: "Can split into sessions of at least 20 minutes.", energy: .deep, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
            .init(id: UUID(), title: "Lunch", kind: .breakTime, start: at(13), end: at(13, 45), status: .scheduled, project: nil, notes: "Protected meal", energy: .low, isFlexible: false, isHardConstraint: true, actualMinutes: nil),
            .init(id: UUID(), title: "Read 20 pages", kind: .habit, start: at(16), end: at(16, 30), status: .scheduled, project: "Learning", notes: "Preferred after 15:00", energy: .medium, isFlexible: true, isHardConstraint: false, actualMinutes: nil),
        ]

        let suggestions = [
            PlanningSuggestion(
                id: UUID(),
                title: "Protect a recovery window",
                summary: "Move “Read 20 pages” to 17:10 and keep 16:00–17:00 free after the dense work block.",
                source: "DayWeave assistant",
                createdAt: now,
                expiresAt: calendar.date(byAdding: .day, value: 7, to: now) ?? now,
                state: .pending
            )
        ]

        let messages = [
            AssistantMessage(
                id: UUID(),
                role: .assistant,
                text: "Your hard commitments fit. The afternoon is intentionally lighter because the morning has two deep-focus blocks.",
                createdAt: now
            )
        ]
        return PlannerStore(
            blocks: blocks,
            suggestions: suggestions,
            assistantMessages: messages,
            lastScheduleMessage: "Schedule is balanced",
            now: { now }
        )
    }
}

extension PlannerStore: GoogleOutboundRecoveryStoring {
    func loadGoogleOutboundRecoveryJournal() throws -> GoogleOutboundRecoveryJournal? {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleOutboundRecoveryError.encryptedPersistenceRequired
        }
        guard googleOutboundRecoveryJournal?.hasValidShape != false else {
            throw PlannerGoogleOutboundRecoveryError.invalidJournal
        }
        return googleOutboundRecoveryJournal
    }

    func saveGoogleOutboundRecoveryJournal(
        _ journal: GoogleOutboundRecoveryJournal
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleOutboundRecoveryError.encryptedPersistenceRequired
        }
        guard journal.hasValidShape,
              Self.googleOutboundTransitionIsValid(
                  from: googleOutboundRecoveryJournal,
                  to: journal
              ) else {
            throw PlannerGoogleOutboundRecoveryError.journalConflict
        }
        if googleOutboundRecoveryJournal == nil,
           googleSchedulePublicationRecoveryJournal != nil {
            throw PlannerGoogleOutboundRecoveryError.journalConflict
        }
        guard googleOutboundRecoveryJournal != journal else { return }

        let previous = googleOutboundRecoveryJournal
        googleOutboundRecoveryJournal = journal
        flushPersistence()
        if let persistenceError {
            googleOutboundRecoveryJournal = previous
            throw persistenceError
        }
    }

    func clearGoogleOutboundRecoveryJournal(
        _ expected: GoogleOutboundRecoveryJournal
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleOutboundRecoveryError.encryptedPersistenceRequired
        }
        guard expected.hasValidShape,
              googleOutboundRecoveryJournal == expected else {
            throw PlannerGoogleOutboundRecoveryError.journalConflict
        }

        googleOutboundRecoveryJournal = nil
        flushPersistence()
        if let persistenceError {
            googleOutboundRecoveryJournal = expected
            throw persistenceError
        }
    }

    private static func googleOutboundTransitionIsValid(
        from existing: GoogleOutboundRecoveryJournal?,
        to replacement: GoogleOutboundRecoveryJournal
    ) -> Bool {
        guard replacement.hasValidShape else { return false }
        guard let existing else { return replacement.stage == .intent }
        guard existing.hasValidShape else { return false }
        if existing == replacement { return true }

        guard existing.recoveryID == replacement.recoveryID,
              existing.operationGeneration == replacement.operationGeneration,
              existing.configurationIdentifier == replacement.configurationIdentifier,
              existing.accountID == replacement.accountID,
              existing.collectionID == replacement.collectionID,
              existing.itemID == replacement.itemID,
              existing.expectedItemRevision == replacement.expectedItemRevision,
              existing.entityKind == replacement.entityKind,
              existing.operation == replacement.operation,
              existing.intentExpiresAt == replacement.intentExpiresAt,
              existing.createdAt == replacement.createdAt else {
            return false
        }

        switch (existing.stage, replacement.stage) {
        case (.intent, .previewed):
            guard let preview = replacement.preview else { return false }
            return (try? existing.recording(preview: preview)) == replacement
        case (.previewed, .approvalAttempted):
            return (try? existing.recordingApprovalAttempt()) == replacement
        case (.approvalAttempted, .approved):
            guard let preview = existing.preview,
                  let capability = replacement.approvalCapability,
                  let expiresAt = replacement.approvalExpiresAt else {
                return false
            }
            let approval = GoogleOutboundApproval(
                previewID: preview.id,
                approvalCapability: capability,
                expiresAt: expiresAt
            )
            return (try? existing.recording(approval: approval)) == replacement
        case (.intent, .intent), (.previewed, .previewed),
             (.approvalAttempted, .approvalAttempted), (.approved, .approved),
             (.intent, .approvalAttempted), (.intent, .approved),
             (.previewed, .intent), (.previewed, .approved),
             (.approvalAttempted, .intent), (.approvalAttempted, .previewed),
             (.approved, .intent), (.approved, .previewed),
             (.approved, .approvalAttempted):
            return false
        }
    }
}

extension PlannerStore: GoogleSchedulePublicationRecoveryStoring {
    func loadGoogleSchedulePublicationRecoveryJournal() throws
        -> GoogleSchedulePublicationRecoveryJournal? {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleSchedulePublicationRecoveryError.encryptedPersistenceRequired
        }
        guard googleSchedulePublicationRecoveryJournal?.hasValidShape != false else {
            throw PlannerGoogleSchedulePublicationRecoveryError.invalidJournal
        }
        return googleSchedulePublicationRecoveryJournal
    }

    func saveGoogleSchedulePublicationRecoveryJournal(
        _ journal: GoogleSchedulePublicationRecoveryJournal
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleSchedulePublicationRecoveryError.encryptedPersistenceRequired
        }
        guard journal.hasValidShape,
              Self.googleSchedulePublicationTransitionIsValid(
                  from: googleSchedulePublicationRecoveryJournal,
                  to: journal
              ) else {
            throw PlannerGoogleSchedulePublicationRecoveryError.journalConflict
        }
        if googleSchedulePublicationRecoveryJournal == nil {
            guard googleOutboundRecoveryJournal == nil,
                  let proof = publishedScheduleProof,
                  proof.hasCurrentImmutablePlanSeal,
                  proof.revisionID == journal.expectedScheduleRevisionID,
                  proof.configurationIdentifier == journal.configurationIdentifier,
                  proof.matchesPublishedPlan(blocks) else {
                throw PlannerGoogleSchedulePublicationRecoveryError
                    .currentPublishedScheduleRequired
            }
        }
        guard googleSchedulePublicationRecoveryJournal != journal else { return }

        let previous = googleSchedulePublicationRecoveryJournal
        googleSchedulePublicationRecoveryJournal = journal
        flushPersistence()
        if let persistenceError {
            googleSchedulePublicationRecoveryJournal = previous
            throw persistenceError
        }
    }

    func clearGoogleSchedulePublicationRecoveryJournal(
        _ expected: GoogleSchedulePublicationRecoveryJournal
    ) throws {
        guard hasEncryptedPersistence, canPersistPlan else {
            throw PlannerGoogleSchedulePublicationRecoveryError.encryptedPersistenceRequired
        }
        guard expected.hasValidShape,
              googleSchedulePublicationRecoveryJournal == expected else {
            throw PlannerGoogleSchedulePublicationRecoveryError.journalConflict
        }
        googleSchedulePublicationRecoveryJournal = nil
        flushPersistence()
        if let persistenceError {
            googleSchedulePublicationRecoveryJournal = expected
            throw persistenceError
        }
    }

    private static func googleSchedulePublicationTransitionIsValid(
        from existing: GoogleSchedulePublicationRecoveryJournal?,
        to replacement: GoogleSchedulePublicationRecoveryJournal
    ) -> Bool {
        guard replacement.hasValidShape else { return false }
        guard let existing else { return replacement.stage == .intent }
        guard existing.hasValidShape else { return false }
        if existing == replacement { return true }
        guard existing.recoveryID == replacement.recoveryID,
              existing.operationGeneration == replacement.operationGeneration,
              existing.configurationIdentifier == replacement.configurationIdentifier,
              existing.accountID == replacement.accountID,
              existing.collectionID == replacement.collectionID,
              existing.expectedScheduleRevisionID == replacement.expectedScheduleRevisionID,
              existing.intentExpiresAt == replacement.intentExpiresAt,
              existing.createdAt == replacement.createdAt else {
            return false
        }

        switch (existing.stage, replacement.stage) {
        case (.intent, .previewed):
            guard let preview = replacement.preview else { return false }
            return (try? existing.recording(preview: preview)) == replacement
        case (.previewed, .approvalAttempted):
            return (try? existing.recordingApprovalAttempt()) == replacement
        case (.approvalAttempted, .approved):
            guard let preview = existing.preview,
                  let capability = replacement.approvalCapability,
                  let expiresAt = replacement.approvalExpiresAt else { return false }
            return (try? existing.recording(approval: GoogleSchedulePublicationApproval(
                previewID: preview.id,
                approvalCapability: capability,
                expiresAt: expiresAt
            ))) == replacement
        case (.approved, .accepted):
            guard let acceptance = replacement.acceptance else { return false }
            return (try? existing.recording(acceptance: acceptance)) == replacement
        case (.accepted, .accepted):
            guard let deliveryStatus = replacement.deliveryStatus else { return false }
            return (try? existing.recording(status: deliveryStatus)) == replacement
        case (.intent, .intent), (.previewed, .previewed),
             (.approvalAttempted, .approvalAttempted), (.approved, .approved),
             (.intent, .approvalAttempted), (.intent, .approved), (.intent, .accepted),
             (.previewed, .intent), (.previewed, .approved), (.previewed, .accepted),
             (.approvalAttempted, .intent), (.approvalAttempted, .previewed),
             (.approvalAttempted, .accepted), (.approved, .intent),
             (.approved, .previewed), (.approved, .approvalAttempted),
             (.accepted, .intent), (.accepted, .previewed),
             (.accepted, .approvalAttempted), (.accepted, .approved):
            return false
        }
    }
}
