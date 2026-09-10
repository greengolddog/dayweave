import Foundation

enum RoutineOccurrenceStateError: Error, Equatable, Sendable {
    case invalidData, configurationChanged, staleState, busy, persistenceRequired
}

/// Historical display data only. A persisted observation never grants a fresh
/// GET lease or permission to compose a complete occurrence lifecycle context.
struct RoutineOccurrenceObservation: Codable, Equatable, Sendable {
    let snapshot: RoutineOccurrenceSnapshot
    let observedAt: Date
    var instanceID: UUID { snapshot.aggregate.manifest.id }
    private enum CodingKeys: String, CodingKey { case snapshot, observedAt }
    init(snapshot: RoutineOccurrenceSnapshot, observedAt: Date) {
        self.snapshot = snapshot; self.observedAt = observedAt
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["snapshot", "observedAt"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        snapshot = try c.decode(RoutineOccurrenceSnapshot.self, forKey: .snapshot)
        observedAt = try c.decode(Date.self, forKey: .observedAt)
        guard observedAt.timeIntervalSinceReferenceDate.isFinite else { throw RoutineOccurrenceStateError.invalidData }
    }
}

/// One unresolved operation per ledger instance: siblings share aggregate CAS.
struct RoutineOccurrenceJournal: Codable, Equatable, Identifiable, Sendable {
    let version: Int
    let instanceID: UUID
    let memberID: UUID
    let configurationIdentifier: String
    let command: RoutineOccurrenceCommand
    let requestBody: Data
    let createdAt: Date
    var wasSensitive: Bool
    var hasBeenSubmitted: Bool
    var noEffectCode: String?
    var id: UUID { command.operationID }
    private enum CodingKeys: String, CodingKey {
        case version, instanceID, memberID, configurationIdentifier, command, requestBody, createdAt
        case wasSensitive, hasBeenSubmitted, noEffectCode
    }
    init(version: Int = 1, instanceID: UUID, memberID: UUID, configurationIdentifier: String,
         command: RoutineOccurrenceCommand, requestBody: Data, createdAt: Date,
         wasSensitive: Bool, hasBeenSubmitted: Bool = false, noEffectCode: String? = nil) {
        self.version = version; self.instanceID = instanceID; self.memberID = memberID
        self.configurationIdentifier = configurationIdentifier; self.command = command; self.requestBody = requestBody
        self.createdAt = createdAt; self.wasSensitive = wasSensitive
        self.hasBeenSubmitted = hasBeenSubmitted; self.noEffectCode = noEffectCode
    }
    var isValid: Bool {
        version == 1 && instanceID != RoutineOccurrenceValidation.nilID && command.isValid(for: memberID)
            && RoutineOccurrenceState.validConfiguration(configurationIdentifier)
            && requestBody.count <= RoutineOccurrenceState.maximumRequestBytes
            && (try? RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: requestBody)) == command
            && createdAt.timeIntervalSinceReferenceDate.isFinite
            && (noEffectCode.map { hasBeenSubmitted && RoutineOccurrenceValidation.definitiveStatuses[$0] != nil } ?? true)
    }
    func retainsCustody(of prior: Self) -> Bool {
        guard !prior.wasSensitive || wasSensitive else { return false }
        var comparable = self; comparable.wasSensitive = prior.wasSensitive
        return comparable == prior
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["version", "instanceID", "memberID", "configurationIdentifier", "command",
            "requestBody", "createdAt", "wasSensitive", "hasBeenSubmitted", "noEffectCode"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version); instanceID = try c.decode(UUID.self, forKey: .instanceID)
        memberID = try c.decode(UUID.self, forKey: .memberID)
        configurationIdentifier = try c.decode(String.self, forKey: .configurationIdentifier)
        command = try c.decode(RoutineOccurrenceCommand.self, forKey: .command)
        requestBody = try c.decode(Data.self, forKey: .requestBody); createdAt = try c.decode(Date.self, forKey: .createdAt)
        wasSensitive = try c.decode(Bool.self, forKey: .wasSensitive)
        hasBeenSubmitted = try c.decode(Bool.self, forKey: .hasBeenSubmitted)
        noEffectCode = try c.decodeIfPresent(String.self, forKey: .noEffectCode)
        guard isValid else { throw RoutineOccurrenceStateError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw RoutineOccurrenceStateError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(instanceID, forKey: .instanceID)
        try c.encode(memberID, forKey: .memberID); try c.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try c.encode(command, forKey: .command); try c.encode(requestBody, forKey: .requestBody)
        try c.encode(createdAt, forKey: .createdAt); try c.encode(wasSensitive, forKey: .wasSensitive)
        try c.encode(hasBeenSubmitted, forKey: .hasBeenSubmitted); try c.encode(noEffectCode, forKey: .noEffectCode)
    }
}

/// Bounded complete-instance observations, not a complete lifecycle replica.
/// The opaque terminal cursor does not prove that every instance is retained.
struct RoutineOccurrenceState: Codable, Equatable, Sendable {
    static let maximumObservations = 256
    static let maximumMembers = 20_000
    static let maximumJournals = 64
    static let maximumRequestBytes = 1_024 * 1_024
    static let maximumSerializedBytes = 8 * 1_024 * 1_024
    static let maximumTerminalPages = 128
    static let maximumTerminalBytes = 32 * 1_024 * 1_024
    static let maximumTerminalMemberVisits = 40_000
    let version: Int
    var configurationIdentifier: String?
    var observations: [RoutineOccurrenceObservation]
    var journals: [RoutineOccurrenceJournal]
    var terminalDeltaCursor: String?
    var minimumCatchUpRevisions: [UUID: UInt64]
    var needsRemoteScheduleCatchUp: Bool

    private struct Storage: Codable {
        let version: Int
        let configurationIdentifier: String?
        let observations: [RoutineOccurrenceObservation]
        let journals: [RoutineOccurrenceJournal]
        let terminalDeltaCursor: String?
        let minimumCatchUpRevisions: [UUID: UInt64]
        let needsRemoteScheduleCatchUp: Bool
    }
    private enum CodingKeys: String, CodingKey {
        case version, configurationIdentifier, observations, journals, terminalDeltaCursor
        case minimumCatchUpRevisions, needsRemoteScheduleCatchUp
    }
    init(version: Int = 1, configurationIdentifier: String?, observations: [RoutineOccurrenceObservation] = [],
         journals: [RoutineOccurrenceJournal] = [], terminalDeltaCursor: String? = nil,
         minimumCatchUpRevisions: [UUID: UInt64] = [:], needsRemoteScheduleCatchUp: Bool = false) {
        self.version = version; self.configurationIdentifier = configurationIdentifier; self.observations = observations
        self.journals = journals; self.terminalDeltaCursor = terminalDeltaCursor
        self.minimumCatchUpRevisions = minimumCatchUpRevisions; self.needsRemoteScheduleCatchUp = needsRemoteScheduleCatchUp
    }
    static let empty = Self(configurationIdentifier: nil)
    static func validConfiguration(_ value: String) -> Bool { !value.isEmpty && value.utf8.count <= 4_096 }
    var hasUnresolvedCustody: Bool { !journals.isEmpty || !minimumCatchUpRevisions.isEmpty || needsRemoteScheduleCatchUp }
    var pinnedInstanceIDs: Set<UUID> { Set(journals.map(\.instanceID)).union(minimumCatchUpRevisions.keys) }
    var recoveryPinnedItemIDs: Set<UUID> {
        Set(journals.map(\.memberID)).union(observations.filter { pinnedInstanceIDs.contains($0.instanceID) }
            .flatMap { $0.snapshot.aggregate.manifest.members.map(\.itemID) })
    }
    private func storage(observations: [RoutineOccurrenceObservation], journals: [RoutineOccurrenceJournal]) -> Storage {
        .init(version: version, configurationIdentifier: configurationIdentifier, observations: observations, journals: journals,
            terminalDeltaCursor: terminalDeltaCursor, minimumCatchUpRevisions: minimumCatchUpRevisions,
            needsRemoteScheduleCatchUp: needsRemoteScheduleCatchUp)
    }
    private var fitsBudgets: Bool {
        guard observations.count <= Self.maximumObservations, journals.count <= Self.maximumJournals,
              minimumCatchUpRevisions.count <= Self.maximumObservations,
              configurationIdentifier.map(Self.validConfiguration) ?? true,
              terminalDeltaCursor.map(RoutineOccurrenceValidation.cursor) ?? true else { return false }
        var membersRemaining = Self.maximumMembers, requestsRemaining = Self.maximumRequestBytes
        for observation in observations {
            let count = observation.snapshot.aggregate.manifest.members.count
            guard count <= membersRemaining,
                  observation.snapshot.aggregate.members.count == count,
                  observation.snapshot.members.count == count else { return false }
            membersRemaining -= count
        }
        for journal in journals {
            guard journal.requestBody.count <= requestsRemaining else { return false }
            requestsRemaining -= journal.requestBody.count
        }
        // Encode rows separately: a programmatic oversized cache never allocates
        // a whole-cache JSON buffer. Include base64 requests and all metadata.
        // Match the enclosing planner's date and slash escaping conventions;
        // a slash-heavy title or request must not bypass the encrypted budget.
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        guard let baseBytes = try? encoder.encode(storage(observations: [], journals: [])).count else { return false }
        var remaining = Self.maximumSerializedBytes - baseBytes - 128
        for journal in journals {
            guard let size = try? encoder.encode(journal).count, size + 64 <= remaining else { return false }
            remaining -= size + 64
        }
        for observation in observations {
            guard observation.snapshot.isValid, observation.observedAt.timeIntervalSinceReferenceDate.isFinite,
                  let size = try? encoder.encode(observation).count, size + 64 <= remaining else { return false }
            remaining -= size + 64
        }
        return remaining >= 0
    }
    var isValid: Bool {
        version == 1 && observations.count <= Self.maximumObservations && journals.count <= Self.maximumJournals
            && minimumCatchUpRevisions.count <= Self.maximumObservations
            && (configurationIdentifier.map(Self.validConfiguration)
                ?? (observations.isEmpty && journals.isEmpty && terminalDeltaCursor == nil && !hasUnresolvedCustody))
            && (terminalDeltaCursor.map(RoutineOccurrenceValidation.cursor) ?? true)
            && Set(observations.map(\.instanceID)).count == observations.count
            && Set(journals.map(\.instanceID)).count == journals.count && Set(journals.map(\.id)).count == journals.count
            && observations.allSatisfy { $0.observedAt.timeIntervalSinceReferenceDate.isFinite }
            && journals.allSatisfy { $0.configurationIdentifier == configurationIdentifier }
            && minimumCatchUpRevisions.allSatisfy { $0.key != RoutineOccurrenceValidation.nilID && RoutineOccurrenceValidation.revision($0.value) }
            && (minimumCatchUpRevisions.isEmpty || needsRemoteScheduleCatchUp)
            && fitsBudgets
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["version", "configurationIdentifier", "observations", "journals",
            "terminalDeltaCursor", "minimumCatchUpRevisions", "needsRemoteScheduleCatchUp"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version)
        configurationIdentifier = try c.decodeIfPresent(String.self, forKey: .configurationIdentifier)
        observations = try c.decode([RoutineOccurrenceObservation].self, forKey: .observations)
        journals = try c.decode([RoutineOccurrenceJournal].self, forKey: .journals)
        terminalDeltaCursor = try c.decodeIfPresent(String.self, forKey: .terminalDeltaCursor)
        // UUID-keyed dictionaries encode as alternating unkeyed pairs. Decode
        // the original pairs ourselves so contradictory duplicate targets cannot
        // be silently normalized by Dictionary's synthesized decoder.
        var targets = try c.nestedUnkeyedContainer(forKey: .minimumCatchUpRevisions)
        guard let targetFieldCount = targets.count, targetFieldCount <= Self.maximumObservations * 2,
              targetFieldCount.isMultiple(of: 2) else { throw RoutineOccurrenceStateError.invalidData }
        var decodedTargets: [UUID: UInt64] = [:]
        while !targets.isAtEnd {
            let id = try targets.decode(UUID.self), revision = try targets.decode(UInt64.self)
            guard decodedTargets.updateValue(revision, forKey: id) == nil else { throw RoutineOccurrenceStateError.invalidData }
        }
        minimumCatchUpRevisions = decodedTargets
        needsRemoteScheduleCatchUp = try c.decode(Bool.self, forKey: .needsRemoteScheduleCatchUp)
        guard isValid else { throw RoutineOccurrenceStateError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw RoutineOccurrenceStateError.invalidData }
        // Explicit nulls are required closed fields, including when unbound.
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try c.encode(observations, forKey: .observations); try c.encode(journals, forKey: .journals)
        try c.encode(terminalDeltaCursor, forKey: .terminalDeltaCursor)
        try c.encode(minimumCatchUpRevisions, forKey: .minimumCatchUpRevisions)
        try c.encode(needsRemoteScheduleCatchUp, forKey: .needsRemoteScheduleCatchUp)
    }
    private func requireBinding(_ requested: String) throws {
        guard Self.validConfiguration(requested), configurationIdentifier == requested else {
            throw RoutineOccurrenceStateError.configurationChanged
        }
    }
    private mutating func fitCache(additionalPins: Set<UUID> = []) throws {
        let pinned = pinnedInstanceIDs.union(additionalPins)
        while !fitsBudgets {
            guard let index = observations.indices.filter({ !pinned.contains(observations[$0].instanceID) })
                .min(by: { observations[$0].observedAt < observations[$1].observedAt }) else {
                throw RoutineOccurrenceStateError.busy
            }
            observations.remove(at: index)
        }
        guard isValid else { throw RoutineOccurrenceStateError.invalidData }
    }
    private mutating func merge(_ snapshot: RoutineOccurrenceSnapshot, at date: Date, historicalReceipt: Bool) throws {
        guard snapshot.isValid, date.timeIntervalSinceReferenceDate.isFinite else { throw RoutineOccurrenceStateError.invalidData }
        let instanceID = snapshot.aggregate.manifest.id
        if let old = observations.first(where: { $0.instanceID == instanceID })?.snapshot {
            guard old.aggregate.manifest == snapshot.aggregate.manifest else { throw RoutineOccurrenceStateError.invalidData }
            if old.aggregate.revision == snapshot.aggregate.revision {
                // Evidence, eligibility and evaluation reasons are ephemeral.
                guard old.aggregate == snapshot.aggregate else { throw RoutineOccurrenceStateError.invalidData }
                if historicalReceipt { return }
            }
            if snapshot.aggregate.revision < old.aggregate.revision {
                if historicalReceipt { return }
                throw RoutineOccurrenceStateError.staleState
            }
        }
        observations.removeAll { $0.instanceID == instanceID }
        observations.append(.init(snapshot: snapshot, observedAt: date))
    }
    mutating func observe(_ snapshot: RoutineOccurrenceSnapshot, configurationIdentifier: String, at date: Date) throws {
        try requireBinding(configurationIdentifier)
        var next = self; try next.merge(snapshot, at: date, historicalReceipt: false)
        try next.fitCache(additionalPins: [snapshot.aggregate.manifest.id]); self = next
    }
    mutating func enqueue(_ journal: RoutineOccurrenceJournal) throws {
        try requireBinding(journal.configurationIdentifier)
        guard journal.isValid, !journal.hasBeenSubmitted, journal.noEffectCode == nil,
              !needsRemoteScheduleCatchUp, minimumCatchUpRevisions[journal.instanceID] == nil,
              !journals.contains(where: { $0.instanceID == journal.instanceID || $0.id == journal.id }),
              let reviewed = observations.first(where: { $0.instanceID == journal.instanceID })?.snapshot,
              reviewed.freshEditEligible,
              reviewed.members.first(where: { $0.itemID == journal.memberID })?.occurrenceEvidenceRequired == false,
              reviewed.aggregate.revision == journal.command.expectedInstanceRevision,
              reviewed.evidenceHash == journal.command.expectedEvidenceHash,
              reviewed.aggregate.members.first(where: { $0.itemID == journal.memberID })?.revision == journal.command.expectedMemberRevision else {
            throw RoutineOccurrenceStateError.busy
        }
        // The live store must additionally require its process-local GET lease.
        var next = self; next.journals.append(journal); try next.fitCache(); self = next
    }
    mutating func markSubmitted(_ expected: RoutineOccurrenceJournal) throws {
        try requireBinding(expected.configurationIdentifier)
        guard let index = journals.firstIndex(where: { $0.retainsCustody(of: expected) }), expected.noEffectCode == nil else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self; next.journals[index].hasBeenSubmitted = true
        guard next.isValid else { throw RoutineOccurrenceStateError.invalidData }; self = next
    }
    mutating func markNoEffect(_ code: String, for expected: RoutineOccurrenceJournal) throws {
        try requireBinding(expected.configurationIdentifier)
        guard expected.hasBeenSubmitted, expected.noEffectCode == nil, RoutineOccurrenceValidation.definitiveStatuses[code] != nil,
              let index = journals.firstIndex(where: { $0.retainsCustody(of: expected) }) else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self; next.journals[index].noEffectCode = code
        guard next.isValid else { throw RoutineOccurrenceStateError.invalidData }; self = next
    }
    mutating func discardUnsubmittedOrRejected(_ expected: RoutineOccurrenceJournal) throws {
        try requireBinding(expected.configurationIdentifier)
        guard !expected.hasBeenSubmitted || expected.noEffectCode != nil,
              let index = journals.firstIndex(where: { $0.retainsCustody(of: expected) }) else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self; next.journals.remove(at: index)
        guard next.isValid else { throw RoutineOccurrenceStateError.invalidData }; self = next
    }
    mutating func settleReceipt(_ receipt: RoutineOccurrenceMutation, for expected: RoutineOccurrenceJournal, at date: Date) throws {
        try requireBinding(expected.configurationIdentifier)
        guard expected.hasBeenSubmitted, expected.noEffectCode == nil,
              let index = journals.firstIndex(where: { $0.retainsCustody(of: expected) }),
              receipt.matches(instanceID: expected.instanceID, memberID: expected.memberID, command: expected.command) else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self
        next.minimumCatchUpRevisions[expected.instanceID] = max(next.minimumCatchUpRevisions[expected.instanceID] ?? 0,
            receipt.occurrence.aggregate.revision)
        next.needsRemoteScheduleCatchUp = true
        try next.merge(receipt.occurrence, at: date, historicalReceipt: true)
        next.journals.remove(at: index)
        try next.fitCache(); self = next
    }
    /// `expected` is captured before the chain starts. It includes the starting
    /// terminal cursor and receipt/journal custody, preventing a late chain from
    /// discharging a receipt accepted after that chain began. Only this chain's
    /// GET responses count toward minimum receipt revisions; cache rows do not.
    /// A cold list starts without a checkpoint; a delta requires one. Bounded
    /// chain admission completes before allocating the history map or folding
    /// any snapshots into the retained cache.
    mutating func installTerminalChanges(_ pages: [RoutineOccurrencePage], replacing expected: Self,
        configurationIdentifier: String, at date: Date, isCurrentState: Bool? = nil) throws {
        try requireBinding(configurationIdentifier)
        let currentState = isCurrentState ?? (expected.terminalDeltaCursor == nil)
        guard self == expected, date.timeIntervalSinceReferenceDate.isFinite,
              currentState == (expected.terminalDeltaCursor == nil),
              !pages.isEmpty, pages.count <= Self.maximumTerminalPages,
              let terminal = pages.last, !terminal.hasMore,
              pages.dropLast().allSatisfy(\.hasMore) else {
            throw RoutineOccurrenceStateError.staleState
        }
        // Count visits before semantic validation or encoding; the cache's
        // retained-entry budget alone cannot bound an accumulated read chain.
        var remainingMembers = Self.maximumTerminalMemberVisits
        var cursors = Set<String>()
        if let starting = expected.terminalDeltaCursor { cursors.insert(starting) }
        for page in pages {
            guard page.schemaVersion == 1, page.changes.count <= 100,
                  RoutineOccurrenceValidation.cursor(page.cursor), !page.hasMore || !page.changes.isEmpty else {
                throw RoutineOccurrenceStateError.invalidData
            }
            let unchangedEmptyTerminal = pages.count == 1 && !page.hasMore && page.changes.isEmpty
                && page.cursor == expected.terminalDeltaCursor
            guard unchangedEmptyTerminal || cursors.insert(page.cursor).inserted else {
                throw RoutineOccurrenceStateError.invalidData
            }
            for change in page.changes {
                let snapshot = change.occurrence, count = snapshot.aggregate.manifest.members.count
                guard count > 0, count <= RoutineOccurrenceValidation.maximumMembers, count <= remainingMembers,
                      snapshot.aggregate.members.count == count, snapshot.members.count == count else {
                    throw RoutineOccurrenceStateError.invalidData
                }
                remainingMembers -= count
            }
        }
        // Page validation bounds every complete instance and sums its encoded
        // rows before a page is encoded. No whole-chain JSON buffer is created;
        // the largest temporary page buffer is the existing 8 MiB wire limit.
        let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
        var remainingBytes = Self.maximumTerminalBytes
        for page in pages {
            guard page.isValid, let count = try? encoder.encode(page).count,
                  count <= RoutineOccurrenceValidation.maximumBytes, count <= remainingBytes else {
                throw RoutineOccurrenceStateError.invalidData
            }
            remainingBytes -= count
        }
        var latestRead: [UUID: RoutineOccurrenceSnapshot] = [:]
        var sequence: UInt64 = 0
        for page in pages {
            for change in page.changes {
                let snapshot = change.occurrence, id = snapshot.aggregate.manifest.id
                guard change.sequence > sequence else { throw RoutineOccurrenceStateError.invalidData }
                if let prior = latestRead[id] {
                    guard !currentState, snapshot.aggregate.manifest == prior.aggregate.manifest,
                          snapshot.aggregate.revision > prior.aggregate.revision else { throw RoutineOccurrenceStateError.invalidData }
                }
                sequence = change.sequence; latestRead[id] = snapshot
            }
        }
        guard minimumCatchUpRevisions.allSatisfy({ (latestRead[$0.key]?.aggregate.revision ?? 0) >= $0.value }) else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self
        for page in pages {
            for change in page.changes {
                // A delta can replay an earlier whole-instance revision before
                // catching up to a newer already reviewed observation.
                try next.merge(change.occurrence, at: date, historicalReceipt: true)
                try next.fitCache()
            }
        }
        next.minimumCatchUpRevisions.removeAll()
        // A changed authenticated checkpoint can include status-inert policy
        // reviews or instances outside the retained cache. It still revokes
        // the prior remote scheduling catch-up, even for an empty delta.
        next.needsRemoteScheduleCatchUp = next.needsRemoteScheduleCatchUp
            || terminal.cursor != expected.terminalDeltaCursor
        next.terminalDeltaCursor = terminal.cursor
        try next.fitCache(); self = next
    }
    /// The caller must have authenticated remote recomposition/publication
    /// begun with this exact captured state. This never creates planner proof.
    mutating func acknowledgeRemoteScheduleCatchUp(replacing expected: Self, configurationIdentifier: String) throws {
        try requireBinding(configurationIdentifier)
        guard self == expected, minimumCatchUpRevisions.isEmpty, journals.isEmpty, terminalDeltaCursor != nil else {
            throw RoutineOccurrenceStateError.staleState
        }
        var next = self; next.needsRemoteScheduleCatchUp = false
        guard next.isValid else { throw RoutineOccurrenceStateError.invalidData }; self = next
    }

    /// A cold read can recover a stale delta token or a receipt target outside
    /// that delta. The old durable checkpoint survives every failed attempt.
    mutating func installColdTerminalChanges(_ pages: [RoutineOccurrencePage], replacing expected: Self,
        configurationIdentifier: String, at date: Date) throws {
        try requireBinding(configurationIdentifier)
        guard self == expected else { throw RoutineOccurrenceStateError.staleState }
        var cold = expected
        cold.terminalDeltaCursor = nil
        let capture = cold
        try cold.installTerminalChanges(pages, replacing: capture,
            configurationIdentifier: configurationIdentifier, at: date, isCurrentState: true)
        cold.needsRemoteScheduleCatchUp = expected.needsRemoteScheduleCatchUp
            || cold.terminalDeltaCursor != expected.terminalDeltaCursor
        guard cold.isValid else { throw RoutineOccurrenceStateError.invalidData }
        self = cold
    }
}
