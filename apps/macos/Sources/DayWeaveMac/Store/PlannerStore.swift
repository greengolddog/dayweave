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
        }
    }
}

enum PlannerSchedulePublicationError: LocalizedError, Equatable, Sendable {
    case invalidJournal
    case publicationAlreadyPending
    case publicationDoesNotMatchJournal

    var errorDescription: String? {
        switch self {
        case .invalidJournal:
            "The exact schedule publication journal is invalid or exceeds its encrypted size limit."
        case .publicationAlreadyPending:
            "An earlier schedule publication has an ambiguous result. Restore its API configuration and sync to recover it exactly."
        case .publicationDoesNotMatchJournal:
            "The schedule publication response does not match the encrypted request awaiting recovery."
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

@MainActor
final class PlannerStore: ObservableObject {
    static let maximumCanonicalTitleScalars = 500
    static let maximumRecurrenceSessionOutcomes = 10_000
    static let maximumCanonicalTrashEntries = 500
    static let maximumCanonicalTrashItemBytes = 256 * 1_024
    static let maximumCanonicalTrashRetainedItemBytes = 4 * 1_024 * 1_024
    static let canonicalTrashRetentionInterval: TimeInterval = 30 * 24 * 60 * 60
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
    @Published private(set) var canonicalConfigurationIdentifier: String? {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var schedulePreviewProvenance: SchedulePreviewProvenance? {
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
    @Published private(set) var localCaptureDiagnostics: [UUID: String] {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var executionState: DayWeaveExecutionDurableState {
        didSet { scheduleAutosave() }
    }
    @Published private(set) var isCanonicalSyncLocked = false
    @Published var isQuickAddPresented = false
    @Published var lastScheduleMessage: String {
        didSet { scheduleAutosave() }
    }
    @Published var protectedFreeMinutes = 90 {
        didSet { scheduleAutosave() }
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
    private var autosaveTask: Task<Void, Never>?
    private var canonicalTrashRetentionTask: Task<Void, Never>?
    private var isCanonicalPreviewValidatedForCurrentLaunch = false
    private var persistenceRevision: PlannerPersistenceRevision = .missing

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
        canonicalConfigurationIdentifier: String? = nil,
        schedulePreviewProvenance: SchedulePreviewProvenance? = nil,
        localScheduleCompositionProvenance: LocalScheduleCompositionProvenance? = nil,
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? = nil,
        proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt] = [],
        pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = [],
        canonicalTrash: [DayWeaveCanonicalTrashEntry] = [],
        googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal? = nil,
        selectedCanonicalItemID: UUID? = nil,
        localCaptureDiagnostics: [UUID: String] = [:],
        executionState: DayWeaveExecutionDurableState = .empty,
        previewValidatedForCurrentLaunch: Bool = false,
        lastScheduleMessage: String = "No schedule yet — add an item when you’re ready",
        persistence: EncryptedPlannerPersistence? = nil,
        restoreFromPersistence: Bool = true,
        autosaveDelay: Duration = .milliseconds(250),
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.persistence = persistence
        self.autosaveDelay = autosaveDelay
        self.now = now

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
        self.canonicalConfigurationIdentifier = initialCanonicalConfigurationIdentifier
        let initialSchedulePreviewProvenance = restoredSnapshot?.schedulePreviewProvenance
            ?? schedulePreviewProvenance
        let initialLocalScheduleCompositionProvenance = restoredSnapshot == nil
            ? localScheduleCompositionProvenance
            : restoredSnapshot?.localScheduleCompositionProvenance
        self.schedulePreviewProvenance = initialSchedulePreviewProvenance
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
        self.pendingSchedulePublication = restoredSnapshot?.pendingSchedulePublication
            ?? pendingSchedulePublication
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
        let initialGoogleOutboundRecoveryJournal = restoredSnapshot == nil
            ? googleOutboundRecoveryJournal
            : restoredSnapshot?.googleOutboundRecoveryJournal
        self.googleOutboundRecoveryJournal = initialGoogleOutboundRecoveryJournal
        if initialGoogleOutboundRecoveryJournal?.hasValidShape == false {
            restorationError = .snapshotDecodingFailed
        }
        self.localCaptureDiagnostics = restoredSnapshot?.localCaptureDiagnostics
            ?? localCaptureDiagnostics
        let initialExecutionState = restoredSnapshot?.executionState ?? executionState
        self.executionState = initialExecutionState
        if !Self.validateExecutionState(initialExecutionState) {
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
        protectedFreeMinutes = restoredSnapshot?.protectedFreeMinutes ?? 90
        freezeHours = restoredSnapshot?.freezeHours ?? 2
        showCompleted = restoredSnapshot?.showCompleted ?? true
        persistenceError = restorationError
        loadState = restorationError == nil ? .ready : .persistenceFailed
        persistenceRevision = restoredRevision

        pruneRecurrenceHistory()
        hardenPendingSensitivityPresentation()

        if persistence != nil, restorationError == nil {
            if restoreFromPersistence, restoredSnapshot == nil {
                scheduleAutosave()
            } else if restoredCanonicalRetentionNeedsRewrite {
                // Retention is a durable privacy boundary. Rewrite an old
                // snapshot before exposing an indefinitely quiet restored app.
                flushPersistence()
            } else {
                scheduleCanonicalTrashRetention()
            }
        }
    }

    deinit {
        autosaveTask?.cancel()
        canonicalTrashRetentionTask?.cancel()
    }

    func flushPersistence() {
        autosaveTask?.cancel()
        autosaveTask = nil
        canonicalTrashRetentionTask?.cancel()
        canonicalTrashRetentionTask = nil
        guard loadState == .ready, let persistence else { return }
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

        do {
            persistenceRevision = try persistence.save(
                makeSnapshot(
                    canonicalTrashOverride: boundedTrash,
                    canonicalAuthoringMutationsOverride: boundedMutations
                ),
                expectedRevision: persistenceRevision
            )
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
        } catch {
            persistenceError = error
            loadState = .persistenceFailed
        }
    }

    var canPersistPlan: Bool {
        loadState == .ready
    }

    var canMutatePlan: Bool {
        canPersistPlan && !isCanonicalSyncLocked
    }

    var hasEncryptedPersistence: Bool {
        persistence != nil
    }

    @discardableResult
    func beginCanonicalSync() -> Bool {
        guard canPersistPlan,
              !isCanonicalSyncLocked,
              pendingProposalApplicationMutation == nil,
              googleOutboundRecoveryJournal == nil else { return false }
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
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        localScheduleCompositionProvenance = nil
        pendingSchedulePublication = nil
        pendingProposalApplicationMutation = nil
        proposalApplicationReceipts = []
        pendingCanonicalAuthoringMutations = preservedCreates
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
            generatedAt = provenance.generatedAt
            asOf = provenance.asOf
            horizonStart = provenance.horizonStart
            horizonEnd = provenance.horizonEnd
            timezoneName = provenance.timezoneName
        } else if let provenance = schedulePreviewProvenance {
            guard provenance.configurationIdentifier == canonicalConfigurationIdentifier else {
                return "The visible preview is not bound to the active API configuration."
            }
            generatedAt = provenance.generatedAt
            asOf = provenance.asOf
            horizonStart = provenance.horizonStart
            horizonEnd = provenance.horizonEnd
            timezoneName = provenance.timezoneName
        } else {
            return "The visible canonical schedule has no trusted composition evidence."
        }
        let currentTime = now()
        guard generatedAt <= currentTime.addingTimeInterval(5 * 60),
              currentTime.timeIntervalSince(generatedAt) <= 6 * 3_600 else {
            return "The visible schedule is older than the six-hour execution safety window. Sync or compose again."
        }
        let currentTimezoneIdentifier = TimeZone.autoupdatingCurrent.identifier == "GMT"
            ? "UTC"
            : TimeZone.autoupdatingCurrent.identifier
        guard let timezone = TimeZone(identifier: timezoneName),
              timezoneName == currentTimezoneIdentifier else {
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

    /// Checked while the shared mutation lock is held, so it deliberately does
    /// not consult `canMutatePlan`. Existing server-preview authority continues
    /// through its publication path; this adds the stricter evidence gate that
    /// locally composed blocks require before execution.
    func canonicalScheduleBlockActionabilityIssue(_ block: ScheduleBlock) -> String? {
        guard block.syncOrigin == .localComposition else { return nil }
        if let issue = canonicalPreviewFreshnessIssue { return issue }
        guard let itemID = block.sourceItemID,
              let revision = block.sourceItemRevision,
              let item = canonicalItem(id: itemID),
              revision == item.revision,
              item.isExecutable else {
            return "The scheduled block no longer matches its canonical item revision."
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
            || schedulePreviewProvenance != nil
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
        let calendar = Calendar.autoupdatingCurrent
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
        guard let index = blocks.firstIndex(where: { $0.id == id }),
              canMutate(blocks[index]),
              blocks[index].isFlexible,
              !blocks[index].isHardConstraint else { return }
        let delta: TimeInterval = 60 * 60
        blocks[index].start.addTimeInterval(delta)
        blocks[index].end.addTimeInterval(delta)
        blocks.sort { $0.start < $1.start }
        lastScheduleMessage = "Moved one hour later locally; constraints will be validated on sync"
    }

    func recomposeSchedule() {
        guard canRecomposeSchedule else { return }
        let frozenUntil = now().addingTimeInterval(TimeInterval(freezeHours * 3_600))
        var cursor: Date?

        for index in blocks.indices where blocks[index].isFlexible && blocks[index].start > frozenUntil {
            if let cursor, blocks[index].start < cursor {
                let duration = blocks[index].end.timeIntervalSince(blocks[index].start)
                blocks[index].start = cursor
                blocks[index].end = cursor.addingTimeInterval(duration)
            }
            cursor = blocks[index].end.addingTimeInterval(10 * 60)
        }
        blocks.sort { $0.start < $1.start }
        lastScheduleMessage = "Locally reordered beyond the \(freezeHours)-hour freeze; sync to validate constraints"
    }

    func applyCanonicalDelta(
        _ changes: [DayWeaveItemDeltaChange],
        nextCursor: String
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
        canonicalTrash = Self.boundedCanonicalTrash(
            canonicalTrash,
            referenceDate: now(),
            pinnedItemIDs: pendingCanonicalRecoveryItemIDs
        )
        canonicalDeltaCursor = nextCursor
        reconcileSelectedCanonicalItem()
        hardenPendingSensitivityPresentation()
        pruneRecurrenceHistory(retainingItemIDs: Set(indexed.keys))
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
        if mutation.baseItem.map({
            DayWeaveCanonicalItemDraft(item: $0).matches(item)
        }) ?? true {
            pendingCanonicalAuthoringMutations.remove(at: index)
        } else {
            pendingCanonicalAuthoringMutations[index].disposition = .conflicted
            pendingCanonicalAuthoringMutations[index].diagnostic =
                "The item was restored elsewhere with different content. Review the retained deleted version and the active revision."
        }
    }

    func replaceCanonicalState(
        changes: [DayWeaveItemDeltaChange],
        nextCursor: String
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
        applyCanonicalDelta(changes, nextCursor: nextCursor)

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
        reconcileSelectedCanonicalItem()
        hardenPendingSensitivityPresentation()
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
    }

    func bindLocalBlock(_ blockID: UUID, to item: DayWeaveCanonicalItem) {
        guard canPersistPlan,
              let index = blocks.firstIndex(where: { $0.id == blockID }) else { return }
        blocks[index].sourceItemID = item.id
        blocks[index].sourceItemRevision = item.revision
        blocks[index].syncOrigin = .canonicalPreview
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
        pendingCanonicalAuthoringMutations.remove(at: index)
        reconcileSelectedCanonicalItem()
        do {
            try flushCanonicalAuthoringTransition()
        } catch {
            pendingCanonicalAuthoringMutations.insert(mutation, at: index)
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
                  mutation.draft?.matches(response) == true else {
                throw PlannerCanonicalAuthoringError.invalidRemoteResponse
            }
        case .replace:
            guard let expected = mutation.expectedRevision,
                  let draft = mutation.draft,
                  response.deletedAt == nil,
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
                      DayWeaveCanonicalItemDraft(item: $0).matches(response)
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
                      DayWeaveCanonicalItemDraft(item: $0).matches(response)
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
        selectedCanonicalItemID = response.id
        guard currentCanonicalAuthoringStateIsValid else {
            canonicalItems = priorItems
            canonicalTrash = priorTrash
            canonicalTombstoneRevisions = priorTombstones
            pendingCanonicalAuthoringMutations = priorMutations
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
              pendingSchedulePublication == nil else {
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
        flushPersistence()
        if let persistenceError { throw persistenceError }
    }

    func commitPendingSchedulePublication(
        _ publication: PendingSchedulePublication,
        blocks newBlocks: [ScheduleBlock]
    ) throws {
        guard pendingSchedulePublication == publication else {
            throw PlannerSchedulePublicationError.publicationDoesNotMatchJournal
        }
        let priorBlocks = blocks
        let priorSelection = selectedBlockID
        let priorProvenance = schedulePreviewProvenance
        let priorLocalProvenance = localScheduleCompositionProvenance
        let priorExecutionState = executionState
        let priorMessage = lastScheduleMessage

        applySchedulePreviewInMemory(
            blocks: newBlocks,
            message: publication.message,
            provenance: publication.provenance
        )
        pendingSchedulePublication = nil
        flushPersistence()
        if let persistenceError {
            blocks = priorBlocks
            selectedBlockID = priorSelection
            schedulePreviewProvenance = priorProvenance
            localScheduleCompositionProvenance = priorLocalProvenance
            executionState = priorExecutionState
            lastScheduleMessage = priorMessage
            pendingSchedulePublication = publication
            // A failed atomic local commit must never leave either the prior
            // or newly published projection actionable in this process.
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
        pendingSchedulePublication = nil
        isCanonicalPreviewValidatedForCurrentLaunch = false
        flushPersistence()
        if let persistenceError {
            pendingSchedulePublication = publication
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
        for mutation in pendingCanonicalMutations {
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

    func canonicalItem(id: UUID) -> DayWeaveCanonicalItem? {
        canonicalItems.first(where: { $0.id == id })
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

    func acceptSuggestion(_ id: UUID) {
        guard canMutatePlan else { return }
        guard let index = suggestions.firstIndex(where: { $0.id == id }) else { return }
        suggestions[index].state = .accepted
    }

    func rejectSuggestion(_ id: UUID) {
        guard canMutatePlan else { return }
        guard let index = suggestions.firstIndex(where: { $0.id == id }) else { return }
        suggestions[index].state = .rejected
    }

    /// Rebuilds durable intent for snapshots written before mutation tracking
    /// existed, and for any status edit made by an older client build.
    func capturePendingCanonicalMutations() {
        guard canPersistPlan else { return }
        let itemByID = Dictionary(uniqueKeysWithValues: canonicalItems.map { ($0.id, $0) })
        let authoringItemIDs = Set(pendingCanonicalAuthoringMutations.map(\.itemID))
        pendingCanonicalMutations.removeAll { authoringItemIDs.contains($0.itemID) }
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
                keysToRemove.contains(.init(
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
        for index in pendingCanonicalMutations.indices
            where pendingCanonicalMutations[index].itemID == itemID {
            pendingCanonicalMutations[index].disposition = .conflicted
            pendingCanonicalMutations[index].diagnostic = diagnostic
            if let sessionID = pendingCanonicalMutations[index].executionSessionID,
               var outcome = executionState.terminalOutcomes[sessionID] {
                outcome.projection = .conflicted(diagnostic)
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
        executionState = next
        if let message { lastScheduleMessage = message }
        flushPersistence()
        if let persistenceError { throw persistenceError }
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

    func keepLatestCanonicalItem(forExecutionSession sessionID: UUID) throws {
        guard var outcome = executionState.terminalOutcomes[sessionID] else {
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
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        localScheduleCompositionProvenance = nil
        pendingSchedulePublication = nil
        pendingProposalApplicationMutation = nil
        proposalApplicationReceipts = []
        pendingCanonicalAuthoringMutations = preservedCreates
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

        let allSessions = state.terminalOutcomes.values.map(\.session)
            + (state.activeSession.map { [$0] } ?? [])
        let newest = allSessions.sorted(by: Self.executionNewestFirst).reduce(
            into: [ExecutionProjectionKey: DayWeaveExecutionSession]()
        ) { result, session in
            let key = ExecutionProjectionKey(
                itemID: session.itemID,
                itemRevision: session.itemRevision,
                occurrenceID: session.occurrenceID,
                sessionIndex: session.sessionIndex
            )
            if result[key] == nil { result[key] = session }
        }

        for outcome in state.terminalOutcomes.values.sorted(by: {
            Self.executionNewestFirst($0.session, $1.session)
        }) {
            let session = outcome.session
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

    private func reconcileExecutionCanonicalProjections(
        state: inout DayWeaveExecutionDurableState
    ) {
        for sessionID in state.terminalOutcomes.keys.sorted(by: {
            $0.uuidString.lowercased() < $1.uuidString.lowercased()
        }) {
            guard var outcome = state.terminalOutcomes[sessionID] else { continue }
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
            case .pause, .resume, .complete, .skip:
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
    private func pruneRecurrenceHistory(retainingItemIDs: Set<UUID>? = nil) {
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
    }

    private static func plannerStatus(
        for status: DayWeaveCanonicalItemStatus
    ) -> PlannerItemStatus {
        switch status {
        case .inbox, .planned: .notStarted
        case .scheduled: .scheduled
        case .inProgress: .active
        case .paused: .paused
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
            assistantMessages: assistantMessages,
            lastScheduleMessage: lastScheduleMessage,
            protectedFreeMinutes: protectedFreeMinutes,
            freezeHours: freezeHours,
            showCompleted: showCompleted,
            canonicalItems: canonicalItems,
            canonicalDeltaCursor: canonicalDeltaCursor,
            canonicalTombstoneRevisions: canonicalTombstoneRevisions,
            completedOccurrenceIDs: completedOccurrenceIDs,
            pendingCanonicalMutations: pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
            recurrenceSessionOutcomes: recurrenceSessionOutcomes,
            canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
            schedulePreviewProvenance: schedulePreviewProvenance,
            localScheduleCompositionProvenance: localScheduleCompositionProvenance,
            pendingSchedulePublication: pendingSchedulePublication,
            pendingProposalApplicationMutation: pendingProposalApplicationMutation,
            proposalApplicationReceipts: proposalApplicationReceipts,
            pendingCanonicalAuthoringMutations: canonicalAuthoringMutationsOverride
                ?? pendingCanonicalAuthoringMutations,
            canonicalTrash: canonicalTrashOverride ?? canonicalTrash,
            googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
            localCaptureDiagnostics: localCaptureDiagnostics,
            executionState: executionState
        )
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
            lastScheduleMessage: "Schedule is balanced"
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
