import Foundation
import CryptoKit
import AppKit
import SwiftUI
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Modern structural authoring")
struct CanonicalStructuralAuthoringTests {
    private static let structuralKeys = ["deadline_kind", "deadline_date", "deadline_strength", "deadline_soft_weight", "has_own_effort"]

    @Test("shared raw creates preserve modern request and editor semantics")
    func sharedValidRequests() throws {
        let fixture = try Self.fixture()
        let cases = try #require(fixture["cases"] as? [[String: Any]])
        #expect(cases.count == 11)
        for testCase in cases {
            let name = try #require(testCase["name"] as? String)
            let raw = try #require(testCase["create"] as? [String: Any])
            let itemID = try #require((raw["id"] as? String).flatMap(UUID.init(uuidString:)))
            let draft = try Self.decoder().decode(DayWeaveCanonicalItemDraft.self,
                from: JSONSerialization.data(withJSONObject: raw))
            #expect(draft.validationIssue(itemID: itemID) == nil, "\(name): \(draft.validationIssue(itemID: itemID) ?? "")")
            let state = CanonicalItemEditorState(itemID: itemID, draft: draft)
            if draft.kind == .event { #expect(state.readOnlyDiagnostic != nil) }
            else { #expect(state.readOnlyDiagnostic == nil, "\(name): \(state.readOnlyDiagnostic ?? "")") }
            #expect(state.draft == draft.normalized, "\(name)")
            let request = try Self.request(draft, itemID: itemID)
            for key in Self.structuralKeys { #expect(request[key] != nil, "\(name): missing \(key)") }
            var expected = raw
            if let constraints = testCase["normalized_flexible_constraints"] {
                expected["flexible_constraints"] = constraints
            }
            // Pre-existing optional null omissions are not new structural
            // authority. Compare the complete semantic body after filling only
            // fixture nulls, never filling an absent modern structural key.
            var comparable = request
            for (key, value) in expected where value is NSNull && comparable[key] == nil {
                comparable[key] = NSNull()
            }
            expected["id"] = itemID.uuidString
            for key in ["deadline_at", "earliest_start_at"] {
                if let timestamp = expected[key] as? String,
                   let instant = CanonicalRFC3339Instant(timestamp) {
                    expected[key] = instant.canonicalUTCString
                }
            }
            #expect(NSDictionary(dictionary: comparable).isEqual(to: expected), "\(name)")
            let response = try Self.item(raw, normalizedConstraints: testCase["normalized_flexible_constraints"])
            #expect(draft.matches(response), "\(name)")
            #expect(response.supportsCanonicalAuthoringReplacement == (draft.kind != .event), "\(name)")
            if response.kind == .project { #expect(!response.supportsLosslessReplacement) }
        }
    }

    @Test("shared malformed deadline effort and recurrence forms cannot be newly authored")
    func sharedInvalidRequests() throws {
        let cases = try #require(Self.fixture()["invalid_cases"] as? [[String: Any]])
        #expect(cases.count == 11)
        for testCase in cases {
            let raw = try #require(testCase["create"] as? [String: Any])
            let itemID = try #require((raw["id"] as? String).flatMap(UUID.init(uuidString:)))
            do {
                let draft = try Self.decoder().decode(DayWeaveCanonicalItemDraft.self,
                    from: JSONSerialization.data(withJSONObject: raw))
                #expect(draft.validationIssue(itemID: itemID) != nil, "\(testCase["name"] ?? "")")
            } catch { /* Strict decoding may reject malformed scalar shapes. */ }
        }
    }

    @Test("date-only successor rejects midnight gaps and chooses the earlier fold")
    func strictCivilBoundary() throws {
        #expect(CanonicalDateDeadline.boundary("2011-12-29", timezoneName: "Pacific/Apia") == nil)
        #expect(CanonicalDateDeadline.boundary("2018-11-03", timezoneName: "America/Sao_Paulo") == nil)
        #expect(CanonicalDateDeadline.boundary("2020-10-31", timezoneName: "America/Havana")
            == CanonicalRFC3339Instant("2020-11-01T04:00:00Z")?.dateAtMicrosecondPrecision)
        #expect(CanonicalDateDeadline.boundary("2024-02-29", timezoneName: "UTC")
            == CanonicalRFC3339Instant("2024-03-01T00:00:00Z")?.dateAtMicrosecondPrecision)
        for invalid in ["0000-01-01", "2023-02-29", "1900-02-29", "9999-12-31", "2026-2-01"] {
            #expect(CanonicalDateDeadline.boundary(invalid, timezoneName: "UTC") == nil, "\(invalid)")
        }
        #expect(CanonicalDateDeadline.boundary("0001-01-01", timezoneName: "UTC") != nil)
        #expect(CanonicalDateDeadline.boundary("1582-10-04", timezoneName: "UTC")
            == CanonicalRFC3339Instant("1582-10-05T00:00:00Z")?.dateAtMicrosecondPrecision)
        for date in ["0001-01-01", "1582-10-05", "2024-02-29", "9999-12-30"] {
            #expect(CanonicalDateDeadline.string(try #require(CanonicalDateDeadline.pickerDate(date))) == date)
        }
        #expect(CanonicalDateDeadline.boundary("9999-12-30", timezoneName: "UTC") != nil)
        #expect(CanonicalDateDeadline.boundary("2026-01-01", timezoneName: "PST") == nil)
        var draft = DayWeaveCanonicalItemDraft(kind: .project, title: "Date boundary", timezoneName: "UTC",
            deadlineKind: .date, deadlineDate: "2026-10-01", deadlineStrength: .hard,
            earliestStartAt: CanonicalRFC3339Instant("2026-10-01T22:00:00Z")?.dateAtMicrosecondPrecision)
        #expect(draft.validationIssue(itemID: UUID()) == nil)
        draft.timezoneName = "Europe/Istanbul"
        #expect(draft.validationIssue(itemID: UUID()) != nil)
    }

    @Test("modern date and soft effort edits remain typed through timezone and title edits")
    func editorPreservesModernFields() throws {
        let draft = DayWeaveCanonicalItemDraft(kind: .project, title: "Structural project", timezoneName: "UTC",
            durationSeconds: 1800, deadlineKind: .date, deadlineDate: "2026-10-01",
            deadlineStrength: .soft, deadlineSoftWeight: 0, hasOwnEffort: true)
        var state = CanonicalItemEditorState(itemID: UUID(), draft: draft)
        state.title = "Renamed project"
        state.setTimezoneName("Pacific/Auckland")
        #expect(state.validationIssue == nil)
        #expect(state.draft.deadlineDate == "2026-10-01")
        #expect(state.draft.deadlineAt == nil)
        #expect(state.draft.deadlineSoftWeight == 0)
        #expect(state.draft.hasOwnEffort)
        state.hasOwnEffort = false
        #expect(!state.draft.hasOwnEffort)
        #expect(state.draft.validationIssue(itemID: UUID()) == nil)
        let soft = DayWeaveCanonicalItemDraft(title: "Soft time", timezoneName: "UTC",
            deadlineKind: .dateTime, deadlineStrength: .soft, deadlineSoftWeight: 1_000_000,
            deadlineAt: CanonicalRFC3339Instant("2026-10-01T10:00:00.123456Z")?.dateAtMicrosecondPrecision)
        var changed = soft
        changed.deadlineAt = changed.deadlineAt?.addingTimeInterval(60)
        #expect(changed.deadlineKind == .dateTime && changed.deadlineStrength == .soft)
        #expect(changed.deadlineSoftWeight == 1_000_000)
    }

    @Test("schema24 replay obtains legacy marker without changing submitted body")
    func legacyReplayAndStrictCurrentShape() throws {
        let itemID = UUID()
        let legacyDraft = DayWeaveCanonicalItemDraft(kind: .project, title: "Historical recurring project",
            timezoneName: "UTC", durationSeconds: 900,
            deadlineAt: CanonicalRFC3339Instant("2026-10-01T12:00:00Z")?.dateAtMicrosecondPrecision,
            recurrence: .object(["type": .string("daily"), "times_per_day": .number(JSONNumber(UInt64(1)))]))
        let original = DayWeavePendingCanonicalAuthoringMutation(itemID: itemID, operation: .create,
            draft: legacyDraft, structuralRequestShapeVersion: 1,
            configurationIdentifier: "synthetic-binding", hasBeenSubmitted: true)
        #expect(original.isValid)
        #expect(legacyDraft.validationIssue(itemID: itemID) != nil)
        #expect(CanonicalItemEditorState(itemID: itemID, draft: legacyDraft).readOnlyDiagnostic != nil)
        let before = try Self.request(legacyDraft, itemID: itemID, version: 1)
        var old = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(original)) as? [String: Any])
        old.removeValue(forKey: "structuralRequestShapeVersion")
        var oldDraft = try #require(old["draft"] as? [String: Any])
        for key in Self.structuralKeys { oldDraft.removeValue(forKey: key) }
        old["draft"] = oldDraft
        let decoder = JSONDecoder()
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 24
        let decoded = try decoder.decode(DayWeavePendingCanonicalAuthoringMutation.self,
            from: JSONSerialization.data(withJSONObject: old))
        #expect(decoded == original)
        #expect(decoded.isValid)
        let rewritten = try JSONEncoder().encode(decoded)
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 25
        let roundtrip = try decoder.decode(DayWeavePendingCanonicalAuthoringMutation.self, from: rewritten)
        let after = try Self.request(#require(roundtrip.draft), itemID: itemID, version: roundtrip.structuralRequestShapeVersion)
        #expect(NSDictionary(dictionary: before).isEqual(to: after))
        for key in Self.structuralKeys { #expect(after[key] == nil) }
        #expect(throws: (any Error).self) {
            try decoder.decode(DayWeavePendingCanonicalAuthoringMutation.self,
                from: JSONSerialization.data(withJSONObject: old))
        }
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 24
        #expect(throws: (any Error).self) {
            try decoder.decode(DayWeavePendingCanonicalAuthoringMutation.self, from: rewritten)
        }
    }

    @Test("chosen ancestry is complete and attachment eligibility differs from replacement")
    @MainActor
    func parentAncestryAndPresets() throws {
        let raw = try #require((Self.fixture()["cases"] as? [[String: Any]])?.first?["create"] as? [String: Any])
        let rootID = UUID(), parentID = UUID(), childID = UUID()
        var rootRaw = raw
        rootRaw["id"] = rootID.uuidString
        rootRaw["status"] = "completed"
        let root = try Self.item(rootRaw)
        var parentRaw = raw
        parentRaw["id"] = parentID.uuidString
        parentRaw["parent_id"] = rootID.uuidString
        parentRaw["status"] = "blocked"
        parentRaw["is_sensitive"] = true
        let parent = try Self.item(parentRaw)
        let draft = DayWeaveCanonicalItemDraft(title: "Child", timezoneName: "UTC", parentID: parentID)
        let store = PlannerStore(canonicalItems: [root, parent], canonicalConfigurationIdentifier: "synthetic-binding",
            restoreFromPersistence: false)
        #expect(!parent.supportsCanonicalAuthoringReplacement)
        #expect(store.canonicalAuthoringEligibleParentIDs() == [parentID])
        #expect(store.canonicalAuthoringDraftHierarchyIsCurrent(draft, itemID: childID, requiresCommittedParent: false))
        let route = try #require(CanonicalHierarchyAuthoring.route(kind: .task, parentID: parentID, store: store, itemID: childID))
        let preset = try #require(route.mode.initialDraft)
        #expect(preset.kind == .task && preset.status == .inbox && preset.parentID == parentID)
        #expect(preset.durationSeconds == nil && preset.flexibleConstraints == .object([:]))
        #expect(route.mode.preservesSensitivePresentation)
        let missing = PlannerStore(canonicalItems: [parent], canonicalConfigurationIdentifier: "synthetic-binding",
            restoreFromPersistence: false)
        #expect(missing.canonicalAuthoringEligibleParentIDs().isEmpty)
        #expect(!missing.canonicalAuthoringDraftHierarchyIsCurrent(draft, itemID: childID, requiresCommittedParent: false))
        #expect(missing.canonicalAuthoringDraftHierarchyIsCurrent(.init(title: "Detached", timezoneName: "UTC"),
            itemID: childID, requiresCommittedParent: false))
        let unbound = PlannerStore(canonicalItems: [root, parent], restoreFromPersistence: false)
        #expect(unbound.canonicalAuthoringEligibleParentIDs().isEmpty)
        #expect(CanonicalHierarchyAuthoring.route(kind: .task, parentID: parentID, store: unbound) == nil)
    }

    @Test("queued parent creation is allowed locally but waits for committed ancestry")
    @MainActor
    func pendingParentAndDepth() throws {
        let count = 5000
        let ids = (0..<count).map { _ in UUID() }
        var pending = ids.enumerated().map { index, id in
            DayWeavePendingCanonicalAuthoringMutation(itemID: id, operation: .create,
                draft: .init(kind: .project, title: "Synthetic \(index)", timezoneName: "UTC",
                    parentID: index == 0 ? nil : ids[index - 1]))
        }
        let store = PlannerStore(pendingCanonicalAuthoringMutations: pending, restoreFromPersistence: false)
        let cache = CanonicalHierarchyAuthoringCache()
        #expect(cache.eligibleParentIDs(for: store).count == count)
        for _ in 0..<20 { #expect(cache.eligibleParentIDs(for: store).count == count) }
        #expect(cache.buildCount == 1)
        let child = DayWeaveCanonicalItemDraft(title: "Child", timezoneName: "UTC", parentID: ids.last)
        #expect(store.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: false))
        #expect(!store.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: true))
        pending[0].hasBeenSubmitted = true
        let frozen = PlannerStore(pendingCanonicalAuthoringMutations: pending, restoreFromPersistence: false)
        #expect(cache.eligibleParentIDs(for: frozen).isEmpty)
        #expect(cache.buildCount == 2)
        #expect(!frozen.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: false))
    }

    @Test("coherent local parent replacements allow capture but replay-only invalid creates do not")
    @MainActor
    func pendingReplacementParentCoherence() throws {
        let raw = try #require((Self.fixture()["cases"] as? [[String: Any]])?.first?["create"] as? [String: Any])
        let parent = try Self.item(raw)
        var draft = DayWeaveCanonicalItemDraft(item: parent)
        draft.title = "Reviewed local project edit"
        var mutation = DayWeavePendingCanonicalAuthoringMutation(itemID: parent.id, operation: .replace,
            draft: draft, expectedRevision: parent.revision, baseItem: parent)
        let child = DayWeaveCanonicalItemDraft(title: "Child", timezoneName: "UTC", parentID: parent.id)
        let store = PlannerStore(canonicalItems: [parent], canonicalConfigurationIdentifier: "synthetic-binding",
            pendingCanonicalAuthoringMutations: [mutation], restoreFromPersistence: false)
        #expect(store.canonicalAuthoringEligibleParentIDs() == [parent.id])
        #expect(store.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: false))
        #expect(!store.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: true))
        mutation.configurationIdentifier = "synthetic-binding"
        let bound = PlannerStore(canonicalItems: [parent], canonicalConfigurationIdentifier: "synthetic-binding",
            pendingCanonicalAuthoringMutations: [mutation], restoreFromPersistence: false)
        #expect(bound.canonicalAuthoringEligibleParentIDs().isEmpty)
        #expect(!bound.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: false))
        draft.recurrence = .object(["type": .string("daily"), "times_per_day": .number(JSONNumber(UInt64(1)))])
        let historical = DayWeavePendingCanonicalAuthoringMutation(itemID: parent.id, operation: .create,
            draft: draft, structuralRequestShapeVersion: 1)
        #expect(historical.isValid)
        let retained = PlannerStore(pendingCanonicalAuthoringMutations: [historical], restoreFromPersistence: false)
        #expect(retained.pendingCanonicalAuthoringMutations == [historical])
        #expect(retained.canonicalAuthoringEligibleParentIDs().isEmpty)
        #expect(!retained.canonicalAuthoringDraftHierarchyIsCurrent(child, itemID: UUID(), requiresCommittedParent: false))
    }

    @Test("encrypted schema24 snapshot rewrites exact legacy requests and preserves publication high-water")
    func encryptedSnapshotMigration() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveStructural-\(UUID())")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let keyData = Data(repeating: 51, count: 32)
        let fileURL = directory.appendingPathComponent("synthetic.encrypted")
        let persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: try PlannerEncryptionKey(data: keyData))
        let mutation = DayWeavePendingCanonicalAuthoringMutation(itemID: UUID(), operation: .create,
            draft: .init(kind: .project, title: "STRUCTURAL-PRIVATE-CANARY", timezoneName: "UTC"),
            createdAt: Date(timeIntervalSince1970: 1_800_000_000),
            structuralRequestShapeVersion: 1, configurationIdentifier: "synthetic-binding", hasBeenSubmitted: true)
        let snapshot = PlannerSnapshot(destination: .projects, selectedBlockID: nil, blocks: [], suggestions: [],
            assistantMessages: [], lastScheduleMessage: "Synthetic", protectedFreeMinutes: 90, freezeHours: 2,
            showCompleted: false, canonicalConfigurationIdentifier: "synthetic-binding",
            publishedScheduleLatestHintRevision: 77, pendingCanonicalAuthoringMutations: [mutation])
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var raw = try #require(JSONSerialization.jsonObject(with: encoder.encode(snapshot)) as? [String: Any])
        raw["schemaVersion"] = 24
        // This historical fixture predates independent progress, even though built with today's initializer.
        raw.removeValue(forKey: "itemProgressState")
        var entries = try #require(raw["pendingCanonicalAuthoringMutations"] as? [[String: Any]])
        entries[0].removeValue(forKey: "structuralRequestShapeVersion")
        var draft = try #require(entries[0]["draft"] as? [String: Any])
        for key in Self.structuralKeys { draft.removeValue(forKey: key) }
        entries[0]["draft"] = draft
        raw["pendingCanonicalAuthoringMutations"] = entries
        let sealed = try AES.GCM.seal(JSONSerialization.data(withJSONObject: raw),
            using: SymmetricKey(data: keyData), authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8))
        let envelope: [String: Any] = ["magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT", "formatVersion": 1,
            "cipher": "AES.GCM.256", "sealedSnapshot": try #require(sealed.combined).base64EncodedString()]
        try JSONSerialization.data(withJSONObject: envelope).write(to: fileURL)
        let migrated = try #require(try persistence.load())
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.pendingCanonicalAuthoringMutations == [mutation])
        #expect(migrated.publishedScheduleLatestHintRevision == 77)
        #expect(try persistence.load() == migrated)
        #expect(try Data(contentsOf: fileURL).range(of: Data("STRUCTURAL-PRIVATE-CANARY".utf8)) == nil)
    }

    @Test("reset and retention never silently upgrade frozen legacy request markers")
    @MainActor
    func resetAndRetentionPreserveRequestCustody() throws {
        let raw = try #require((Self.fixture()["cases"] as? [[String: Any]])?.first?["create"] as? [String: Any])
        let base = try Self.item(raw)
        let now = Date(timeIntervalSince1970: 1_900_000_000)
        let trash = DayWeavePendingCanonicalAuthoringMutation(itemID: base.id, operation: .trash,
            expectedRevision: base.revision, baseItem: base, createdAt: now.addingTimeInterval(-40 * 86400),
            structuralRequestShapeVersion: 1, configurationIdentifier: "synthetic-binding", hasBeenSubmitted: true)
        let retained = PlannerStore(canonicalItems: [base], canonicalConfigurationIdentifier: "synthetic-binding",
            pendingCanonicalAuthoringMutations: [trash], restoreFromPersistence: false, now: { now })
        let bounded = try #require(retained.pendingCanonicalAuthoringMutations.first)
        #expect(bounded.baseItem == nil && bounded.id == trash.id)
        #expect(bounded.structuralRequestShapeVersion == 1 && bounded.durationWireShape == trash.durationWireShape)
        for variant in 0..<3 {
            let mutation = DayWeavePendingCanonicalAuthoringMutation(itemID: UUID(), operation: .create,
                draft: .init(kind: .project, title: "Legacy draft", timezoneName: "UTC"),
                structuralRequestShapeVersion: 1,
                configurationIdentifier: variant == 1 ? "synthetic-binding" : nil,
                disposition: variant == 2 ? .conflicted : .pending,
                diagnostic: variant == 2 ? "Synthetic conflict" : nil)
            let store = PlannerStore(canonicalConfigurationIdentifier: "synthetic-binding",
                pendingCanonicalAuthoringMutations: [mutation], restoreFromPersistence: false)
            store.resetCanonicalSyncState()
            #expect(store.pendingCanonicalAuthoringMutations == [mutation])
            #expect(store.canonicalConfigurationIdentifier == (variant == 0 ? nil : "synthetic-binding"))
        }
    }

    @Test("pending create and replace rows show typed deadline policy before synchronization")
    func pendingDeadlinePresentation() throws {
        let raw = try #require((Self.fixture()["cases"] as? [[String: Any]])?[1]["create"] as? [String: Any])
        let item = try Self.item(raw)
        for operation in [CanonicalAuthoringOperation.create, .replace] {
            let draft = DayWeaveCanonicalItemDraft(item: item)
            let mutation = DayWeavePendingCanonicalAuthoringMutation(itemID: item.id, operation: operation,
                draft: draft, expectedRevision: operation == .replace ? item.revision : nil,
                baseItem: operation == .replace ? item : nil)
            let row = try #require(CanonicalInboxPresentation.build(activeItems: operation == .replace ? [item] : [],
                pendingMutations: [mutation], trashEntries: []).hierarchyRows.first)
            #expect(row.deadlineKind == .date && row.deadlineDate == "2026-10-01")
            #expect(row.deadlineAt == nil && row.deadlineStrength == .soft && row.deadlineSoftWeight == 0)
            #expect(!row.isReadOnly)
        }
    }

    @Test("ordinary hierarchy creates persist separate modern parent and child journals")
    @MainActor
    func hierarchyJournalIntegration() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveHierarchyAuthoring-\(UUID())")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("synthetic.encrypted"),
            key: try PlannerEncryptionKey(data: Data(repeating: 52, count: 32)))
        let store = PlannerStore(persistence: persistence, restoreFromPersistence: false)
        let projectID = UUID()
        let route = try #require(CanonicalHierarchyAuthoring.route(kind: .project, store: store, itemID: projectID))
        var project = try #require(route.mode.initialDraft)
        project.title = "Synthetic project"
        project.status = .planned
        project.durationSeconds = 1800
        project.hasOwnEffort = true
        project.deadlineKind = .date
        project.deadlineDate = "2026-10-01"
        project.deadlineStrength = .soft
        project.deadlineSoftWeight = 100
        let queued = try store.enqueueCanonicalCreate(itemID: projectID, draft: project)
        #expect(queued.structuralRequestShapeVersion == 2)
        let childRoute = try #require(CanonicalHierarchyAuthoring.route(kind: .task, parentID: projectID, store: store))
        var child = try #require(childRoute.mode.initialDraft)
        child.title = "Synthetic leaf"
        _ = try store.enqueueCanonicalCreate(itemID: childRoute.mode.itemID, draft: child)
        #expect(store.onboardingFirstItemAnchor == nil)
        #expect(!project.createsPlanningDemand(itemID: projectID, hasActiveChildren: true))
        #expect(project.createsPlanningDemand(itemID: projectID, hasActiveChildren: false))
        let restored = PlannerStore(persistence: persistence)
        #expect(restored.pendingCanonicalAuthoringMutations.count == 2)
        #expect(restored.pendingCanonicalAuthoringMutations.allSatisfy { $0.structuralRequestShapeVersion == 2 })
        #expect(restored.canonicalAuthoringMutation(itemID: projectID)?.draft == project.normalized)
        #expect(restored.canonicalAuthoringMutation(itemID: childRoute.mode.itemID)?.draft?.parentID == projectID)
        #expect(restored.onboardingFirstItemAnchor == nil)
        let binding = "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"
        #expect(store.beginCanonicalSync())
        try store.prepareCanonicalSync(configurationIdentifier: binding)
        _ = try store.bindCanonicalAuthoringMutation(queued.id, configurationIdentifier: binding)
        store.endCanonicalSync()
        let before = store.pendingCanonicalAuthoringMutations
        #expect(throws: (any Error).self) { try store.updateCanonicalAuthoringDraft(queued.id, draft: project) }
        #expect(store.pendingCanonicalAuthoringMutations == before)
    }

    @Test("project authoring and hierarchy controls render in isolated native hosts")
    @MainActor
    func syntheticAuthoringRender() throws {
        guard let path = ProcessInfo.processInfo.environment["DAYWEAVE_HIERARCHY_RENDER_DIRECTORY"] else { return }
        let store = PlannerStore(restoreFromPersistence: false)
        let project = DayWeaveCanonicalItemDraft(kind: .project, title: "Build a personal observatory",
            notes: "Synthetic visual fixture. This project has a flexible date target and independent leaf effort.",
            timezoneName: "Europe/Istanbul", durationSeconds: 5400,
            deadlineKind: .date, deadlineDate: "2026-10-01", deadlineStrength: .soft,
            deadlineSoftWeight: 100, hasOwnEffort: true)
        let surface = CanonicalItemEditorView(mode: .createHierarchy(itemID: UUID(), draft: project, sensitiveContext: false),
            profileTimezoneName: "UTC").environmentObject(store)
            .environment(\.colorScheme, .light).frame(width: 780, height: 1080)
            .background(Color(nsColor: .windowBackgroundColor))
        let host = NSHostingView(rootView: surface)
        host.appearance = NSAppearance(named: .aqua)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 1080),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = host
        defer { window.close() }
        host.frame = NSRect(x: 0, y: 0, width: 780, height: 1080)
        host.layoutSubtreeIfNeeded()
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        #expect(bitmap.pixelsWide >= 780 && bitmap.pixelsHigh >= 1080)
        try #require(bitmap.representation(using: .png, properties: [:])).write(to:
            URL(fileURLWithPath: path).appendingPathComponent("macos-project-authoring-native-synthetic.png"))
    }

    private static func fixture() throws -> [String: Any] {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return try #require(JSONSerialization.jsonObject(with: Data(contentsOf:
            root.appendingPathComponent("fixtures/structural-authoring/requests-v1.json"))) as? [String: Any])
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = CanonicalRFC3339Instant(value)?.dateAtMicrosecondPrecision else {
                throw DecodingError.dataCorruptedError(in: container, debugDescription: "Invalid synthetic timestamp")
            }
            return date
        }
        return decoder
    }

    private static func request(_ draft: DayWeaveCanonicalItemDraft, itemID: UUID, version: Int = 2) throws -> [String: Any] {
        let body = DayWeaveNewCanonicalItem(id: itemID,
            fields: draft.requestFields(durationWireShape: .richV2, structuralRequestShapeVersion: version))
        return try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(body)) as? [String: Any])
    }

    private static func item(_ raw: [String: Any], normalizedConstraints: Any? = nil) throws -> DayWeaveCanonicalItem {
        var value = raw
        value["revision"] = 1
        value["is_executable"] = true
        value["created_at"] = "2026-09-01T10:00:00Z"
        value["updated_at"] = "2026-09-01T10:00:00Z"
        value["completed_at"] = value["status"] as? String == "completed" ? "2026-09-01T10:00:00Z" as Any : NSNull()
        value["deleted_at"] = NSNull()
        value["blocked_reason_kind"] = value["status"] as? String == "blocked" ? "manual" as Any : NSNull()
        value["blocked_by_item_id"] = NSNull()
        value["blocked_reason"] = value["status"] as? String == "blocked" ? "Synthetic prerequisite" as Any : NSNull()
        if let normalizedConstraints { value["flexible_constraints"] = normalizedConstraints }
        return try decoder().decode(DayWeaveCanonicalItem.self, from: JSONSerialization.data(withJSONObject: value))
    }
}
#endif
