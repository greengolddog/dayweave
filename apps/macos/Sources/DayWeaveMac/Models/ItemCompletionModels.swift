import Foundation

enum ItemCompletionValidation {
    static let nilID = ItemProgressValidation.nilID
    static let maximumRevision = UInt64(Int64.max)
    static let maximumItems: UInt64 = 20_000

    static func hash(_ value: String) -> Bool {
        value.utf8.count == 71 && value.hasPrefix("sha256:")
            && value.dropFirst(7).utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func timestamp(_ value: String) -> Bool {
        value.utf8.count <= 32
            && value.range(of: #"\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?(Z|\+00:00)\z"#,
                           options: .regularExpression) != nil
            && CanonicalRFC3339Instant(value)?.hasPostgresPrecision == true
    }
    static func keys(_ decoder: any Decoder, _ names: Set<String>) throws {
        try ItemProgressValidation.keys(decoder, names)
    }
    static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        guard data.count <= 64 * 1_024,
              StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: data) else {
            throw ItemCompletionError.invalidData
        }
        return try JSONDecoder().decode(type, from: data)
    }
}

enum ItemCompletionMode: String, Codable, CaseIterable, Sendable {
    case automatic, keepOpen = "keep_open", complete
}

struct ItemCompletionReopenState: Codable, Equatable, Sendable {
    enum Status: String, Codable, Sendable { case inbox, planned, blocked }
    enum BlockedReasonKind: String, Codable, Sendable { case dependency, manual, external }
    let status: Status
    let blockedReasonKind: BlockedReasonKind?
    let blockedByItemID: UUID?
    let blockedReason: String?
    private enum CodingKeys: String, CodingKey {
        case status, blockedReasonKind = "blocked_reason_kind"
        case blockedByItemID = "blocked_by_item_id", blockedReason = "blocked_reason"
    }
    init(status: Status, blockedReasonKind: BlockedReasonKind? = nil,
         blockedByItemID: UUID? = nil, blockedReason: String? = nil) {
        self.status = status; self.blockedReasonKind = blockedReasonKind
        self.blockedByItemID = blockedByItemID; self.blockedReason = blockedReason
    }
    var isValid: Bool {
        guard blockedReason.map({ ItemProgressValidation.text($0, limit: 1_000) }) ?? true else { return false }
        switch (status, blockedReasonKind, blockedByItemID, blockedReason) {
        case (.blocked, .dependency?, let blocker?, _):
            return blocker != ItemCompletionValidation.nilID
        case (.blocked, .manual?, nil, .some(_)), (.blocked, .external?, nil, .some(_)): return true
        case (.inbox, nil, nil, nil), (.planned, nil, nil, nil): return true
        default: return false
        }
    }
    func isValid(for itemID: UUID) -> Bool {
        isValid && itemID != ItemCompletionValidation.nilID && blockedByItemID != itemID
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["status", "blocked_reason_kind", "blocked_by_item_id", "blocked_reason"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decode(Status.self, forKey: .status)
        blockedReasonKind = try c.decodeIfPresent(BlockedReasonKind.self, forKey: .blockedReasonKind)
        blockedByItemID = try c.decodeIfPresent(UUID.self, forKey: .blockedByItemID)
        blockedReason = try c.decodeIfPresent(String.self, forKey: .blockedReason)
        guard isValid else { throw ItemCompletionError.invalidData }
        // The enclosing policy/command supplies the target identity for self-dependency validation.
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(status, forKey: .status)
        try c.encode(blockedReasonKind, forKey: .blockedReasonKind)
        try c.encode(blockedByItemID, forKey: .blockedByItemID)
        try c.encode(blockedReason, forKey: .blockedReason)
    }
}

struct ItemCompletionProvenance: Codable, Equatable, Sendable {
    enum Kind: String, Codable, Sendable { case automatic, manual }
    let kind: Kind
    let reopen: ItemCompletionReopenState
    private enum CodingKeys: String, CodingKey { case kind, reopen }
    init(kind: Kind, reopen: ItemCompletionReopenState) { self.kind = kind; self.reopen = reopen }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["kind", "reopen"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decode(Kind.self, forKey: .kind)
        reopen = try c.decode(ItemCompletionReopenState.self, forKey: .reopen)
    }
}

/// The server's six-key policy state, separate from the local recovery ledger.
struct ItemCompletionPolicy: Codable, Equatable, Sendable {
    let itemID: UUID
    let revision: UInt64
    let requiredForParent: Bool
    let mode: ItemCompletionMode
    let provenance: ItemCompletionProvenance?
    let updatedAt: String?
    private enum CodingKeys: String, CodingKey {
        case itemID = "item_id", revision, requiredForParent = "required_for_parent", mode, provenance, updatedAt = "updated_at"
    }
    init(itemID: UUID, revision: UInt64 = 0, requiredForParent: Bool = true,
         mode: ItemCompletionMode = .automatic, provenance: ItemCompletionProvenance? = nil, updatedAt: String? = nil) {
        self.itemID = itemID; self.revision = revision; self.requiredForParent = requiredForParent
        self.mode = mode; self.provenance = provenance; self.updatedAt = updatedAt
    }
    static func empty(itemID: UUID) -> Self { Self(itemID: itemID) }
    var isValid: Bool {
        guard itemID != ItemCompletionValidation.nilID, revision <= ItemCompletionValidation.maximumRevision else { return false }
        if revision == 0 { return requiredForParent && mode == .automatic && provenance == nil && updatedAt == nil }
        guard updatedAt.map(ItemCompletionValidation.timestamp) == true else { return false }
        if let provenance {
            return provenance.reopen.isValid(for: itemID)
                && ((mode == .automatic && provenance.kind == .automatic) || (mode == .complete && provenance.kind == .manual))
        }
        return mode != .complete
    }
    func hasSameRevisionContent(as other: Self) -> Bool {
        itemID == other.itemID && revision == other.revision && requiredForParent == other.requiredForParent
            && mode == other.mode && provenance == other.provenance
            && updatedAt.flatMap(CanonicalRFC3339Instant.init) == other.updatedAt.flatMap(CanonicalRFC3339Instant.init)
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["item_id", "revision", "required_for_parent", "mode", "provenance", "updated_at"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        itemID = try c.decode(UUID.self, forKey: .itemID); revision = try c.decode(UInt64.self, forKey: .revision)
        requiredForParent = try c.decode(Bool.self, forKey: .requiredForParent); mode = try c.decode(ItemCompletionMode.self, forKey: .mode)
        provenance = try c.decodeIfPresent(ItemCompletionProvenance.self, forKey: .provenance)
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw ItemCompletionError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(itemID, forKey: .itemID); try c.encode(revision, forKey: .revision)
        try c.encode(requiredForParent, forKey: .requiredForParent); try c.encode(mode, forKey: .mode)
        try c.encode(provenance, forKey: .provenance); try c.encode(updatedAt, forKey: .updatedAt)
    }
}

struct ItemCompletionCounts: Codable, Equatable, Sendable {
    let requiredDescendants: UInt64
    let completed: UInt64
    let incomplete: UInt64
    let occurrenceEvidenceRequired: UInt64
    private enum CodingKeys: String, CodingKey {
        case requiredDescendants = "required_descendants", completed, incomplete, occurrenceEvidenceRequired = "occurrence_evidence_required"
    }
    init(requiredDescendants: UInt64 = 0, completed: UInt64 = 0, incomplete: UInt64 = 0, occurrenceEvidenceRequired: UInt64 = 0) {
        self.requiredDescendants = requiredDescendants; self.completed = completed
        self.incomplete = incomplete; self.occurrenceEvidenceRequired = occurrenceEvidenceRequired
    }
    var isValid: Bool {
        [requiredDescendants, completed, incomplete, occurrenceEvidenceRequired].allSatisfy { $0 <= ItemCompletionValidation.maximumItems }
            && requiredDescendants == completed + incomplete + occurrenceEvidenceRequired
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["required_descendants", "completed", "incomplete", "occurrence_evidence_required"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        requiredDescendants = try c.decode(UInt64.self, forKey: .requiredDescendants)
        completed = try c.decode(UInt64.self, forKey: .completed); incomplete = try c.decode(UInt64.self, forKey: .incomplete)
        occurrenceEvidenceRequired = try c.decode(UInt64.self, forKey: .occurrenceEvidenceRequired)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
}

struct ItemCompletionSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: Int
    let itemID: UUID
    let itemRevision: UInt64
    let state: ItemCompletionPolicy
    let evidenceHash: String
    let counts: ItemCompletionCounts
    let occurrenceEvidenceRequired: Bool
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", itemID = "item_id", itemRevision = "item_revision"
        case state, evidenceHash = "evidence_hash", counts, occurrenceEvidenceRequired = "occurrence_evidence_required"
    }
    init(schemaVersion: Int = 1, itemID: UUID, itemRevision: UInt64, state: ItemCompletionPolicy,
         evidenceHash: String, counts: ItemCompletionCounts = .init(), occurrenceEvidenceRequired: Bool = false) {
        self.schemaVersion = schemaVersion; self.itemID = itemID; self.itemRevision = itemRevision
        self.state = state; self.evidenceHash = evidenceHash; self.counts = counts
        self.occurrenceEvidenceRequired = occurrenceEvidenceRequired
    }
    var isValid: Bool {
        schemaVersion == 1 && itemID != ItemCompletionValidation.nilID && itemRevision > 0
            && itemRevision <= ItemCompletionValidation.maximumRevision && state.isValid && state.itemID == itemID
            && ItemCompletionValidation.hash(evidenceHash) && counts.isValid
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["schema_version", "item_id", "item_revision", "state", "evidence_hash", "counts", "occurrence_evidence_required"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); itemID = try c.decode(UUID.self, forKey: .itemID)
        itemRevision = try c.decode(UInt64.self, forKey: .itemRevision); state = try c.decode(ItemCompletionPolicy.self, forKey: .state)
        evidenceHash = try c.decode(String.self, forKey: .evidenceHash); counts = try c.decode(ItemCompletionCounts.self, forKey: .counts)
        occurrenceEvidenceRequired = try c.decode(Bool.self, forKey: .occurrenceEvidenceRequired)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
}

struct ItemCompletionCommand: Codable, Equatable, Sendable {
    let schemaVersion: Int
    let operationID: UUID
    let expectedItemRevision: UInt64
    let expectedCompletionRevision: UInt64
    let expectedEvidenceHash: String
    let requiredForParent: Bool
    let mode: ItemCompletionMode
    let reopening: ItemCompletionReopenState?
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", operationID = "operation_id", expectedItemRevision = "expected_item_revision"
        case expectedCompletionRevision = "expected_completion_revision", expectedEvidenceHash = "expected_evidence_hash"
        case requiredForParent = "required_for_parent", mode, reopening
    }
    init(operationID: UUID = UUID(), expectedItemRevision: UInt64, expectedCompletionRevision: UInt64,
         expectedEvidenceHash: String, requiredForParent: Bool, mode: ItemCompletionMode, reopening: ItemCompletionReopenState? = nil) {
        schemaVersion = 1; self.operationID = operationID; self.expectedItemRevision = expectedItemRevision
        self.expectedCompletionRevision = expectedCompletionRevision; self.expectedEvidenceHash = expectedEvidenceHash
        self.requiredForParent = requiredForParent; self.mode = mode; self.reopening = reopening
    }
    var isValid: Bool {
        schemaVersion == 1 && operationID != ItemCompletionValidation.nilID && expectedItemRevision > 0
            && expectedItemRevision <= ItemCompletionValidation.maximumRevision
            && expectedCompletionRevision <= ItemCompletionValidation.maximumRevision
            && ItemCompletionValidation.hash(expectedEvidenceHash)
            && (reopening?.isValid ?? true)
    }
    func isValid(for itemID: UUID) -> Bool {
        isValid && itemID != ItemCompletionValidation.nilID && (reopening?.isValid(for: itemID) ?? true)
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["schema_version", "operation_id", "expected_item_revision", "expected_completion_revision", "expected_evidence_hash", "required_for_parent", "mode", "reopening"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); operationID = try c.decode(UUID.self, forKey: .operationID)
        expectedItemRevision = try c.decode(UInt64.self, forKey: .expectedItemRevision)
        expectedCompletionRevision = try c.decode(UInt64.self, forKey: .expectedCompletionRevision)
        expectedEvidenceHash = try c.decode(String.self, forKey: .expectedEvidenceHash)
        requiredForParent = try c.decode(Bool.self, forKey: .requiredForParent); mode = try c.decode(ItemCompletionMode.self, forKey: .mode)
        reopening = try c.decodeIfPresent(ItemCompletionReopenState.self, forKey: .reopening)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw ItemCompletionError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(schemaVersion, forKey: .schemaVersion); try c.encode(operationID, forKey: .operationID)
        try c.encode(expectedItemRevision, forKey: .expectedItemRevision); try c.encode(expectedCompletionRevision, forKey: .expectedCompletionRevision)
        try c.encode(expectedEvidenceHash, forKey: .expectedEvidenceHash); try c.encode(requiredForParent, forKey: .requiredForParent)
        try c.encode(mode, forKey: .mode); try c.encode(reopening, forKey: .reopening)
    }
    func bytes() throws -> Data {
        guard isValid else { throw ItemCompletionError.invalidData }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(self)
    }
}

struct ItemCompletionReceipt: Codable, Equatable, Sendable {
    let operationID: UUID
    let replayed: Bool
    let completion: ItemCompletionSnapshot
    private enum CodingKeys: String, CodingKey { case operationID = "operation_id", replayed, completion }
    init(operationID: UUID, replayed: Bool, completion: ItemCompletionSnapshot) {
        self.operationID = operationID; self.replayed = replayed; self.completion = completion
    }
    var isValid: Bool {
        operationID != ItemCompletionValidation.nilID && completion.isValid
            && completion.itemRevision >= 2 && completion.state.revision > 0
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["operation_id", "replayed", "completion"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try c.decode(UUID.self, forKey: .operationID); replayed = try c.decode(Bool.self, forKey: .replayed)
        completion = try c.decode(ItemCompletionSnapshot.self, forKey: .completion)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
    func matches(itemID: UUID, command: ItemCompletionCommand) -> Bool {
        guard command.isValid(for: itemID), isValid,
              command.expectedItemRevision < ItemCompletionValidation.maximumRevision,
              command.expectedCompletionRevision < ItemCompletionValidation.maximumRevision else { return false }
        return operationID == command.operationID && completion.itemID == itemID
            && completion.itemRevision == command.expectedItemRevision + 1
            && completion.state.revision == command.expectedCompletionRevision + 1
            && completion.state.requiredForParent == command.requiredForParent && completion.state.mode == command.mode
    }
}

enum ItemCompletionError: Error, Equatable, LocalizedError {
    case invalidData, unavailable, persistenceRequired, staleReview, busy, configurationChanged, privacyBoundary
    case definitive(String)
    var errorDescription: String? {
        switch self {
        case .invalidData: "The completion response or saved data could not be verified."
        case .unavailable: "Refresh this item's completion review before changing its policy."
        case .persistenceRequired: "Trusted encrypted storage is required to save completion policy."
        case .staleReview: "Items, completion policy, or execution changed. Refresh and review your choices."
        case .busy: "Another operation is in progress. Your saved intent is retained."
        case .configurationChanged: "Restore the original connection to recover saved completion policy."
        case .privacyBoundary: "Unlock DayWeave and reopen this completion review."
        case .definitive: "This completion operation was not applied. Refresh and review your saved choices."
        }
    }
}

struct ItemCompletionObservation: Codable, Equatable, Sendable {
    let snapshot: ItemCompletionSnapshot
    let observedAt: Date
    var isReadProof: Bool
    private enum CodingKeys: String, CodingKey { case snapshot, observedAt, isReadProof }
    init(snapshot: ItemCompletionSnapshot, observedAt: Date, isReadProof: Bool = true) {
        self.snapshot = snapshot; self.observedAt = observedAt; self.isReadProof = isReadProof
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["snapshot", "observedAt", "isReadProof"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        snapshot = try c.decode(ItemCompletionSnapshot.self, forKey: .snapshot)
        observedAt = try c.decode(Date.self, forKey: .observedAt); isReadProof = try c.decode(Bool.self, forKey: .isReadProof)
        guard observedAt.timeIntervalSinceReferenceDate.isFinite else { throw ItemCompletionError.invalidData }
    }
}

struct ItemCompletionJournal: Codable, Equatable, Identifiable, Sendable {
    let version: Int
    let itemID: UUID
    let configurationIdentifier: String
    let command: ItemCompletionCommand
    let requestBody: Data
    let createdAt: Date
    var wasSensitive: Bool
    var hasBeenSubmitted: Bool
    var noEffectCode: String?
    var id: UUID { command.operationID }
    private enum CodingKeys: String, CodingKey {
        case version, itemID, configurationIdentifier, command, requestBody, createdAt, wasSensitive, hasBeenSubmitted, noEffectCode
    }
    init(version: Int = 1, itemID: UUID, configurationIdentifier: String, command: ItemCompletionCommand,
         requestBody: Data, createdAt: Date, wasSensitive: Bool, hasBeenSubmitted: Bool, noEffectCode: String?) {
        self.version = version; self.itemID = itemID; self.configurationIdentifier = configurationIdentifier
        self.command = command; self.requestBody = requestBody; self.createdAt = createdAt
        self.wasSensitive = wasSensitive; self.hasBeenSubmitted = hasBeenSubmitted; self.noEffectCode = noEffectCode
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["version", "itemID", "configurationIdentifier", "command", "requestBody", "createdAt", "wasSensitive", "hasBeenSubmitted", "noEffectCode"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version); itemID = try c.decode(UUID.self, forKey: .itemID)
        configurationIdentifier = try c.decode(String.self, forKey: .configurationIdentifier)
        command = try c.decode(ItemCompletionCommand.self, forKey: .command); requestBody = try c.decode(Data.self, forKey: .requestBody)
        createdAt = try c.decode(Date.self, forKey: .createdAt); wasSensitive = try c.decode(Bool.self, forKey: .wasSensitive)
        hasBeenSubmitted = try c.decode(Bool.self, forKey: .hasBeenSubmitted); noEffectCode = try c.decodeIfPresent(String.self, forKey: .noEffectCode)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw ItemCompletionError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(itemID, forKey: .itemID)
        try c.encode(configurationIdentifier, forKey: .configurationIdentifier); try c.encode(command, forKey: .command)
        try c.encode(requestBody, forKey: .requestBody); try c.encode(createdAt, forKey: .createdAt)
        try c.encode(wasSensitive, forKey: .wasSensitive); try c.encode(hasBeenSubmitted, forKey: .hasBeenSubmitted)
        try c.encode(noEffectCode, forKey: .noEffectCode)
    }
    static let definitiveStatuses: [String: Int] = [
        "item_completion_invalid": 422, "item_completion_parent_required": 422,
        "item_completion_item_stale": 409, "item_completion_revision_stale": 409,
        "item_completion_operation_reused": 409, "item_completion_evidence_stale": 409,
        "item_completion_reopening_review_required": 409, "item_completion_occurrence_evidence_required": 409,
        "item_completion_execution_conflict": 409, "item_completion_item_missing": 404,
        "item_completion_too_large": 413,
    ]
    static let definitiveCodes = Set(definitiveStatuses.keys)
    var isValid: Bool {
        version == 1 && command.isValid(for: itemID) && !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 4_096
            && (try? ItemCompletionValidation.decode(ItemCompletionCommand.self, from: requestBody)) == command
            && createdAt.timeIntervalSinceReferenceDate.isFinite
            && (noEffectCode.map { Self.definitiveCodes.contains($0) } ?? true)
    }
    /// Only privacy may strengthen while a response is awaiting durable settlement.
    func retainsCustody(of prior: Self) -> Bool {
        guard !prior.wasSensitive || wasSensitive else { return false }
        var comparable = self; comparable.wasSensitive = prior.wasSensitive
        return comparable == prior
    }
}

/// Exact recovery intent and cached observations. Read permission additionally
/// requires the store's process-local full-forest/execution admission lease.
struct ItemCompletionState: Codable, Equatable, Sendable {
    let version: Int
    var configurationIdentifier: String?
    var observations: [ItemCompletionObservation]
    var journals: [ItemCompletionJournal]
    var needsCanonicalCatchUp: Bool
    private enum CodingKeys: String, CodingKey { case version, configurationIdentifier, observations, journals, needsCanonicalCatchUp }
    init(version: Int = 1, configurationIdentifier: String?, observations: [ItemCompletionObservation],
         journals: [ItemCompletionJournal], needsCanonicalCatchUp: Bool = false) {
        self.version = version; self.configurationIdentifier = configurationIdentifier
        self.observations = observations; self.journals = journals; self.needsCanonicalCatchUp = needsCanonicalCatchUp
    }
    init(from decoder: any Decoder) throws {
        try ItemCompletionValidation.keys(decoder, ["version", "configurationIdentifier", "observations", "journals", "needsCanonicalCatchUp"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version); configurationIdentifier = try c.decodeIfPresent(String.self, forKey: .configurationIdentifier)
        observations = try c.decode([ItemCompletionObservation].self, forKey: .observations)
        journals = try c.decode([ItemCompletionJournal].self, forKey: .journals)
        needsCanonicalCatchUp = try c.decode(Bool.self, forKey: .needsCanonicalCatchUp)
        guard isValid else { throw ItemCompletionError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw ItemCompletionError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try c.encode(observations, forKey: .observations); try c.encode(journals, forKey: .journals)
        try c.encode(needsCanonicalCatchUp, forKey: .needsCanonicalCatchUp)
    }
    static let empty = Self(configurationIdentifier: nil, observations: [], journals: [])
    var isValid: Bool {
        version == 1 && observations.count <= 256 && journals.count <= 100
            && Set(observations.map { $0.snapshot.itemID }).count == observations.count
            && Set(journals.map(\.itemID)).count == journals.count && Set(journals.map(\.id)).count == journals.count
            && observations.allSatisfy { $0.snapshot.isValid && $0.observedAt.timeIntervalSinceReferenceDate.isFinite }
            && journals.allSatisfy { $0.isValid && $0.configurationIdentifier == configurationIdentifier }
            && (configurationIdentifier.map { !$0.isEmpty && $0.utf8.count <= 4_096 }
                ?? (observations.isEmpty && journals.isEmpty && !needsCanonicalCatchUp))
    }
    mutating func observe(_ snapshot: ItemCompletionSnapshot, at date: Date, isReadProof: Bool = true) throws {
        guard snapshot.isValid, date.timeIntervalSinceReferenceDate.isFinite else { throw ItemCompletionError.invalidData }
        if let prior = observations.first(where: { $0.snapshot.itemID == snapshot.itemID }) {
            let old = prior.snapshot
            if snapshot.state.revision == old.state.revision,
               !snapshot.state.hasSameRevisionContent(as: old.state) { throw ItemCompletionError.invalidData }
            if snapshot.itemRevision == old.itemRevision && snapshot.state.revision != old.state.revision {
                throw ItemCompletionError.invalidData
            }
            if !isReadProof {
                // A historical receipt must neither regress nor replace fresh GET
                // counts/evidence at the same policy+canonical revision.
                if snapshot.itemRevision <= old.itemRevision && snapshot.state.revision <= old.state.revision { return }
                guard snapshot.itemRevision > old.itemRevision && snapshot.state.revision >= old.state.revision else {
                    throw ItemCompletionError.invalidData
                }
            } else {
                guard snapshot.itemRevision >= old.itemRevision && snapshot.state.revision >= old.state.revision else {
                    throw ItemCompletionError.staleReview
                }
            }
        }
        var next = self
        next.observations.removeAll { $0.snapshot.itemID == snapshot.itemID }
        next.observations.append(.init(snapshot: snapshot, observedAt: date, isReadProof: isReadProof))
        let pinned = Set(journals.map(\.itemID)).union([snapshot.itemID])
        while next.observations.count > 256 {
            guard let index = next.observations.indices.filter({ !pinned.contains(next.observations[$0].snapshot.itemID) })
                .min(by: { next.observations[$0].observedAt < next.observations[$1].observedAt }) else { throw ItemCompletionError.busy }
            next.observations.remove(at: index)
        }
        guard next.isValid else { throw ItemCompletionError.invalidData }
        self = next
    }
}
