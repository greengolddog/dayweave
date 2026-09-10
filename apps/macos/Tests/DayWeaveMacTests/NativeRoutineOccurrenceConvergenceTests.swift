import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

/// Separate-process phases run only against the disposable runner's loopback
/// service. All storage and credentials are supplied privately by that runner.
@Suite("Controlled native routine occurrence convergence", .serialized)
@MainActor
struct NativeRoutineOccurrenceConvergenceTests {
    @Test("selected macOS occurrence phase uses real encrypted recovery and remote composition",
          .enabled(if: ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_ROUTINE_CONFIG"] != nil
            || ProcessInfo.processInfo.environment["DAYWEAVE_NATIVE_ROUTINE_PHASE"] != nil))
    func runSelectedPhaseAgainstDisposableService() async throws {
        let context = try Context()
        defer { context.session.invalidateAndCancel(); NativeRoutinePublicationProtocol.capture.reset() }
        let referenceDate = context.asOf
        let planner = PlannerStore(scheduleProfile: try .legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 0),
            persistence: context.persistence, restoreFromPersistence: context.phase != .prepare,
            now: { referenceDate })
        try #require(planner.persistenceError == nil && planner.canPersistPlan)
        let composer = NativeRoutineForbiddenComposer()
        let sync = CanonicalSyncStore(planner: planner,
            configurationStore: NativeRoutineConfigurationStore(baseURL: context.config.baseURL),
            tokenStore: NativeRoutineCredentialStore(token: context.config.bearerToken,
                origin: try DayWeaveAPIBaseURL(context.config.baseURL).credentialOriginIdentifier),
            session: context.session, localComposer: composer,
            itemStreamTransportProvider: { _ in nil }, scheduleStreamTransportProvider: { _ in nil },
            scheduleReplicaRequiresDurableBinding: false, now: { referenceDate })
        NativeRoutinePublicationProtocol.capture.configure(baseURL: context.config.baseURL,
            authorization: "Bearer " + context.config.bearerToken,
            dropFirstSuccess: context.phase == .replayLostPublication, beforeForward: { body in
                guard let saved = try? context.persistence.load(),
                      let publication = saved.pendingSchedulePublication,
                      publication == planner.pendingSchedulePublication,
                      publication.preparedRequest.body == body else { return false }
                // Even when an old publication is being replayed, the latch is
                // retained until a distinct freshly composed publication lands.
                if context.phase == .recover || context.phase == .replayLostPublication {
                    return saved.routineOccurrenceState?.needsRemoteScheduleCatchUp == true
                        && saved.routineOccurrenceState == planner.routineOccurrenceState
                }
                return true
            })
        let recording = NativeRoutineRecordingTransport(client: context.client,
            dropSuccessfulReply: context.phase == .submitLost, failPages: context.phase == .submitLost,
            beforePut: { instanceID, memberID, body in
                guard let saved = try? context.persistence.load(), let state = saved.routineOccurrenceState,
                      let journal = state.journals.nativeRoutineOnly,
                      journal.hasBeenSubmitted, journal.noEffectCode == nil,
                      journal.instanceID == instanceID, journal.memberID == memberID,
                      journal.requestBody == body, state == planner.routineOccurrenceState else { return false }
                return true
            })
        var scheduleCaptures: [RoutineOccurrenceState] = []
        let occurrences = RoutineOccurrenceStore(planner: planner, connection: { recording }, catchUp: {
            guard let saved = try? context.persistence.load(), let state = saved.routineOccurrenceState,
                  state == planner.routineOccurrenceState, state.journals.isEmpty,
                  state.minimumCatchUpRevisions.isEmpty, state.needsRemoteScheduleCatchUp,
                  state.terminalDeltaCursor != nil else { return false }
            scheduleCaptures.append(state)
            return await sync.syncThroughFreshComposition()
        }, now: { referenceDate }, sleep: { _ in throw CancellationError() }, automaticOutbox: false)
        occurrences.activate()
        defer { occurrences.suspendForPrivacyBoundary() }
        let owner = UUID()
        if context.phase != .prepare {
            try #require(planner.canonicalConfigurationIdentifier == context.client.configurationIdentifier)
            try #require(planner.canonicalDeltaCursor != nil)
            occurrences.showDetail(seriesItemID: context.config.rootID, occurrenceID: context.config.occurrenceID, owner: owner)
            try #require(occurrences.selectedSnapshot != nil)
            try #require(occurrences.reviewLease(memberID: context.config.requiredLeafID, owner: owner) == nil)
        }
        let initialExecution = planner.executionState

        switch context.phase {
        case .prepare:
            try #require(await sync.syncThroughFreshComposition())
            try #require(await occurrences.replayPending())
            try #require(scheduleCaptures.count == 1)
            try #require(planner.publishedScheduleProof != nil && planner.pendingSchedulePublication == nil)
            occurrences.showDetail(seriesItemID: context.config.rootID, occurrenceID: context.config.occurrenceID, owner: owner)
            try #require(await occurrences.refreshSelected())
            let snapshot = try #require(occurrences.selectedSnapshot)
            try requireIdentityAndMembers(snapshot.aggregate, config: context.config, revision: 1)
            try requireInitialStates(snapshot.aggregate, config: context.config)
            let root = try #require(snapshot.members.first { $0.itemID == context.config.rootID })
            try #require(root.counts == .init(requiredDescendants: 2, completed: 0, incomplete: 2))
            try #require(snapshot.freshEditEligible && snapshot.members.allSatisfy { !$0.occurrenceEvidenceRequired })
            let sentinel = try cached(context.config.sentinelInstanceID, planner: planner)
            try #require(sentinel.manifest.occurrenceID == context.config.sentinelOccurrenceID && sentinel.revision == 1)
            try requireInitialStates(sentinel, config: context.config)
            let lease = try #require(occurrences.reviewLease(memberID: context.config.requiredLeafID, owner: owner))
            try occurrences.queueReviewed(lease: lease, baseline: snapshot, action: .setOutcome(status: .completed))
            let journal = try #require(planner.routineOccurrenceState.journals.nativeRoutineOnly)
            try #require(journal.isValid && !journal.hasBeenSubmitted && journal.wasSensitive && journal.noEffectCode == nil)
            try #require(journal.instanceID == context.config.instanceID && journal.memberID == context.config.requiredLeafID)
            try #require(await recording.requests().isEmpty)
            try context.saveBaseline(.init(schemaVersion: 1, runID: context.config.runID, journal: journal,
                initial: snapshot.aggregate, sentinel: sentinel, canonicalProjection: try Context.itemProjection(planner)))

        case .submitLost:
            let baseline = try context.loadBaseline()
            try #require(planner.routineOccurrenceState.journals == [baseline.journal])
            try #require(!(await occurrences.replayPending()))
            let receipt = try #require(await recording.lastReceipt())
            try #require(!receipt.replayed && receipt.matches(instanceID: context.config.instanceID,
                memberID: context.config.requiredLeafID, command: baseline.journal.command))
            try requireIdentityAndMembers(receipt.occurrence.aggregate, config: context.config, revision: 2)
            try #require(receipt.occurrence.aggregate.members.first { $0.itemID == context.config.requiredLeafID }?.status == .completed)
            var submitted = baseline.journal; submitted.hasBeenSubmitted = true
            try #require(planner.routineOccurrenceState.journals == [submitted])
            try #require(planner.routineOccurrenceState.minimumCatchUpRevisions.isEmpty)
            try #require(!planner.routineOccurrenceState.needsRemoteScheduleCatchUp && scheduleCaptures.isEmpty)
            try #require(try cached(context.config.instanceID, planner: planner) == baseline.initial)
            try #require(await recording.requests() == [baseline.journal.requestBody])
            try #require(await recording.reads() == [context.config.instanceID])
            try #require(NativeRoutinePublicationProtocol.capture.records().isEmpty)
            try context.writePrivate(try JSONEncoder().encode(receipt), named: "operation-a-receipt.json")

        case .replayLostPublication:
            let baseline = try context.loadBaseline()
            var submitted = baseline.journal; submitted.hasBeenSubmitted = true
            try #require(planner.routineOccurrenceState.journals == [submitted])
            try #require(await occurrences.refreshSelected())
            let newer = try #require(occurrences.selectedSnapshot)
            try requireIdentityAndMembers(newer.aggregate, config: context.config, revision: 3)
            try #require(newer.aggregate.members.first { $0.itemID == context.config.rootID }?.mode == .keepOpen)
            try #require(!(await occurrences.replayPending()))
            let receipt = try #require(await recording.lastReceipt())
            let original = try RoutineOccurrenceValidation.decode(RoutineOccurrenceMutation.self,
                from: context.readPrivate("operation-a-receipt.json", maximumBytes: 128 * 1_024))
            try #require(receipt.replayed && receipt.operationID == original.operationID && receipt.occurrence == original.occurrence)
            try #require(await recording.requests() == [baseline.journal.requestBody])
            try #require(await recording.reads().isEmpty, "submitted receipt recovery must not perform a pre-PUT GET")
            try #require(try cached(context.config.instanceID, planner: planner) == newer.aggregate)
            try #require(planner.routineOccurrenceState.journals.isEmpty && planner.routineOccurrenceState.minimumCatchUpRevisions.isEmpty)
            try #require(planner.routineOccurrenceState.needsRemoteScheduleCatchUp && scheduleCaptures.count == 1)
            let publication = try #require(planner.pendingSchedulePublication)
            let sent = try #require(NativeRoutinePublicationProtocol.capture.records().nativeRoutineOnly)
            try #require(sent.dropped && sent.statusCode == 200 && sent.body == publication.preparedRequest.body)
            try #require(sent.operationID == publication.preparedRequest.request.idempotencyKey && sent.replayed == false)
            try context.writePrivate(sent.body, named: "publication-lost-body.json")
            try context.writePrivate(sent.responseBody, named: "publication-lost-response.json")

        case .recover:
            let lost = try context.readPrivate("publication-lost-body.json", maximumBytes: 1_024 * 1_024)
            let originalResponse = try context.readPrivate("publication-lost-response.json", maximumBytes: 1_024 * 1_024)
            let pending = try #require(planner.pendingSchedulePublication)
            try #require(pending.preparedRequest.body == lost && planner.routineOccurrenceState.needsRemoteScheduleCatchUp)
            try #require(await occurrences.replayPending())
            try #require(scheduleCaptures.count == 1 && planner.pendingSchedulePublication == nil)
            let publications = NativeRoutinePublicationProtocol.capture.records()
            try #require(publications.count >= 2)
            let recovered = try #require(publications.first), fresh = try #require(publications.last)
            let recoveredOperation = try #require(recovered.operationID), freshOperation = try #require(fresh.operationID)
            let freshRevision = try #require(fresh.revisionID)
            try #require(recovered.body == lost && recovered.replayed == true)
            try #require(recovered.revisionID == NativeRoutinePublicationRecord.revisionID(in: originalResponse))
            try #require(freshOperation != recoveredOperation && fresh.body != lost && fresh.replayed == false && fresh.statusCode == 200)
            try #require(planner.publishedScheduleProof?.revisionID == freshRevision)
            try #require(!planner.routineOccurrenceState.hasUnresolvedCustody)
            try #require(await recording.requests().isEmpty)
            try #require(await recording.reads().isEmpty)
            try #require(try cached(context.config.instanceID, planner: planner).revision == 3)

        case .verify:
            try #require(await occurrences.replayPending())
            try #require(scheduleCaptures.count == 1 && planner.pendingSchedulePublication == nil)
            try #require(await occurrences.refreshSelected())
            let current = try #require(occurrences.selectedSnapshot)
            try requireIdentityAndMembers(current.aggregate, config: context.config, revision: 6)
            try #require(!planner.routineOccurrenceState.hasUnresolvedCustody)
            let states = Dictionary(uniqueKeysWithValues: current.aggregate.members.map { ($0.itemID, $0) })
            for id in [context.config.rootID, context.config.branchID, context.config.requiredLeafID] {
                try #require(states[id]?.status == .completed && states[id]?.mode == .automatic)
            }
            try #require(states[context.config.optionalLeafID]?.status == .planned)
            try #require(states[context.config.inboxLeafID]?.status == .inbox)
            try #require(states[context.config.blockedLeafID]?.status == .blocked)
            try #require(states[context.config.blockedLeafID]?.open == expectedBlockedOpen)
            let root = try #require(current.members.first { $0.itemID == context.config.rootID })
            try #require(root.counts == .init(requiredDescendants: 2, completed: 2, incomplete: 0))
            try #require(await recording.requests().isEmpty)
        }

        let baseline = try context.loadBaseline()
        try #require(try Context.itemProjection(planner) == baseline.canonicalProjection)
        try #require(try cached(context.config.sentinelInstanceID, planner: planner) == baseline.sentinel)
        try #require(planner.executionState == initialExecution && planner.pendingCanonicalMutations.isEmpty)
        try #require(planner.pendingCanonicalAuthoringMutations.isEmpty && planner.pendingCanonicalSensitivityMutations.isEmpty)
        try #require(planner.localScheduleCompositionProvenance == nil && composer.calls() == 0)
        try #require(!sync.canRecomposeLocally, "managed recurring work cannot enter helper v1")
        let restarted = PlannerStore(persistence: context.persistence, now: { referenceDate })
        try #require(restarted.persistenceError == nil && restarted.routineOccurrenceState == planner.routineOccurrenceState)
        try #require(restarted.pendingSchedulePublication == planner.pendingSchedulePublication)
        let encrypted = try context.readPrivate("planner.encrypted", maximumBytes: 8 * 1_024 * 1_024)
        try #require(encrypted.range(of: baseline.journal.requestBody) == nil)
        try context.writeMarker(planner: planner, occurrence: cached(context.config.instanceID, planner: planner),
            sentinel: cached(context.config.sentinelInstanceID, planner: planner))
    }

    private var expectedBlockedOpen: ItemCompletionReopenState {
        .init(status: .blocked, blockedReasonKind: .manual, blockedReason: "Synthetic waiting for input")
    }
    private func cached(_ instanceID: UUID, planner: PlannerStore) throws -> RoutineOccurrenceAggregate {
        try #require(planner.routineOccurrenceState.observations.first { $0.instanceID == instanceID }?.snapshot.aggregate)
    }
    private func requireIdentityAndMembers(_ value: RoutineOccurrenceAggregate, config: Config, revision: UInt64) throws {
        try #require(value.manifest.id == config.instanceID && value.manifest.occurrenceID == config.occurrenceID)
        try #require(value.manifest.id != value.manifest.occurrenceID && value.manifest.seriesItemID == config.rootID)
        try #require(value.revision == revision)
        let expected = Set([config.rootID, config.branchID, config.requiredLeafID,
            config.optionalLeafID, config.inboxLeafID, config.blockedLeafID])
        try #require(Set(value.manifest.members.map(\.itemID)) == expected && value.members.count == expected.count)
        let definitions = Dictionary(uniqueKeysWithValues: value.manifest.members.map { ($0.itemID, $0) })
        try #require(definitions[config.rootID]?.kind == .routine && definitions[config.rootID]?.recurs == true)
        try #require(definitions[config.branchID]?.parentID == config.rootID)
        try #require(definitions[config.requiredLeafID]?.parentID == config.branchID)
        for id in [config.optionalLeafID, config.inboxLeafID, config.blockedLeafID] {
            try #require(definitions[id]?.parentID == config.rootID && definitions[id]?.requiredForParent == false)
        }
    }
    private func requireInitialStates(_ value: RoutineOccurrenceAggregate, config: Config) throws {
        for member in value.members {
            let expected: RoutineOccurrenceStatus = member.itemID == config.inboxLeafID ? .inbox
                : member.itemID == config.blockedLeafID ? .blocked : .planned
            try #require(member.status == expected && member.revision == 1 && member.mode == .automatic)
            if member.itemID == config.blockedLeafID { try #require(member.open == expectedBlockedOpen) }
        }
    }

    private enum Phase: String {
        case prepare, submitLost = "submit_lost", replayLostPublication = "replay_lost_publication", recover, verify
    }
    private struct Config: Decodable {
        let schemaVersion: Int; let runID: UUID; let baseURL: String; let bearerToken: String; let workDirectory: String
        let rootID: UUID; let branchID: UUID; let requiredLeafID: UUID; let optionalLeafID: UUID
        let inboxLeafID: UUID; let blockedLeafID: UUID; let occurrenceID: UUID; let sentinelOccurrenceID: UUID
        let instanceID: UUID; let sentinelInstanceID: UUID; let asOf: String
        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version", runID = "run_id", baseURL = "base_url", bearerToken = "bearer_token"
            case workDirectory = "work_directory", rootID = "root_id", branchID = "branch_id", requiredLeafID = "required_leaf_id"
            case optionalLeafID = "optional_leaf_id", inboxLeafID = "inbox_leaf_id", blockedLeafID = "blocked_leaf_id"
            case occurrenceID = "occurrence_id", sentinelOccurrenceID = "sentinel_occurrence_id", instanceID = "instance_id"
            case sentinelInstanceID = "sentinel_instance_id", asOf = "as_of"
        }
    }
    private struct Baseline: Codable {
        let schemaVersion: Int; let runID: UUID; let journal: RoutineOccurrenceJournal
        let initial: RoutineOccurrenceAggregate; let sentinel: RoutineOccurrenceAggregate; let canonicalProjection: Data
    }
    @MainActor
    private struct Context {
        let config: Config; let phase: Phase; let directory: URL; let asOf: Date
        let persistence: EncryptedPlannerPersistence; let session: URLSession; let client: DayWeaveAPIClient
        init() throws {
            try Task.checkCancellation()
            let environment = ProcessInfo.processInfo.environment
            guard let raw = environment["DAYWEAVE_NATIVE_ROUTINE_CONFIG"], !raw.isEmpty,
                  let rawPhase = environment["DAYWEAVE_NATIVE_ROUTINE_PHASE"], let phase = Phase(rawValue: rawPhase) else {
                throw NativeRoutineHarnessError.invalidConfiguration
            }
            let input = URL(fileURLWithPath: raw).standardizedFileURL
            try Self.requirePrivate(input, directory: false, maximumBytes: 32 * 1_024)
            let file = input.resolvingSymlinksInPath(), root = file.deletingLastPathComponent()
            guard file.lastPathComponent == "config.json",
                  root.deletingLastPathComponent().path == URL(fileURLWithPath: "/tmp", isDirectory: true).resolvingSymlinksInPath().path,
                  root.lastPathComponent.hasPrefix("dayweave-native-routine.") else { throw NativeRoutineHarnessError.unsafeArtifact }
            try Self.requirePrivate(root, directory: true)
            let bytes = try Data(contentsOf: file)
            guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: bytes),
                  let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any], Set(object.keys) == [
                    "schema_version", "run_id", "base_url", "bearer_token", "work_directory", "root_id", "branch_id",
                    "required_leaf_id", "optional_leaf_id", "inbox_leaf_id", "blocked_leaf_id", "occurrence_id",
                    "sentinel_occurrence_id", "instance_id", "sentinel_instance_id", "as_of"
                  ] else { throw NativeRoutineHarnessError.invalidConfiguration }
            let config = try JSONDecoder().decode(Config.self, from: bytes)
            let ids = [config.rootID, config.branchID, config.requiredLeafID, config.optionalLeafID,
                config.inboxLeafID, config.blockedLeafID, config.occurrenceID, config.sentinelOccurrenceID,
                config.instanceID, config.sentinelInstanceID]
            let dateFormatter = ISO8601DateFormatter()
            guard config.schemaVersion == 1, config.runID != RoutineOccurrenceValidation.nilID,
                  Set(ids).count == ids.count, !ids.contains(RoutineOccurrenceValidation.nilID),
                  dayWeaveIsRFC4122VersionFiveUUID(config.occurrenceID), dayWeaveIsRFC4122VersionFiveUUID(config.sentinelOccurrenceID),
                  let asOf = dateFormatter.date(from: config.asOf), dateFormatter.string(from: asOf) == config.asOf,
                  config.asOf.hasSuffix("Z"), abs(asOf.timeIntervalSinceNow) <= 300,
                  URL(fileURLWithPath: config.workDirectory, isDirectory: true).standardizedFileURL.resolvingSymlinksInPath().path == root.path,
                  config.bearerToken.hasPrefix("dw_da1_"), config.bearerToken.utf8.count == 50,
                  config.bearerToken.utf8.dropFirst(7).allSatisfy({ (65...90).contains($0) || (97...122).contains($0)
                      || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
                  let endpoint = URLComponents(string: config.baseURL), endpoint.scheme == "http", endpoint.host == "127.0.0.1",
                  endpoint.port.map({ (1...65_535).contains($0) }) == true, endpoint.path == "/",
                  endpoint.user == nil, endpoint.password == nil, endpoint.query == nil, endpoint.fragment == nil else {
                throw NativeRoutineHarnessError.invalidConfiguration
            }
            let directory = root.appendingPathComponent("macos", isDirectory: true)
            let keyURL = directory.appendingPathComponent("snapshot-key.bin"), snapshotURL = directory.appendingPathComponent("planner.encrypted")
            if phase == .prepare {
                guard !FileManager.default.fileExists(atPath: directory.path) else { throw NativeRoutineHarnessError.unsafeArtifact }
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
                let key = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
                try key.write(to: keyURL, options: .withoutOverwriting)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyURL.path)
            } else { try Self.requirePrivate(snapshotURL, directory: false, maximumBytes: 8 * 1_024 * 1_024) }
            try Self.requirePrivate(directory, directory: true)
            try Self.requirePrivate(keyURL, directory: false, maximumBytes: 32)
            let key = try Data(contentsOf: keyURL)
            guard key.count == 32, !FileManager.default.fileExists(atPath: directory.appendingPathComponent("\(phase.rawValue).json").path) else {
                throw NativeRoutineHarnessError.unsafeArtifact
            }
            let configuration = NativeRoutinePublicationProtocol.ephemeralConfiguration()
            configuration.protocolClasses = [NativeRoutinePublicationProtocol.self]
            let session = URLSession(configuration: configuration)
            self.config = config; self.phase = phase; self.directory = directory; self.asOf = asOf; self.session = session
            persistence = EncryptedPlannerPersistence(fileURL: snapshotURL, key: try PlannerEncryptionKey(data: key))
            client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(config.baseURL), session: session, bearerToken: config.bearerToken)
        }
        func saveBaseline(_ baseline: Baseline) throws {
            try writePrivate(baseline.journal.requestBody, named: "operation-a.json")
            let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
            try writePrivate(encoder.encode(baseline), named: "operation-a-baseline.json")
        }
        func loadBaseline() throws -> Baseline {
            let bytes = try readPrivate("operation-a-baseline.json", maximumBytes: 128 * 1_024)
            guard StrictJSONObjectKeyScanner.hasUniqueKeys(in: bytes) else { throw NativeRoutineHarnessError.unsafeArtifact }
            let baseline = try JSONDecoder().decode(Baseline.self, from: bytes)
            let body = try readPrivate("operation-a.json", maximumBytes: 16 * 1_024)
            guard baseline.schemaVersion == 1, baseline.runID == config.runID, baseline.journal.isValid,
                  baseline.journal.configurationIdentifier == client.configurationIdentifier,
                  baseline.journal.requestBody == body, !baseline.journal.hasBeenSubmitted, baseline.journal.noEffectCode == nil,
                  baseline.journal.instanceID == config.instanceID, baseline.journal.memberID == config.requiredLeafID,
                  baseline.journal.command.action == .setOutcome(status: .completed) else { throw NativeRoutineHarnessError.unsafeArtifact }
            return baseline
        }
        static func itemProjection(_ planner: PlannerStore) throws -> Data {
            let rows: [[String: Any]] = planner.canonicalItems.sorted { $0.id.uuidString < $1.id.uuidString }.map { item in [
                "id": item.id.uuidString.lowercased(), "revision": item.revision, "status": item.status.wireValue,
                "parent_id": item.parentID?.uuidString.lowercased() as Any? ?? NSNull(),
                "blocked_reason_kind": item.blockedReasonKind?.wireValue as Any? ?? NSNull(),
                "blocked_by_item_id": item.blockedByItemID?.uuidString.lowercased() as Any? ?? NSNull(),
                "blocked_reason": item.blockedReason as Any? ?? NSNull()
            ] }
            return try JSONSerialization.data(withJSONObject: rows, options: [.sortedKeys])
        }
        func writeMarker(planner: PlannerStore, occurrence: RoutineOccurrenceAggregate, sentinel: RoutineOccurrenceAggregate) throws {
            let ledger = planner.routineOccurrenceState
            let publicationID = planner.pendingSchedulePublication?.preparedRequest.request.idempotencyKey
                ?? NativeRoutinePublicationProtocol.capture.records().last?.operationID
            let marker: [String: Any] = [
                "schema_version": 1, "run_id": config.runID.uuidString.lowercased(), "phase": phase.rawValue, "status": "passed",
                "pending_count": ledger.journals.count, "submitted_count": ledger.journals.filter(\.hasBeenSubmitted).count,
                "receipt_target_count": ledger.minimumCatchUpRevisions.count,
                "needs_remote_schedule_catch_up": ledger.needsRemoteScheduleCatchUp,
                "has_pending_publication": planner.pendingSchedulePublication != nil,
                "terminal_cursor": ledger.terminalDeltaCursor as Any? ?? NSNull(),
                "items": try JSONSerialization.jsonObject(with: Self.itemProjection(planner)),
                "occurrence": Self.lowercaseUUIDs(try JSONSerialization.jsonObject(with: JSONEncoder().encode(occurrence))),
                "sentinel": Self.lowercaseUUIDs(try JSONSerialization.jsonObject(with: JSONEncoder().encode(sentinel))),
                "publication_operation_id": publicationID?.uuidString.lowercased() as Any? ?? NSNull(),
                "publication_revision_id": planner.publishedScheduleProof?.revisionID.uuidString.lowercased() as Any? ?? NSNull()
            ]
            try writePrivate(JSONSerialization.data(withJSONObject: marker, options: [.sortedKeys]), named: "\(phase.rawValue).json")
        }
        func readPrivate(_ name: String, maximumBytes: Int) throws -> Data {
            guard !name.contains("/"), name != ".", name != ".." else { throw NativeRoutineHarnessError.unsafeArtifact }
            let url = directory.appendingPathComponent(name)
            try Self.requirePrivate(url, directory: false, maximumBytes: maximumBytes)
            return try Data(contentsOf: url)
        }
        func writePrivate(_ data: Data, named name: String) throws {
            try Task.checkCancellation()
            guard !name.contains("/"), name != ".", name != "..", !data.isEmpty, data.count <= 1_024 * 1_024 else {
                throw NativeRoutineHarnessError.unsafeArtifact
            }
            try Self.requirePrivate(directory, directory: true)
            let target = directory.appendingPathComponent(name)
            try data.write(to: target, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
            try Self.requirePrivate(target, directory: false, maximumBytes: 1_024 * 1_024)
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
                throw NativeRoutineHarnessError.unsafeArtifact
            }
        }
    }
}

private extension Array { var nativeRoutineOnly: Element? { count == 1 ? first : nil } }
private enum NativeRoutineHarnessError: Error { case invalidConfiguration, unsafeArtifact, unexpectedMutation }
private struct NativeRoutineConfigurationStore: SuggestionAPIConfigurationStoring {
    let baseURL: String
    func loadBaseURL() -> String? { baseURL }
    func saveBaseURL(_ value: String) {}
}
private struct NativeRoutineCredentialStore: BearerTokenStoring {
    let token: String; let origin: String
    func loadCredential() throws -> OriginBoundBearerCredential? { .init(token: token, origin: origin) }
    func saveCredential(_ credential: OriginBoundBearerCredential) throws { throw NativeRoutineHarnessError.unexpectedMutation }
    func deleteCredential() throws { throw NativeRoutineHarnessError.unexpectedMutation }
}
private final class NativeRoutineForbiddenComposer: LocalScheduleComposing, @unchecked Sendable {
    private let lock = NSLock(); private var count = 0
    func compose(canonicalItems: [DayWeaveCanonicalItem], schedule: DayWeaveSchedulePreviewRequest) async throws -> LocalScheduleComposition {
        lock.withLock { count += 1 }
        throw NativeRoutineHarnessError.unexpectedMutation
    }
    func calls() -> Int { lock.withLock { count } }
}
private actor NativeRoutineRecordingTransport: RoutineOccurrenceTransport {
    nonisolated let configurationIdentifier: String
    private let client: DayWeaveAPIClient
    private let dropSuccessfulReply: Bool; private let failPages: Bool
    private let beforePut: @MainActor @Sendable (UUID, UUID, Data) -> Bool
    private var sent: [Data] = []; private var fetched: [UUID] = []; private var receipt: RoutineOccurrenceMutation?
    init(client: DayWeaveAPIClient, dropSuccessfulReply: Bool, failPages: Bool,
         beforePut: @escaping @MainActor @Sendable (UUID, UUID, Data) -> Bool) {
        self.client = client; self.dropSuccessfulReply = dropSuccessfulReply; self.failPages = failPages; self.beforePut = beforePut
        configurationIdentifier = client.configurationIdentifier
    }
    func lookupRoutineOccurrence(seriesItemID: UUID, occurrenceID: UUID) async throws -> RoutineOccurrenceSnapshot {
        try await client.lookupRoutineOccurrence(seriesItemID: seriesItemID, occurrenceID: occurrenceID)
    }
    func routineOccurrence(instanceID: UUID) async throws -> RoutineOccurrenceSnapshot {
        fetched.append(instanceID); return try await client.routineOccurrence(instanceID: instanceID)
    }
    func routineOccurrences(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage {
        if failPages { throw RoutineOccurrenceError.unavailable }
        return try await client.routineOccurrences(cursor: cursor, limit: limit)
    }
    func routineOccurrenceDelta(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage {
        if failPages { throw RoutineOccurrenceError.unavailable }
        return try await client.routineOccurrenceDelta(cursor: cursor, limit: limit)
    }
    func putRoutineOccurrenceMember(instanceID: UUID, memberID: UUID, requestBody: Data) async throws -> RoutineOccurrenceMutation {
        try Task.checkCancellation()
        guard await beforePut(instanceID, memberID, requestBody) else { throw NativeRoutineHarnessError.unexpectedMutation }
        sent.append(requestBody)
        let result = try await client.putRoutineOccurrenceMember(instanceID: instanceID, memberID: memberID, requestBody: requestBody)
        receipt = result
        if dropSuccessfulReply { throw RoutineOccurrenceError.unavailable }
        return result
    }
    func requests() -> [Data] { sent }
    func reads() -> [UUID] { fetched }
    func lastReceipt() -> RoutineOccurrenceMutation? { receipt }
}

private struct NativeRoutinePublicationRecord: Sendable {
    let body: Data; let responseBody: Data; let statusCode: Int; let dropped: Bool
    var operationID: UUID? {
        guard let value = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let id = value["idempotency_key"] as? String else { return nil }
        return UUID(uuidString: id)
    }
    var revisionID: UUID? { Self.revisionID(in: responseBody) }
    static func revisionID(in body: Data) -> UUID? {
        guard let value = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let revision = value["revision"] as? [String: Any], let id = revision["id"] as? String else { return nil }
        return UUID(uuidString: id)
    }
    var replayed: Bool? {
        (try? JSONSerialization.jsonObject(with: responseBody) as? [String: Any])?["replayed"] as? Bool
    }
}

/// Only this test session installs the protocol; it forwards the original
/// request to real loopback HTTP. No production client or auth hook is changed.
private final class NativeRoutinePublicationProtocol: URLProtocol, @unchecked Sendable {
    final class Capture: @unchecked Sendable {
        private let lock = NSLock()
        private var baseURL: String?; private var authorization: String?; private var dropFirstSuccess = false
        private var values: [NativeRoutinePublicationRecord] = []
        private var beforeForward: (@MainActor @Sendable (Data) -> Bool)?
        func configure(baseURL: String, authorization: String, dropFirstSuccess: Bool,
                       beforeForward: @escaping @MainActor @Sendable (Data) -> Bool) {
            lock.withLock {
                self.baseURL = baseURL; self.authorization = authorization; self.dropFirstSuccess = dropFirstSuccess
                self.beforeForward = beforeForward; values = []
            }
        }
        func reset() { lock.withLock { baseURL = nil; authorization = nil; beforeForward = nil; values = []; dropFirstSuccess = false } }
        func validator(for request: URLRequest) -> (@MainActor @Sendable (Data) -> Bool)? {
            lock.withLock {
                guard request.url?.absoluteString == baseURL.map({ $0 + "v1/schedule/publish" }),
                      request.httpMethod == "POST", request.value(forHTTPHeaderField: "Authorization") == authorization else { return nil }
                return beforeForward
            }
        }
        func record(body: Data, response: Data, status: Int) -> Bool {
            lock.withLock {
                let drop = status == 200 && dropFirstSuccess
                if drop { dropFirstSuccess = false }
                values.append(.init(body: body, responseBody: response, statusCode: status, dropped: drop))
                return drop
            }
        }
        func records() -> [NativeRoutinePublicationRecord] { lock.withLock { values } }
    }
    static let capture = Capture()
    private let lock = NSLock(); private var forwardingTask: Task<Void, Never>?; private var stopped = false
    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.scheme == "http" && request.url?.host == "127.0.0.1" && request.url?.path == "/v1/schedule/publish"
    }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        // URLProtocol may call stopLoading concurrently. Capture its explicitly
        // Sendable, lock-protected owner instead of transferring an inferred
        // task-isolated closure out of this Foundation callback.
        let work = Task { @Sendable [self] in
            let session = URLSession(configuration: Self.ephemeralConfiguration())
            defer { session.invalidateAndCancel() }
            do {
                guard let validate = Self.capture.validator(for: request) else { throw NativeRoutineHarnessError.invalidConfiguration }
                var forwarded = request
                let body = try Self.body(of: request)
                forwarded.httpBodyStream = nil; forwarded.httpBody = body
                guard await validate(body) else { throw NativeRoutineHarnessError.unexpectedMutation }
                let (data, response) = try await session.data(for: forwarded, delegate: NativeRoutineRejectRedirect())
                try Task.checkCancellation()
                guard let http = response as? HTTPURLResponse, data.count <= 8 * 1_024 * 1_024 else {
                    throw NativeRoutineHarnessError.invalidConfiguration
                }
                let drop = Self.capture.record(body: body, response: data, status: http.statusCode)
                guard !lock.withLock({ stopped }) else { return }
                if drop { client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost)); return }
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            } catch {
                guard !lock.withLock({ stopped }) else { return }
                // Private response/request content never becomes a diagnostic.
                client?.urlProtocol(self, didFailWithError: URLError(.cannotLoadFromNetwork))
            }
        }
        lock.withLock { if stopped { work.cancel() } else { forwardingTask = work } }
    }
    override func stopLoading() { lock.withLock { stopped = true; forwardingTask?.cancel(); forwardingTask = nil } }
    static func ephemeralConfiguration() -> URLSessionConfiguration {
        let value = URLSessionConfiguration.ephemeral
        value.connectionProxyDictionary = [:]; value.urlCache = nil; value.httpCookieStorage = nil
        value.httpShouldSetCookies = false; value.urlCredentialStorage = nil
        value.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        value.timeoutIntervalForRequest = 15; value.timeoutIntervalForResource = 60
        return value
    }
    private static func body(of request: URLRequest) throws -> Data {
        if let body = request.httpBody, !body.isEmpty, body.count <= 1_024 * 1_024 { return body }
        guard let stream = request.httpBodyStream else { throw NativeRoutineHarnessError.invalidConfiguration }
        stream.open(); defer { stream.close() }
        var result = Data(), buffer = [UInt8](repeating: 0, count: 16 * 1_024)
        while true {
            let count = stream.read(&buffer, maxLength: buffer.count)
            guard count >= 0 else { throw NativeRoutineHarnessError.invalidConfiguration }
            if count == 0 { break }
            guard result.count + count <= 1_024 * 1_024 else { throw NativeRoutineHarnessError.invalidConfiguration }
            result.append(contentsOf: buffer.prefix(count))
        }
        guard !result.isEmpty else { throw NativeRoutineHarnessError.invalidConfiguration }
        return result
    }
}
private final class NativeRoutineRejectRedirect: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
#endif
