import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
/// Opt-in real HTTP checkpoint. The runner owns a fresh loopback API/database;
/// this test never instantiates the app, user defaults, Keychain or providers.
@Suite("Controlled native progress convergence", .serialized)
@MainActor
struct NativeProgressConvergenceTests {
    @Test("selected macOS phase against a disposable PostgreSQL-backed service",
          .enabled(if: ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_CONVERGENCE_CONFIG"] != nil
            || ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_CONVERGENCE_PHASE"] != nil))
    func runSelectedPhaseAgainstDisposableService() async throws {
        let context = try Context()
        let phase = context.phase
        let client = context.client
        let planner: PlannerStore
        if phase == "prepare" {
            let page = try await client.itemDelta(cursor: nil)
            #expect(!page.hasMore)
            // A delta is an ordered change stream, not a unique-item snapshot.
            // Feed the complete page through the actual native hydration path.
            planner = PlannerStore(canonicalConfigurationIdentifier: client.configurationIdentifier,
                persistence: context.persistence, restoreFromPersistence: false)
            planner.replaceCanonicalState(changes: page.changes, nextCursor: page.nextCursor)
            planner.flushPersistence()
        } else {
            planner = PlannerStore(persistence: context.persistence)
        }
        #expect(planner.persistenceError == nil)
        #expect(planner.canonicalConfigurationIdentifier == client.configurationIdentifier)
        #expect(planner.canonicalItems.count == 2 && planner.blocks.isEmpty)
        #expect(Set(planner.canonicalItems.map(\.id)) == [context.config.itemID, context.config.childID])
        #expect(planner.canonicalItems.first { $0.id == context.config.itemID }?.kind == .goal)
        #expect(planner.canonicalItems.first { $0.id == context.config.childID }?.parentID == context.config.itemID)
        let beforeItems = planner.canonicalItems
        let beforeExecution = planner.executionState
        let recording = RecordingProgressTransport(client: client, dropSuccessfulReply: phase == "submit_lost")
        let progress = ItemProgressStore(planner: planner, connection: { recording },
            now: { Date(timeIntervalSince1970: 1_788_854_400) },
            sleep: { _ in throw CancellationError() })
        progress.activate()
        defer { progress.suspendForPrivacyBoundary() }

        switch phase {
        case "prepare":
            #expect(await progress.refresh(context.config.itemID))
            let baseline = try #require(progress.observation(for: context.config.itemID)?.snapshot)
            #expect(baseline.revision == 0 && baseline.components.isEmpty)
            try progress.queue(itemID: context.config.itemID, baseline: baseline,
                components: context.config.initialComponents)
            let journal = try #require(planner.itemProgressState.journals.first)
            #expect(!journal.hasBeenSubmitted)
            try context.writePrivate(journal.requestBody, named: "operation-a.json")
            #expect(await recording.requests().isEmpty)
        case "submit_lost":
            let originalBytes = try context.readPrivate("operation-a.json")
            let pending = try #require(planner.itemProgressState.journals.first)
            #expect(pending.requestBody == originalBytes && !pending.hasBeenSubmitted)
            #expect(await progress.replayPending() == false)
            let receipt = try #require(await recording.lastReceipt())
            #expect(!receipt.replayed && receipt.progress.revision == 1)
            #expect(receipt.progress.components == context.config.initialComponents)
            #expect(planner.itemProgressState.journals.first?.hasBeenSubmitted == true)
            #expect(await recording.requests() == [originalBytes])
            #expect(progress.observation(for: context.config.itemID)?.snapshot.revision == 0)
        case "replay":
            let originalBytes = try context.readPrivate("operation-a.json")
            let pending = try #require(planner.itemProgressState.journals.first)
            #expect(pending.hasBeenSubmitted && pending.requestBody == originalBytes)
            #expect(await progress.refresh(context.config.itemID))
            let latest = try #require(progress.observation(for: context.config.itemID))
            #expect(latest.isReadProof && latest.snapshot.revision == 2)
            #expect(latest.snapshot.components == context.config.replacementComponents)
            #expect(await progress.replayPending())
            let historical = try #require(await recording.lastReceipt())
            #expect(historical.replayed && historical.progress.revision == 1)
            #expect(historical.progress.components == context.config.initialComponents)
            #expect(await recording.requests() == [originalBytes])
            #expect(planner.itemProgressState.journals.isEmpty)
            #expect(progress.observation(for: context.config.itemID) == latest)
            #expect(await progress.refresh(context.config.itemID))
            #expect(progress.observation(for: context.config.itemID)?.snapshot == latest.snapshot)
        default:
            throw ConvergenceFailure.invalidPhase
        }
        #expect(planner.canonicalItems == beforeItems && planner.executionState == beforeExecution)
        #expect(planner.blocks.isEmpty)
        #expect(planner.canonicalItems.allSatisfy { $0.revision > 0 && $0.status == .planned })
        let child = try await client.itemProgress(context.config.childID)
        #expect(child.revision == 0 && child.components.isEmpty)
        let loaded = try context.persistence.load()
        let durable = try #require(loaded)
        #expect(durable.itemProgressState == planner.itemProgressState)
        let observation = try #require(progress.observation(for: context.config.itemID))
        let canonicalEncoder = JSONEncoder()
        canonicalEncoder.outputFormatting = [.sortedKeys]
        canonicalEncoder.dateEncodingStrategy = .millisecondsSince1970
        let canonicalHash = Self.digest(try canonicalEncoder.encode(planner.canonicalItems))
        let result: [String: Any] = ["phase": phase, "run_id": context.config.runID.uuidString.lowercased(),
            "status": "passed", "canonical_sha256": canonicalHash,
            "progress_revision": observation.snapshot.revision, "pending_count": planner.itemProgressState.journals.count,
            "operation_body_sha256": Self.digest(try context.readPrivate("operation-a.json"))]
        try context.writePrivate(JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), named: "\(phase).json")
    }

    private static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }

    private enum ConvergenceFailure: Error { case invalidConfiguration, invalidPhase, unsafeArtifact }

    private struct Config: Decodable {
        let schemaVersion: Int
        let runID: UUID
        let baseURL: String
        let bearerToken: String
        let itemID: UUID
        let childID: UUID
        let workDirectory: String
        let initialComponents: [ItemProgressComponent]
        let replacementComponents: [ItemProgressComponent]
        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version", runID = "run_id", baseURL = "base_url", bearerToken = "bearer_token"
            case itemID = "item_id", childID = "child_id", workDirectory = "work_directory"
            case initialComponents = "initial_components", replacementComponents = "replacement_components"
        }
    }

    @MainActor
    private struct Context {
        let config: Config
        let phase: String
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let client: DayWeaveAPIClient

        init() throws {
            guard let raw = ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_CONVERGENCE_CONFIG"],
                  let phase = ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_CONVERGENCE_PHASE"],
                  ["prepare", "submit_lost", "replay"].contains(phase) else { throw ConvergenceFailure.invalidPhase }
            let configURL = URL(fileURLWithPath: raw).standardizedFileURL.resolvingSymlinksInPath()
            let root = configURL.deletingLastPathComponent()
            guard configURL.lastPathComponent == "config.json",
                  root.deletingLastPathComponent().path == URL(fileURLWithPath: "/tmp", isDirectory: true)
                    .resolvingSymlinksInPath().path,
                  root.lastPathComponent.hasPrefix("dayweave-native-convergence.") else { throw ConvergenceFailure.unsafeArtifact }
            try Self.requirePrivate(configURL, directory: false)
            try Self.requirePrivate(root, directory: true)
            let config = try JSONDecoder().decode(Config.self, from: Data(contentsOf: configURL))
            guard config.schemaVersion == 1, config.itemID != config.childID,
                  URL(fileURLWithPath: config.workDirectory, isDirectory: true).resolvingSymlinksInPath().path == root.path,
                  config.bearerToken.hasPrefix("native-convergence-"),
                  let url = URLComponents(string: config.baseURL), url.scheme == "http", url.host == "127.0.0.1",
                  url.port.map({ (1...65535).contains($0) }) == true, url.path == "/",
                  url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
                  ItemProgressValidation.components(config.initialComponents),
                  ItemProgressValidation.components(config.replacementComponents) else { throw ConvergenceFailure.invalidConfiguration }
            let directory = root.appendingPathComponent("macos", isDirectory: true)
            if phase == "prepare" {
                guard !FileManager.default.fileExists(atPath: directory.path) else { throw ConvergenceFailure.unsafeArtifact }
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700])
                let key = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
                let keyURL = directory.appendingPathComponent("snapshot-key.bin")
                try key.write(to: keyURL, options: .withoutOverwriting)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyURL.path)
            }
            try Self.requirePrivate(directory, directory: true)
            let keyURL = directory.appendingPathComponent("snapshot-key.bin")
            try Self.requirePrivate(keyURL, directory: false)
            self.config = config; self.phase = phase; self.directory = directory
            persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("planner.encrypted"),
                key: try PlannerEncryptionKey(data: Data(contentsOf: keyURL)))
            let sessionConfiguration = URLSessionConfiguration.ephemeral
            sessionConfiguration.connectionProxyDictionary = [:]
            sessionConfiguration.urlCache = nil
            sessionConfiguration.httpCookieStorage = nil
            sessionConfiguration.urlCredentialStorage = nil
            client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(config.baseURL),
                session: URLSession(configuration: sessionConfiguration), bearerToken: config.bearerToken)
        }

        func writePrivate(_ data: Data, named name: String) throws {
            let target = directory.appendingPathComponent(name)
            try data.write(to: target, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
        }
        func readPrivate(_ name: String) throws -> Data {
            let target = directory.appendingPathComponent(name)
            try Self.requirePrivate(target, directory: false)
            return try Data(contentsOf: target)
        }
        private static func requirePrivate(_ url: URL, directory: Bool) throws {
            let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
            guard attributes[.type] as? FileAttributeType == (directory ? .typeDirectory : .typeRegular),
                  (attributes[.posixPermissions] as? NSNumber)?.intValue == (directory ? 0o700 : 0o600),
                  (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid() else { throw ConvergenceFailure.unsafeArtifact }
        }
    }
}

private actor RecordingProgressTransport: ItemProgressTransport {
    nonisolated let configurationIdentifier: String
    private let client: DayWeaveAPIClient
    private let dropSuccessfulReply: Bool
    private var sent: [Data] = []
    private var receipt: ItemProgressReceipt?
    init(client: DayWeaveAPIClient, dropSuccessfulReply: Bool) {
        self.client = client; self.dropSuccessfulReply = dropSuccessfulReply
        configurationIdentifier = client.configurationIdentifier
    }
    func itemProgress(_ itemID: UUID) async throws -> ItemProgressSnapshot { try await client.itemProgress(itemID) }
    func putItemProgress(_ itemID: UUID, requestBody: Data) async throws -> ItemProgressReceipt {
        sent.append(requestBody)
        let result = try await client.putItemProgress(itemID, requestBody: requestBody)
        receipt = result
        if dropSuccessfulReply { throw ItemProgressError.unavailable }
        return result
    }
    func requests() -> [Data] { sent }
    func lastReceipt() -> ItemProgressReceipt? { receipt }
}
#endif
