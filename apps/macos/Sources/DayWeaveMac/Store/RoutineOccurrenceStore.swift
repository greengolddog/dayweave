import Foundation

/// Process-local authorization, never serialized with historical observations.
struct RoutineOccurrenceReviewLease: Equatable {
    fileprivate let generation: UInt64
    fileprivate let selectionID: UUID
    fileprivate let memberID: UUID
    fileprivate let canonicalGeneration: UInt64
    fileprivate let occurrenceGeneration: UInt64
}

struct RoutineOccurrenceRecoveryEntry: Identifiable, Equatable {
    let id: UUID
    let instanceID: UUID
    let memberID: UUID
    let isRejected: Bool
    let canDiscard: Bool
}

@MainActor
final class RoutineOccurrenceStore: ObservableObject {
    @Published private(set) var message = "Review this occurrence independently from its recurring template."
    @Published private(set) var isWorking = false
    @Published private(set) var selectionRevision: UInt64 = 0
    private let planner: PlannerStore
    private let connection: @MainActor () throws -> any RoutineOccurrenceTransport
    private let catchUp: @MainActor () async -> Bool
    private let now: @Sendable () -> Date
    private let sleep: @Sendable (Duration) async throws -> Void
    private let connectivity: (@Sendable () -> AsyncStream<Bool>)?
    private let automaticOutbox: Bool
    private var privacyAvailable = false
    private var generation: UInt64 = 0
    private var operationID: UUID?
    private var loop: ItemProgressPollingLoop?
    private var connectivityTask: Task<Void, Never>?
    private struct Selection: Equatable {
        let id = UUID()
        let seriesItemID: UUID
        let occurrenceID: UUID
        let owner: UUID
        let binding: String
    }
    private struct ReadAdmission {
        let selection: Selection
        let snapshot: RoutineOccurrenceSnapshot
        let canonicalGeneration: UInt64
        let occurrenceGeneration: UInt64
        let generation: UInt64
    }
    private var selection: Selection?
    private var admission: ReadAdmission?

    init(planner: PlannerStore, connection: @escaping @MainActor () throws -> any RoutineOccurrenceTransport,
         catchUp: @escaping @MainActor () async -> Bool = { false }, now: @escaping @Sendable () -> Date = Date.init,
         sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
         connectivity: (@Sendable () -> AsyncStream<Bool>)? = nil, automaticOutbox: Bool = true) {
        self.planner = planner; self.connection = connection; self.catchUp = catchUp
        self.now = now; self.sleep = sleep; self.connectivity = connectivity; self.automaticOutbox = automaticOutbox
    }

    convenience init(planner: PlannerStore, canonicalSync: CanonicalSyncStore, authCoordinator: DurableAuthCoordinator) {
        let configuration = UserDefaultsSuggestionAPIConfigurationStore()
        let session = makeDayWeaveEphemeralSession()
        self.init(planner: planner, connection: {
            guard let raw = configuration.loadBaseURL() else { throw RoutineOccurrenceStateError.configurationChanged }
            let base = try DayWeaveAPIBaseURL(raw)
            guard authCoordinator.hasUsableCredential(boundTo: base) else { throw RoutineOccurrenceStateError.configurationChanged }
            return DayWeaveAPIClient(baseURL: base, session: session, authCoordinator: authCoordinator)
        }, catchUp: { await canonicalSync.syncThroughFreshComposition() },
           connectivity: { ItemProgressConnectivity.updates() })
    }

    var hasAdmittedConnection: Bool {
        privacyAvailable && planner.canPersistPlan && planner.persistenceError == nil
            && planner.canonicalConfigurationIdentifier != nil
            && (try? connection().configurationIdentifier) == planner.canonicalConfigurationIdentifier
            && (planner.routineOccurrenceState.configurationIdentifier == nil
                || planner.routineOccurrenceState.configurationIdentifier == planner.canonicalConfigurationIdentifier)
    }
    var hasPendingRecovery: Bool { planner.routineOccurrenceState.hasUnresolvedCustody }
    var selectedSnapshot: RoutineOccurrenceSnapshot? {
        guard hasAdmittedConnection, let selection, selection.binding == planner.canonicalConfigurationIdentifier else { return nil }
        return planner.routineOccurrenceState.observations.first {
            $0.snapshot.aggregate.manifest.seriesItemID == selection.seriesItemID
                && $0.snapshot.aggregate.manifest.occurrenceID == selection.occurrenceID
        }?.snapshot
    }
    var selectedJournal: RoutineOccurrenceJournal? {
        guard let instanceID = selectedSnapshot?.aggregate.manifest.id else { return nil }
        return planner.routineOccurrenceState.journals.first { $0.instanceID == instanceID }
    }
    var recoveryEntries: [RoutineOccurrenceRecoveryEntry] {
        guard hasAdmittedConnection else { return [] }
        return planner.routineOccurrenceState.journals.map {
            .init(id: $0.id, instanceID: $0.instanceID, memberID: $0.memberID,
                isRejected: $0.noEffectCode != nil, canDiscard: !$0.hasBeenSubmitted || $0.noEffectCode != nil)
        }
    }

    func activate() {
        guard !privacyAvailable else { return }
        privacyAvailable = true; generation &+= 1
        let current = generation
        if automaticOutbox {
            loop = ItemProgressPollingLoop(sleep: sleep) { [weak self] in
                guard let self, self.privacyAvailable, self.generation == current else { return .unavailable }
                guard !self.isWorking, !self.planner.isCanonicalSyncLocked else { return .busy }
                let recovered = await self.replayPending()
                guard self.privacyAvailable, self.generation == current else { return .unavailable }
                if self.selection != nil { _ = await self.refreshSelected() }
                return recovered ? .success : .unavailable
            }
        }
        if let connectivity {
            connectivityTask = Task { [weak self] in
                for await available in connectivity() {
                    guard let self, !Task.isCancelled, self.privacyAvailable, self.generation == current else { return }
                    if available { self.connectionBecameAvailable() }
                }
            }
        }
    }
    func suspendForPrivacyBoundary() {
        privacyAvailable = false; generation &+= 1; admission = nil
        loop?.stop(); loop = nil; connectivityTask?.cancel(); connectivityTask = nil
        planner.invalidateRoutineOccurrencePlanningEvidence()
        selectionRevision &+= 1
        message = "Unlock DayWeave to review occurrence completion."
    }
    func configurationDidChange() {
        let available = privacyAvailable
        suspendForPrivacyBoundary()
        if selection?.binding != planner.canonicalConfigurationIdentifier
            || (try? connection().configurationIdentifier) != selection?.binding { selection = nil }
        if available { activate() }
    }
    func connectionBecameAvailable() { if privacyAvailable { loop?.wake() } }
    func showDetail(seriesItemID: UUID, occurrenceID: UUID, owner: UUID) {
        guard let binding = planner.canonicalConfigurationIdentifier else { return }
        if selection?.seriesItemID == seriesItemID, selection?.occurrenceID == occurrenceID,
           selection?.owner == owner, selection?.binding == binding { return }
        selection = .init(seriesItemID: seriesItemID, occurrenceID: occurrenceID, owner: owner, binding: binding)
        admission = nil; selectionRevision &+= 1; loop?.wake()
    }
    func hideDetail(owner: UUID) {
        guard selection?.owner == owner else { return }
        selection = nil; admission = nil; selectionRevision &+= 1
    }

    func reviewLease(memberID: UUID, owner: UUID) -> RoutineOccurrenceReviewLease? {
        guard hasAdmittedConnection, !isWorking, planner.canMutatePlan,
              !planner.hasPendingItemCompletionAuthorityChanges,
              let selection, selection.owner == owner, let admission,
              admission.selection == selection, admission.generation == generation,
              admission.canonicalGeneration == planner.itemCompletionEvidenceGeneration,
              admission.occurrenceGeneration == planner.routineOccurrencePlanningGeneration,
              admission.snapshot == selectedSnapshot, admission.snapshot.freshEditEligible,
              admission.snapshot.members.first(where: { $0.itemID == memberID })?.occurrenceEvidenceRequired == false,
              !planner.routineOccurrenceState.needsRemoteScheduleCatchUp,
              planner.routineOccurrenceState.minimumCatchUpRevisions.isEmpty,
              selectedJournal == nil else { return nil }
        return .init(generation: generation, selectionID: selection.id, memberID: memberID,
            canonicalGeneration: admission.canonicalGeneration, occurrenceGeneration: admission.occurrenceGeneration)
    }
    func reviewIsCurrent(_ lease: RoutineOccurrenceReviewLease, baseline: RoutineOccurrenceSnapshot) -> Bool {
        guard let selection else { return false }
        return reviewLease(memberID: lease.memberID, owner: selection.owner) == lease && selectedSnapshot == baseline
    }
    func queueReviewed(lease: RoutineOccurrenceReviewLease, baseline: RoutineOccurrenceSnapshot,
                       action: RoutineOccurrenceAction) throws {
        guard reviewIsCurrent(lease, baseline: baseline),
              let member = baseline.aggregate.members.first(where: { $0.itemID == lease.memberID }),
              let binding = planner.canonicalConfigurationIdentifier else { throw RoutineOccurrenceStateError.staleState }
        let parent = baseline.aggregate.manifest.members.contains { $0.parentID == lease.memberID }
        switch action {
        case .setOutcome: guard !parent else { throw RoutineOccurrenceError.invalidData }
        case let .reopen(open):
            guard !parent, open == member.open else { throw RoutineOccurrenceError.invalidData }
        case let .setPolicy(_, mode):
            guard parent || mode == .automatic else { throw RoutineOccurrenceError.invalidData }
        }
        let command = RoutineOccurrenceCommand(schemaVersion: 1, operationID: UUID(),
            expectedInstanceRevision: baseline.aggregate.revision, expectedMemberRevision: member.revision,
            expectedEvidenceHash: baseline.evidenceHash, action: action)
        let journal = RoutineOccurrenceJournal(instanceID: baseline.aggregate.manifest.id, memberID: lease.memberID,
            configurationIdentifier: binding, command: command, requestBody: try command.bytes(), createdAt: now(), wasSensitive: true)
        let prior = planner.routineOccurrenceState; var next = prior
        try next.enqueue(journal)
        try planner.commitRoutineOccurrenceState(next, replacing: prior)
        admission = nil
        message = "The exact reviewed request is saved in the encrypted outbox."
        loop?.wake()
    }
    func discardReviewedIntent(instanceID: UUID, expectedOperationID: UUID) throws {
        guard hasAdmittedConnection, !isWorking,
              let journal = planner.routineOccurrenceState.journals.first(where: {
                  $0.instanceID == instanceID && $0.id == expectedOperationID
              }) else { throw RoutineOccurrenceStateError.staleState }
        let prior = planner.routineOccurrenceState; var next = prior
        try next.discardUnsubmittedOrRejected(journal)
        try planner.commitRoutineOccurrenceState(next, replacing: prior)
        admission = nil; message = "Saved request discarded. Fetch and explicitly review before a new change."
    }

    @discardableResult
    func refreshSelected() async -> Bool {
        guard let selected = selection else { return false }
        let current = generation
        do {
            let client = try connection(), operation = try begin(client)
            defer { end(operation) }
            let canonical = planner.itemCompletionEvidenceGeneration
            let occurrenceGeneration = planner.routineOccurrencePlanningGeneration
            let value = try await client.lookupRoutineOccurrence(seriesItemID: selected.seriesItemID,
                occurrenceID: selected.occurrenceID)
            try assertCurrent(operation, generation: current, client: client)
            guard selection == selected, canonical == planner.itemCompletionEvidenceGeneration,
                  occurrenceGeneration == planner.routineOccurrencePlanningGeneration,
                  value.isValid, value.aggregate.manifest.seriesItemID == selected.seriesItemID,
                  value.aggregate.manifest.occurrenceID == selected.occurrenceID else { throw RoutineOccurrenceStateError.staleState }
            let prior = planner.routineOccurrenceState; var next = prior
            try next.observe(value, configurationIdentifier: client.configurationIdentifier, at: now())
            if let old = prior.observations.first(where: { $0.instanceID == value.aggregate.manifest.id }),
               old.snapshot != value { next.needsRemoteScheduleCatchUp = true }
            try planner.commitRoutineOccurrenceState(next, replacing: prior)
            admission = .init(selection: selected, snapshot: value, canonicalGeneration: canonical,
                occurrenceGeneration: planner.routineOccurrencePlanningGeneration, generation: current)
            message = value.freshEditEligible
                ? "Current occurrence fetched. Review applies only to this instance."
                : "This occurrence's source is no longer eligible. History and exact pending retries remain available."
            return true
        } catch {
            if privacyAvailable, generation == current, selection == selected, !Task.isCancelled { report(error) }
            return false
        }
    }

    /// Submitted requests are recovered using their ledger identity alone.
    /// No current canonical item, visible selection, GET or template is needed.
    @discardableResult
    func replayPending() async -> Bool {
        guard hasAdmittedConnection, !isWorking, !Task.isCancelled else { return false }
        let current = generation
        var allSucceeded = true
        let pending = planner.routineOccurrenceState.journals.filter { $0.noEffectCode == nil }
        for journal in pending {
            guard privacyAvailable, generation == current, !Task.isCancelled else { return false }
            do { try await send(journal) }
            catch {
                allSucceeded = false
                if privacyAvailable, generation == current, !Task.isCancelled { report(error) }
            }
        }
        guard privacyAvailable, generation == current, !Task.isCancelled else { return false }
        // A failed/sibling journal must not prevent independent terminal reads.
        let terminal = await terminalCatchUp()
        guard privacyAvailable, generation == current, !Task.isCancelled else { return false }
        let schedule = terminal ? await recoverRemoteSchedule() : false
        return allSucceeded && terminal && schedule && !planner.routineOccurrenceState.hasUnresolvedCustody
    }

    private func send(_ retained: RoutineOccurrenceJournal) async throws {
        let client = try connection(), operation = try begin(client)
        let current = generation
        defer { end(operation) }
        guard retained.configurationIdentifier == client.configurationIdentifier,
              var journal = planner.routineOccurrenceState.journals.first(where: { $0.retainsCustody(of: retained) }),
              journal.noEffectCode == nil else { throw RoutineOccurrenceStateError.configurationChanged }
        if !journal.hasBeenSubmitted {
            guard !planner.hasPendingItemCompletionAuthorityChanges else { throw RoutineOccurrenceStateError.busy }
            let canonical = planner.itemCompletionEvidenceGeneration
            let occurrenceGeneration = planner.routineOccurrencePlanningGeneration
            // Always fetch before a first send, including within this process.
            // GET errors cannot mark a journal as definitively rejected.
            let baseline = try await client.routineOccurrence(instanceID: journal.instanceID)
            try assertCurrent(operation, generation: current, client: client)
            guard canonical == planner.itemCompletionEvidenceGeneration,
                  occurrenceGeneration == planner.routineOccurrencePlanningGeneration,
                  baseline.isValid, baseline.aggregate.manifest.id == journal.instanceID,
                  !planner.hasPendingItemCompletionAuthorityChanges else { throw RoutineOccurrenceStateError.staleState }
            let beforeRead = planner.routineOccurrenceState; var reviewed = beforeRead
            try reviewed.observe(baseline, configurationIdentifier: client.configurationIdentifier, at: now())
            try planner.commitRoutineOccurrenceState(reviewed, replacing: beforeRead)
            guard baseline.freshEditEligible,
                  baseline.members.first(where: { $0.itemID == journal.memberID })?.occurrenceEvidenceRequired == false,
                  baseline.aggregate.revision == journal.command.expectedInstanceRevision,
                  baseline.evidenceHash == journal.command.expectedEvidenceHash,
                  baseline.aggregate.members.first(where: { $0.itemID == journal.memberID })?.revision == journal.command.expectedMemberRevision else {
                message = "The saved review changed before its first send. Discard it and explicitly review current evidence."
                throw RoutineOccurrenceStateError.staleState
            }
            let prior = planner.routineOccurrenceState; var next = prior
            try next.markSubmitted(journal)
            try planner.commitRoutineOccurrenceState(next, replacing: prior)
            journal = try currentJournal(journal.id)
        }
        do {
            let receipt = try await client.putRoutineOccurrenceMember(instanceID: journal.instanceID,
                memberID: journal.memberID, requestBody: journal.requestBody)
            try assertCurrent(operation, generation: current, client: client)
            let prior = planner.routineOccurrenceState; var next = prior
            try next.settleReceipt(receipt, for: journal, at: now())
            try planner.commitRoutineOccurrenceState(next, replacing: prior)
            message = "Occurrence change confirmed. Reading terminal history and composing a fresh remote schedule."
        } catch let RoutineOccurrenceError.definitive(code) {
            // Only the PUT's closed named error settles no-effect custody.
            try assertCurrent(operation, generation: current, client: client)
            let prior = planner.routineOccurrenceState; var next = prior
            try next.markNoEffect(code, for: journal)
            try planner.commitRoutineOccurrenceState(next, replacing: prior)
            message = "The server rejected this exact request. Discard it before reviewing a replacement."
        }
        admission = nil
    }

    @discardableResult
    func terminalCatchUp() async -> Bool {
        let current = generation
        do {
            let client = try connection(), operation = try begin(client)
            defer { end(operation) }
            let capture = planner.routineOccurrenceState
            let canonical = planner.itemCompletionEvidenceGeneration
            let occurrenceGeneration = planner.routineOccurrencePlanningGeneration
            let useDelta = capture.terminalDeltaCursor != nil
            do {
                let pages = try await readChain(client: client, cursor: capture.terminalDeltaCursor, currentState: !useDelta,
                    operation: operation, generation: current)
                try assertReadCapture(operation, generation: current, client: client, canonical: canonical,
                    occurrenceGeneration: occurrenceGeneration, state: capture)
                var next = capture
                try next.installTerminalChanges(pages, replacing: capture, configurationIdentifier: client.configurationIdentifier,
                    at: now(), isCurrentState: !useDelta)
                try planner.commitRoutineOccurrenceState(next, replacing: capture)
            } catch {
                // Retry one bounded cold list without consuming the failed delta.
                guard useDelta, !Task.isCancelled else { throw error }
                try assertReadCapture(operation, generation: current, client: client, canonical: canonical,
                    occurrenceGeneration: occurrenceGeneration, state: capture)
                let pages = try await readChain(client: client, cursor: nil, currentState: true,
                    operation: operation, generation: current)
                try assertReadCapture(operation, generation: current, client: client, canonical: canonical,
                    occurrenceGeneration: occurrenceGeneration, state: capture)
                var next = capture
                try next.installColdTerminalChanges(pages, replacing: capture,
                    configurationIdentifier: client.configurationIdentifier, at: now())
                try planner.commitRoutineOccurrenceState(next, replacing: capture)
            }
            return true
        } catch {
            if privacyAvailable, generation == current, !Task.isCancelled { report(error) }
            return false
        }
    }

    private func readChain(client: any RoutineOccurrenceTransport, cursor starting: String?, currentState: Bool,
                           operation: UUID, generation: UInt64) async throws -> [RoutineOccurrencePage] {
        var cursor = starting, pages: [RoutineOccurrencePage] = [], seen = Set<String>()
        if let cursor { seen.insert(cursor) }
        var bytes = 0, visits = 0
        let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
        while pages.count < RoutineOccurrenceState.maximumTerminalPages {
            let page = try await (currentState ? client.routineOccurrences(cursor: cursor, limit: 50)
                : client.routineOccurrenceDelta(cursor: cursor, limit: 50))
            try assertCurrent(operation, generation: generation, client: client)
            guard page.isValid, !currentState || page.isCurrentStatePage else { throw RoutineOccurrenceError.invalidData }
            bytes += try encoder.encode(page).count
            visits += page.changes.reduce(0) { $0 + $1.occurrence.aggregate.manifest.members.count }
            guard bytes <= RoutineOccurrenceState.maximumTerminalBytes,
                  visits <= RoutineOccurrenceState.maximumTerminalMemberVisits else { throw RoutineOccurrenceStateError.busy }
            let unchanged = pages.isEmpty && !page.hasMore && page.changes.isEmpty && page.cursor == starting
            guard unchanged || seen.insert(page.cursor).inserted else { throw RoutineOccurrenceError.invalidData }
            pages.append(page)
            if !page.hasMore { return pages }
            cursor = page.cursor
        }
        throw RoutineOccurrenceStateError.busy
    }

    private func recoverRemoteSchedule() async -> Bool {
        let capture = planner.routineOccurrenceState
        guard capture.needsRemoteScheduleCatchUp else { return true }
        guard hasAdmittedConnection, !isWorking, capture.journals.isEmpty,
              capture.minimumCatchUpRevisions.isEmpty, capture.terminalDeltaCursor != nil,
              let binding = capture.configurationIdentifier else { return false }
        let current = generation, operation = UUID()
        operationID = operation; isWorking = true
        let occurrenceGeneration = planner.routineOccurrencePlanningGeneration
        defer { if operationID == operation { operationID = nil; isWorking = false } }
        // The production closure guarantees a new authenticated composition,
        // including a second sync when the first only recovers an old receipt.
        guard await catchUp(), !Task.isCancelled, privacyAvailable, current == generation,
              operationID == operation, hasAdmittedConnection,
              binding == planner.canonicalConfigurationIdentifier,
              planner.routineOccurrencePlanningGeneration == occurrenceGeneration,
              planner.routineOccurrenceState == capture else { return false }
        do {
            var next = capture
            try next.acknowledgeRemoteScheduleCatchUp(replacing: capture, configurationIdentifier: binding)
            try planner.commitRoutineOccurrenceState(next, replacing: capture)
            message = "Occurrence history and the freshly composed remote schedule are synchronized."
            return true
        } catch { report(error); return false }
    }

    private func currentJournal(_ id: UUID) throws -> RoutineOccurrenceJournal {
        guard let journal = planner.routineOccurrenceState.journals.first(where: { $0.id == id }) else {
            throw RoutineOccurrenceStateError.staleState
        }
        return journal
    }
    private func begin(_ client: any RoutineOccurrenceTransport) throws -> UUID {
        guard hasAdmittedConnection, !Task.isCancelled else { throw RoutineOccurrenceStateError.configurationChanged }
        guard !isWorking, operationID == nil, planner.beginRoutineOccurrenceSync() else { throw RoutineOccurrenceStateError.busy }
        do {
            guard client.configurationIdentifier == planner.canonicalConfigurationIdentifier else {
                throw RoutineOccurrenceStateError.configurationChanged
            }
            if planner.routineOccurrenceState.configurationIdentifier == nil {
                let prior = planner.routineOccurrenceState; var bound = prior
                bound.configurationIdentifier = client.configurationIdentifier
                try planner.commitRoutineOccurrenceState(bound, replacing: prior)
            }
            planner.invalidateRoutineOccurrencePlanningEvidence()
            admission = nil
            let id = UUID(); operationID = id; isWorking = true
            return id
        } catch { planner.endCanonicalSync(); throw error }
    }
    private func end(_ id: UUID) {
        guard operationID == id else { return }
        operationID = nil; isWorking = false; planner.endCanonicalSync()
    }
    private func assertCurrent(_ id: UUID, generation expected: UInt64, client: any RoutineOccurrenceTransport) throws {
        guard privacyAvailable, !Task.isCancelled, generation == expected, operationID == id,
              hasAdmittedConnection, client.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              planner.routineOccurrenceState.configurationIdentifier == client.configurationIdentifier else {
            throw RoutineOccurrenceStateError.configurationChanged
        }
    }
    private func assertReadCapture(_ id: UUID, generation: UInt64, client: any RoutineOccurrenceTransport,
                                   canonical: UInt64, occurrenceGeneration: UInt64, state: RoutineOccurrenceState) throws {
        try assertCurrent(id, generation: generation, client: client)
        guard planner.itemCompletionEvidenceGeneration == canonical,
              planner.routineOccurrencePlanningGeneration == occurrenceGeneration,
              planner.routineOccurrenceState == state else { throw RoutineOccurrenceStateError.staleState }
    }
    private func report(_ error: any Error) {
        guard privacyAvailable else { return }
        if error as? RoutineOccurrenceStateError == .staleState {
            message = "Occurrence evidence changed. The exact request and previous checkpoint remain saved; refresh or discard only an unsubmitted or rejected request."
        } else {
            message = "Occurrence review is temporarily unavailable. Exact requests and the previous terminal checkpoint are retained."
        }
    }
}
