import Foundation

enum ItemProgressValidation {
    static let nilID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
    static let maximumSeconds: UInt64 = 3_155_760_000
    static func text(_ value: String, limit: Int) -> Bool {
        !value.isEmpty && value.unicodeScalars.count <= limit
            && value.unicodeScalars.first.map { !isWhitespace($0.value) } == true
            && value.unicodeScalars.last.map { !isWhitespace($0.value) } == true
            && !value.unicodeScalars.contains { $0.value <= 31 || (127...159).contains($0.value) }
    }
    // Exact Unicode White_Space set shared with the core and database. Platform
    // convenience character sets also classify some format characters as whitespace.
    private static func isWhitespace(_ scalar: UInt32) -> Bool {
        switch scalar {
        case 0x09...0x0D, 0x20, 0x85, 0xA0, 0x1680, 0x2000...0x200A,
             0x2028, 0x2029, 0x202F, 0x205F, 0x3000: true
        default: false
        }
    }
    static func decimal(_ value: String) -> Bool {
        value.utf8.count <= 20 && value != "-0"
            && value.range(of: #"\A-?(0|[1-9][0-9]{0,11})(\.[0-9]{0,5}[1-9])?\z"#,
                           options: .regularExpression) != nil
    }
    static func normalizedInputDecimal(_ input: String) -> String? {
        guard input.utf8.count <= 64,
              input.range(of: #"\A-?[0-9]+(\.[0-9]+)?\z"#, options: .regularExpression) != nil else { return nil }
        let negative = input.hasPrefix("-")
        let parts = (negative ? String(input.dropFirst()) : input).split(separator: ".")
        let integer = parts[0].drop(while: { $0 == "0" })
        var fraction = parts.count == 2 ? String(parts[1]) : ""
        while fraction.last == "0" { fraction.removeLast() }
        let magnitude = (integer.isEmpty ? "0" : String(integer)) + (fraction.isEmpty ? "" : "." + fraction)
        let value = (negative && magnitude != "0" ? "-" : "") + magnitude
        return decimal(value) ? value : nil
    }
    static func components(_ values: [ItemProgressComponent]) -> Bool {
        values.count <= 16 && Set(values.map(\.id)).count == values.count && values.allSatisfy(\.isValid)
    }
    static func keys(_ decoder: any Decoder, _ names: Set<String>) throws {
        let container = try decoder.container(keyedBy: Key.self)
        guard Set(container.allKeys.map(\.stringValue)) == names else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "Unsupported item progress shape"))
        }
    }
    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { return nil }
    }
}

struct ItemProgressTarget: Codable, Equatable, Sendable {
    enum Direction: String, Codable, CaseIterable, Sendable { case atLeast = "at_least", atMost = "at_most" }
    var value: String
    var direction: Direction
    private enum CodingKeys: String, CodingKey { case value, direction }
    init(value: String, direction: Direction) { self.value = value; self.direction = direction }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["value", "direction"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        value = try c.decode(String.self, forKey: .value)
        direction = try c.decode(Direction.self, forKey: .direction)
        guard ItemProgressValidation.decimal(value) else { throw ItemProgressError.invalidData }
    }
}

enum ItemProgressValue: Codable, Equatable, Sendable {
    case percentage(basisPoints: UInt16)
    case time(elapsedSeconds: UInt64, remainingSeconds: UInt64?)
    case quantity(current: String, unit: String, target: ItemProgressTarget?)
    private enum CodingKeys: String, CodingKey {
        case type, basisPoints = "basis_points", elapsedSeconds = "elapsed_seconds"
        case remainingSeconds = "remaining_seconds", current, unit, target
    }
    var isValid: Bool {
        switch self {
        case let .percentage(points): points <= 10_000
        case let .time(elapsed, remaining): elapsed <= ItemProgressValidation.maximumSeconds
            && remaining.map { $0 <= ItemProgressValidation.maximumSeconds } ?? true
        case let .quantity(current, unit, target): ItemProgressValidation.decimal(current)
            && ItemProgressValidation.text(unit, limit: 32)
            && target.map { ItemProgressValidation.decimal($0.value) } ?? true
        }
    }
    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "percentage":
            try ItemProgressValidation.keys(decoder, ["type", "basis_points"])
            self = .percentage(basisPoints: try c.decode(UInt16.self, forKey: .basisPoints))
        case "time":
            try ItemProgressValidation.keys(decoder, ["type", "elapsed_seconds", "remaining_seconds"])
            self = .time(elapsedSeconds: try c.decode(UInt64.self, forKey: .elapsedSeconds),
                         remainingSeconds: try c.decodeIfPresent(UInt64.self, forKey: .remainingSeconds))
        case "quantity":
            try ItemProgressValidation.keys(decoder, ["type", "current", "unit", "target"])
            self = .quantity(current: try c.decode(String.self, forKey: .current),
                unit: try c.decode(String.self, forKey: .unit), target: try c.decodeIfPresent(ItemProgressTarget.self, forKey: .target))
        default: throw ItemProgressError.invalidData
        }
        guard isValid else { throw ItemProgressError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        guard isValid else { throw ItemProgressError.invalidData }
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .percentage(points):
            try c.encode("percentage", forKey: .type); try c.encode(points, forKey: .basisPoints)
        case let .time(elapsed, remaining):
            try c.encode("time", forKey: .type); try c.encode(elapsed, forKey: .elapsedSeconds)
            if let remaining { try c.encode(remaining, forKey: .remainingSeconds) }
            else { try c.encodeNil(forKey: .remainingSeconds) }
        case let .quantity(current, unit, target):
            try c.encode("quantity", forKey: .type); try c.encode(current, forKey: .current)
            try c.encode(unit, forKey: .unit)
            if let target { try c.encode(target, forKey: .target) } else { try c.encodeNil(forKey: .target) }
        }
    }
    var description: String {
        switch self {
        case let .percentage(points): "\(points / 100).\(String(format: "%02d", points % 100))%"
        case let .time(elapsed, remaining): "\(elapsed)s elapsed · " + (remaining.map { "\($0)s remaining" } ?? "remaining unknown")
        case let .quantity(current, unit, target): "\(current) \(unit)" + (target.map {
            " · target \($0.direction == .atLeast ? "at least" : "at most") \($0.value)"
        } ?? " · no target")
        }
    }
}

struct ItemProgressComponent: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    var name: String
    var value: ItemProgressValue
    var isValid: Bool { id != ItemProgressValidation.nilID && ItemProgressValidation.text(name, limit: 80) && value.isValid }
    private enum CodingKeys: String, CodingKey { case id, name, value }
    init(id: UUID = UUID(), name: String, value: ItemProgressValue) { self.id = id; self.name = name; self.value = value }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["id", "name", "value"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(UUID.self, forKey: .id); name = try c.decode(String.self, forKey: .name)
        value = try c.decode(ItemProgressValue.self, forKey: .value)
        guard isValid else { throw ItemProgressError.invalidData }
    }
}

struct ItemProgressSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: Int
    let itemID: UUID
    let itemRevision: UInt64
    let revision: UInt64
    let components: [ItemProgressComponent]
    let updatedAt: String?
    var isValid: Bool {
        schemaVersion == 1 && itemID != ItemProgressValidation.nilID
            && itemRevision > 0 && itemRevision <= UInt64(Int64.max) && revision <= UInt64(Int64.max)
            && ItemProgressValidation.components(components)
            && (revision == 0 ? components.isEmpty && updatedAt == nil
                : updatedAt?.hasSuffix("Z") == true && updatedAt.flatMap(CanonicalRFC3339Instant.init)?.hasPostgresPrecision == true)
    }
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", itemID = "item_id", itemRevision = "item_revision"
        case revision, components, updatedAt = "updated_at"
    }
    init(schemaVersion: Int = 1, itemID: UUID, itemRevision: UInt64, revision: UInt64,
         components: [ItemProgressComponent], updatedAt: String?) {
        self.schemaVersion = schemaVersion; self.itemID = itemID; self.itemRevision = itemRevision
        self.revision = revision; self.components = components; self.updatedAt = updatedAt
    }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["schema_version", "item_id", "item_revision", "revision", "components", "updated_at"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); itemID = try c.decode(UUID.self, forKey: .itemID)
        itemRevision = try c.decode(UInt64.self, forKey: .itemRevision); revision = try c.decode(UInt64.self, forKey: .revision)
        components = try c.decode([ItemProgressComponent].self, forKey: .components)
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt)
        guard isValid else { throw ItemProgressError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(schemaVersion, forKey: .schemaVersion); try c.encode(itemID, forKey: .itemID)
        try c.encode(itemRevision, forKey: .itemRevision); try c.encode(revision, forKey: .revision)
        try c.encode(components, forKey: .components)
        if let updatedAt { try c.encode(updatedAt, forKey: .updatedAt) } else { try c.encodeNil(forKey: .updatedAt) }
    }
}

struct ItemProgressCommand: Codable, Equatable, Sendable {
    let schemaVersion: Int
    let operationID: UUID
    let expectedItemRevision: UInt64
    let expectedProgressRevision: UInt64
    let components: [ItemProgressComponent]
    var isValid: Bool {
        schemaVersion == 1 && operationID != ItemProgressValidation.nilID
            && expectedItemRevision > 0 && expectedItemRevision <= UInt64(Int64.max)
            && expectedProgressRevision < UInt64(Int64.max) && ItemProgressValidation.components(components)
    }
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", operationID = "operation_id"
        case expectedItemRevision = "expected_item_revision", expectedProgressRevision = "expected_progress_revision", components
    }
    init(operationID: UUID = UUID(), expectedItemRevision: UInt64, expectedProgressRevision: UInt64, components: [ItemProgressComponent]) {
        schemaVersion = 1; self.operationID = operationID; self.expectedItemRevision = expectedItemRevision
        self.expectedProgressRevision = expectedProgressRevision; self.components = components
    }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["schema_version", "operation_id", "expected_item_revision", "expected_progress_revision", "components"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); operationID = try c.decode(UUID.self, forKey: .operationID)
        expectedItemRevision = try c.decode(UInt64.self, forKey: .expectedItemRevision)
        expectedProgressRevision = try c.decode(UInt64.self, forKey: .expectedProgressRevision)
        components = try c.decode([ItemProgressComponent].self, forKey: .components)
        guard isValid else { throw ItemProgressError.invalidData }
    }
    func bytes() throws -> Data {
        guard isValid else { throw ItemProgressError.invalidData }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(self)
    }
}

struct ItemProgressReceipt: Decodable, Equatable, Sendable {
    let operationID: UUID
    let replayed: Bool
    let progress: ItemProgressSnapshot
    private enum CodingKeys: String, CodingKey { case operationID = "operation_id", replayed, progress }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["operation_id", "replayed", "progress"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try c.decode(UUID.self, forKey: .operationID); replayed = try c.decode(Bool.self, forKey: .replayed)
        progress = try c.decode(ItemProgressSnapshot.self, forKey: .progress)
    }
    func matches(itemID: UUID, command: ItemProgressCommand) -> Bool {
        operationID == command.operationID && progress.itemID == itemID
            && progress.itemRevision == command.expectedItemRevision
            && progress.revision == command.expectedProgressRevision + 1 && progress.components == command.components
    }
}

enum ItemProgressError: Error, Equatable, LocalizedError {
    case invalidData, unavailable, persistenceRequired, staleReview, busy, configurationChanged, privacyBoundary
    case definitive(String)
    var errorDescription: String? {
        switch self {
        case .invalidData: "The progress response or saved data could not be verified."
        case .unavailable: "Sync this item's current canonical details before editing progress."
        case .persistenceRequired: "Trusted encrypted storage is required to save progress."
        case .staleReview: "Progress or item details changed. Refresh and review a new edit."
        case .busy: "Another operation is in progress. Your saved intent is retained."
        case .configurationChanged: "Restore the original connection to recover saved progress."
        case .privacyBoundary: "Unlock DayWeave and reopen this progress review."
        case .definitive: "This progress operation was not applied. Refresh and review your saved values."
        }
    }
}

struct ItemProgressObservation: Codable, Equatable, Sendable {
    let snapshot: ItemProgressSnapshot
    let observedAt: Date
    var isReadProof: Bool = true
    private enum CodingKeys: String, CodingKey { case snapshot, observedAt, isReadProof }
    init(snapshot: ItemProgressSnapshot, observedAt: Date, isReadProof: Bool = true) {
        self.snapshot = snapshot; self.observedAt = observedAt; self.isReadProof = isReadProof
    }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["snapshot", "observedAt", "isReadProof"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        snapshot = try c.decode(ItemProgressSnapshot.self, forKey: .snapshot)
        observedAt = try c.decode(Date.self, forKey: .observedAt); isReadProof = try c.decode(Bool.self, forKey: .isReadProof)
    }
}

struct ItemProgressJournal: Codable, Equatable, Identifiable, Sendable {
    let version: Int
    let itemID: UUID
    let configurationIdentifier: String
    let command: ItemProgressCommand
    let requestBody: Data
    let createdAt: Date
    var wasSensitive: Bool
    var hasBeenSubmitted: Bool
    var noEffectCode: String?
    var id: UUID { command.operationID }
    private enum CodingKeys: String, CodingKey {
        case version, itemID, configurationIdentifier, command, requestBody, createdAt, wasSensitive, hasBeenSubmitted, noEffectCode
    }
    init(version: Int = 1, itemID: UUID, configurationIdentifier: String, command: ItemProgressCommand,
         requestBody: Data, createdAt: Date, wasSensitive: Bool, hasBeenSubmitted: Bool, noEffectCode: String?) {
        self.version = version; self.itemID = itemID; self.configurationIdentifier = configurationIdentifier
        self.command = command; self.requestBody = requestBody; self.createdAt = createdAt
        self.wasSensitive = wasSensitive
        self.hasBeenSubmitted = hasBeenSubmitted; self.noEffectCode = noEffectCode
    }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["version", "itemID", "configurationIdentifier", "command", "requestBody", "createdAt", "wasSensitive", "hasBeenSubmitted", "noEffectCode"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version); itemID = try c.decode(UUID.self, forKey: .itemID)
        configurationIdentifier = try c.decode(String.self, forKey: .configurationIdentifier)
        command = try c.decode(ItemProgressCommand.self, forKey: .command); requestBody = try c.decode(Data.self, forKey: .requestBody)
        createdAt = try c.decode(Date.self, forKey: .createdAt); hasBeenSubmitted = try c.decode(Bool.self, forKey: .hasBeenSubmitted)
        wasSensitive = try c.decode(Bool.self, forKey: .wasSensitive)
        noEffectCode = try c.decodeIfPresent(String.self, forKey: .noEffectCode)
        guard isValid else { throw ItemProgressError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(itemID, forKey: .itemID)
        try c.encode(configurationIdentifier, forKey: .configurationIdentifier); try c.encode(command, forKey: .command)
        try c.encode(requestBody, forKey: .requestBody); try c.encode(createdAt, forKey: .createdAt)
        try c.encode(wasSensitive, forKey: .wasSensitive)
        try c.encode(hasBeenSubmitted, forKey: .hasBeenSubmitted)
        if let noEffectCode { try c.encode(noEffectCode, forKey: .noEffectCode) } else { try c.encodeNil(forKey: .noEffectCode) }
    }
    static let definitiveCodes: Set<String> = ["item_progress_item_stale", "item_progress_revision_stale",
        "item_progress_operation_reused", "item_progress_item_missing", "item_progress_invalid"]
    /// Privacy may strengthen during a request without changing its custody.
    /// No other field, including submitted/rejection state, may differ.
    func retainsCustody(of prior: Self) -> Bool {
        guard !prior.wasSensitive || wasSensitive else { return false }
        var comparable = self
        comparable.wasSensitive = prior.wasSensitive
        return comparable == prior
    }
    var isValid: Bool {
        version == 1 && itemID != ItemProgressValidation.nilID && !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 4_096 && command.isValid && requestBody.count <= 64 * 1_024
            && StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: requestBody)
            && (try? JSONDecoder().decode(ItemProgressCommand.self, from: requestBody)) == command
            && createdAt.timeIntervalSinceReferenceDate.isFinite
            && noEffectCode.map { Self.definitiveCodes.contains($0) } ?? true
    }
}

struct ItemProgressState: Codable, Equatable, Sendable {
    let version: Int
    var configurationIdentifier: String?
    var observations: [ItemProgressObservation]
    var journals: [ItemProgressJournal]
    private enum CodingKeys: String, CodingKey { case version, configurationIdentifier, observations, journals }
    init(version: Int = 1, configurationIdentifier: String?, observations: [ItemProgressObservation], journals: [ItemProgressJournal]) {
        self.version = version; self.configurationIdentifier = configurationIdentifier
        self.observations = observations; self.journals = journals
    }
    init(from decoder: any Decoder) throws {
        try ItemProgressValidation.keys(decoder, ["version", "configurationIdentifier", "observations", "journals"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(Int.self, forKey: .version)
        configurationIdentifier = try c.decodeIfPresent(String.self, forKey: .configurationIdentifier)
        observations = try c.decode([ItemProgressObservation].self, forKey: .observations)
        journals = try c.decode([ItemProgressJournal].self, forKey: .journals)
        guard isValid else { throw ItemProgressError.invalidData }
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version); try c.encode(observations, forKey: .observations)
        try c.encode(journals, forKey: .journals)
        if let configurationIdentifier { try c.encode(configurationIdentifier, forKey: .configurationIdentifier) }
        else { try c.encodeNil(forKey: .configurationIdentifier) }
    }
    static let empty = Self(version: 1, configurationIdentifier: nil, observations: [], journals: [])
    var isValid: Bool {
        version == 1 && observations.count <= 256 && journals.count <= 100
            && Set(observations.map { $0.snapshot.itemID }).count == observations.count
            && Set(journals.map(\.itemID)).count == journals.count && Set(journals.map(\.id)).count == journals.count
            && observations.allSatisfy { $0.snapshot.isValid && $0.observedAt.timeIntervalSinceReferenceDate.isFinite }
            && journals.allSatisfy { $0.isValid && $0.configurationIdentifier == configurationIdentifier }
            && (configurationIdentifier.map { !$0.isEmpty && $0.utf8.count <= 4_096 }
                ?? (observations.isEmpty && journals.isEmpty))
    }
    mutating func observe(_ snapshot: ItemProgressSnapshot, at date: Date, isReadProof: Bool = true) throws {
        guard snapshot.isValid else { throw ItemProgressError.invalidData }
        if let prior = observations.first(where: { $0.snapshot.itemID == snapshot.itemID }) {
            if !isReadProof && prior.snapshot.revision > snapshot.revision { return }
            guard snapshot.revision >= prior.snapshot.revision,
                  snapshot.revision != prior.snapshot.revision || (snapshot.components == prior.snapshot.components
                    && snapshot.updatedAt == prior.snapshot.updatedAt) else { throw ItemProgressError.invalidData }
            if !isReadProof && (snapshot.itemRevision < prior.snapshot.itemRevision || snapshot.revision == prior.snapshot.revision) {
                if snapshot.revision > prior.snapshot.revision,
                   let index = observations.firstIndex(where: { $0.snapshot.itemID == snapshot.itemID }) {
                    observations[index].isReadProof = false
                }
                return
            }
            guard snapshot.itemRevision >= prior.snapshot.itemRevision else { throw ItemProgressError.staleReview }
        }
        observations.removeAll { $0.snapshot.itemID == snapshot.itemID }
        observations.append(.init(snapshot: snapshot, observedAt: date, isReadProof: isReadProof))
        let pinned = Set(journals.map(\.itemID)).union([snapshot.itemID])
        while observations.count > 256 {
            guard let index = observations.indices.filter({ !pinned.contains(observations[$0].snapshot.itemID) })
                .min(by: { observations[$0].observedAt < observations[$1].observedAt }) else { throw ItemProgressError.busy }
            observations.remove(at: index)
        }
    }
}
