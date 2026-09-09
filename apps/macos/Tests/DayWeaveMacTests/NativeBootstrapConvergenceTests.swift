import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

/// Explicitly opted-in loopback fixture only. Never opens the app, owner
/// defaults, Keychain, real providers or an owner workspace.
@Suite("Controlled native current-state bootstrap", .serialized)
@MainActor
struct NativeBootstrapConvergenceTests {
    @Test("cold Mac hydrates a history-heavy PostgreSQL forest through the real HTTP loader",
          .enabled(if: ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_BOOTSTRAP_CONFIG"] != nil))
    func coldRealHTTPBootstrap() async throws {
        guard let raw = ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_BOOTSTRAP_CONFIG"] else {
            throw FixtureError.invalidConfiguration
        }
        let file = URL(fileURLWithPath: raw).standardizedFileURL.resolvingSymlinksInPath()
        let root = file.deletingLastPathComponent()
        guard file.lastPathComponent == "config.json",
              root.deletingLastPathComponent().path == URL(fileURLWithPath: "/tmp", isDirectory: true)
                .resolvingSymlinksInPath().path,
              root.lastPathComponent.hasPrefix("dayweave-native-bootstrap.") else {
            throw FixtureError.unsafeLocation
        }
        try Self.requirePrivate(root, directory: true)
        try Self.requirePrivate(file, directory: false)
        let config = try JSONDecoder().decode(Config.self, from: Data(contentsOf: file))
        guard config.schemaVersion == 1, config.expectedActiveCount == 5_000,
              config.bearerToken.hasPrefix("native-bootstrap-"),
              let url = URLComponents(string: config.baseURL),
              url.scheme == "http", url.host == "127.0.0.1", url.port != nil,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil else {
            throw FixtureError.invalidConfiguration
        }
        let directory = root.appendingPathComponent("macos-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: try PlannerEncryptionKey(data: Data(repeating: 67, count: 32)))
        let sessionConfiguration = URLSessionConfiguration.ephemeral
        sessionConfiguration.connectionProxyDictionary = [:]
        sessionConfiguration.urlCache = nil
        sessionConfiguration.httpCookieStorage = nil
        sessionConfiguration.urlCredentialStorage = nil
        let session = URLSession(configuration: sessionConfiguration)
        defer { session.invalidateAndCancel() }
        let planner = PlannerStore(persistence: persistence, restoreFromPersistence: false)
        let sync = CanonicalSyncStore(planner: planner,
            configurationStore: BootstrapConfigurationStore(baseURL: config.baseURL),
            tokenStore: TestBearerTokenStore(token: config.bearerToken, origin: config.baseURL), session: session,
            scheduleReplicaRequiresDurableBinding: false)

        // This fixture proves item hydration, not schedule composition. The
        // unrelated current-publication resource may be absent/unconfigured.
        _ = await sync.bootstrapForegroundActivation()
        #expect(planner.persistenceError == nil)
        try #require(planner.canonicalItems.count == config.expectedActiveCount,
            "The controlled canonical read did not finish: \(sync.status.message)")
        #expect(planner.canonicalItems.first?.id == config.expectedRootID)
        #expect(planner.canonicalItems.last?.id == config.expectedLeafID)
        let byID = Dictionary(uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) })
        var current: UUID? = config.expectedLeafID
        var visited = Set<UUID>()
        while let id = current {
            guard visited.insert(id).inserted, let item = byID[id] else {
                throw FixtureError.invalidForest
            }
            current = item.parentID
        }
        #expect(visited.count == config.expectedActiveCount)
        let trash = try #require(planner.canonicalTrashEntry(id: config.expectedTrashID))
        #expect(trash.lastKnownItem == nil)
        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(planner.blocks.isEmpty)
        let cursor = try #require(planner.canonicalDeltaCursor)
        let restarted = PlannerStore(persistence: persistence)
        #expect(restarted.persistenceError == nil)
        #expect(restarted.canonicalItems == planner.canonicalItems)
        #expect(restarted.canonicalTrashEntry(id: config.expectedTrashID) == trash)
        #expect(restarted.canonicalDeltaCursor == cursor)
        let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(config.baseURL),
            session: session, bearerToken: config.bearerToken)
        let incremental = try await client.itemDelta(cursor: cursor, limit: 1)
        #expect(incremental.changes.isEmpty && !incremental.hasMore)
        #expect(incremental.nextCursor == cursor)
    }

    private enum FixtureError: Error { case invalidConfiguration, unsafeLocation, unsafeArtifact, invalidForest }

    private struct BootstrapConfigurationStore: SuggestionAPIConfigurationStoring {
        let baseURL: String
        func loadBaseURL() -> String? { baseURL }
        func saveBaseURL(_ value: String) {}
    }

    private struct Config: Decodable {
        let schemaVersion: Int
        let baseURL: String
        let bearerToken: String
        let expectedActiveCount: Int
        let expectedTrashID: UUID
        let expectedRootID: UUID
        let expectedLeafID: UUID
        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version", baseURL = "base_url", bearerToken = "bearer_token"
            case expectedActiveCount = "expected_active_count", expectedTrashID = "expected_trash_id"
            case expectedRootID = "expected_root_id", expectedLeafID = "expected_leaf_id"
        }
    }

    private static func requirePrivate(_ url: URL, directory: Bool) throws {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        guard attributes[.type] as? FileAttributeType == (directory ? .typeDirectory : .typeRegular),
              (attributes[.posixPermissions] as? NSNumber)?.intValue == (directory ? 0o700 : 0o600),
              (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid() else {
            throw FixtureError.unsafeArtifact
        }
    }
}
#endif
