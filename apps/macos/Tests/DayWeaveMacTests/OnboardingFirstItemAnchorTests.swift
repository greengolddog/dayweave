import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Encrypted onboarding first-item anchor", .serialized)
@MainActor
struct OnboardingFirstItemAnchorTests {
    @Test("planning demand is pure and fails closed")
    func testPlanningDemandPredicateFailsClosed() {
        let itemID = UUID()
        let plannedTask = DayWeaveCanonicalItemDraft(
            kind: .task,
            status: .planned,
            title: "First planned task",
            timezoneName: "UTC",
            durationSeconds: 1_800
        )

        #expect(plannedTask.createsPlanningDemand(itemID: itemID))
        #expect(!plannedTask.createsPlanningDemand(
            itemID: itemID,
            hasActiveChildren: true
        ))

        var parentWithOwnEffort = plannedTask
        parentWithOwnEffort.flexibleConstraints = .object(["has_own_effort": .bool(true)])
        #expect(parentWithOwnEffort.createsPlanningDemand(
            itemID: itemID,
            hasActiveChildren: true
        ))

        var inbox = plannedTask
        inbox.status = .inbox
        #expect(!inbox.createsPlanningDemand(itemID: itemID))

        var missingDuration = plannedTask
        missingDuration.durationSeconds = nil
        #expect(!missingDuration.createsPlanningDemand(itemID: itemID))

        var goal = plannedTask
        goal.kind = .goal
        #expect(!goal.createsPlanningDemand(itemID: itemID))
        goal.flexibleConstraints = .object(["has_own_effort": .bool(true)])
        #expect(goal.createsPlanningDemand(itemID: itemID))

        var invalid = plannedTask
        invalid.title = ""
        #expect(!invalid.createsPlanningDemand(itemID: itemID))
    }

    @Test("canonical container demand still requires explicitly reviewed own effort")
    func testCanonicalPlanningDemandPredicateMatchesDraft() throws {
        var leaf = try Self.canonicalItem(id: UUID(), revision: 1)
        #expect(leaf.createsPlanningDemand)

        var goalLeaf = leaf
        goalLeaf.kind = .goal
        goalLeaf.flexibleConstraints = .object(["has_own_effort": .bool(false)])
        #expect(!goalLeaf.createsPlanningDemand)
        goalLeaf.flexibleConstraints = .object(["has_own_effort": .bool(true)])
        #expect(goalLeaf.createsPlanningDemand)

        let parent = try Self.canonicalItem(
            id: UUID(),
            revision: 2,
            isExecutable: false
        )
        var child = try Self.canonicalItem(id: UUID(), revision: 1)
        child.parentID = parent.id
        #expect(!parent.createsPlanningDemand(canonicalItems: [parent, child]))

        for kind in [
            DayWeaveCanonicalItemKind.task,
            .habit,
            .breakTime,
            .goal,
            .routine,
        ] {
            var independentParent = parent
            independentParent.kind = kind
            independentParent.flexibleConstraints = .object([
                "has_own_effort": .bool(true),
            ])
            #expect(independentParent.createsPlanningDemand(
                canonicalItems: [independentParent, child]
            ))
        }

        var eventParent = parent
        eventParent.kind = .event
        #expect(eventParent.createsPlanningDemand(canonicalItems: [eventParent, child]))

        leaf = try Self.canonicalItem(id: UUID(), revision: 3, isExecutable: false)
        #expect(!leaf.createsPlanningDemand(canonicalItems: [leaf]))
    }

    @Test("prepared create and anchor commit together and promote on exact response")
    func testEncryptedCreateAndPromotionRoundTrip() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "7b100000-0000-4000-8000-000000000001")!
        let draft = Self.plannedDraft(title: "FIRST-ITEM-PRIVATE-CANARY")
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )

        let queued = try store.enqueueOnboardingFirstItemCreate(
            itemID: itemID,
            draft: draft
        )

        #expect(store.onboardingFirstItemAnchor == .init(
            itemID: itemID,
            canonicalRevision: nil
        ))
        #expect(store.pendingCanonicalAuthoringMutations == [queued])
        #expect(!store.hasExactOnboardingFirstPlanProof)
        let encrypted = try Data(contentsOf: context.fileURL)
        #expect(encrypted.range(of: Data(draft.title.utf8)) == nil)
        #expect(encrypted.range(of: Data(itemID.uuidString.lowercased().utf8)) == nil)

        let queuedRestart = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(queuedRestart.onboardingFirstItemAnchor == store.onboardingFirstItemAnchor)
        #expect(queuedRestart.pendingCanonicalAuthoringMutations == [queued])

        let configuration = Self.configurationIdentifier
        #expect(store.beginCanonicalSync())
        try store.prepareCanonicalSync(configurationIdentifier: configuration)
        _ = try store.bindCanonicalAuthoringMutation(
            queued.id,
            configurationIdentifier: configuration
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(queued.id)
        let response = try Self.canonicalItem(
            id: itemID,
            revision: 7,
            title: draft.title
        )
        try store.applyCanonicalAuthoringResponse(queued.id, item: response)
        store.endCanonicalSync()

        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(store.onboardingFirstItemAnchor == .init(
            itemID: itemID,
            canonicalRevision: 7
        ))
        let canonicalRestart = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(canonicalRestart.onboardingFirstItemAnchor == .init(
            itemID: itemID,
            canonicalRevision: 7
        ))
        #expect(canonicalRestart.canonicalItem(id: itemID)?.revision == 7)
    }

    @Test("failed prepared-create persistence rolls back journal and anchor")
    func testAtomicCreateRollback() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        store.flushPersistence()
        #expect(store.persistenceError == nil)
        try FileManager.default.removeItem(at: context.directory)

        #expect(throws: (any Error).self) {
            try store.enqueueOnboardingFirstItemCreate(
                itemID: UUID(),
                draft: Self.plannedDraft()
            )
        }

        #expect(store.onboardingFirstItemAnchor == nil)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(store.persistenceError != nil)
    }

    @Test("only an exact canonical revision without a journal proves the plan")
    func testExactPublishedPlanProof() throws {
        let itemID = UUID()
        let item = try Self.canonicalItem(id: itemID, revision: 3)
        let proof = Self.publicationProof(itemID: itemID, itemRevision: 3)
        let canonicalAnchor = DayWeaveOnboardingFirstItemAnchor(
            itemID: itemID,
            canonicalRevision: 3
        )

        #expect(canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [],
            publishedScheduleProof: proof
        ))

        let parentItem = try Self.canonicalItem(
            id: itemID,
            revision: 3,
            isExecutable: false
        )
        var child = try Self.canonicalItem(id: UUID(), revision: 1)
        child.parentID = itemID
        #expect(!canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [parentItem, child],
            pendingAuthoringMutations: [],
            publishedScheduleProof: proof
        ))

        var childDraft = Self.plannedDraft(title: "Pending child")
        childDraft.parentID = itemID
        let pendingChild = DayWeavePendingCanonicalAuthoringMutation(
            itemID: child.id,
            operation: .create,
            draft: childDraft,
            createdAt: Self.now
        )
        #expect(!canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [pendingChild],
            publishedScheduleProof: proof
        ))

        let movingChild = try Self.canonicalItem(id: UUID(), revision: 4)
        var movedDraft = Self.plannedDraft(title: movingChild.title)
        movedDraft.parentID = itemID
        let pendingMove = DayWeavePendingCanonicalAuthoringMutation(
            itemID: movingChild.id,
            operation: .replace,
            draft: movedDraft,
            expectedRevision: movingChild.revision,
            baseItem: movingChild,
            createdAt: Self.now
        )
        #expect(!canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [item, movingChild],
            pendingAuthoringMutations: [pendingMove],
            publishedScheduleProof: proof
        ))

        var ownEffortItem = parentItem
        ownEffortItem.flexibleConstraints = .object(["has_own_effort": .bool(true)])
        #expect(canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [ownEffortItem, child],
            pendingAuthoringMutations: [pendingChild],
            publishedScheduleProof: proof
        ))

        var eventItem = parentItem
        eventItem.kind = .event
        #expect(canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [eventItem, child],
            pendingAuthoringMutations: [pendingChild],
            publishedScheduleProof: proof
        ))

        let pending = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: Self.plannedDraft(),
            createdAt: Self.now,
            configurationIdentifier: Self.configurationIdentifier,
            hasBeenSubmitted: true
        )
        #expect(!canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [pending],
            publishedScheduleProof: proof
        ))
        #expect(!DayWeaveOnboardingFirstItemAnchor(
            itemID: itemID,
            canonicalRevision: nil
        ).hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [pending],
            publishedScheduleProof: proof
        ))
        #expect(!DayWeaveOnboardingFirstItemAnchor(
            itemID: itemID,
            canonicalRevision: 2
        ).hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [],
            publishedScheduleProof: proof
        ))
        #expect(!canonicalAnchor.hasExactPublishedPlanProof(
            canonicalItems: [item],
            pendingAuthoringMutations: [],
            publishedScheduleProof: Self.publicationProof(
                itemID: UUID(),
                itemRevision: 3
            )
        ))
    }

    @Test("generic reconciliation requires the retained create to match")
    func testGenericReconciliationRequiresMatchingRetainedCreate() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let draft = Self.plannedDraft(title: "Reviewed first item")
        let create = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: draft,
            createdAt: Self.now
        )
        let store = PlannerStore(
            onboardingFirstItemAnchor: .init(
                itemID: itemID,
                canonicalRevision: nil
            ),
            pendingCanonicalAuthoringMutations: [create],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )

        store.upsertCanonicalItem(try Self.canonicalItem(
            id: itemID,
            revision: 1,
            title: "Unrelated same-ID item"
        ))

        #expect(store.onboardingFirstItemAnchor == .init(
            itemID: itemID,
            canonicalRevision: nil
        ))
        try store.discardCanonicalAuthoringMutation(create.id)
        #expect(store.onboardingFirstItemAnchor == nil)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)

        let matchingStore = PlannerStore(
            onboardingFirstItemAnchor: .init(
                itemID: itemID,
                canonicalRevision: nil
            ),
            pendingCanonicalAuthoringMutations: [create],
            restoreFromPersistence: false,
            now: { Self.now }
        )
        matchingStore.upsertCanonicalItem(try Self.canonicalItem(
            id: itemID,
            revision: 1,
            title: draft.title
        ))
        #expect(matchingStore.onboardingFirstItemAnchor == .init(
            itemID: itemID,
            canonicalRevision: 1
        ))

        matchingStore.upsertCanonicalItem(try Self.canonicalItem(
            id: itemID,
            revision: 2,
            title: "Cross-device edit that was not reviewed here"
        ))
        #expect(matchingStore.onboardingFirstItemAnchor == nil)
    }

    @Test("schema eighteen cannot inject an onboarding anchor")
    func testSchemaEighteenMigrationDropsAnchor() throws {
        let profile = try ScheduleProfile.legacyDefault(
            timezoneName: "UTC",
            protectedFreeMinutes: 90
        )
        let injected = DayWeaveOnboardingFirstItemAnchor(
            itemID: UUID(),
            canonicalRevision: nil
        )
        let legacy = PlannerSnapshot(
            schemaVersion: 18,
            savedAt: Self.now,
            destination: .today,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Legacy",
            protectedFreeMinutes: 90,
            scheduleProfile: profile,
            freezeHours: 2,
            showCompleted: true,
            onboardingFirstItemAnchor: injected
        )

        let migrated = try legacy.migratedToCurrentSchema()

        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.onboardingFirstItemAnchor == nil)
    }

    @Test("nil-revision anchors require their exact valid planning create")
    func testMalformedNilRevisionAnchorIsRejected() throws {
        let itemID = UUID()
        let profile = try ScheduleProfile.legacyDefault(
            timezoneName: "UTC",
            protectedFreeMinutes: 90
        )

        func snapshot(
            mutations: [DayWeavePendingCanonicalAuthoringMutation]
        ) -> PlannerSnapshot {
            PlannerSnapshot(
                savedAt: Self.now,
                destination: .today,
                selectedBlockID: nil,
                blocks: [],
                suggestions: [],
                assistantMessages: [],
                lastScheduleMessage: "Current",
                protectedFreeMinutes: 90,
                scheduleProfile: profile,
                freezeHours: 2,
                showCompleted: true,
                onboardingFirstItemAnchor: .init(
                    itemID: itemID,
                    canonicalRevision: nil
                ),
                pendingCanonicalAuthoringMutations: mutations
            )
        }

        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try snapshot(mutations: []).migratedToCurrentSchema()
        }
        let inbox = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: .init(title: "Inbox only", timezoneName: "UTC"),
            createdAt: Self.now
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try snapshot(mutations: [inbox]).migratedToCurrentSchema()
        }
        let planned = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .create,
            draft: Self.plannedDraft(),
            createdAt: Self.now
        )
        #expect(try snapshot(mutations: [planned]).migratedToCurrentSchema()
            .onboardingFirstItemAnchor?.itemID == itemID)
    }

    nonisolated private static let now = Date(timeIntervalSince1970: 1_800_000_000)
    nonisolated private static let configurationIdentifier =
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"

    private static func plannedDraft(
        title: String = "First planned task"
    ) -> DayWeaveCanonicalItemDraft {
        .init(
            kind: .task,
            status: .planned,
            title: title,
            timezoneName: "UTC",
            durationSeconds: 1_800
        )
    }

    private static func canonicalItem(
        id: UUID,
        revision: UInt64,
        title: String = "First planned task",
        isExecutable: Bool = true
    ) throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"\#(id.uuidString.lowercased())","is_sensitive":false,
          "kind":"task","status":"planned","title":"\#(title)","notes":null,
          "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
          "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
          "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
          "parent_id":null,"sibling_order":0,"is_executable":\#(isExecutable),
          "revision":\#(revision),"created_at":"2027-01-15T10:00:00Z",
          "updated_at":"2027-01-15T10:00:00Z","completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func publicationProof(
        itemID: UUID,
        itemRevision: UInt64
    ) -> DayWeavePublishedScheduleProof {
        let revisionID = UUID()
        let block = ScheduleBlock(
            id: UUID(),
            title: "First planned task",
            kind: .task,
            start: now,
            end: now.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: itemRevision,
            occurrenceID: nil,
            sessionIndex: 0,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
        return .init(
            configurationIdentifier: configurationIdentifier,
            revisionID: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: "sha256:\(String(repeating: "b", count: 64))",
            asOf: now,
            horizonStart: now.addingTimeInterval(-3_600),
            horizonEnd: now.addingTimeInterval(86_400),
            timezoneName: "UTC",
            publishedAt: now,
            publishedBlocks: [DayWeavePublishedScheduleBlockProof(block: block)!]
        )
    }

    private static func persistenceContext() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveOnboardingAnchorTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data(repeating: 97, count: 32))
        return (
            directory,
            fileURL,
            EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        )
    }
}
#endif
