import Foundation

/// Settings recovery carries no component names or values, even when the
/// current item/ancestry is unavailable. Content review belongs in admitted detail.
struct ItemProgressRecoveryEntry: Identifiable, Equatable {
    let id: UUID
    let itemID: UUID
    let isRejected: Bool
    let canDiscard: Bool
}

@MainActor
final class ItemProgressStore: ObservableObject {
    @Published private(set) var message = "Independent progress is stored separately from lifecycle and child totals."
    @Published private(set) var isWorking = false
    private let planner: PlannerStore
    private let connection: @MainActor () throws -> any ItemProgressTransport
    private let catchUp: @MainActor () async -> Void
    private let now: @Sendable () -> Date
    private let sleep: @Sendable (Duration) async throws -> Void
    private var generation: UInt64 = 0
    private var privacyAvailable = false
    private var operationID: UUID?
    private var outboxTask: Task<Void, Never>?
    private var visibleTask: Task<Void, Never>?
    private var visibleID: UUID?
    private var sourceCache = CanonicalHierarchySourceCache()

    init(planner: PlannerStore, connection: @escaping @MainActor () throws -> any ItemProgressTransport,
         catchUp: @escaping @MainActor () async -> Void = {}, now: @escaping @Sendable () -> Date = Date.init,
         sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.planner = planner; self.connection = connection; self.catchUp = catchUp; self.now = now; self.sleep = sleep
    }

    convenience init(planner: PlannerStore, canonicalSync: CanonicalSyncStore, authCoordinator: DurableAuthCoordinator) {
        let configuration = UserDefaultsSuggestionAPIConfigurationStore()
        let session = makeDayWeaveEphemeralSession()
        self.init(planner: planner, connection: {
            guard let raw = configuration.loadBaseURL() else { throw ItemProgressError.configurationChanged }
            let base = try DayWeaveAPIBaseURL(raw)
            guard authCoordinator.hasUsableCredential(boundTo: base) else { throw ItemProgressError.configurationChanged }
            return DayWeaveAPIClient(baseURL: base, session: session, authCoordinator: authCoordinator)
        }, catchUp: { await canonicalSync.refreshItemProgressCanonicalEvidence() })
    }

    var hasPendingRecovery: Bool { planner.itemProgressState.journals.contains { $0.noEffectCode == nil } }
    var hasAdmittedConnection: Bool {
        privacyAvailable && (try? connection().configurationIdentifier) == planner.canonicalConfigurationIdentifier
            && planner.canonicalConfigurationIdentifier != nil
    }

    func observation(for itemID: UUID) -> ItemProgressObservation? {
        guard hasAdmittedConnection, admittedItem(itemID) != nil,
              planner.itemProgressState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return nil }
        return planner.itemProgressState.observations.first { $0.snapshot.itemID == itemID }
    }

    func journal(for itemID: UUID) -> ItemProgressJournal? {
        guard admittedItem(itemID) != nil else { return nil }
        return recoveryJournal(for: itemID)
    }

    var recoveryEntries: [ItemProgressRecoveryEntry] {
        guard hasAdmittedConnection,
              planner.itemProgressState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return [] }
        return planner.itemProgressState.journals.map {
            .init(id: $0.id, itemID: $0.itemID, isRejected: $0.noEffectCode != nil,
                canDiscard: !$0.hasBeenSubmitted || $0.noEffectCode != nil)
        }
    }

    private func recoveryJournal(for itemID: UUID) -> ItemProgressJournal? {
        guard hasAdmittedConnection,
              planner.itemProgressState.configurationIdentifier == planner.canonicalConfigurationIdentifier else { return nil }
        return planner.itemProgressState.journals.first { $0.itemID == itemID }
    }

    func canReview(_ itemID: UUID) -> Bool {
        guard privacyAvailable, !isWorking, planner.hasEncryptedPersistence, planner.canMutatePlan,
              let item = admittedItem(itemID), let observation = observation(for: itemID), observation.isReadProof,
              observation.snapshot.itemRevision == item.revision else { return false }
        let pending = journal(for: itemID)
        return pending == nil || pending?.noEffectCode != nil || pending?.hasBeenSubmitted == false
    }

    func isSensitive(_ itemID: UUID) -> Bool {
        planner.canonicalSensitivityPresentationIndex()[itemID] != .standard
            || planner.itemProgressState.journals.contains { $0.itemID == itemID && $0.wasSensitive }
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
        outboxTask = Task { [weak self] in
            guard let self else { return }
            var delay: Int64 = 5
            while self.privacyAvailable && self.generation == started && !Task.isCancelled {
                do { try await self.sleep(.seconds(delay)) } catch { return }
                let success = await self.replayPending()
                delay = success ? 5 : min(delay * 2, 60)
            }
        }
    }

    func suspendForPrivacyBoundary() {
        privacyAvailable = false
        generation &+= 1
        outboxTask?.cancel(); outboxTask = nil
        hideDetail()
        sourceCache.clear()
        message = "Unlock DayWeave to review independent progress."
    }

    func configurationDidChange() {
        let wasAvailable = privacyAvailable
        let priorVisibleID = visibleID
        suspendForPrivacyBoundary()
        if wasAvailable {
            activate()
            if let priorVisibleID { showDetail(priorVisibleID) }
        }
    }

    func showDetail(_ itemID: UUID) {
        guard privacyAvailable else { return }
        hideDetail()
        visibleID = itemID
        visibleTask = Task { [weak self] in
            guard let self else { return }
            var delay: Int64 = 5
            while self.visibleID == itemID && self.privacyAvailable && !Task.isCancelled {
                let success = await self.refresh(itemID)
                delay = success ? 5 : min(delay * 2, 60)
                do { try await self.sleep(.seconds(delay)) } catch { return }
            }
        }
    }

    func hideDetail() { visibleTask?.cancel(); visibleTask = nil; visibleID = nil }

    @discardableResult
    func refresh(_ itemID: UUID) async -> Bool {
        for attempt in 0..<2 {
            guard privacyAvailable, !Task.isCancelled, admittedItem(itemID) != nil else { return false }
            do {
                let currentGeneration = generation
                let received = try await fetch(itemID)
                guard privacyAvailable, generation == currentGeneration, !Task.isCancelled else { throw ItemProgressError.privacyBoundary }
                guard let item = admittedItem(itemID) else { throw ItemProgressError.unavailable }
                if received.itemRevision != item.revision {
                    message = "Canonical item details need to catch up before progress can be edited."
                    if attempt == 0 { await catchUp(); continue }
                    return false
                }
                var next = planner.itemProgressState
                let prior = next
                next.configurationIdentifier = planner.canonicalConfigurationIdentifier
                try next.observe(received, at: now())
                try planner.commitItemProgressState(next, replacing: prior)
                message = "Progress fetched from this connection. Offline values remain a saved observation."
                return true
            } catch {
                report(error)
                if error as? ItemProgressError == .definitive("item_progress_item_missing"), attempt == 0 {
                    await catchUp()
                }
                return false
            }
        }
        return false
    }

    private func fetch(_ itemID: UUID) async throws -> ItemProgressSnapshot {
        let client = try connection()
        let operation = try begin(client)
        let current = generation
        defer { end(operation) }
        let value = try await client.itemProgress(itemID)
        try assertCurrent(operation, generation: current, client: client)
        guard value.isValid && value.itemID == itemID else { throw ItemProgressError.invalidData }
        return value
    }

    func queue(itemID: UUID, baseline: ItemProgressSnapshot, components: [ItemProgressComponent]) throws {
        guard canReview(itemID), let item = admittedItem(itemID),
              observation(for: itemID)?.snapshot == baseline, item.revision == baseline.itemRevision,
              ItemProgressValidation.components(components) else { throw ItemProgressError.staleReview }
        let client = try connection()
        guard client.configurationIdentifier == planner.canonicalConfigurationIdentifier else { throw ItemProgressError.configurationChanged }
        let command = ItemProgressCommand(expectedItemRevision: item.revision,
            expectedProgressRevision: baseline.revision, components: components)
        let journal = ItemProgressJournal(version: 1, itemID: itemID, configurationIdentifier: client.configurationIdentifier,
            command: command, requestBody: try command.bytes(), createdAt: now(), wasSensitive: isSensitive(itemID),
            hasBeenSubmitted: false, noEffectCode: nil)
        var next = planner.itemProgressState
        let prior = next
        next.journals.removeAll { $0.itemID == itemID }
        next.journals.append(journal)
        try planner.commitItemProgressState(next, replacing: prior)
        message = "Reviewed progress is saved in the encrypted outbox, separate from confirmed values."
    }

    func discardReviewedIntent(_ itemID: UUID, expectedOperationID: UUID) throws {
        guard privacyAvailable, !isWorking, let journal = recoveryJournal(for: itemID),
              !journal.hasBeenSubmitted || journal.noEffectCode != nil else { throw ItemProgressError.busy }
        guard journal.id == expectedOperationID else { throw ItemProgressError.staleReview }
        var next = planner.itemProgressState; let prior = next
        next.journals.removeAll { $0.id == journal.id }
        try planner.commitItemProgressState(next, replacing: prior)
    }

    @discardableResult
    func replayPending() async -> Bool {
        guard privacyAvailable, !Task.isCancelled else { return false }
        let pending = planner.itemProgressState.journals.filter { $0.noEffectCode == nil }
        guard !pending.isEmpty else { return true }
        for journal in pending {
            do { try await send(journal) } catch { report(error); return false }
        }
        return true
    }

    private func send(_ retained: ItemProgressJournal) async throws {
        let client = try connection()
        guard retained.configurationIdentifier == client.configurationIdentifier else { throw ItemProgressError.configurationChanged }
        let operation = try begin(client)
        let current = generation
        defer { end(operation) }
        guard var journal = planner.itemProgressState.journals.first(where: { $0.retainsCustody(of: retained) }) else {
            throw ItemProgressError.staleReview
        }
        if !journal.hasBeenSubmitted {
            guard let item = admittedItem(journal.itemID), item.revision == journal.command.expectedItemRevision,
                  let baseline = observation(for: journal.itemID), baseline.isReadProof,
                  baseline.snapshot.revision == journal.command.expectedProgressRevision else {
                try recordNoEffect(journal, code: "item_progress_item_stale")
                return
            }
            var next = planner.itemProgressState; let prior = next
            journal.hasBeenSubmitted = true
            next.journals = next.journals.map { $0.id == journal.id ? journal : $0 }
            try planner.commitItemProgressState(next, replacing: prior)
        }
        do {
            let receipt = try await client.putItemProgress(journal.itemID, requestBody: journal.requestBody)
            try assertCurrent(operation, generation: current, client: client)
            guard receipt.matches(itemID: journal.itemID, command: journal.command),
                  planner.itemProgressState.journals.contains(where: { $0.retainsCustody(of: journal) }) else {
                throw ItemProgressError.invalidData
            }
            var next = planner.itemProgressState; let prior = next
            next.journals.removeAll { $0.id == journal.id }
            try next.observe(receipt.progress, at: now(), isReadProof: false)
            try planner.commitItemProgressState(next, replacing: prior)
            message = "The exact progress operation was confirmed. Refresh for a current item-scoped observation."
        } catch let ItemProgressError.definitive(code) {
            try assertCurrent(operation, generation: current, client: client)
            try recordNoEffect(journal, code: code)
        }
    }

    private func recordNoEffect(_ journal: ItemProgressJournal, code: String) throws {
        guard planner.itemProgressState.journals.contains(where: { $0.retainsCustody(of: journal) }),
              ItemProgressJournal.definitiveCodes.contains(code) else {
            throw ItemProgressError.invalidData
        }
        var next = planner.itemProgressState; let prior = next
        next.journals = next.journals.map { value in
            guard value.id == journal.id else { return value }
            var result = value; result.noEffectCode = code; return result
        }
        try planner.commitItemProgressState(next, replacing: prior)
        message = "Progress was not applied. Refresh and explicitly review the saved values."
    }

    private func begin(_ client: any ItemProgressTransport) throws -> UUID {
        guard privacyAvailable, !Task.isCancelled else { throw ItemProgressError.privacyBoundary }
        guard operationID == nil, planner.beginCanonicalSync() else { throw ItemProgressError.busy }
        guard client.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              planner.itemProgressState.configurationIdentifier == nil
                || planner.itemProgressState.configurationIdentifier == client.configurationIdentifier else {
            planner.endCanonicalSync(); throw ItemProgressError.configurationChanged
        }
        let id = UUID(); operationID = id; isWorking = true; return id
    }
    private func end(_ id: UUID) {
        guard operationID == id else { return }
        operationID = nil; isWorking = false; planner.endCanonicalSync()
    }
    private func assertCurrent(_ id: UUID, generation expected: UInt64, client: any ItemProgressTransport) throws {
        guard !Task.isCancelled, privacyAvailable, generation == expected, operationID == id else { throw ItemProgressError.privacyBoundary }
        guard try connection().configurationIdentifier == client.configurationIdentifier,
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier else { throw ItemProgressError.configurationChanged }
    }
    private func report(_ error: any Error) {
        guard privacyAvailable else { return }
        message = (error as? ItemProgressError)?.errorDescription
            ?? "Progress is temporarily unavailable. Saved observations and exact pending requests are retained."
    }
}
