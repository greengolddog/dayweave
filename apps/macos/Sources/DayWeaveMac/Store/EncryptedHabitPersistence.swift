import CryptoKit
import Darwin
import Foundation

enum DayWeavePendingHabitMutation: Codable, Equatable, Identifiable, Sendable {
    case outcome(PendingOutcome)
    case pauseStart(PendingPauseStart)
    case pauseResume(PendingPauseResume)
    case missedReconcile(PendingMissedReconcile)
    case missedResolution(PendingMissedResolution)

    struct PendingOutcome: Codable, Equatable, Sendable {
        let habitID: UUID
        let occurrenceID: UUID
        let idempotencyKey: String
        let command: DayWeaveHabitOutcomeCommand
        let createdAt: Date
        var conflictDetected: Bool
    }

    struct PendingPauseStart: Codable, Equatable, Sendable {
        let habitID: UUID
        let idempotencyKey: String
        let command: DayWeaveHabitPauseStartCommand
        let createdAt: Date
        var conflictDetected: Bool
    }

    struct PendingPauseResume: Codable, Equatable, Sendable {
        let habitID: UUID
        let pauseID: UUID
        let idempotencyKey: String
        let command: DayWeaveHabitPauseResumeCommand
        let createdAt: Date
        var conflictDetected: Bool
    }

    struct PendingMissedReconcile: Codable, Equatable, Sendable {
        let idempotencyKey: String
        let command: DayWeaveHabitMissedReconcileCommand
        let limit: Int
        let createdAt: Date
        var conflictDetected: Bool
    }

    struct PendingMissedResolution: Codable, Equatable, Sendable {
        let habitID: UUID
        let occurrenceID: UUID
        let idempotencyKey: String
        let command: DayWeaveHabitMissedResolveCommand
        let createdAt: Date
        var conflictDetected: Bool
    }

    var id: UUID {
        switch self {
        case let .outcome(value): value.command.operationID
        case let .pauseStart(value): value.command.operationID
        case let .pauseResume(value): value.command.operationID
        case let .missedReconcile(value): value.command.operationID
        case let .missedResolution(value): value.command.operationID
        }
    }

    var habitID: UUID? {
        switch self {
        case let .outcome(value): value.habitID
        case let .pauseStart(value): value.habitID
        case let .pauseResume(value): value.habitID
        case .missedReconcile: nil
        case let .missedResolution(value): value.habitID
        }
    }

    var targetID: UUID? {
        switch self {
        case let .outcome(value): value.occurrenceID
        case let .pauseStart(value): value.command.pauseID
        case let .pauseResume(value): value.pauseID
        case .missedReconcile: nil
        case let .missedResolution(value): value.occurrenceID
        }
    }

    var idempotencyKey: String {
        switch self {
        case let .outcome(value): value.idempotencyKey
        case let .pauseStart(value): value.idempotencyKey
        case let .pauseResume(value): value.idempotencyKey
        case let .missedReconcile(value): value.idempotencyKey
        case let .missedResolution(value): value.idempotencyKey
        }
    }

    var conflictDetected: Bool {
        switch self {
        case let .outcome(value): value.conflictDetected
        case let .pauseStart(value): value.conflictDetected
        case let .pauseResume(value): value.conflictDetected
        case let .missedReconcile(value): value.conflictDetected
        case let .missedResolution(value): value.conflictDetected
        }
    }

    var canRequireUserConflictReview: Bool {
        if case .missedReconcile = self { return false }
        return true
    }

    func markingConflict() -> Self {
        switch self {
        case var .outcome(value):
            value.conflictDetected = true
            return .outcome(value)
        case var .pauseStart(value):
            value.conflictDetected = true
            return .pauseStart(value)
        case var .pauseResume(value):
            value.conflictDetected = true
            return .pauseResume(value)
        case var .missedReconcile(value):
            value.conflictDetected = true
            return .missedReconcile(value)
        case var .missedResolution(value):
            value.conflictDetected = true
            return .missedResolution(value)
        }
    }

    var hasValidShape: Bool {
        let nilID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
        guard id != nilID,
              habitID.map({ $0 != nilID }) ?? true,
              Self.isValidIdempotencyKey(idempotencyKey) else { return false }
        switch self {
        case let .outcome(value):
            return value.occurrenceID != nilID
                && value.command.operationID == id
                && value.command.hasValidShape
                && value.createdAt.timeIntervalSinceReferenceDate.isFinite
        case let .pauseStart(value):
            return value.command.operationID == id
                && value.command.hasValidShape
                && value.createdAt.timeIntervalSinceReferenceDate.isFinite
        case let .pauseResume(value):
            return value.pauseID != nilID
                && value.command.operationID == id
                && value.command.hasValidShape
                && value.createdAt.timeIntervalSinceReferenceDate.isFinite
        case let .missedReconcile(value):
            return value.command.operationID == id
                && value.command.hasValidShape
                && (1...200).contains(value.limit)
                && value.createdAt.timeIntervalSinceReferenceDate.isFinite
                && !value.conflictDetected
        case let .missedResolution(value):
            return value.habitID != nilID
                && value.occurrenceID != nilID
                && value.command.operationID == id
                && value.command.hasValidShape
                && value.createdAt.timeIntervalSinceReferenceDate.isFinite
        }
    }

    private static func isValidIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count) && value.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
                || [45, 46, 58, 95].contains(byte)
        }
    }
}

struct DayWeaveHabitClientSnapshot: Codable, Equatable, Sendable {
    static let currentSchemaVersion = 2
    static let maximumOccurrences = 20_000
    static let maximumPauses = 2_000
    static let maximumAnalytics = 2_000
    static let maximumPendingMutations = 500

    let schemaVersion: Int
    let savedAt: Date
    let configurationIdentifier: String?
    let deltaCursor: String?
    /// A cursor is composition-authoritative only after it was committed with
    /// a terminal delta page. Legacy snapshots omit this field and therefore
    /// migrate fail-closed.
    let deltaCaughtUp: Bool
    let occurrences: [DayWeaveHabitOccurrence]
    let pauses: [DayWeaveHabitPause]
    let analytics: [DayWeaveHabitAnalytics]
    let pendingMutations: [DayWeavePendingHabitMutation]

    init(
        schemaVersion: Int = Self.currentSchemaVersion,
        savedAt: Date,
        configurationIdentifier: String?,
        deltaCursor: String?,
        deltaCaughtUp: Bool = false,
        occurrences: [DayWeaveHabitOccurrence],
        pauses: [DayWeaveHabitPause],
        analytics: [DayWeaveHabitAnalytics],
        pendingMutations: [DayWeavePendingHabitMutation]
    ) {
        self.schemaVersion = schemaVersion
        self.savedAt = savedAt
        self.configurationIdentifier = configurationIdentifier
        self.deltaCursor = deltaCursor
        self.deltaCaughtUp = deltaCaughtUp
        self.occurrences = occurrences
        self.pauses = pauses
        self.analytics = analytics
        self.pendingMutations = pendingMutations
    }

    static func empty(at date: Date) -> Self {
        .init(
            savedAt: date,
            configurationIdentifier: nil,
            deltaCursor: nil,
            deltaCaughtUp: false,
            occurrences: [],
            pauses: [],
            analytics: [],
            pendingMutations: []
        )
    }

    var hasValidShape: Bool {
        guard schemaVersion == Self.currentSchemaVersion,
              savedAt.timeIntervalSinceReferenceDate.isFinite,
              occurrences.count <= Self.maximumOccurrences,
              pauses.count <= Self.maximumPauses,
              analytics.count <= Self.maximumAnalytics,
              pendingMutations.count <= Self.maximumPendingMutations,
              configurationIdentifier.map(Self.isValidBinding) ?? isEmpty,
              deltaCursor.map(Self.isValidCursor) ?? true,
              !deltaCaughtUp || deltaCursor != nil,
              occurrences.allSatisfy({
                  $0.evidence.hasValidShape
                      && ($0.outcome?.hasValidShape ?? true)
                      && ($0.missedResolution?.hasValidShape ?? true)
                      && ($0.missedResolution?.belongs(to: $0.evidence) ?? true)
              }),
              pauses.allSatisfy(\.hasValidShape),
              analytics.allSatisfy(\.hasValidShape),
              pendingMutations.allSatisfy(\.hasValidShape),
              Set(occurrences.map(\.id)).count == occurrences.count,
              Set(occurrences.map(\.evidence.plannerOccurrenceID)).count == occurrences.count,
              Set(pauses.map(\.id)).count == pauses.count,
              Set(analytics.map(\.habitID)).count == analytics.count,
              Set(pendingMutations.map(\.id)).count == pendingMutations.count,
              Set(pendingMutations.map(\.idempotencyKey)).count == pendingMutations.count else {
            return false
        }
        return hasValidPauseTopology && hasValidPendingMutationRelations
    }

    private var isEmpty: Bool {
        deltaCursor == nil && !deltaCaughtUp
            && occurrences.isEmpty && pauses.isEmpty && analytics.isEmpty
            && pendingMutations.isEmpty
    }

    private var hasValidPauseTopology: Bool {
        for habitPauses in Dictionary(grouping: pauses, by: \.habitID).values {
            let ordered = habitPauses.sorted {
                if $0.startedAt == $1.startedAt { return $0.id.uuidString < $1.id.uuidString }
                return $0.startedAt < $1.startedAt
            }
            guard ordered.count(where: { $0.endedAt == nil }) <= 1 else { return false }
            for (prior, next) in zip(ordered, ordered.dropFirst()) {
                guard let priorEnd = prior.endedAt, priorEnd <= next.startedAt else { return false }
            }
        }
        return true
    }

    /// Only unresolved writes retain replay authority. A conflict that has
    /// already been surfaced for review remains inspectable even if a later
    /// authoritative delta advances or removes its original target.
    private var hasValidPendingMutationRelations: Bool {
        let unresolved = pendingMutations.filter { !$0.conflictDetected }
        let targets = unresolved.compactMap { mutation -> HabitMutationTarget? in
            guard let habitID = mutation.habitID, let targetID = mutation.targetID else { return nil }
            return HabitMutationTarget(habitID: habitID, id: targetID)
        }
        guard Set(targets).count == targets.count else { return false }
        let pauseHabits = unresolved.compactMap { mutation -> UUID? in
            switch mutation {
            case .pauseStart, .pauseResume: return mutation.habitID
            default: return nil
            }
        }
        guard Set(pauseHabits).count == pauseHabits.count else { return false }
        guard unresolved.count(where: {
            if case .missedReconcile = $0 { return true }
            return false
        }) <= 1 else { return false }

        let occurrenceByID = Dictionary(uniqueKeysWithValues: occurrences.map { ($0.id, $0) })
        let pauseByID = Dictionary(uniqueKeysWithValues: pauses.map { ($0.id, $0) })
        for mutation in unresolved {
            switch mutation {
            case let .outcome(value):
                guard let occurrence = occurrenceByID[value.occurrenceID],
                      occurrence.evidence.habitID == value.habitID,
                      (occurrence.outcome?.revision ?? 0) == value.command.expectedRevision else {
                    return false
                }
                if value.command.outcome.quantity != nil,
                   let expectedUnit = occurrence.evidence.expectedUnit,
                   value.command.outcome.unit != expectedUnit {
                    return false
                }
            case let .pauseStart(value):
                guard pauseByID[value.command.pauseID] == nil,
                      !pauses.contains(where: {
                          $0.habitID == value.habitID && $0.endedAt == nil
                      }) else { return false }
            case let .pauseResume(value):
                guard let pause = pauseByID[value.pauseID],
                      pause.habitID == value.habitID,
                      pause.endedAt == nil,
                      pause.revision == value.command.expectedRevision,
                      value.command.endedAt > pause.startedAt else { return false }
            case .missedReconcile:
                break
            case let .missedResolution(value):
                guard let occurrence = occurrenceByID[value.occurrenceID],
                      occurrence.evidence.habitID == value.habitID,
                      let resolution = occurrence.missedResolution,
                      resolution.action.isDecisionRequired,
                      resolution.revision == value.command.expectedRevision else {
                    return false
                }
            }
        }
        return true
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion
        case savedAt
        case configurationIdentifier
        case deltaCursor
        case deltaCaughtUp
        case occurrences
        case pauses
        case analytics
        case pendingMutations
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let storedSchemaVersion = try container.decode(Int.self, forKey: .schemaVersion)
        schemaVersion = storedSchemaVersion == 1 ? Self.currentSchemaVersion : storedSchemaVersion
        savedAt = try container.decode(Date.self, forKey: .savedAt)
        configurationIdentifier = try container.decodeIfPresent(
            String.self,
            forKey: .configurationIdentifier
        )
        let decodedCursor = try container.decodeIfPresent(String.self, forKey: .deltaCursor)
        if container.contains(.deltaCaughtUp) {
            deltaCursor = decodedCursor
            let decodedCaughtUp = try container.decode(Bool.self, forKey: .deltaCaughtUp)
            // Schema 1 predates missed-resolution replication. Its terminal
            // cursor cannot prove that coordinate was reconciled, so retain
            // the incremental cursor but revoke composition authority until
            // a new terminal delta page is durably committed.
            deltaCaughtUp = storedSchemaVersion == Self.currentSchemaVersion
                ? decodedCaughtUp
                : false
        } else {
            // Legacy clients bounded occurrence history without retaining every
            // correction-safe completion anchor. Their tail cursor cannot
            // repair an already-evicted anchor, so migrate to a full replay.
            deltaCursor = nil
            deltaCaughtUp = false
        }
        let decodedOccurrences = try container.decode(
            [DayWeaveHabitOccurrence].self,
            forKey: .occurrences
        )
        if storedSchemaVersion == 1 {
            // Schema 1 could not create a missed-resolution projection. Strip a
            // relabelled newer field before the migrated snapshot can regain
            // composition authority after its next terminal delta page.
            occurrences = decodedOccurrences.map { occurrence in
                .init(
                    evidence: occurrence.evidence,
                    outcome: occurrence.outcome,
                    missedResolution: nil
                )
            }
        } else {
            occurrences = decodedOccurrences
        }
        pauses = try container.decode([DayWeaveHabitPause].self, forKey: .pauses)
        analytics = try container.decode([DayWeaveHabitAnalytics].self, forKey: .analytics)
        let decodedPendingMutations = try container.decode(
            [DayWeavePendingHabitMutation].self,
            forKey: .pendingMutations
        )
        if storedSchemaVersion == 1 {
            // Likewise, the predecessor format cannot mint either of the new
            // missed-occurrence replay authorities by carrying a future enum
            // case under an old schema label. Preserve every genuine v1 write.
            pendingMutations = decodedPendingMutations.filter { mutation in
                switch mutation {
                case .outcome, .pauseStart, .pauseResume: true
                case .missedReconcile, .missedResolution: false
                }
            }
        } else {
            pendingMutations = decodedPendingMutations
        }
    }

    private static func isValidBinding(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 4_096
            && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }

    private static func isValidCursor(_ value: String) -> Bool {
        DayWeaveHabitCursorContract.isValidTransportToken(value)
    }
}

private struct HabitMutationTarget: Hashable {
    let habitID: UUID
    let id: UUID
}

enum HabitPersistenceError: Error, Equatable, LocalizedError, Sendable {
    case storageUnavailable
    case keyUnavailable
    case malformedEnvelope
    case unsupportedVersion
    case authenticationFailed
    case invalidSnapshot
    case snapshotTooLarge
    case readFailed
    case writeFailed
    case lockUnavailable
    case concurrentModification

    var errorDescription: String? {
        switch self {
        case .storageUnavailable: "Private habit storage is unavailable."
        case .keyUnavailable: "The habit encryption key is unavailable."
        case .malformedEnvelope: "The encrypted habit cache is malformed."
        case .unsupportedVersion: "The encrypted habit cache version is unsupported."
        case .authenticationFailed: "The encrypted habit cache failed authentication."
        case .invalidSnapshot: "The private habit cache contains invalid data."
        case .snapshotTooLarge: "The private habit cache exceeds its safe size limit."
        case .readFailed: "The encrypted habit cache could not be read safely."
        case .writeFailed: "The encrypted habit cache could not be saved safely."
        case .lockUnavailable: "The private habit cache is busy."
        case .concurrentModification: "Another DayWeave process changed the private habit cache."
        }
    }
}

struct HabitPersistenceRevision: Equatable, Sendable {
    static let missing = Self(digest: nil)
    fileprivate let digest: Data?
}

struct EncryptedHabitPersistence: Sendable {
    static let maximumPlaintextBytes = 8 * 1_048_576
    static let maximumEnvelopeBytes = 12 * 1_048_576
    private static let version = 1
    private static let cipher = "AES.GCM.256"
    private static let magic = "DAYWEAVE-ENCRYPTED-HABITS"

    let fileURL: URL
    private let keyProvider: any PlannerEncryptionKeyProviding

    init(fileURL: URL, keyProvider: any PlannerEncryptionKeyProviding) {
        self.fileURL = fileURL
        self.keyProvider = keyProvider
    }

    init(fileURL: URL, key: PlannerEncryptionKey) {
        self.init(fileURL: fileURL, keyProvider: HabitFixedKeyProvider(key: key))
    }

    static func applicationDefault() throws -> Self {
        guard let root = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else { throw HabitPersistenceError.storageUnavailable }
        return Self(
            fileURL: root
                .appendingPathComponent("DayWeave", isDirectory: true)
                .appendingPathComponent("habits.snapshot.encrypted"),
            keyProvider: KeychainPlannerKeyProvider(
                service: "com.greengolddog.dayweave.habit-encryption",
                account: "device-key-v1"
            )
        )
    }

    func loadRevisioned() throws -> (
        snapshot: DayWeaveHabitClientSnapshot?,
        revision: HabitPersistenceRevision
    ) {
        try prepareDirectory()
        return try withLock {
            try removeOrphanedTemporaryFiles()
            guard let data = try readData() else { return (nil, .missing) }
            return (try decode(data), Self.revision(data))
        }
    }

    @discardableResult
    func save(
        _ snapshot: DayWeaveHabitClientSnapshot,
        expectedRevision: HabitPersistenceRevision
    ) throws -> HabitPersistenceRevision {
        let data = try encode(snapshot)
        try prepareDirectory()
        return try withLock {
            try removeOrphanedTemporaryFiles()
            let current = try readData()
            guard Self.revision(current) == expectedRevision else {
                throw HabitPersistenceError.concurrentModification
            }
            try writeData(data)
            return Self.revision(data)
        }
    }

    func preflightSave(_ snapshot: DayWeaveHabitClientSnapshot) throws {
        _ = try Self.plaintext(snapshot)
    }

    private func encode(_ snapshot: DayWeaveHabitClientSnapshot) throws -> Data {
        let plaintext = try Self.plaintext(snapshot)
        let key: PlannerEncryptionKey
        do { key = try keyProvider.loadOrCreateKey() } catch { throw HabitPersistenceError.keyUnavailable }
        let sealed: AES.GCM.SealedBox
        do {
            sealed = try AES.GCM.seal(
                plaintext,
                using: SymmetricKey(data: key.data),
                authenticating: Self.authenticatedHeader
            )
        } catch { throw HabitPersistenceError.writeFailed }
        guard let combined = sealed.combined else { throw HabitPersistenceError.writeFailed }
        let envelope = HabitEncryptedEnvelope(
            magic: Self.magic,
            version: Self.version,
            cipher: Self.cipher,
            payload: combined
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(envelope), data.count <= Self.maximumEnvelopeBytes else {
            throw HabitPersistenceError.snapshotTooLarge
        }
        return data
    }

    private func decode(_ data: Data) throws -> DayWeaveHabitClientSnapshot {
        guard data.count <= Self.maximumEnvelopeBytes,
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: data),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == ["cipher", "magic", "payload", "version"],
              let envelope = try? JSONDecoder().decode(HabitEncryptedEnvelope.self, from: data),
              envelope.magic == Self.magic else { throw HabitPersistenceError.malformedEnvelope }
        guard envelope.version == Self.version, envelope.cipher == Self.cipher else {
            throw HabitPersistenceError.unsupportedVersion
        }
        let sealed: AES.GCM.SealedBox
        do { sealed = try AES.GCM.SealedBox(combined: envelope.payload) }
        catch { throw HabitPersistenceError.malformedEnvelope }
        let key: PlannerEncryptionKey
        do { key = try keyProvider.loadOrCreateKey() } catch { throw HabitPersistenceError.keyUnavailable }
        let plaintext: Data
        do {
            plaintext = try AES.GCM.open(
                sealed,
                using: SymmetricKey(data: key.data),
                authenticating: Self.authenticatedHeader
            )
        } catch { throw HabitPersistenceError.authenticationFailed }
        guard plaintext.count <= Self.maximumPlaintextBytes else {
            throw HabitPersistenceError.snapshotTooLarge
        }
        guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: plaintext),
              let root = try? JSONSerialization.jsonObject(with: plaintext) as? [String: Any],
              Set([
                  "schemaVersion", "savedAt", "occurrences", "pauses", "analytics",
                  "pendingMutations",
              ]).isSubset(of: Set(root.keys)),
              Set(root.keys).isSubset(of: [
                  "schemaVersion", "savedAt", "configurationIdentifier", "deltaCursor",
                  "deltaCaughtUp", "occurrences", "pauses", "analytics", "pendingMutations",
              ]) else { throw HabitPersistenceError.invalidSnapshot }
        let snapshot: DayWeaveHabitClientSnapshot
        do { snapshot = try Self.decoder().decode(DayWeaveHabitClientSnapshot.self, from: plaintext) }
        catch { throw HabitPersistenceError.invalidSnapshot }
        guard snapshot.hasValidShape else { throw HabitPersistenceError.invalidSnapshot }
        return snapshot
    }

    private static func plaintext(_ snapshot: DayWeaveHabitClientSnapshot) throws -> Data {
        guard snapshot.hasValidShape else { throw HabitPersistenceError.invalidSnapshot }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            guard let instant = CanonicalRFC3339Instant(date: date) else {
                throw EncodingError.invalidValue(
                    date,
                    .init(codingPath: encoder.codingPath, debugDescription: "Invalid date")
                )
            }
            var container = encoder.singleValueContainer()
            try container.encode(instant.canonicalUTCString)
        }
        encoder.outputFormatting = [.sortedKeys]
        let data: Data
        do { data = try encoder.encode(snapshot) }
        catch { throw HabitPersistenceError.invalidSnapshot }
        guard data.count <= Self.maximumPlaintextBytes else {
            throw HabitPersistenceError.snapshotTooLarge
        }
        return data
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let text = try container.decode(String.self)
            guard let instant = CanonicalRFC3339Instant(text),
                  instant.hasPostgresPrecision,
                  let date = instant.exactlyRepresentableDate else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Invalid timestamp"
                )
            }
            return date
        }
        return decoder
    }

    private func prepareDirectory() throws {
        let directory = fileURL.deletingLastPathComponent()
        var metadata = stat()
        let status = directory.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.lstat(path, &metadata)
        }
        if status == 0 {
            guard metadata.st_mode & S_IFMT == S_IFDIR,
                  metadata.st_uid == geteuid(),
                  Darwin.chmod(directory.path, mode_t(S_IRWXU)) == 0 else {
                throw HabitPersistenceError.storageUnavailable
            }
            return
        }
        guard errno == ENOENT else { throw HabitPersistenceError.storageUnavailable }
        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: NSNumber(value: Int16(0o700))]
            )
        } catch { throw HabitPersistenceError.storageUnavailable }
        var createdMetadata = stat()
        let createdStatus = directory.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.lstat(path, &createdMetadata)
        }
        guard createdStatus == 0,
              createdMetadata.st_mode & S_IFMT == S_IFDIR,
              createdMetadata.st_uid == geteuid(),
              Darwin.chmod(directory.path, mode_t(S_IRWXU)) == 0 else {
            throw HabitPersistenceError.storageUnavailable
        }
    }

    /// Remove only regular, owned siblings matching this writer's exact UUID
    /// temporary-file spelling. This runs under the same process lock as load
    /// and save, bounding encrypted copies left by a crash before rename.
    private func removeOrphanedTemporaryFiles() throws {
        let directory = fileURL.deletingLastPathComponent()
        let prefix = ".\(fileURL.lastPathComponent)."
        let suffix = ".tmp"
        let names: [String]
        do { names = try FileManager.default.contentsOfDirectory(atPath: directory.path) }
        catch { throw HabitPersistenceError.writeFailed }

        for name in names where name.hasPrefix(prefix) && name.hasSuffix(suffix) {
            let identifierStart = name.index(name.startIndex, offsetBy: prefix.count)
            let identifierEnd = name.index(name.endIndex, offsetBy: -suffix.count)
            let identifierText = String(name[identifierStart..<identifierEnd])
            guard let identifier = UUID(uuidString: identifierText),
                  identifier.uuidString == identifierText else { continue }
            let orphan = directory.appendingPathComponent(name, isDirectory: false)
            var metadata = stat()
            let status = orphan.withUnsafeFileSystemRepresentation { path -> Int32 in
                guard let path else { return -1 }
                return Darwin.lstat(path, &metadata)
            }
            if status != 0 {
                if errno == ENOENT { continue }
                throw HabitPersistenceError.writeFailed
            }
            guard metadata.st_mode & S_IFMT == S_IFREG,
                  metadata.st_uid == geteuid() else { continue }
            let unlinkStatus = orphan.withUnsafeFileSystemRepresentation { path -> Int32 in
                guard let path else { return -1 }
                return Darwin.unlink(path)
            }
            if unlinkStatus != 0, errno != ENOENT { throw HabitPersistenceError.writeFailed }
        }
    }

    private func readData() throws -> Data? {
        let descriptor = fileURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
        }
        if descriptor < 0 {
            if errno == ENOENT { return nil }
            throw HabitPersistenceError.readFailed
        }
        defer { _ = Darwin.close(descriptor) }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_uid == geteuid(),
              metadata.st_mode & mode_t(S_IRWXG | S_IRWXO) == 0,
              metadata.st_size >= 0,
              metadata.st_size <= off_t(Self.maximumEnvelopeBytes) else {
            throw HabitPersistenceError.readFailed
        }
        var result = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1_024)
        while true {
            let count = Darwin.read(descriptor, &buffer, buffer.count)
            if count < 0, errno == EINTR { continue }
            guard count >= 0 else { throw HabitPersistenceError.readFailed }
            if count == 0 { break }
            result.append(buffer, count: count)
            guard result.count <= Self.maximumEnvelopeBytes else {
                throw HabitPersistenceError.snapshotTooLarge
            }
        }
        return result
    }

    private func writeData(_ data: Data) throws {
        let directory = fileURL.deletingLastPathComponent()
        let temporary = directory.appendingPathComponent(
            ".\(fileURL.lastPathComponent).\(UUID().uuidString).tmp"
        )
        let descriptor = temporary.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
        }
        guard descriptor >= 0 else { throw HabitPersistenceError.writeFailed }
        var shouldRemove = true
        defer {
            _ = Darwin.close(descriptor)
            if shouldRemove { _ = Darwin.unlink(temporary.path) }
        }
        do {
            guard Darwin.fchmod(descriptor, mode_t(S_IRUSR | S_IWUSR)) == 0 else {
                throw HabitPersistenceError.writeFailed
            }
            try data.withUnsafeBytes { bytes in
                guard let address = bytes.baseAddress else { return }
                var offset = 0
                while offset < bytes.count {
                    let count = Darwin.write(descriptor, address.advanced(by: offset), bytes.count - offset)
                    if count < 0, errno == EINTR { continue }
                    guard count > 0 else { throw HabitPersistenceError.writeFailed }
                    offset += count
                }
            }
            guard fsync(descriptor) == 0,
                  Darwin.rename(temporary.path, fileURL.path) == 0 else {
                throw HabitPersistenceError.writeFailed
            }
            shouldRemove = false
            _ = chmod(fileURL.path, mode_t(S_IRUSR | S_IWUSR))
            let directoryDescriptor = Darwin.open(
                directory.path,
                O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW
            )
            guard directoryDescriptor >= 0 else { throw HabitPersistenceError.writeFailed }
            defer { _ = Darwin.close(directoryDescriptor) }
            guard fsync(directoryDescriptor) == 0 else { throw HabitPersistenceError.writeFailed }
        } catch let error as HabitPersistenceError { throw error }
        catch { throw HabitPersistenceError.writeFailed }
    }

    private func withLock<T>(_ body: () throws -> T) throws -> T {
        let lockURL = fileURL.appendingPathExtension("lock")
        let descriptor = lockURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0o600)
        }
        guard descriptor >= 0 else { throw HabitPersistenceError.lockUnavailable }
        defer { _ = Darwin.close(descriptor) }
        guard Darwin.fchmod(descriptor, mode_t(S_IRUSR | S_IWUSR)) == 0 else {
            throw HabitPersistenceError.lockUnavailable
        }
        var metadata = stat()
        guard Darwin.fstat(descriptor, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_uid == geteuid(),
              metadata.st_mode & mode_t(S_IRWXG | S_IRWXO) == 0 else {
            throw HabitPersistenceError.lockUnavailable
        }
        guard flock(descriptor, LOCK_EX) == 0 else { throw HabitPersistenceError.lockUnavailable }
        defer { _ = flock(descriptor, LOCK_UN) }
        return try body()
    }

    private static func revision(_ data: Data?) -> HabitPersistenceRevision {
        guard let data else { return .missing }
        return .init(digest: Data(SHA256.hash(data: data)))
    }

    private static var authenticatedHeader: Data {
        Data("DayWeave.HabitSnapshot|\(version)|\(cipher)".utf8)
    }
}

private struct HabitFixedKeyProvider: PlannerEncryptionKeyProviding {
    let key: PlannerEncryptionKey
    func loadOrCreateKey() throws(PlannerPersistenceError) -> PlannerEncryptionKey { key }
}

private struct HabitEncryptedEnvelope: Codable {
    let magic: String
    let version: Int
    let cipher: String
    let payload: Data
}
