import Foundation

/// Ephemeral GET authority. It is never restored from disk or a PUT receipt.
struct ItemCompletionReadAdmission: Equatable {
    let generation: UInt64
    let snapshot: ItemCompletionSnapshot
    var evidenceHash: String { snapshot.evidenceHash }
}

/// Settings recovery carries no policy choices or reopening details, even when the
/// current item/ancestry is unavailable. Content review belongs in admitted detail.
struct ItemCompletionRecoveryEntry: Identifiable, Equatable {
    let id: UUID
    let itemID: UUID
    let isRejected: Bool
    let canDiscard: Bool
}

struct ItemCompletionReviewLease: Equatable {
    fileprivate let generation: UInt64
    fileprivate let selection: UUID
    fileprivate let itemID: UUID
    fileprivate let operationID: UUID?
    fileprivate let evidenceGeneration: UInt64
}

@MainActor
final class ItemCompletionStore: ObservableObject {
    @Published private(set) var message = "Review which descendants are required and how this parent completes."
    @Published private(set) var isWorking = false
    private let planner: PlannerStore
    private let connection: @MainActor () throws -> any ItemCompletionTransport
    private let catchUp: @MainActor () async -> Bool
    private let now: @Sendable () -> Date
    private let sleep: @Sendable (Duration) async throws -> Void
    private let connectivity: (@Sendable () -> AsyncStream<Bool>)?
    private let automaticOutbox: Bool
    private var generation: UInt64 = 0
    private var privacyAvailable = false
    private var operationID: UUID?
    private var outboxLoop: ItemProgressPollingLoop?
    private var visibleLoop: ItemProgressPollingLoop?
    private var connectivityTask: Task<Void, Never>?
    private struct DetailSelection: Equatable {
        let id = UUID()
        let owner: UUID
        let itemID: UUID
        let configuration: String
    }
    private var detail: DetailSelection?
    private var catchUpBinding: String?
    private var catchUpItems = Set<UUID>()
    private var sourceCache = CanonicalHierarchySourceCache()

    init(planner: PlannerStore, connection: @escaping @MainActor () throws -> any ItemCompletionTransport,
         catchUp: @escaping @MainActor () async -> Bool = { false }, now: @escaping @Sendable () -> Date = Date.init,
         sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
         connectivity: (@Sendable () -> AsyncStream<Bool>)? = nil, automaticOutbox: Bool = true) {
        self.planner = planner; self.connection = connection; self.catchUp = catchUp; self.now = now; self.sleep = sleep
        self.connectivity = connectivity
        self.automaticOutbox = automaticOutbox
    }

    convenience init(planner: PlannerStore, canonicalSync: CanonicalSyncStore, authCoordinator: DurableAuthCoordinator) {
        let configuration = UserDefaultsSuggestionAPIConfigurationStore()
        let session = makeDayWeaveEphemeralSession()
        self.init(planner: planner, connection: {
            guard let raw = configuration.loadBaseURL() else { throw ItemCompletionError.configurationChanged }
            let base = try DayWeaveAPIBaseURL(raw)
            guard authCoordinator.hasUsableCredential(boundTo: base) else { throw ItemCompletionError.configurationChanged }
            return DayWeaveAPIClient(baseURL: base, session: session, authCoordinator: authCoordinator)
        }, catchUp: { await canonicalSync.refreshItemCompletionCanonicalEvidence() },
           connectivity: { ItemProgressConnectivity.updates() })
    }

    var hasPendingRecovery: Bool {
        planner.itemCompletionState.needsCanonicalCatchUp
            || planner.itemCompletionState.journals.contains { $0.noEffectCode == nil }
    }
    var hasAdmittedConnection: Bool {
        privacyAvailable && (try? connection().configurationIdentifier) == planner.canonicalConfigurationIdentifier
            && planner.canonicalConfigurationIdentifier != nil
    }

    func observation(for itemID: UUID) -> ItemCompletionObservation? {
        guard hasAdmittedConnection, admittedItem(itemID) != nil,
              planner.itemCompletionState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return nil }
        return planner.itemCompletionState.observations.first { $0.snapshot.itemID == itemID }
    }

    func journal(for itemID: UUID) -> ItemCompletionJournal? {
        guard hasAdmittedConnection, admittedItem(itemID) != nil else { return nil }
        return recoveryJournal(for: itemID)
    }

    var recoveryEntries: [ItemCompletionRecoveryEntry] {
        guard hasAdmittedConnection,
              planner.itemCompletionState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return [] }
        return planner.itemCompletionState.journals.map {
            .init(id: $0.id, itemID: $0.itemID, isRejected: $0.noEffectCode != nil,
                canDiscard: !$0.hasBeenSubmitted || $0.noEffectCode != nil)
        }
    }

    private func recoveryJournal(for itemID: UUID) -> ItemCompletionJournal? {
        guard hasAdmittedConnection,
              planner.itemCompletionState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return nil }
        return planner.itemCompletionState.journals.first { $0.itemID == itemID }
    }

    func canReview(_ itemID: UUID) -> Bool {
        guard privacyAvailable, !isWorking, planner.hasEncryptedPersistence, planner.canMutatePlan,
              !planner.hasPendingItemCompletionAuthorityChanges,
              !requiresCanonicalCatchUp(itemID),
              let item = admittedItem(itemID), let observation = observation(for: itemID), observation.isReadProof,
              observation.snapshot.itemRevision == item.revision,
              planner.hasCurrentItemCompletionRead(itemID, snapshot: observation.snapshot) else { return false }
        guard !planner.itemCompletionState.journals.contains(where: {
            $0.itemID != itemID && $0.noEffectCode == nil
        }) else { return false }
        let pending = journal(for: itemID)
        return pending == nil || pending?.noEffectCode != nil || pending?.hasBeenSubmitted == false
    }

    func isSensitive(_ itemID: UUID) -> Bool {
        // V1 has no server-issued subtree-privacy or canonical-cursor witness.
        // A remote private grandchild can change these counts without changing
        // this parent's revision. Even a fresh GET cannot safely relax privacy
        // using the older local forest; all derived review/intent is protected.
        true
    }

    private func requiresCanonicalCatchUp(_ itemID: UUID) -> Bool {
        planner.itemCompletionState.needsCanonicalCatchUp
            || (catchUpBinding == planner.canonicalConfigurationIdentifier && catchUpItems.contains(itemID))
    }

    private func requireCanonicalCatchUp(_ itemID: UUID) {
        if catchUpBinding != planner.canonicalConfigurationIdentifier { catchUpItems.removeAll() }
        catchUpBinding = planner.canonicalConfigurationIdentifier
        catchUpItems.insert(itemID)
    }

    private func admittedItem(_ itemID: UUID) -> DayWeaveCanonicalItem? {
        guard planner.canPersistPlan, planner.persistenceError == nil,
              planner.canonicalConfigurationIdentifier?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false,
              planner.canonicalDeltaCursor?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false,
              planner.pendingProposalApplicationMutation == nil,
              let row = sourceCache.presentation(for: planner).hierarchyRows.first(where: { $0.itemID == itemID }),
              !row.hasUnsafeAncestry, !row.hasMissingParent, !row.hasHierarchyCycle,
              let item = row.activeCanonicalItem, item.deletedAt == nil else { return nil }
        if case .unknown = item.kind { return nil }
        if case .unknown = item.status { return nil }
        let byID = Dictionary(planner.canonicalItems.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let pending = Set(planner.pendingCanonicalAuthoringMutations.map(\.itemID))
            .union(planner.pendingCanonicalSensitivityMutations.map(\.itemID))
            .union(planner.pendingCanonicalMutations.map(\.itemID))
        var cursor: UUID? = itemID
        var visited = Set<UUID>()
        while let id = cursor {
            guard visited.insert(id).inserted, !pending.contains(id), let node = byID[id], node.deletedAt == nil else { return nil }
            cursor = node.parentID
        }
        return item
    }

    func activate() {
        guard !privacyAvailable else { return }
        privacyAvailable = true
        generation &+= 1
        let started = generation
        if automaticOutbox {
            outboxLoop = ItemProgressPollingLoop(initialDelay: .seconds(5), sleep: sleep) { [weak self] in
                guard let self, self.privacyAvailable, self.generation == started else { return .unavailable }
                guard !self.planner.isCanonicalSyncLocked else { return .busy }
                let hadPending = self.hasPendingRecovery
                let success = await self.replayPending()
                if success && hadPending { self.visibleLoop?.wake() }
                return success ? .success : .unavailable
            }
        }
        startDetailLoop()
        if let connectivity {
            connectivityTask = Task { [weak self] in
                for await available in connectivity() {
                    guard let self, !Task.isCancelled, self.privacyAvailable, self.generation == started else { return }
                    // A satisfied replacement route also merits retry, even if
                    // the bounded stream coalesced its preceding offline event.
                    if available { self.connectionBecameAvailable() }
                }
            }
        }
    }

    func suspendForPrivacyBoundary() {
        privacyAvailable = false
        generation &+= 1
        outboxLoop?.stop(); outboxLoop = nil
        visibleLoop?.stop(); visibleLoop = nil
        connectivityTask?.cancel(); connectivityTask = nil
        sourceCache.clear()
        planner.invalidateItemCompletionReadEvidence()
        message = "Unlock DayWeave to review completion."
    }

    func configurationDidChange() {
        let wasAvailable = privacyAvailable
        let sameBindingSelection = detail.flatMap { selection in
            selection.configuration == planner.canonicalConfigurationIdentifier
                && (try? connection().configurationIdentifier) == selection.configuration ? selection : nil
        }
        suspendForPrivacyBoundary()
        // Preserve a mounted panel only when both durable and live authority
        // still prove its exact binding. Never retarget it to a new account.
        detail = sameBindingSelection
        if wasAvailable { activate() }
    }

    func showDetail(_ itemID: UUID, owner: UUID) {
        guard let configuration = planner.canonicalConfigurationIdentifier, !configuration.isEmpty else { return }
        if detail?.owner == owner && detail?.itemID == itemID && detail?.configuration == configuration { return }
        visibleLoop?.stop(); visibleLoop = nil
        detail = DetailSelection(owner: owner, itemID: itemID, configuration: configuration)
        startDetailLoop()
    }

    private func startDetailLoop() {
        guard privacyAvailable, let selection = detail,
              selection.configuration == planner.canonicalConfigurationIdentifier else { return }
        let started = generation
        visibleLoop = ItemProgressPollingLoop(sleep: sleep) { [weak self] in
            guard let self, self.privacyAvailable, self.generation == started, self.detail == selection else { return .unavailable }
            guard !self.planner.isCanonicalSyncLocked else { return .busy }
            return await self.refresh(selection.itemID, selection: selection) ? .success : .unavailable
        }
    }

    func hideDetail(owner: UUID) {
        guard detail?.owner == owner else { return }
        visibleLoop?.stop(); visibleLoop = nil; detail = nil
    }

    func refreshVisibleDetail(owner: UUID) {
        guard privacyAvailable, detail?.owner == owner else { return }
        visibleLoop?.wake()
    }

    func connectionBecameAvailable() {
        guard privacyAvailable else { return }
        outboxLoop?.wake(); visibleLoop?.wake()
    }

    func reviewLease(itemID: UUID, owner: UUID) -> ItemCompletionReviewLease? {
        guard canReview(itemID), let selection = detail, selection.owner == owner,
              selection.itemID == itemID, selection.configuration == planner.canonicalConfigurationIdentifier else { return nil }
        return .init(generation: generation, selection: selection.id, itemID: itemID,
                     operationID: journal(for: itemID)?.id,
                     evidenceGeneration: planner.itemCompletionEvidenceGeneration)
    }

    func queueReviewed(lease: ItemCompletionReviewLease, baseline: ItemCompletionSnapshot,
                       requiredForParent: Bool, mode: ItemCompletionMode) throws {
        guard lease.generation == generation, detail?.id == lease.selection,
              lease.evidenceGeneration == planner.itemCompletionEvidenceGeneration else { throw ItemCompletionError.staleReview }
        try queue(itemID: lease.itemID, baseline: baseline, requiredForParent: requiredForParent,
                  mode: mode, expectedOperationID: lease.operationID)
    }

    func reviewIsCurrent(_ lease: ItemCompletionReviewLease, baseline: ItemCompletionSnapshot) -> Bool {
        canReview(lease.itemID) && lease.generation == generation && detail?.id == lease.selection
            && lease.evidenceGeneration == planner.itemCompletionEvidenceGeneration
            && journal(for: lease.itemID)?.id == lease.operationID
            && observation(for: lease.itemID)?.snapshot == baseline
    }

    @discardableResult
    func refresh(_ itemID: UUID) async -> Bool {
        await refresh(itemID, selection: nil)
    }

    private func refresh(_ itemID: UUID, selection: DetailSelection?) async -> Bool {
        let currentGeneration = generation
        if planner.itemCompletionState.needsCanonicalCatchUp, !(await recoverCanonical()) { return false }
        for attempt in 0..<2 {
            guard privacyAvailable, generation == currentGeneration, !Task.isCancelled,
                  selection == nil || detail == selection, admittedItem(itemID) != nil else { return false }
            do {
                let evidenceGeneration = planner.itemCompletionEvidenceGeneration
                let received = try await fetch(itemID)
                guard privacyAvailable, generation == currentGeneration, !Task.isCancelled,
                      selection == nil || detail == selection else { throw ItemCompletionError.privacyBoundary }
                guard let item = admittedItem(itemID) else { throw ItemCompletionError.unavailable }
                guard evidenceGeneration == planner.itemCompletionEvidenceGeneration else {
                    throw ItemCompletionError.staleReview
                }
                if received.itemRevision != item.revision {
                    planner.invalidateItemCompletionReadEvidence()
                    requireCanonicalCatchUp(itemID)
                    message = "Task details need to catch up before completion can be reviewed."
                    if attempt == 0 { _ = await catchUp(); continue }
                    return false
                }
                var next = planner.itemCompletionState
                let prior = next
                next.configurationIdentifier = planner.canonicalConfigurationIdentifier
                try next.observe(received, at: now())
                try planner.commitItemCompletionState(next, replacing: prior)
                try planner.admitItemCompletionRead(received, generation: evidenceGeneration)
                if catchUpBinding == planner.canonicalConfigurationIdentifier { catchUpItems.remove(itemID) }
                message = "Completion evidence fetched. Changes to tasks or execution require a new review."
                return true
            } catch {
                guard privacyAvailable, generation == currentGeneration, !Task.isCancelled,
                      selection == nil || detail == selection else { return false }
                if error as? ItemCompletionError == .definitive("item_completion_item_missing"), attempt == 0 {
                    planner.invalidateItemCompletionReadEvidence()
                    requireCanonicalCatchUp(itemID)
                    _ = await catchUp()
                }
                if privacyAvailable, generation == currentGeneration, !Task.isCancelled { report(error) }
                return false
            }
        }
        return false
    }

    private func fetch(_ itemID: UUID) async throws -> ItemCompletionSnapshot {
        let client = try connection()
        let operation = try begin(client)
        let current = generation
        defer { end(operation) }
        let value = try await client.itemCompletion(itemID)
        try assertCurrent(operation, generation: current, client: client)
        guard value.isValid && value.itemID == itemID else { throw ItemCompletionError.invalidData }
        return value
    }

    func queue(itemID: UUID, baseline: ItemCompletionSnapshot, requiredForParent: Bool, mode: ItemCompletionMode,
               expectedOperationID: UUID? = nil) throws {
        guard canReview(itemID), let item = admittedItem(itemID),
              journal(for: itemID)?.id == expectedOperationID,
              observation(for: itemID)?.snapshot == baseline, item.revision == baseline.itemRevision else {
            throw ItemCompletionError.staleReview
        }
        guard mode == baseline.state.mode || (!baseline.occurrenceEvidenceRequired && canOverride(itemID)) else {
            throw ItemCompletionError.staleReview
        }
        let client = try connection()
        guard client.configurationIdentifier == planner.canonicalConfigurationIdentifier else { throw ItemCompletionError.configurationChanged }
        let command = ItemCompletionCommand(expectedItemRevision: item.revision,
            expectedCompletionRevision: baseline.state.revision, expectedEvidenceHash: baseline.evidenceHash,
            requiredForParent: requiredForParent, mode: mode)
        let journal = ItemCompletionJournal(version: 1, itemID: itemID, configurationIdentifier: client.configurationIdentifier,
            command: command, requestBody: try command.bytes(), createdAt: now(), wasSensitive: isSensitive(itemID),
            hasBeenSubmitted: false, noEffectCode: nil)
        var next = planner.itemCompletionState
        let prior = next
        next.journals.removeAll { $0.itemID == itemID }
        next.journals.append(journal)
        try planner.commitItemCompletionState(next, replacing: prior)
        message = "Reviewed completion request saved in the encrypted outbox. Task status is not changed locally."
        outboxLoop?.wake()
    }

    func discardReviewedIntent(_ itemID: UUID, expectedOperationID: UUID) throws {
        guard privacyAvailable, !isWorking, let journal = recoveryJournal(for: itemID),
              !journal.hasBeenSubmitted || journal.noEffectCode != nil else { throw ItemCompletionError.busy }
        guard journal.id == expectedOperationID else { throw ItemCompletionError.staleReview }
        var next = planner.itemCompletionState; let prior = next
        next.journals.removeAll { $0.id == journal.id }
        try planner.commitItemCompletionState(next, replacing: prior)
    }

    @discardableResult
    func replayPending() async -> Bool {
        guard privacyAvailable, !Task.isCancelled else { return false }
        let started = generation
        if planner.itemCompletionState.needsCanonicalCatchUp, !(await recoverCanonical()) { return false }
        let pending = planner.itemCompletionState.journals.filter { $0.noEffectCode == nil }
        guard !pending.isEmpty else { return true }
        for journal in pending {
            guard privacyAvailable, generation == started, !Task.isCancelled else { return false }
            do { try await send(journal) }
            catch {
                if privacyAvailable, generation == started, !Task.isCancelled { report(error) }
                return false
            }
        }
        return privacyAvailable && generation == started && !Task.isCancelled
            && !planner.itemCompletionState.needsCanonicalCatchUp
    }

    private func send(_ retained: ItemCompletionJournal) async throws {
        if !retained.hasBeenSubmitted {
            guard !planner.hasPendingItemCompletionAuthorityChanges else { throw ItemCompletionError.busy }
            if !planner.hasCurrentItemCompletionRead(retained.itemID) {
                guard await refresh(retained.itemID) else { throw ItemCompletionError.unavailable }
            }
        }
        let client = try connection()
        guard retained.configurationIdentifier == client.configurationIdentifier else { throw ItemCompletionError.configurationChanged }
        let operation = try begin(client)
        let current = generation
        defer { end(operation) }
        guard var journal = planner.itemCompletionState.journals.first(where: { $0.retainsCustody(of: retained) }) else {
            throw ItemCompletionError.staleReview
        }
        if !journal.hasBeenSubmitted {
            guard !requiresCanonicalCatchUp(journal.itemID),
                  let item = admittedItem(journal.itemID), item.revision == journal.command.expectedItemRevision,
                  let baseline = observation(for: journal.itemID), baseline.isReadProof,
                  planner.hasCurrentItemCompletionRead(journal.itemID, snapshot: baseline.snapshot),
                  baseline.snapshot.state.revision == journal.command.expectedCompletionRevision,
                  baseline.snapshot.evidenceHash == journal.command.expectedEvidenceHash else {
                try recordNoEffect(journal, code: "item_completion_evidence_stale")
                return
            }
            var next = planner.itemCompletionState; let prior = next
            journal.hasBeenSubmitted = true
            next.journals = next.journals.map { $0.id == journal.id ? journal : $0 }
            try planner.commitItemCompletionState(next, replacing: prior)
        }
        do {
            let receipt = try await client.putItemCompletion(journal.itemID, requestBody: journal.requestBody)
            try assertCurrent(operation, generation: current, client: client)
            guard receipt.matches(itemID: journal.itemID, command: journal.command),
                  planner.itemCompletionState.journals.contains(where: { $0.retainsCustody(of: journal) }) else {
                throw ItemCompletionError.invalidData
            }
            var next = planner.itemCompletionState; let prior = next
            next.journals.removeAll { $0.id == journal.id }
            try next.observe(receipt.completion, at: now(), isReadProof: false)
            next.needsCanonicalCatchUp = true
            try planner.commitItemCompletionState(next, replacing: prior)
            message = "Completion confirmed. Waiting for the complete task update before further editing."
        } catch let ItemCompletionError.definitive(code) {
            try assertCurrent(operation, generation: current, client: client)
            try recordNoEffect(journal, code: code)
        }
        end(operation)
        if planner.itemCompletionState.needsCanonicalCatchUp { _ = await recoverCanonical() }
    }

    private func recordNoEffect(_ journal: ItemCompletionJournal, code: String) throws {
        guard planner.itemCompletionState.journals.contains(where: { $0.retainsCustody(of: journal) }),
              ItemCompletionJournal.definitiveCodes.contains(code) else {
            throw ItemCompletionError.invalidData
        }
        var next = planner.itemCompletionState; let prior = next
        next.journals = next.journals.map { value in
            guard value.id == journal.id else { return value }
            var result = value; result.noEffectCode = code; return result
        }
        try planner.commitItemCompletionState(next, replacing: prior)
        // A definitive rejection disproves the reviewed workspace-wide evidence.
        // Retain the exact choices, but never revive an old GET lease afterward.
        planner.invalidateItemCompletionReadEvidence()
        message = "Completion was not applied. Refresh and explicitly review the saved choices."
    }

    private func begin(_ client: any ItemCompletionTransport) throws -> UUID {
        guard privacyAvailable, !Task.isCancelled else { throw ItemCompletionError.privacyBoundary }
        guard !isWorking, operationID == nil, planner.beginCanonicalSync() else { throw ItemCompletionError.busy }
        guard client.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              planner.itemCompletionState.configurationIdentifier == nil
                || planner.itemCompletionState.configurationIdentifier == client.configurationIdentifier else {
            planner.endCanonicalSync(); throw ItemCompletionError.configurationChanged
        }
        let id = UUID(); operationID = id; isWorking = true; return id
    }
    private func end(_ id: UUID) {
        guard operationID == id else { return }
        operationID = nil; isWorking = false; planner.endCanonicalSync()
    }
    private func assertCurrent(_ id: UUID, generation expected: UInt64, client: any ItemCompletionTransport) throws {
        guard !Task.isCancelled, privacyAvailable, generation == expected, operationID == id else { throw ItemCompletionError.privacyBoundary }
        guard try connection().configurationIdentifier == client.configurationIdentifier,
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier else { throw ItemCompletionError.configurationChanged }
    }
    private func report(_ error: any Error) {
        guard privacyAvailable else { return }
        message = (error as? ItemCompletionError)?.errorDescription
            ?? "Completion is temporarily unavailable. Saved observations and exact requests are retained."
    }

    func canOverride(_ itemID: UUID) -> Bool {
        guard let snapshot = observation(for: itemID)?.snapshot, !snapshot.occurrenceEvidenceRequired else { return false }
        return planner.canonicalItems.contains { $0.deletedAt == nil && $0.parentID == itemID }
            || snapshot.state.mode != .automatic || snapshot.state.provenance != nil
    }

    private func recoverCanonical() async -> Bool {
        guard planner.itemCompletionState.needsCanonicalCatchUp else { return true }
        guard hasAdmittedConnection, !isWorking, !Task.isCancelled else { return false }
        let current = generation
        let binding = planner.canonicalConfigurationIdentifier
        isWorking = true
        defer { isWorking = false }
        guard await catchUp(), privacyAvailable, generation == current, !Task.isCancelled,
              binding == planner.canonicalConfigurationIdentifier,
              (try? connection().configurationIdentifier) == binding else { return false }
        do {
            var next = planner.itemCompletionState; let prior = next
            next.needsCanonicalCatchUp = false
            try planner.commitItemCompletionState(next, replacing: prior)
            message = "Complete task state synchronized. Refresh completion evidence before another review."
            return true
        } catch { report(error); return false }
    }
}

extension PlannerStore {
    var hasPendingItemCompletionAuthorityChanges: Bool {
        !pendingCanonicalMutations.isEmpty || !pendingCanonicalSensitivityMutations.isEmpty
            || !pendingCanonicalAuthoringMutations.isEmpty || pendingProposalApplicationMutation != nil
            || executionState.pendingCommand != nil || pendingExecutionDeferIntent != nil
    }

    func hasCurrentItemCompletionRead(_ itemID: UUID, snapshot: ItemCompletionSnapshot? = nil) -> Bool {
        guard !itemCompletionState.needsCanonicalCatchUp,
              let admission = itemCompletionReadAdmissions[itemID],
              admission.generation == itemCompletionEvidenceGeneration,
              snapshot == nil || snapshot == admission.snapshot,
              itemCompletionState.configurationIdentifier == canonicalConfigurationIdentifier,
              let observation = itemCompletionState.observations.first(where: { $0.snapshot.itemID == itemID }),
              observation.isReadProof, observation.snapshot == admission.snapshot,
              canonicalItems.first(where: { $0.id == itemID && $0.deletedAt == nil })?.revision == admission.snapshot.itemRevision
        else { return false }
        return true
    }

    func itemCompletionQualifiesParent(_ itemID: UUID, admission: ItemCompletionParentAdmission? = nil) -> Bool {
        guard canPersistPlan, persistenceError == nil,
              canonicalConfigurationIdentifier?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false,
              canonicalDeltaCursor?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false,
              !itemCompletionState.needsCanonicalCatchUp,
              itemCompletionState.journals.allSatisfy({ $0.noEffectCode != nil }),
              let item = canonicalItems.first(where: { $0.id == itemID && $0.deletedAt == nil }),
              item.status == .completed else { return false }
        if case .unknown = item.kind { return false }
        let snapshot: ItemCompletionSnapshot
        if let admission {
            guard isCanonicalSyncLocked,
                  admission.configurationIdentifier == canonicalConfigurationIdentifier,
                  admission.evidenceGeneration == itemCompletionEvidenceGeneration,
                  admission.mutation.configurationIdentifier == admission.configurationIdentifier,
                  admission.mutation.disposition == .pending, !admission.mutation.hasBeenSubmitted,
                  admission.mutation.operation == .create || admission.mutation.operation == .replace,
                  admission.mutation.draft?.parentID == itemID,
                  canonicalAuthoringMutation(id: admission.mutation.id) == admission.mutation,
                  pendingCanonicalAuthoringMutations.allSatisfy({ $0 == admission.mutation }),
                  pendingCanonicalMutations.isEmpty, pendingCanonicalSensitivityMutations.isEmpty,
                  pendingProposalApplicationMutation == nil, executionState.pendingCommand == nil,
                  pendingExecutionDeferIntent == nil else { return false }
            snapshot = admission.snapshot
        } else {
            guard !hasPendingItemCompletionAuthorityChanges, hasCurrentItemCompletionRead(itemID),
                  let read = itemCompletionReadAdmissions[itemID] else { return false }
            snapshot = read.snapshot
        }
        return snapshot.isValid && snapshot.itemID == itemID && snapshot.itemRevision == item.revision
            && !snapshot.occurrenceEvidenceRequired && snapshot.state.provenance != nil
    }

    func itemCompletionRequiresSensitivePresentation(_ itemID: UUID) -> Bool {
        let sensitivity = canonicalSensitivityPresentationIndex()
        var children: [UUID: [UUID]] = [:]
        for item in canonicalItems where item.deletedAt == nil {
            if let parent = item.parentID { children[parent, default: []].append(item.id) }
        }
        var pending = [itemID], visited = Set<UUID>()
        while let id = pending.popLast() {
            guard visited.insert(id).inserted else { return true }
            if sensitivity[id] != .standard { return true }
            pending.append(contentsOf: children[id] ?? [])
        }
        if let blocker = itemCompletionState.observations.first(where: { $0.snapshot.itemID == itemID })?
            .snapshot.state.provenance?.reopen.blockedByItemID,
           sensitivity[blocker] != .standard { return true }
        return false
    }
}
