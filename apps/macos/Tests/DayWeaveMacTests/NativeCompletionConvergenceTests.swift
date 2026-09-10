import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

/// Opt-in, separate-process phases against the runner's disposable loopback service.
/// No app, defaults, Keychain, provider, execution command, or schedule engine is used.
@Suite("Controlled native completion convergence", .serialized)
@MainActor
struct NativeCompletionConvergenceTests {
    @Test("selected macOS completion phase uses real HTTP and encrypted native recovery",
          .enabled(if: ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_COMPLETION_CONFIG"] != nil
            || ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_COMPLETION_PHASE"] != nil))
    func runSelectedPhaseAgainstDisposableService() async throws {
        let context = try Context()
        defer { context.session.invalidateAndCancel() }
        let planner = PlannerStore(persistence: context.persistence,
            restoreFromPersistence: context.phase != .prepare)
        try #require(planner.persistenceError == nil && planner.canPersistPlan)
        if context.phase != .prepare {
            try #require(planner.canonicalConfigurationIdentifier == context.client.configurationIdentifier)
            try #require(planner.canonicalDeltaCursor != nil)
            try #require(planner.itemCompletionReadAdmissions.isEmpty)
        }
        let sync = CanonicalSyncStore(planner: planner,
            configurationStore: NativeCompletionConfigurationStore(baseURL: context.config.baseURL),
            tokenStore: NativeCompletionCredentialStore(token: context.config.bearerToken,
                origin: try DayWeaveAPIBaseURL(context.config.baseURL).credentialOriginIdentifier),
            session: context.session, localComposer: NativeCompletionForbiddenComposer(),
            itemStreamTransportProvider: { _ in nil }, scheduleStreamTransportProvider: { _ in nil },
            scheduleReplicaRequiresDurableBinding: false)
        let recording = NativeCompletionRecordingTransport(client: context.client,
            dropSuccessfulReply: context.phase == .submitLost)
        var catchUpStates: [ItemCompletionState] = []
        let completion = ItemCompletionStore(planner: planner, connection: { recording }, catchUp: {
            // Acknowledgement must already be durable before the canonical read starts.
            guard let durable = try? context.persistence.load(),
                  let ledger = durable.itemCompletionState,
                  ledger == planner.itemCompletionState,
                  ledger.journals.isEmpty, ledger.needsCanonicalCatchUp else { return false }
            catchUpStates.append(ledger)
            return await sync.refreshItemCompletionCanonicalEvidence()
        }, now: { Date(timeIntervalSince1970: Date().timeIntervalSince1970.rounded(.down)) },
           sleep: { _ in throw CancellationError() }, automaticOutbox: false)
        completion.activate()
        defer { completion.suspendForPrivacyBoundary() }
        let initialExecution = planner.executionState

        switch context.phase {
        case .prepare:
            try #require(await sync.refreshItemCompletionCanonicalEvidence())
            try requireForest(planner, config: context.config, phase: .prepare)
            let before = planner.canonicalItems
            try #require(await completion.refresh(context.config.rootID))
            let observed = try #require(completion.observation(for: context.config.rootID))
            try #require(observed.isReadProof && observed.snapshot.state == .empty(itemID: context.config.rootID))
            try #require(observed.snapshot.counts == ItemCompletionCounts(requiredDescendants: 3,
                completed: 0, incomplete: 3, occurrenceEvidenceRequired: 0))
            try #require(completion.canReview(context.config.rootID))
            try completion.queue(itemID: context.config.rootID, baseline: observed.snapshot,
                requiredForParent: true, mode: .complete)
            let journal = try #require(planner.itemCompletionState.journals.nativeCompletionOnly)
            try #require(!journal.hasBeenSubmitted && journal.noEffectCode == nil && journal.wasSensitive)
            try #require(journal.command.reopening == nil)
            try context.saveBaseline(journal)
            try #require(await recording.requests().isEmpty)
            try #require(await recording.reads() == [context.config.rootID])
            try #require(planner.canonicalItems == before)

        case .submitLost:
            try requireForest(planner, config: context.config, phase: .submitLost)
            let original = try context.loadBaseline()
            let journal = try #require(planner.itemCompletionState.journals.nativeCompletionOnly)
            try #require(journal == original.journal && !journal.hasBeenSubmitted)
            let before = planner.canonicalItems
            // The production store must obtain a fresh GET after encrypted restart.
            try #require(!(await completion.replayPending()))
            let receipt = try #require(await recording.lastReceipt())
            try #require(!receipt.replayed && receipt.matches(itemID: journal.itemID, command: journal.command))
            try #require(receipt.completion.state.provenance == expectedManualProvenance)
            let retained = try #require(planner.itemCompletionState.journals.nativeCompletionOnly)
            var expected = original.journal; expected.hasBeenSubmitted = true
            try #require(retained == expected && retained.requestBody == original.requestBody)
            try #require(!planner.itemCompletionState.needsCanonicalCatchUp && catchUpStates.isEmpty)
            try #require(await recording.requests() == [original.requestBody])
            try #require(await recording.reads() == [context.config.rootID])
            try #require(completion.observation(for: context.config.rootID)?.snapshot.state.revision == 0)
            try #require(planner.canonicalItems == before)
            // A pre-send GET may remain cached; unresolved submitted intent must
            // still deny both a new review and qualified-parent admission.
            try #require(!completion.canReview(context.config.rootID))
            try #require(!planner.itemCompletionQualifiesParent(context.config.rootID))

        case .replay:
            let original = try context.loadBaseline()
            let retained = try #require(planner.itemCompletionState.journals.nativeCompletionOnly)
            var expected = original.journal; expected.hasBeenSubmitted = true
            try #require(retained == expected && retained.requestBody == original.requestBody)
            try #require(await sync.refreshItemCompletionCanonicalEvidence())
            try requireForest(planner, config: context.config, phase: .replay)
            try #require(await completion.refresh(context.config.rootID))
            let latest = try #require(completion.observation(for: context.config.rootID))
            try #require(latest.isReadProof && latest.snapshot.state.revision == 3)
            try #require(latest.snapshot.state.mode == .automatic && latest.snapshot.state.provenance == nil)
            try #require(latest.snapshot.itemRevision > original.journal.command.expectedItemRevision + 1)
            let newestItems = planner.canonicalItems
            try #require(await completion.replayPending())
            let historical = try #require(await recording.lastReceipt())
            try #require(historical.replayed && historical.matches(itemID: retained.itemID, command: retained.command))
            try #require(historical.completion.state.revision == 1)
            try #require(historical.completion.state.provenance == expectedManualProvenance)
            try #require(await recording.requests() == [original.requestBody])
            try #require(await recording.reads() == [context.config.rootID])
            try #require(planner.itemCompletionState.journals.isEmpty && !planner.itemCompletionState.needsCanonicalCatchUp)
            try #require(catchUpStates.count == 1)
            try #require(completion.observation(for: context.config.rootID) == latest)
            try #require(planner.canonicalItems == newestItems)
            // A retained newer observation is not revived by the historical PUT or no-op catch-up.
            try #require(!planner.hasCurrentItemCompletionRead(context.config.rootID))
            try #require(!completion.canReview(context.config.rootID))

        case .verifyCascade, .verifyReopen:
            try #require(planner.itemCompletionState.journals.isEmpty && !planner.itemCompletionState.needsCanonicalCatchUp)
            try #require(await sync.refreshItemCompletionCanonicalEvidence())
            try requireForest(planner, config: context.config, phase: context.phase)
            try #require(await completion.refresh(context.config.rootID))
            let root = try #require(completion.observation(for: context.config.rootID)?.snapshot)
            let branch = try await context.client.itemCompletion(context.config.branchID)
            try #require(root.state.mode == .automatic && branch.state.mode == .automatic)
            if context.phase == .verifyCascade {
                try #require(root.state.provenance == ItemCompletionProvenance(kind: .automatic,
                    reopen: expectedManualProvenance.reopen))
                try #require(branch.state.provenance == ItemCompletionProvenance(kind: .automatic,
                    reopen: ItemCompletionReopenState(status: .planned)))
                try #require(root.counts == ItemCompletionCounts(requiredDescendants: 2,
                    completed: 2, incomplete: 0, occurrenceEvidenceRequired: 0))
            } else {
                try #require(root.state.provenance == nil && branch.state.provenance == nil)
                try #require(root.counts == ItemCompletionCounts(requiredDescendants: 3,
                    completed: 1, incomplete: 2, occurrenceEvidenceRequired: 0))
            }
            try #require(!root.occurrenceEvidenceRequired && !branch.occurrenceEvidenceRequired)
            try #require(await recording.requests().isEmpty && catchUpStates.isEmpty)
        }

        try Task.checkCancellation()
        try #require(planner.persistenceError == nil && planner.canonicalConfigurationIdentifier == context.client.configurationIdentifier)
        try #require(planner.executionState == initialExecution && planner.blocks.isEmpty)
        try #require(planner.pendingCanonicalAuthoringMutations.isEmpty && planner.pendingSchedulePublication == nil)
        if [.replay, .verifyCascade, .verifyReopen].contains(context.phase) {
            let optional = try await context.client.itemCompletion(context.config.optionalLeafID)
            try #require(!optional.state.requiredForParent && optional.state.mode == .automatic)
        }
        // submit_lost intentionally leaves the old cursor/cache: consuming the committed
        // write here would weaken the ambiguous-receipt checkpoint being measured.
        if context.phase != .submitLost {
            let cursor = try #require(planner.canonicalDeltaCursor)
            let next = try await context.client.itemDelta(cursor: cursor, limit: 1)
            try #require(next.changes.isEmpty && !next.hasMore && next.nextCursor == cursor)
        }
        let restarted = PlannerStore(persistence: context.persistence)
        try #require(restarted.persistenceError == nil && restarted.canonicalItems == planner.canonicalItems)
        try #require(restarted.canonicalDeltaCursor == planner.canonicalDeltaCursor)
        try #require(restarted.itemCompletionState == planner.itemCompletionState)
        try #require(restarted.itemCompletionReadAdmissions.isEmpty)
        let saved = try #require(try context.persistence.load())
        try #require(saved.itemCompletionState == planner.itemCompletionState)
        let encrypted = try context.readPrivate("planner.encrypted", maximumBytes: 4 * 1_024 * 1_024)
        let baseline = try context.loadBaseline()
        try #require(encrypted.range(of: baseline.requestBody) == nil)
        try context.writeMarker(planner: planner,
            root: completion.observation(for: context.config.rootID)?.snapshot)
    }

    private var expectedManualProvenance: ItemCompletionProvenance {
        .init(kind: .manual, reopen: .init(status: .blocked, blockedReasonKind: .manual,
            blockedReason: "Synthetic waiting for input"))
    }

    private func requireForest(_ planner: PlannerStore, config: Config, phase: Phase) throws {
        var expected = Set([config.rootID, config.branchID, config.requiredLeafID, config.optionalLeafID])
        if phase == .verifyReopen { expected.insert(config.newChildID) }
        try #require(planner.canonicalItems.count == expected.count && Set(planner.canonicalItems.map(\.id)) == expected)
        let root = try #require(planner.canonicalItems.first { $0.id == config.rootID })
        let branch = try #require(planner.canonicalItems.first { $0.id == config.branchID })
        let required = try #require(planner.canonicalItems.first { $0.id == config.requiredLeafID })
        let optional = try #require(planner.canonicalItems.first { $0.id == config.optionalLeafID })
        try #require(root.kind == .goal && root.parentID == nil)
        try #require(branch.kind == .project && branch.parentID == root.id)
        try #require(required.kind == .task && required.parentID == branch.id)
        try #require(optional.kind == .task && optional.parentID == root.id && optional.status == .planned)
        try #require(planner.canonicalItems.allSatisfy { $0.deletedAt == nil && $0.revision > 0 && $0.recurrence == nil })
        try #require(required.status == ([.verifyCascade, .verifyReopen].contains(phase) ? .completed : .planned))
        if phase == .verifyCascade {
            try #require(root.status == .completed && branch.status == .completed)
            try #require(root.blockedReasonKind == nil && root.blockedByItemID == nil && root.blockedReason == nil)
        } else {
            try #require(root.status == .blocked && branch.status == .planned)
            try #require(root.blockedReasonKind == .manual && root.blockedByItemID == nil)
            try #require(root.blockedReason == "Synthetic waiting for input")
        }
        if phase == .verifyReopen {
            let added = try #require(planner.canonicalItems.first { $0.id == config.newChildID })
            try #require(added.kind == .task && added.parentID == branch.id && added.status == .planned)
        }
    }

    private enum Phase: String { case prepare, submitLost = "submit_lost", replay, verifyCascade = "verify_cascade", verifyReopen = "verify_reopen" }
    private enum Failure: Error { case invalidConfiguration, unsafeArtifact }

    private struct Config: Decodable {
        let schemaVersion: Int
        let runID: UUID
        let baseURL: String
        let bearerToken: String
        let workDirectory: String
        let rootID: UUID
        let branchID: UUID
        let requiredLeafID: UUID
        let optionalLeafID: UUID
        let newChildID: UUID
        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version", runID = "run_id", baseURL = "base_url", bearerToken = "bearer_token"
            case workDirectory = "work_directory", rootID = "root_id", branchID = "branch_id"
            case requiredLeafID = "required_leaf_id", optionalLeafID = "optional_leaf_id", newChildID = "new_child_id"
        }
    }

    private struct Baseline: Codable {
        let schemaVersion: Int
        let runID: UUID
        let requestBody: Data
        let journal: ItemCompletionJournal
        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version", runID = "run_id", requestBody = "request_body_base64", journal
        }
    }

    @MainActor
    private struct Context {
        let config: Config
        let phase: Phase
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let session: URLSession
        let client: DayWeaveAPIClient

        init() throws {
            try Task.checkCancellation()
            let environment = ProcessInfo.processInfo.environment
            guard let raw = environment["DAYWEAVE_NATIVE_COMPLETION_CONFIG"], !raw.isEmpty,
                  let rawPhase = environment["DAYWEAVE_NATIVE_COMPLETION_PHASE"],
                  let phase = Phase(rawValue: rawPhase) else { throw Failure.invalidConfiguration }
            let input = URL(fileURLWithPath: raw).standardizedFileURL
            try Self.requirePrivate(input, directory: false, maximumBytes: 32 * 1_024)
            let file = input.resolvingSymlinksInPath()
            let root = file.deletingLastPathComponent()
            guard file.lastPathComponent == "config.json",
                  root.deletingLastPathComponent().path == URL(fileURLWithPath: "/tmp", isDirectory: true)
                    .resolvingSymlinksInPath().path,
                  root.lastPathComponent.hasPrefix("dayweave-native-completion.") else { throw Failure.unsafeArtifact }
            try Self.requirePrivate(root, directory: true)
            let bytes = try Data(contentsOf: file)
            guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: bytes),
                  let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
                  Set(object.keys) == ["schema_version", "run_id", "base_url", "bearer_token", "work_directory",
                    "root_id", "branch_id", "required_leaf_id", "optional_leaf_id", "new_child_id"] else {
                throw Failure.invalidConfiguration
            }
            let config = try JSONDecoder().decode(Config.self, from: bytes)
            let ids = [config.rootID, config.branchID, config.requiredLeafID, config.optionalLeafID, config.newChildID]
            guard config.schemaVersion == 1, config.runID != ItemCompletionValidation.nilID,
                  Set(ids).count == ids.count, !ids.contains(ItemCompletionValidation.nilID),
                  URL(fileURLWithPath: config.workDirectory, isDirectory: true).standardizedFileURL
                    .resolvingSymlinksInPath().path == root.path,
                  // The disposable driver enrolls this scoped device through the real auth API.
                  config.bearerToken.hasPrefix("dw_da1_"), config.bearerToken.utf8.count == 50,
                  config.bearerToken.utf8.dropFirst(7).allSatisfy({
                      (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
                  }),
                  let endpoint = URLComponents(string: config.baseURL), endpoint.scheme == "http",
                  endpoint.host == "127.0.0.1", endpoint.port.map({ (1...65_535).contains($0) }) == true,
                  endpoint.path == "/", endpoint.user == nil, endpoint.password == nil,
                  endpoint.query == nil, endpoint.fragment == nil else { throw Failure.invalidConfiguration }
            let directory = root.appendingPathComponent("macos", isDirectory: true)
            let keyURL = directory.appendingPathComponent("snapshot-key.bin")
            let snapshotURL = directory.appendingPathComponent("planner.encrypted")
            if phase == .prepare {
                guard !FileManager.default.fileExists(atPath: directory.path) else { throw Failure.unsafeArtifact }
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700])
                let key = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
                try key.write(to: keyURL, options: .withoutOverwriting)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyURL.path)
            } else {
                try Self.requirePrivate(snapshotURL, directory: false, maximumBytes: 4 * 1_024 * 1_024)
            }
            try Self.requirePrivate(directory, directory: true)
            try Self.requirePrivate(keyURL, directory: false, maximumBytes: 32)
            let key = try Data(contentsOf: keyURL)
            guard key.count == 32 else { throw Failure.unsafeArtifact }
            guard !FileManager.default.fileExists(atPath: directory.appendingPathComponent("\(phase.rawValue).json").path) else {
                throw Failure.unsafeArtifact
            }
            let configuration = URLSessionConfiguration.ephemeral
            configuration.connectionProxyDictionary = [:]
            configuration.urlCache = nil
            configuration.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
            configuration.httpCookieStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.urlCredentialStorage = nil
            configuration.timeoutIntervalForRequest = 15
            configuration.timeoutIntervalForResource = 60
            let session = URLSession(configuration: configuration)
            self.config = config; self.phase = phase; self.directory = directory; self.session = session
            persistence = EncryptedPlannerPersistence(fileURL: snapshotURL, key: try PlannerEncryptionKey(data: key))
            // Production request methods supply their redirect-rejecting per-task delegate.
            client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(config.baseURL),
                session: session, bearerToken: config.bearerToken)
        }

        func saveBaseline(_ journal: ItemCompletionJournal) throws {
            guard journal.isValid && journal.configurationIdentifier == client.configurationIdentifier,
                  journal.itemID == config.rootID, !journal.hasBeenSubmitted, journal.noEffectCode == nil else {
                throw Failure.invalidConfiguration
            }
            let baseline = Baseline(schemaVersion: 1, runID: config.runID, requestBody: journal.requestBody, journal: journal)
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            // Match the durable model's full Date precision; the exact request is independent.
            try writePrivate(journal.requestBody, named: "operation-a.json")
            try writePrivate(encoder.encode(baseline), named: "operation-a-baseline.json")
        }

        func loadBaseline() throws -> Baseline {
            let bytes = try readPrivate("operation-a-baseline.json", maximumBytes: 64 * 1_024)
            guard StrictJSONObjectKeyScanner.hasUniqueKeys(in: bytes) else { throw Failure.unsafeArtifact }
            let value = try JSONDecoder().decode(Baseline.self, from: bytes)
            let raw = try readPrivate("operation-a.json", maximumBytes: 16 * 1_024)
            guard value.schemaVersion == 1, value.runID == config.runID,
                  value.requestBody == raw, value.journal.requestBody == raw,
                  value.journal.isValid, value.journal.itemID == config.rootID,
                  value.journal.configurationIdentifier == client.configurationIdentifier,
                  !value.journal.hasBeenSubmitted, value.journal.noEffectCode == nil,
                  value.journal.command.mode == .complete, value.journal.command.requiredForParent,
                  value.journal.command.reopening == nil else { throw Failure.unsafeArtifact }
            return value
        }

        func writeMarker(planner: PlannerStore, root: ItemCompletionSnapshot?) throws {
            try Task.checkCancellation()
            let rows: [[String: Any]] = planner.canonicalItems.sorted {
                $0.id.uuidString.lowercased() < $1.id.uuidString.lowercased()
            }.map { item in [
                "id": item.id.uuidString.lowercased(), "revision": item.revision, "status": item.status.wireValue,
                "parent_id": item.parentID?.uuidString.lowercased() as Any? ?? NSNull(),
                "blocked_reason_kind": item.blockedReasonKind?.wireValue as Any? ?? NSNull(),
                "blocked_by_item_id": item.blockedByItemID?.uuidString.lowercased() as Any? ?? NSNull(),
                "blocked_reason": item.blockedReason as Any? ?? NSNull(),
            ] }
            let completion: Any
            if let root {
                guard root.isValid && root.itemID == config.rootID else { throw Failure.invalidConfiguration }
                completion = Self.lowercaseUUIDs(try JSONSerialization.jsonObject(with: JSONEncoder().encode(root)))
            } else { completion = NSNull() }
            let marker: [String: Any] = ["schema_version": 1, "run_id": config.runID.uuidString.lowercased(),
                "phase": phase.rawValue, "status": "passed", "pending_count": planner.itemCompletionState.journals.count,
                "needs_canonical_catch_up": planner.itemCompletionState.needsCanonicalCatchUp,
                "items": rows, "root_completion": completion]
            try writePrivate(JSONSerialization.data(withJSONObject: marker, options: [.sortedKeys]), named: "\(phase.rawValue).json")
        }

        func readPrivate(_ name: String, maximumBytes: Int) throws -> Data {
            guard !name.contains("/"), name != ".", name != ".." else { throw Failure.unsafeArtifact }
            let url = directory.appendingPathComponent(name)
            try Self.requirePrivate(url, directory: false, maximumBytes: maximumBytes)
            return try Data(contentsOf: url)
        }

        private func writePrivate(_ data: Data, named name: String) throws {
            try Task.checkCancellation()
            guard !name.contains("/"), name != ".", name != "..", data.count <= 64 * 1_024 else { throw Failure.unsafeArtifact }
            try Self.requirePrivate(directory, directory: true)
            let target = directory.appendingPathComponent(name)
            try data.write(to: target, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
            try Self.requirePrivate(target, directory: false, maximumBytes: 64 * 1_024)
        }

        private static func lowercaseUUIDs(_ value: Any) -> Any {
            if let text = value as? String, let uuid = UUID(uuidString: text) { return uuid.uuidString.lowercased() }
            if let array = value as? [Any] { return array.map(lowercaseUUIDs) }
            if let object = value as? [String: Any] { return object.mapValues(lowercaseUUIDs) }
            return value
        }

        private static func requirePrivate(_ url: URL, directory: Bool, maximumBytes: Int = 0) throws {
            let values = try FileManager.default.attributesOfItem(atPath: url.path)
            guard values[.type] as? FileAttributeType == (directory ? .typeDirectory : .typeRegular),
                  (values[.posixPermissions] as? NSNumber)?.intValue == (directory ? 0o700 : 0o600),
                  (values[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
                  directory || (values[.size] as? NSNumber).map({ $0.intValue > 0 && $0.intValue <= maximumBytes }) == true else {
                throw Failure.unsafeArtifact
            }
        }
    }
}

private extension Array {
    var nativeCompletionOnly: Element? { count == 1 ? first : nil }
}

private struct NativeCompletionConfigurationStore: SuggestionAPIConfigurationStoring {
    let baseURL: String
    func loadBaseURL() -> String? { baseURL }
    func saveBaseURL(_ value: String) {}
}

private struct NativeCompletionCredentialStore: BearerTokenStoring {
    let token: String
    let origin: String
    func loadCredential() throws -> OriginBoundBearerCredential? { .init(token: token, origin: origin) }
    func saveCredential(_ credential: OriginBoundBearerCredential) throws { throw NativeCompletionHarnessError.unexpectedMutation }
    func deleteCredential() throws { throw NativeCompletionHarnessError.unexpectedMutation }
}

private struct NativeCompletionForbiddenComposer: LocalScheduleComposing {
    func compose(canonicalItems: [DayWeaveCanonicalItem], schedule: DayWeaveSchedulePreviewRequest) async throws -> LocalScheduleComposition {
        throw NativeCompletionHarnessError.unexpectedMutation
    }
}

private enum NativeCompletionHarnessError: Error { case unexpectedMutation }

private actor NativeCompletionRecordingTransport: ItemCompletionTransport {
    nonisolated let configurationIdentifier: String
    private let client: DayWeaveAPIClient
    private let dropSuccessfulReply: Bool
    private var sent: [Data] = []
    private var fetched: [UUID] = []
    private var receipt: ItemCompletionReceipt?

    init(client: DayWeaveAPIClient, dropSuccessfulReply: Bool) {
        self.client = client; self.dropSuccessfulReply = dropSuccessfulReply
        configurationIdentifier = client.configurationIdentifier
    }
    func itemCompletion(_ itemID: UUID) async throws -> ItemCompletionSnapshot {
        try Task.checkCancellation()
        fetched.append(itemID)
        return try await client.itemCompletion(itemID)
    }
    func putItemCompletion(_ itemID: UUID, requestBody: Data) async throws -> ItemCompletionReceipt {
        try Task.checkCancellation()
        sent.append(requestBody)
        let result = try await client.putItemCompletion(itemID, requestBody: requestBody)
        receipt = result
        if dropSuccessfulReply { throw ItemCompletionError.unavailable }
        return result
    }
    func requests() -> [Data] { sent }
    func reads() -> [UUID] { fetched }
    func lastReceipt() -> ItemCompletionReceipt? { receipt }
}
#endif
