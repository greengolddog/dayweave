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
            "Reconcile the pending execution or canonical projection before replacing credentials."
        }
    }
}

enum CanonicalSensitivityPresentation: Equatable, Sendable {
    case standard
    case own
    case inherited
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
    @Published var destination: SidebarDestination? = .today {
        didSet { scheduleAutosave() }
    }
    @Published var selectedBlockID: UUID? {
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
        self.blocks = initialBlocks
        self.suggestions = restoredSnapshot?.suggestions ?? suggestions
        self.assistantMessages = restoredSnapshot?.assistantMessages ?? assistantMessages
        self.canonicalItems = restoredSnapshot?.canonicalItems ?? canonicalItems
        self.canonicalDeltaCursor = restoredSnapshot?.canonicalDeltaCursor ?? canonicalDeltaCursor
        self.canonicalTombstoneRevisions = restoredSnapshot?.canonicalTombstoneRevisions
            ?? canonicalTombstoneRevisions
        self.completedOccurrenceIDs = restoredSnapshot?.completedOccurrenceIDs ?? completedOccurrenceIDs
        self.pendingCanonicalMutations = restoredSnapshot?.pendingCanonicalMutations ?? pendingCanonicalMutations
        self.pendingCanonicalSensitivityMutations = restoredSnapshot?.pendingCanonicalSensitivityMutations
            ?? pendingCanonicalSensitivityMutations
        self.recurrenceSessionOutcomes = restoredSnapshot?.recurrenceSessionOutcomes ?? recurrenceSessionOutcomes
        self.canonicalConfigurationIdentifier = restoredSnapshot?.canonicalConfigurationIdentifier
            ?? canonicalConfigurationIdentifier
        self.schedulePreviewProvenance = restoredSnapshot?.schedulePreviewProvenance
            ?? schedulePreviewProvenance
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
        self.lastScheduleMessage = restoredSnapshot?.lastScheduleMessage ?? lastScheduleMessage
        protectedFreeMinutes = restoredSnapshot?.protectedFreeMinutes ?? 90
        freezeHours = restoredSnapshot?.freezeHours ?? 2
        showCompleted = restoredSnapshot?.showCompleted ?? true
        persistenceError = restorationError
        loadState = restorationError == nil ? .ready : .persistenceFailed
        persistenceRevision = restoredRevision

        pruneRecurrenceHistory()

        if persistence != nil, restoreFromPersistence, restoredSnapshot == nil, restorationError == nil {
            scheduleAutosave()
        }
    }

    func flushPersistence() {
        autosaveTask?.cancel()
        autosaveTask = nil
        guard loadState == .ready, let persistence else { return }

        do {
            persistenceRevision = try persistence.save(
                makeSnapshot(),
                expectedRevision: persistenceRevision
            )
            persistenceError = nil
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
        guard canPersistPlan, !isCanonicalSyncLocked else { return false }
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
        guard canMutatePlan, !hasExecutionCredentialReplacementBlocker else { return }
        blocks.removeAll {
            $0.sourceItemID != nil
                || $0.syncOrigin == .canonicalPreview
                || $0.syncOrigin == .externalPreview
                || $0.syncOrigin == .remoteExecutionLease
        }
        canonicalItems = []
        canonicalDeltaCursor = nil
        canonicalTombstoneRevisions = [:]
        completedOccurrenceIDs = []
        pendingCanonicalMutations = []
        pendingCanonicalSensitivityMutations = []
        recurrenceSessionOutcomes = []
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        localCaptureDiagnostics = localCaptureDiagnostics.filter { id, _ in
            blocks.contains { $0.id == id && $0.isLocallyAuthored && $0.sourceItemID == nil }
        }
        var resetExecution = DayWeaveExecutionDurableState.empty
        resetExecution.deviceID = executionState.deviceID
        executionState = resetExecution
        isCanonicalPreviewValidatedForCurrentLaunch = false
        selectedBlockID = blocks.first?.id
        lastScheduleMessage = "Canonical cache reset locally; no server data was changed"
        flushPersistence()
    }

    var canonicalPreviewFreshnessIssue: String? {
        guard isCanonicalPreviewValidatedForCurrentLaunch else {
            return "Sync successfully in this app session before changing canonical preview blocks."
        }
        guard let provenance = schedulePreviewProvenance,
              provenance.configurationIdentifier == canonicalConfigurationIdentifier else {
            return "The visible preview is not bound to the active API configuration."
        }
        let currentTime = now()
        guard provenance.generatedAt <= currentTime.addingTimeInterval(5 * 60),
              currentTime.timeIntervalSince(provenance.generatedAt) <= 6 * 3_600 else {
            return "The visible preview is older than the six-hour execution safety window. Sync again."
        }
        let currentTimezoneIdentifier = TimeZone.autoupdatingCurrent.identifier == "GMT"
            ? "UTC"
            : TimeZone.autoupdatingCurrent.identifier
        guard let timezone = TimeZone(identifier: provenance.timezoneName),
              provenance.timezoneName == currentTimezoneIdentifier else {
            return "The planning timezone changed. Sync again before changing canonical blocks."
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timezone
        guard calendar.isDate(provenance.asOf, inSameDayAs: currentTime),
              currentTime >= provenance.horizonStart,
              currentTime < provenance.horizonEnd else {
            return "The visible preview is outside its generated day or planning horizon. Sync again."
        }
        return nil
    }

    func canMutate(_ block: ScheduleBlock) -> Bool {
        guard canMutatePlan else { return false }
        guard block.syncOrigin != .remoteExecutionLease else { return false }
        guard block.syncOrigin == .canonicalPreview || block.syncOrigin == .externalPreview else {
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
            guard block.syncOrigin == .canonicalPreview || block.syncOrigin == .externalPreview else {
                return true
            }
            return canMutate(block)
        }
    }

    private var hasCanonicalRemoteState: Bool {
        !canonicalItems.isEmpty
            || canonicalDeltaCursor != nil
            || !canonicalTombstoneRevisions.isEmpty
            || !completedOccurrenceIDs.isEmpty
            || !pendingCanonicalMutations.isEmpty
            || !pendingCanonicalSensitivityMutations.isEmpty
            || !recurrenceSessionOutcomes.isEmpty
            || schedulePreviewProvenance != nil
            || blocks.contains {
                $0.syncOrigin == .canonicalPreview
                    || $0.syncOrigin == .externalPreview
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
                    indexed.removeValue(forKey: tombstone.id)
                    canonicalTombstoneRevisions[tombstone.id] = tombstone.revision
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
        canonicalItems = Self.hierarchicallySorted(Array(indexed.values))
        canonicalDeltaCursor = nextCursor
        hardenPendingSensitivityPresentation()
        pruneRecurrenceHistory(retainingItemIDs: Set(indexed.keys))
    }

    func replaceCanonicalState(
        changes: [DayWeaveItemDeltaChange],
        nextCursor: String
    ) {
        guard canPersistPlan else { return }
        canonicalItems = []
        canonicalDeltaCursor = nil
        canonicalTombstoneRevisions = [:]
        applyCanonicalDelta(changes, nextCursor: nextCursor)
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

    func applySchedulePreview(
        blocks newBlocks: [ScheduleBlock],
        message: String,
        provenance: SchedulePreviewProvenance
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
        schedulePreviewProvenance = provenance
        isCanonicalPreviewValidatedForCurrentLaunch = true
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
        let items = Dictionary(uniqueKeysWithValues: canonicalItems.map { ($0.id, $0) })
        let pendingMarks: Set<UUID> = includingPendingMarks
            ? Set(pendingCanonicalSensitivityMutations.compactMap {
                $0.requiresSensitivePresentation ? $0.itemID : nil
            })
            : Set()
        var visited = Set<UUID>()
        var currentID: UUID? = itemID
        var sensitive = false
        while let id = currentID {
            guard visited.insert(id).inserted, let item = items[id] else { return true }
            sensitive = sensitive || item.isSensitive || pendingMarks.contains(id)
            currentID = item.parentID
        }
        return sensitive
    }

    func canonicalSensitivityPresentation(itemID: UUID) -> CanonicalSensitivityPresentation {
        guard let item = canonicalItem(id: itemID) else { return .inherited }
        if item.isSensitive { return .own }
        return effectiveSensitivity(itemID: itemID, includingPendingMarks: false)
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
            if effectiveSensitivity(itemID: itemID) { blocks[index].isSensitive = true }
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
        guard block.sourceItemID != nil,
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
        blocks.removeAll {
            $0.sourceItemID != nil
                || $0.syncOrigin == .canonicalPreview
                || $0.syncOrigin == .externalPreview
                || $0.syncOrigin == .remoteExecutionLease
        }
        canonicalItems = []
        canonicalDeltaCursor = nil
        canonicalTombstoneRevisions = [:]
        completedOccurrenceIDs = []
        pendingCanonicalMutations = []
        pendingCanonicalSensitivityMutations = []
        recurrenceSessionOutcomes = []
        canonicalConfigurationIdentifier = nil
        schedulePreviewProvenance = nil
        isCanonicalPreviewValidatedForCurrentLaunch = false
        var empty = DayWeaveExecutionDurableState.empty
        empty.deviceID = deviceID
        executionState = empty
        selectedBlockID = blocks.first?.id
        lastScheduleMessage = "Credential-bound canonical state was quarantined locally"
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

    private func makeSnapshot() -> PlannerSnapshot {
        PlannerSnapshot(
            destination: destination,
            selectedBlockID: selectedBlockID,
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
