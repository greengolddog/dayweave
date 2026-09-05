import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("On-device canonical composition", .serialized)
@MainActor
struct LocalCompositionStoreTests {
    private static let configurationIdentifier =
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"

    @Test("a signed local result installs atomically with distinct provenance")
    func happyPathInstallsAndPersistsLocalComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        var priorCanonical = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        priorCanonical.status = .paused
        priorCanonical.actualMinutes = 7
        let localCapture = ScheduleBlock(
            id: UUID(),
            title: "Unpublished Inbox capture",
            kind: .task,
            start: now.addingTimeInterval(7_200),
            end: now.addingTimeInterval(9_000),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            syncOrigin: .local
        )
        let context = try Self.makePlanner(
            now: now,
            item: item,
            blocks: [priorCanonical, localCapture]
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = RecordingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)

        #expect(store.canRecomposeLocally)
        #expect(await store.recomposeLocally())

        #expect(await composer.calls() == 1)
        let request = try #require(await composer.lastRequest())
        #expect(request.asOf == now)
        #expect(request.horizonEnd > request.horizonStart)
        #expect(store.lastPreview == nil)
        #expect(store.warnings.isEmpty)
        #expect(store.lastLocalComposition != nil)
        #expect(store.lastLocalCompositionScore?.scheduledMinutes == 30)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.schedulePreviewProvenance == nil)
        let provenance = try #require(context.planner.localScheduleCompositionProvenance)
        #expect(provenance.configurationIdentifier == Self.configurationIdentifier)
        #expect(provenance.sourceItemRevisions == [item.id: item.revision])
        #expect(provenance.localInputFingerprint.hasPrefix("local-sha256:"))
        let canonical = try #require(context.planner.blocks.first { $0.sourceItemID == item.id })
        #expect(canonical.syncOrigin == .localComposition)
        #expect(canonical.status == .paused)
        #expect(canonical.actualMinutes == 7)
        #expect(context.planner.blocks.contains { $0.id == localCapture.id && $0.syncOrigin == .local })
        #expect(context.planner.canonicalPreviewFreshnessIssue == nil)
        #expect(context.planner.canMutate(canonical))
        #expect(store.canRecomposeLocally)

        let loaded = try context.persistence.load()
        let restored = try #require(loaded)
        #expect(restored.localScheduleCompositionProvenance == provenance)
        #expect(restored.schedulePreviewProvenance == nil)
        #expect(restored.pendingSchedulePublication == nil)
        #expect(restored.blocks == context.planner.blocks)
    }

    @Test("incomplete cache and every pending journal fail closed before helper launch")
    func preflightAndPendingJournalGates() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)

        do {
            let context = try Self.makePlanner(
                now: now,
                item: item,
                cursor: nil,
                configurationIdentifier: nil
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
            #expect(!store.canRecomposeLocally)
            #expect(!(await store.recomposeLocally()))
            #expect(await composer.calls() == 0)
            #expect(store.localCompositionStatus.message.contains("normal Sync"))
        }

        for variant in PendingJournalVariant.allCases {
            let pending = try Self.pendingState(variant, item: item, now: now)
            let context = try Self.makePlanner(
                now: now,
                item: item,
                pendingSchedulePublication: pending.schedulePublication,
                pendingProposalApplicationMutation: pending.proposalApplication,
                pendingCanonicalMutations: pending.statusMutations,
                pendingCanonicalSensitivityMutations: pending.sensitivityMutations,
                pendingCanonicalAuthoringMutations: pending.authoringMutations,
                googleOutboundRecoveryJournal: pending.googleRecovery
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let originalBlocks = context.planner.blocks
            let store = Self.makeStore(planner: context.planner, composer: composer, now: now)

            #expect(!store.canRecomposeLocally, "\(variant) should disable local composition")
            #expect(!(await store.recomposeLocally()), "\(variant) should fail closed")
            #expect(await composer.calls() == 0)
            #expect(context.planner.blocks == originalBlocks)
            #expect(store.localCompositionStatus.message.contains("normal Sync"))
        }
    }

    @Test("active habits require an exact complete and idle habit checkpoint")
    func habitCheckpointPreflightFailsClosed() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        let ready = Self.habitCheckpoint(item: habit, now: now)
        let variants: [HabitCompositionCheckpoint?] = [
            nil,
            Self.habitCheckpoint(item: habit, now: now, configurationIdentifier: "other"),
            Self.habitCheckpoint(item: habit, now: now, deltaCursor: nil),
            Self.habitCheckpoint(item: habit, now: now, deltaCaughtUp: false),
            Self.habitCheckpoint(item: habit, now: now, pendingMutationIDs: [UUID()]),
            Self.habitCheckpoint(item: habit, now: now, hasActiveOperation: true),
            Self.habitCheckpoint(
                item: habit,
                now: now,
                sourceItemRevision: habit.revision + 1
            ),
        ]

        for checkpoint in variants {
            let context = try Self.makePlanner(now: now, item: habit)
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let provider = checkpoint.map(HabitCheckpointStub.init)
            let store = Self.makeStore(
                planner: context.planner,
                composer: composer,
                now: now,
                habitProvider: provider
            )

            #expect(!store.canRecomposeLocally)
            #expect(!(await store.recomposeLocally()))
            #expect(await composer.calls() == 0)
        }
        #expect(ready.fingerprint?.hasPrefix("habit-sha256:") == true)
    }

    @Test("authoritative habit outcomes progress and pauses feed local composition provenance")
    func habitLedgerFeedsComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        let checkpoint = Self.habitCheckpoint(item: habit, now: now)
        let provider = HabitCheckpointStub(checkpoint)
        let context = try Self.makePlanner(now: now, item: habit)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = RecordingLocalComposer()
        let store = Self.makeStore(
            planner: context.planner,
            composer: composer,
            now: now,
            habitProvider: provider
        )

        #expect(await store.recomposeLocally())
        let request = try #require(await composer.lastRequest())
        guard case let .array(completed)? = request.recurrenceContext["completed_occurrence_ids"],
              case let .object(anchors)? = request.recurrenceContext["completion_anchors"],
              case let .object(partial)? = request.recurrenceContext["partial_progress"],
              case let .array(pauses)? = request.recurrenceContext["pauses"],
              case let .array(exceptions)? = request.recurrenceContext["exceptions"] else {
            Issue.record("Expected the complete authoritative habit recurrence projection")
            return
        }
        #expect(completed == [.string(Self.completedPlannerOccurrenceID.uuidString.lowercased())])
        #expect(anchors[habit.id.uuidString.lowercased()] == .string(Self.timestamp(now)))
        #expect(partial[Self.partialPlannerOccurrenceID.uuidString.lowercased()] == .object([
            "progress_basis_points": .number(.init(UInt64(2_500))),
            "expected_duration_minutes": .number(.init(UInt64(30))),
        ]))
        #expect(pauses.count == 1)
        #expect(exceptions.contains(.object([
            "item_id": .string(habit.id.uuidString.lowercased()),
            "selector": .object([
                "type": .string("occurrence"),
                "id": .string(Self.skippedPlannerOccurrenceID.uuidString.lowercased()),
            ]),
            "action": .object(["type": .string("skip")]),
        ])))
        let provenance = try #require(context.planner.localScheduleCompositionProvenance)
        #expect(provenance.habitCheckpointFingerprint == checkpoint.fingerprint)
        #expect(context.planner.canonicalPreviewFreshnessIssue == nil)

        provider.update(Self.habitCheckpoint(
            item: habit,
            now: now,
            deltaCursor: "habit-cursor-two"
        ))
        #expect(context.planner.canonicalPreviewFreshnessIssue != nil)
    }

    @Test("missed skip and reduction actions never conflict with partial progress")
    func missedSkipsExcludePartialProgress() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])

        for action in [MissedCompositionAction.skipSource, .reducePartialTarget] {
            let checkpoint = Self.missedCheckpoint(
                item: habit,
                now: now,
                action: action,
                sourceItemRevision: habit.revision
            )
            let context = try Self.makePlanner(
                now: now,
                item: habit,
                publishedOccurrences: Self.publishedOccurrences(for: checkpoint)
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(
                planner: context.planner,
                composer: composer,
                now: now,
                habitProvider: HabitCheckpointStub(checkpoint)
            )

            #expect(await store.recomposeLocally())
            let request = try #require(await composer.lastRequest())
            guard case let .object(partial)? = request.recurrenceContext["partial_progress"],
                  case let .array(exceptions)? = request.recurrenceContext["exceptions"] else {
                Issue.record("Expected missed-habit recurrence context")
                return
            }
            let skippedID = action == .skipSource
                ? Self.missedSourcePlannerOccurrenceID
                : Self.missedTargetPlannerOccurrenceID
            if action == .skipSource {
                #expect(partial[skippedID.uuidString.lowercased()] == nil)
                #expect(exceptions.contains(Self.skipException(
                    itemID: habit.id,
                    occurrenceID: skippedID
                )))
            } else {
                // A later partial outcome is a newer lifecycle coordinate and
                // wins over the cached reduction until server reconciliation
                // rebinds that reduction to another target.
                #expect(partial[skippedID.uuidString.lowercased()] != nil)
                #expect(!exceptions.contains(Self.skipException(
                    itemID: habit.id,
                    occurrenceID: skippedID
                )))
            }

            let mismatchedCheckpoint = Self.missedCheckpoint(
                item: habit,
                now: now,
                action: action,
                sourceItemRevision: habit.revision - 1,
                useMatchingPolicyFingerprint: false
            )
            let mismatchedContext = try Self.makePlanner(
                now: now,
                item: habit,
                publishedOccurrences: Self.publishedOccurrences(
                    for: mismatchedCheckpoint
                )
            )
            defer { try? FileManager.default.removeItem(at: mismatchedContext.directory) }
            let mismatchedComposer = RecordingLocalComposer()
            let mismatchedStore = Self.makeStore(
                planner: mismatchedContext.planner,
                composer: mismatchedComposer,
                now: now,
                habitProvider: HabitCheckpointStub(mismatchedCheckpoint)
            )
            #expect(await mismatchedStore.recomposeLocally())
            let mismatchedRequest = try #require(await mismatchedComposer.lastRequest())
            guard case let .array(mismatchedExceptions)? =
                    mismatchedRequest.recurrenceContext["exceptions"] else {
                Issue.record("Expected mismatched-policy recurrence context")
                return
            }
            #expect(!mismatchedExceptions.contains(Self.skipException(
                itemID: habit.id,
                occurrenceID: skippedID
            )))
        }
    }

    @Test("local missed carries require a contained destination and matching recurrence policy")
    func missedCarryAuthorityFences() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let profile = try ScheduleProfile.legacyDefault(
            timezoneName: habit.timezoneName,
            protectedFreeMinutes: 90
        )
        let horizon = try profile.expanded(asOf: now)

        func projectedSourceAction(
            start: Date,
            end: Date,
            sourceRevision: UInt64,
            includeSourceIdentity: Bool = true,
            useMatchingPolicyFingerprint: Bool = true
        ) async throws -> String? {
            let context = try Self.makePlanner(now: now, item: habit)
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let checkpoint = Self.missedCheckpoint(
                item: habit,
                now: now,
                action: .carry(start: start, end: end),
                sourceItemRevision: sourceRevision,
                sourceNominalStart: now.addingTimeInterval(-3_600),
                includeSourceIdentity: includeSourceIdentity,
                useMatchingPolicyFingerprint: useMatchingPolicyFingerprint
            )
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(
                planner: context.planner,
                composer: composer,
                now: now,
                habitProvider: HabitCheckpointStub(checkpoint)
            )
            #expect(await store.recomposeLocally())
            let request = try #require(await composer.lastRequest())
            guard case let .array(exceptions)? = request.recurrenceContext["exceptions"] else {
                return nil
            }
            return exceptions.compactMap { exception -> String? in
                guard case let .object(fields) = exception,
                      case let .object(selector)? = fields["selector"],
                      selector["id"] == .string(
                          Self.missedSourcePlannerOccurrenceID.uuidString.lowercased()
                      ),
                      case let .object(action)? = fields["action"],
                      case let .string(type)? = action["type"] else { return nil }
                return type
            }.first
        }

        #expect(try await projectedSourceAction(
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            sourceRevision: habit.revision
        ) == "move")
        #expect(try await projectedSourceAction(
            start: horizon.horizonStart.addingTimeInterval(-1),
            end: horizon.horizonStart.addingTimeInterval(1_800),
            sourceRevision: habit.revision
        ) == "skip")
        #expect(try await projectedSourceAction(
            start: horizon.horizonEnd.addingTimeInterval(-1_800),
            end: horizon.horizonEnd.addingTimeInterval(1),
            sourceRevision: habit.revision
        ) == "skip")
        #expect(try await projectedSourceAction(
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            sourceRevision: habit.revision - 1
        ) == "move")
        #expect(try await projectedSourceAction(
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            sourceRevision: habit.revision - 1,
            useMatchingPolicyFingerprint: false
        ) == nil)
        #expect(try await projectedSourceAction(
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            sourceRevision: habit.revision,
            includeSourceIdentity: false
        ) == nil)
    }

    @Test("terminal outcomes and pauses suppress stale missed scheduling effects")
    func missedEffectsRespectIndependentLifecycleCoordinates() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let carryStart = now.addingTimeInterval(3_600)
        let carryEnd = now.addingTimeInterval(5_400)

        func recurrenceContext(
            action: MissedCompositionAction,
            outcome: DayWeaveHabitOutcomeStatus? = nil,
            pause: (start: Date, end: Date)? = nil,
            targetOutcome: DayWeaveHabitOutcomeStatus? = .partial,
            itemStatus: DayWeaveCanonicalItemStatus = .planned,
            useMatchingSourcePolicyFingerprint: Bool = true,
            targetMissedAction: DayWeaveHabitMissedResolutionAction? = nil,
            targetNominalStart: Date? = nil,
            additionalOccurrences: [HabitCompositionCheckpoint.Occurrence] = [],
            publishedStateOverrides: [UUID: String] = [:],
            omittedPublishedOccurrenceIDs: Set<UUID> = [],
            publishedProofVersion: Int = DayWeavePublishedScheduleProof.currentVersion
        ) async throws -> [String: JSONValue] {
            var contextualHabit = habit
            contextualHabit.status = itemStatus
            let checkpoint = Self.missedCheckpoint(
                item: contextualHabit,
                now: now,
                action: action,
                sourceItemRevision: habit.revision,
                sourceOutcomeStatus: outcome,
                pauseWindow: pause,
                targetOutcomeStatus: targetOutcome,
                useMatchingPolicyFingerprint: useMatchingSourcePolicyFingerprint,
                targetMissedAction: targetMissedAction,
                targetNominalStart: targetNominalStart,
                additionalOccurrences: additionalOccurrences
            )
            let context = try Self.makePlanner(
                now: now,
                item: contextualHabit,
                publishedOccurrences: Self.publishedOccurrences(
                    for: checkpoint,
                    stateOverrides: publishedStateOverrides,
                    omitting: omittedPublishedOccurrenceIDs
                ),
                publishedProofVersion: publishedProofVersion
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(
                planner: context.planner,
                composer: composer,
                now: now,
                habitProvider: HabitCheckpointStub(checkpoint)
            )
            #expect(await store.recomposeLocally())
            return try #require(await composer.lastRequest()).recurrenceContext
        }

        for terminal in [DayWeaveHabitOutcomeStatus.completed, .skipped] {
            let context = try await recurrenceContext(
                action: .carry(start: carryStart, end: carryEnd),
                outcome: terminal
            )
            guard case let .array(exceptions)? = context["exceptions"] else {
                Issue.record("Expected recurrence exceptions")
                continue
            }
            #expect(!exceptions.contains(where: Self.isMissedSourceMove))
        }

        let pausedCarry = try await recurrenceContext(
            action: .carry(start: carryStart, end: carryEnd),
            pause: (carryStart.addingTimeInterval(-60), carryEnd.addingTimeInterval(60))
        )
        guard case let .array(pausedExceptions)? = pausedCarry["exceptions"] else {
            Issue.record("Expected recurrence exceptions")
            return
        }
        #expect(!pausedExceptions.contains(where: Self.isMissedSourceMove))

        let targetCarryStart = now.addingTimeInterval(90_000)
        let targetCarry: DayWeaveHabitMissedResolutionAction = .carry(
            windowStart: targetCarryStart,
            windowEnd: targetCarryStart.addingTimeInterval(1_800)
        )
        func targetAction(_ context: [String: JSONValue]) -> String? {
            guard case let .array(exceptions)? = context["exceptions"] else { return nil }
            return exceptions.compactMap {
                Self.exceptionAction($0, occurrenceID: Self.missedTargetPlannerOccurrenceID)
            }.first
        }

        for inactiveStatus in [
            DayWeaveCanonicalItemStatus.blocked,
            .completed,
            .skipped,
            .cancelled,
            .unknown("future_status"),
        ] {
            let inactiveCarry = try await recurrenceContext(
                action: .carry(start: carryStart, end: carryEnd),
                itemStatus: inactiveStatus
            )
            guard case let .array(inactiveExceptions)? = inactiveCarry["exceptions"] else {
                Issue.record("Expected inactive-item exceptions")
                continue
            }
            #expect(!inactiveExceptions.contains(where: Self.isMissedSourceMove))
        }

        let sourceSpecificInvalidations: [[String: JSONValue]] = try await [
            recurrenceContext(
                action: .reducePartialTarget,
                outcome: .completed,
                targetOutcome: nil,
                targetMissedAction: targetCarry
            ),
            recurrenceContext(
                action: .reducePartialTarget,
                pause: (
                    now.addingTimeInterval(-87_000),
                    now.addingTimeInterval(-84_000)
                ),
                targetOutcome: nil,
                targetMissedAction: targetCarry
            ),
            recurrenceContext(
                action: .reducePartialTarget,
                targetOutcome: nil,
                useMatchingSourcePolicyFingerprint: false,
                targetMissedAction: targetCarry
            ),
        ]
        for context in sourceSpecificInvalidations {
            #expect(targetAction(context) == "move")
        }

        for inactiveStatus in [
            DayWeaveCanonicalItemStatus.blocked,
            .completed,
            .skipped,
            .cancelled,
            .unknown("future_status"),
        ] {
            let inactiveReduction = try await recurrenceContext(
                action: .reducePartialTarget,
                targetOutcome: nil,
                itemStatus: inactiveStatus,
                targetMissedAction: targetCarry
            )
            #expect(targetAction(inactiveReduction) == nil)
        }

        var nonLeafHabit = try LocalCompositionFixture.item(
            revision: 2,
            kind: "habit",
            // Fail closed even if an inconsistent remote snapshot leaves the
            // derived executable bit stale while an active child exists.
            isExecutable: true
        )
        nonLeafHabit.recurrence = habit.recurrence
        let child = try LocalCompositionFixture.item(
            revision: 1,
            id: UUID(uuidString: "b1000000-0000-4000-8000-000000000002")!,
            parentID: nonLeafHabit.id
        )
        let nonLeafCheckpoint = Self.missedCheckpoint(
            item: nonLeafHabit,
            now: now,
            action: .carry(start: carryStart, end: carryEnd),
            sourceItemRevision: nonLeafHabit.revision
        )
        let nonLeafContext = try Self.makePlanner(
            now: now,
            item: child,
            additionalItems: [nonLeafHabit],
            publishedOccurrences: Self.publishedOccurrences(
                for: nonLeafCheckpoint
            )
        )
        defer { try? FileManager.default.removeItem(at: nonLeafContext.directory) }
        let nonLeafComposer = RecordingLocalComposer()
        let nonLeafStore = Self.makeStore(
            planner: nonLeafContext.planner,
            composer: nonLeafComposer,
            now: now,
            habitProvider: HabitCheckpointStub(nonLeafCheckpoint)
        )
        #expect(await nonLeafStore.recomposeLocally())
        let nonLeafRequest = try #require(await nonLeafComposer.lastRequest())
        guard case let .array(nonLeafExceptions)? =
                nonLeafRequest.recurrenceContext["exceptions"] else {
            Issue.record("Expected non-leaf recurrence context")
            return
        }
        #expect(!nonLeafExceptions.contains(where: Self.isMissedSourceMove))

        let nonLeafReductionCheckpoint = Self.missedCheckpoint(
            item: nonLeafHabit,
            now: now,
            action: .reducePartialTarget,
            sourceItemRevision: nonLeafHabit.revision,
            targetOutcomeStatus: nil,
            targetMissedAction: targetCarry
        )
        let nonLeafReductionContext = try Self.makePlanner(
            now: now,
            item: child,
            additionalItems: [nonLeafHabit],
            publishedOccurrences: Self.publishedOccurrences(
                for: nonLeafReductionCheckpoint
            )
        )
        defer { try? FileManager.default.removeItem(at: nonLeafReductionContext.directory) }
        let nonLeafReductionComposer = RecordingLocalComposer()
        let nonLeafReductionStore = Self.makeStore(
            planner: nonLeafReductionContext.planner,
            composer: nonLeafReductionComposer,
            now: now,
            habitProvider: HabitCheckpointStub(nonLeafReductionCheckpoint)
        )
        #expect(await nonLeafReductionStore.recomposeLocally())
        let nonLeafReductionRequest = try #require(await nonLeafReductionComposer.lastRequest())
        #expect(targetAction(nonLeafReductionRequest.recurrenceContext) == nil)

        let completedReduction = try await recurrenceContext(
            action: .reducePartialTarget,
            outcome: .completed
        )
        guard case let .array(reductionExceptions)? = completedReduction["exceptions"] else {
            Issue.record("Expected recurrence exceptions")
            return
        }
        #expect(!reductionExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))

        let eligibleReduction = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil
        )
        guard case let .array(eligibleReductionExceptions)? =
                eligibleReduction["exceptions"] else {
            Issue.record("Expected eligible reduction exceptions")
            return
        }
        #expect(eligibleReductionExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))

        let targetOwnActionIsSuppressed = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetCarry
        )
        #expect(targetAction(targetOwnActionIsSuppressed) == "skip")

        let chainTargetStart = now.addingTimeInterval(172_800)
        let chainTargetDate = try #require(DayWeaveLocalDate.containing(
            chainTargetStart,
            timezoneName: habit.timezoneName
        ))
        let chainTarget = HabitCompositionCheckpoint.Occurrence(
            id: UUID(uuidString: "c2000000-0000-4000-8000-000000000006")!,
            habitID: habit.id,
            plannerOccurrenceID: Self.missedChainTargetPlannerOccurrenceID,
            sourceItemRevision: habit.revision,
            policyFingerprint: habit.habitPolicyFingerprint,
            nominalStart: chainTargetStart,
            windowStart: chainTargetStart.addingTimeInterval(-1_800),
            windowEnd: chainTargetStart.addingTimeInterval(3_600),
            expectedDurationSeconds: 1_800,
            outcome: nil,
            identity: .object([
                "type": .string("calendar_day"),
                "date": .string(chainTargetDate.rawValue),
                "bucket_ordinal": .number(.init(UInt64(0))),
            ]),
            nominalEnd: chainTargetStart.addingTimeInterval(1_800),
            localDate: chainTargetDate
        )
        let targetReduction: DayWeaveHabitMissedResolutionAction = .reduceFrequency(
            suppressedPlannerOccurrenceIDs: [Self.missedChainTargetPlannerOccurrenceID]
        )
        let activeChain = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetReduction,
            additionalOccurrences: [chainTarget]
        )
        #expect(targetAction(activeChain) == "skip")
        guard case let .array(activeChainExceptions)? = activeChain["exceptions"] else {
            Issue.record("Expected active-chain exceptions")
            return
        }
        #expect(!activeChainExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedChainTargetPlannerOccurrenceID
        )))

        let omittedByCurrentPublication = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetReduction,
            additionalOccurrences: [chainTarget],
            omittedPublishedOccurrenceIDs: [Self.missedTargetPlannerOccurrenceID]
        )
        guard case let .array(omittedByCurrentPublicationExceptions)? =
                omittedByCurrentPublication["exceptions"] else {
            Issue.record("Expected omitted-target chain exceptions")
            return
        }
        #expect(!omittedByCurrentPublicationExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))
        #expect(omittedByCurrentPublicationExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedChainTargetPlannerOccurrenceID
        )))

        let skippedMemberStillRestoresExistingEdge = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetReduction,
            additionalOccurrences: [chainTarget],
            publishedStateOverrides: [Self.missedTargetPlannerOccurrenceID: "skipped"]
        )
        guard case let .array(skippedMemberExceptions)? =
                skippedMemberStillRestoresExistingEdge["exceptions"] else {
            Issue.record("Expected skipped-member chain exceptions")
            return
        }
        #expect(skippedMemberExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))
        #expect(!skippedMemberExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedChainTargetPlannerOccurrenceID
        )))

        let legacyProofRestoresTargetAction = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetCarry,
            publishedProofVersion: 2
        )
        #expect(targetAction(legacyProofRestoresTargetAction) == "move")

        let inHorizonChainTargetStart = now.addingTimeInterval(3_600)
        let inHorizonChainTargetDate = try #require(DayWeaveLocalDate.containing(
            inHorizonChainTargetStart,
            timezoneName: habit.timezoneName
        ))
        let inHorizonChainTarget = HabitCompositionCheckpoint.Occurrence(
            id: chainTarget.id,
            habitID: chainTarget.habitID,
            plannerOccurrenceID: chainTarget.plannerOccurrenceID,
            sourceItemRevision: chainTarget.sourceItemRevision,
            policyFingerprint: chainTarget.policyFingerprint,
            nominalStart: inHorizonChainTargetStart,
            windowStart: inHorizonChainTargetStart.addingTimeInterval(-1_800),
            windowEnd: inHorizonChainTargetStart.addingTimeInterval(3_600),
            expectedDurationSeconds: chainTarget.expectedDurationSeconds,
            outcome: nil,
            identity: .object([
                "type": .string("calendar_day"),
                "date": .string(inHorizonChainTargetDate.rawValue),
                "bucket_ordinal": .number(.init(UInt64(0))),
            ]),
            nominalEnd: inHorizonChainTargetStart.addingTimeInterval(1_800),
            localDate: inHorizonChainTargetDate
        )
        let historicalUpstreamChain = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: nil,
            targetMissedAction: targetReduction,
            targetNominalStart: now.addingTimeInterval(-43_200),
            additionalOccurrences: [inHorizonChainTarget]
        )
        guard case let .array(historicalUpstreamExceptions)? =
                historicalUpstreamChain["exceptions"] else {
            Issue.record("Expected historical-upstream chain exceptions")
            return
        }
        #expect(!historicalUpstreamExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedChainTargetPlannerOccurrenceID
        )))

        let restoredChain = try await recurrenceContext(
            action: .reducePartialTarget,
            outcome: .completed,
            targetOutcome: nil,
            targetMissedAction: targetReduction,
            additionalOccurrences: [chainTarget]
        )
        guard case let .array(restoredChainExceptions)? = restoredChain["exceptions"] else {
            Issue.record("Expected restored-chain exceptions")
            return
        }
        #expect(!restoredChainExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))
        #expect(restoredChainExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedChainTargetPlannerOccurrenceID
        )))

        for terminal in [DayWeaveHabitOutcomeStatus.partial, .completed] {
            let targetWins = try await recurrenceContext(
                action: .reducePartialTarget,
                targetOutcome: terminal
            )
            guard case let .array(targetExceptions)? = targetWins["exceptions"] else {
                Issue.record("Expected target-precedence exceptions")
                continue
            }
            #expect(!targetExceptions.contains(Self.skipException(
                itemID: habit.id,
                occurrenceID: Self.missedTargetPlannerOccurrenceID
            )))
        }

        let skippedTarget = try await recurrenceContext(
            action: .reducePartialTarget,
            targetOutcome: .skipped
        )
        guard case let .array(skippedTargetExceptions)? = skippedTarget["exceptions"] else {
            Issue.record("Expected skipped-target exceptions")
            return
        }
        // The target's own terminal outcome still suppresses it; the missed
        // reduction is no longer the authority for that suppression.
        #expect(skippedTargetExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))

        let targetStart = now.addingTimeInterval(86_400)
        let pausedReduction = try await recurrenceContext(
            action: .reducePartialTarget,
            pause: (targetStart.addingTimeInterval(-60), targetStart.addingTimeInterval(60)),
            targetOutcome: nil
        )
        guard case let .array(pausedReductionExceptions)? = pausedReduction["exceptions"] else {
            Issue.record("Expected paused-target exceptions")
            return
        }
        #expect(!pausedReductionExceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))
    }

    @Test("published occurrence authority survives local composition and restart")
    func publishedOccurrenceAuthoritySurvivesLocalComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let checkpoint = Self.missedCheckpoint(
            item: habit,
            now: now,
            action: .reducePartialTarget,
            sourceItemRevision: habit.revision,
            targetOutcomeStatus: nil
        )
        let context = try Self.makePlanner(
            now: now,
            item: habit,
            publishedOccurrences: Self.publishedOccurrences(for: checkpoint)
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let originalProof = try #require(context.planner.publishedScheduleProof)
        let store = Self.makeStore(
            planner: context.planner,
            composer: RecordingLocalComposer(),
            now: now,
            habitProvider: HabitCheckpointStub(checkpoint)
        )

        #expect(await store.recomposeLocally())
        #expect(context.planner.schedulePreviewProvenance == nil)
        #expect(context.planner.localScheduleCompositionProvenance != nil)
        #expect(context.planner.publishedScheduleProof == originalProof)

        let restored = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        #expect(restored.persistenceError == nil)
        #expect(restored.localScheduleCompositionProvenance != nil)
        #expect(restored.publishedScheduleProof == originalProof)
        #expect(restored.publishedScheduleProof?.currentOccurrenceAuthority != nil)
    }

    @Test("server-owned missed exceptions cannot be replaced by stored moves")
    func missedExceptionsOverrideStoredMoves() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])

        func move(occurrenceID: UUID, nominalStart: Date, movedAt: Date) -> RecurrenceOccurrenceMove {
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime]
            let localDate = DayWeaveLocalDate.containing(
                nominalStart,
                timezoneName: habit.timezoneName
            )!
            return RecurrenceOccurrenceMove(
                itemID: habit.id,
                occurrenceID: occurrenceID,
                startAt: now.addingTimeInterval(10_800),
                endAt: now.addingTimeInterval(12_600),
                movedAt: movedAt,
                source: .init(
                    itemRevision: habit.revision,
                    identity: .calendarDay(date: localDate.rawValue, bucketOrdinal: 0),
                    nominalStart: formatter.string(from: nominalStart),
                    nominalEnd: formatter.string(
                        from: nominalStart.addingTimeInterval(1_800)
                    ),
                    localDate: localDate.rawValue,
                    ordinal: 0
                )
            )
        }

        func recurrenceContext(
            action: MissedCompositionAction,
            moveTimestamp: Date,
            targetOutcome: DayWeaveHabitOutcomeStatus? = nil,
            pause: (start: Date, end: Date)? = nil
        ) async throws -> [String: JSONValue] {
            let isReduction = action == .reducePartialTarget
            let occurrenceID = isReduction
                ? Self.missedTargetPlannerOccurrenceID
                : Self.missedSourcePlannerOccurrenceID
            let nominalStart = isReduction
                ? now.addingTimeInterval(86_400)
                : now.addingTimeInterval(-3_600)
            let storedMove = move(
                occurrenceID: occurrenceID,
                nominalStart: nominalStart,
                movedAt: moveTimestamp
            )
            #expect(storedMove.hasValidShape)
            let checkpoint = Self.missedCheckpoint(
                item: habit,
                now: now,
                action: action,
                sourceItemRevision: habit.revision,
                pauseWindow: pause,
                targetOutcomeStatus: targetOutcome
            )
            let context = try Self.makePlanner(
                now: now,
                item: habit,
                publishedOccurrences: Self.publishedOccurrences(for: checkpoint),
                recurrenceOccurrenceMoves: [storedMove]
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(
                planner: context.planner,
                composer: composer,
                now: now,
                habitProvider: HabitCheckpointStub(checkpoint)
            )
            #expect(await store.recomposeLocally())
            return try #require(await composer.lastRequest()).recurrenceContext
        }

        for action in [MissedCompositionAction.skipSource, .reducePartialTarget] {
            let occurrenceID = action == .skipSource
                ? Self.missedSourcePlannerOccurrenceID
                : Self.missedTargetPlannerOccurrenceID
            for offset in [-120.0, 120.0] {
                let context = try await recurrenceContext(
                    action: action,
                    moveTimestamp: now.addingTimeInterval(offset)
                )
                guard case let .array(exceptions)? = context["exceptions"] else {
                    Issue.record("Expected authoritative recurrence exceptions")
                    continue
                }
                let actions = exceptions.compactMap {
                    Self.exceptionAction($0, occurrenceID: occurrenceID)
                }
                #expect(actions == ["skip"])
            }
        }

        let partialContext = try await recurrenceContext(
            action: .reducePartialTarget,
            moveTimestamp: now.addingTimeInterval(120),
            targetOutcome: .partial
        )
        guard case let .array(partialExceptions)? = partialContext["exceptions"] else {
            Issue.record("Expected partial-target recurrence exceptions")
            return
        }
        #expect(partialExceptions.compactMap {
            Self.exceptionAction($0, occurrenceID: Self.missedTargetPlannerOccurrenceID)
        } == ["move"])

        let targetStart = now.addingTimeInterval(86_400)
        let pausedContext = try await recurrenceContext(
            action: .reducePartialTarget,
            moveTimestamp: now.addingTimeInterval(120),
            pause: (
                targetStart.addingTimeInterval(-60),
                targetStart.addingTimeInterval(60)
            )
        )
        guard case let .array(pausedExceptions)? = pausedContext["exceptions"],
              case let .array(pauses)? = pausedContext["pauses"] else {
            Issue.record("Expected paused recurrence context")
            return
        }
        #expect(pausedExceptions.compactMap {
            Self.exceptionAction($0, occurrenceID: Self.missedTargetPlannerOccurrenceID)
        } == ["move"])
        #expect(pauses.count == 1)
    }

    @Test("a habit operation crossing the helper await invalidates the in-flight fence")
    func habitCheckpointRaceDiscardsHelperResult() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        let checkpoint = Self.habitCheckpoint(item: habit, now: now)
        let provider = HabitCheckpointStub(checkpoint)
        let context = try Self.makePlanner(now: now, item: habit)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(
            planner: context.planner,
            composer: composer,
            now: now,
            habitProvider: provider
        )
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()

        provider.replaceWithoutNotification(Self.habitCheckpoint(
            item: habit,
            now: now,
            operationGeneration: checkpoint.operationGeneration + 1
        ))
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(store.localCompositionStatus.message.contains("changed"))
    }

    @Test("a newer publication hint crossing the helper await invalidates the fence")
    func publicationHintRaceDiscardsHelperResult() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let checkpoint = Self.missedCheckpoint(
            item: habit,
            now: now,
            action: .reducePartialTarget,
            sourceItemRevision: habit.revision,
            targetOutcomeStatus: nil
        )
        let context = try Self.makePlanner(
            now: now,
            item: habit,
            publishedOccurrences: Self.publishedOccurrences(for: checkpoint)
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proof = try #require(context.planner.publishedScheduleProof)
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(
            planner: context.planner,
            composer: composer,
            now: now,
            habitProvider: HabitCheckpointStub(checkpoint)
        )
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()

        try context.planner.persistPublishedScheduleRevisionHint(
            proof.revisionNumber + 1
        )
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.publishedScheduleProof == proof)
        #expect(store.localCompositionStatus.message.contains("changed"))
    }

    @Test("a newer publication hint remains fail-closed after offline restart")
    func publicationHintHighWaterSurvivesRestart() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let checkpoint = Self.missedCheckpoint(
            item: habit,
            now: now,
            action: .reducePartialTarget,
            sourceItemRevision: habit.revision,
            targetOutcomeStatus: nil
        )
        let context = try Self.makePlanner(
            now: now,
            item: habit,
            publishedOccurrences: Self.publishedOccurrences(for: checkpoint)
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proof = try #require(context.planner.publishedScheduleProof)

        // Model an authenticated R+1 stream hint followed by a failed current
        // publication fetch: no replacement head is installed before relaunch.
        try context.planner.persistPublishedScheduleRevisionHint(
            proof.revisionNumber + 1
        )
        let restored = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        #expect(restored.persistenceError == nil)
        #expect(restored.publishedScheduleProof == proof)
        #expect(
            restored.publishedScheduleLatestHintRevision
                == proof.revisionNumber + 1
        )

        let composer = RecordingLocalComposer()
        let store = Self.makeStore(
            planner: restored,
            composer: composer,
            now: now,
            habitProvider: HabitCheckpointStub(checkpoint)
        )
        #expect(await store.recomposeLocally())
        let request = try #require(await composer.lastRequest())
        guard case let .array(exceptions)? = request.recurrenceContext["exceptions"] else {
            Issue.record("Expected recurrence exceptions after offline relaunch")
            return
        }
        #expect(!exceptions.contains(Self.skipException(
            itemID: habit.id,
            occurrenceID: Self.missedTargetPlannerOccurrenceID
        )))
    }

    @Test("a failed hint checkpoint does not advance accepted in-memory authority")
    func publicationHintPersistenceFailureRollsBackHighWater() throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        var habit = try LocalCompositionFixture.item(revision: 2, kind: "habit")
        habit.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(1))),
        ])
        let checkpoint = Self.missedCheckpoint(
            item: habit,
            now: now,
            action: .reducePartialTarget,
            sourceItemRevision: habit.revision,
            targetOutcomeStatus: nil
        )
        let context = try Self.makePlanner(
            now: now,
            item: habit,
            publishedOccurrences: Self.publishedOccurrences(for: checkpoint)
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let proof = try #require(context.planner.publishedScheduleProof)
        context.planner.flushPersistence()
        #expect(context.planner.persistenceError == nil)

        let otherWriter = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        otherWriter.showCompleted.toggle()
        otherWriter.flushPersistence()
        #expect(otherWriter.persistenceError == nil)

        #expect(throws: PlannerPersistenceError.concurrentModification) {
            try context.planner.persistPublishedScheduleRevisionHint(
                proof.revisionNumber + 1
            )
        }
        #expect(
            context.planner.publishedScheduleLatestHintRevision
                == proof.revisionNumber
        )
        #expect(
            try context.persistence.load()?.publishedScheduleLatestHintRevision
                == proof.revisionNumber
        )
    }

    @Test("a canonical revision change while awaiting the helper discards its result")
    func revisionRaceDiscardsHelperResult() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()
        #expect(!store.canRecomposeLocally)

        context.planner.upsertCanonicalItem(try LocalCompositionFixture.item(revision: 2))
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.blocks == [prior])
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(store.localCompositionStatus.message.contains("changed"))
    }

    @Test("helper failure is explicit, offline, and leaves blocks and publication untouched")
    func helperFailureHasNoSideEffectsOrNetworkFallback() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let token = "local-helper-failure-\(UUID().uuidString)"
        URLProtocolStub.storage.reset(key: token)
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: ThrowingLocalComposer(),
            now: { now }
        )

        #expect(!(await store.recomposeLocally()))

        #expect(context.planner.blocks == [prior])
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).isEmpty)
        #expect(store.localCompositionStatus.message.contains("normal Sync"))
        #expect(store.localCompositionStatus.message.contains("No network request"))
    }

    @Test("local success replaces prior server preview evidence")
    func localSuccessClearsPriorServerTransientState() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let token = "local-to-server-failure"
        let matchingConfigurationIdentifier =
            "https://api.example.com/gateway|auth=static-v1:e33e31f9fe3a55cde7bd83e095f8351e3192dfd3d1aa1a0524306fadfe1fe1af"
        let context = try Self.makePlanner(
            now: now,
            item: item,
            configurationIdentifier: matchingConfigurationIdentifier
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let serverPreview = try LocalCompositionFixture.serverRejectedPreview(
            item: item,
            request: LocalCompositionFixture.scheduleRequest(asOf: now)
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"complete-cursor","has_more":false}"#.utf8)
            ),
            .init(statusCode: 200, body: try encoder.encode(serverPreview))
        )
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: RecordingLocalComposer(),
            now: { now }
        )

        await store.sync()
        #expect(store.lastPreview != nil)
        #expect(!store.warnings.isEmpty)
        #expect(context.planner.schedulePreviewProvenance != nil)
        #expect(store.canRecomposeLocally)

        #expect(await store.recomposeLocally())

        #expect(store.lastPreview == nil)
        #expect(store.warnings.isEmpty)
        #expect(context.planner.schedulePreviewProvenance == nil)
        #expect(context.planner.localScheduleCompositionProvenance != nil)
    }

    @Test("a failed normal sync cannot keep transient local composition evidence")
    func failedNormalSyncClearsTransientLocalComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let token = "local-to-server-failure"
        let matchingConfigurationIdentifier =
            "https://api.example.com/gateway|auth=static-v1:e33e31f9fe3a55cde7bd83e095f8351e3192dfd3d1aa1a0524306fadfe1fe1af"
        let context = try Self.makePlanner(
            now: now,
            item: item,
            configurationIdentifier: matchingConfigurationIdentifier
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: RecordingLocalComposer(),
            now: { now }
        )
        #expect(await store.recomposeLocally())
        if case .composed = store.localCompositionStatus {} else {
            Issue.record("Expected local composition evidence before normal sync")
        }
        #expect(store.lastLocalComposition != nil)
        #expect(store.lastLocalCompositionScore != nil)

        await store.sync()

        #expect(store.status.isFailure)
        if case .composed = store.localCompositionStatus {
            Issue.record("Failed normal sync must not claim a current local composition")
        }
        #expect(store.lastLocalComposition == nil)
        #expect(store.lastLocalCompositionScore == nil)
        #expect(store.localCompositionWarnings.isEmpty)
        #expect(context.planner.localScheduleCompositionProvenance != nil)
        #expect(context.planner.canonicalPreviewFreshnessIssue != nil)
        #expect(URLProtocolStub.storage.requests(for: token).count == 1)
    }

    @Test("a persistence race rolls local blocks and provenance back together")
    func persistenceFailureRollsBackAtomicInstall() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()

        let otherWriter = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        otherWriter.showCompleted.toggle()
        otherWriter.flushPersistence()
        #expect(otherWriter.persistenceError == nil)
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.persistenceError == .concurrentModification)
        #expect(context.planner.blocks == [prior])
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(try context.persistence.load()?.localScheduleCompositionProvenance == nil)
    }

    @Test("a server schedule replaces local provenance and origin")
    func serverInstallClearsLocalProvenance() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let context = try Self.makePlanner(now: now, item: item)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = Self.makeStore(
            planner: context.planner,
            composer: RecordingLocalComposer(),
            now: now
        )
        #expect(await store.recomposeLocally())
        #expect(context.planner.localScheduleCompositionProvenance != nil)

        let publication = try Self.pendingSchedulePublication(item: item, now: now)
        try context.planner.persistPendingSchedulePublication(publication)
        let serverBlocks = context.planner.blocks.compactMap { block -> ScheduleBlock? in
            guard block.syncOrigin == .localComposition else { return nil }
            var serverBlock = block
            serverBlock.syncOrigin = block.sourceItemID == nil
                ? .externalPreview
                : .canonicalPreview
            return serverBlock
        }
        #expect(serverBlocks.count == publication.preview.plan.blocks.count)
        let revisionID = UUID(uuidString: "b3000000-0000-4000-8000-000000000003")!
        let request = publication.preparedRequest.request.schedule
        let revision = DayWeavePublishedScheduleRevision(
            id: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: publication.preview.inputDigest,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName,
            publishedAt: now
        )
        #expect(throws: PlannerSchedulePublicationError.replayedReceiptCannotAuthorize) {
            try context.planner.commitPendingSchedulePublication(
                publication,
                blocks: serverBlocks,
                response: .init(revision: revision, replayed: true)
            )
        }
        #expect(context.planner.pendingSchedulePublication == publication)
        #expect(context.planner.publishedScheduleProof == nil)
        try context.planner.commitPendingSchedulePublication(
            publication,
            blocks: serverBlocks,
            response: .init(revision: revision, replayed: false)
        )

        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.schedulePreviewProvenance == publication.provenance)
        #expect(context.planner.publishedScheduleProof != nil)
        #expect(context.planner.blocks.first { $0.sourceItemID == item.id }?.syncOrigin == .canonicalPreview)
        let loaded = try context.persistence.load()
        let restored = try #require(loaded)
        #expect(restored.localScheduleCompositionProvenance == nil)
        #expect(restored.schedulePreviewProvenance == publication.provenance)
        #expect(restored.publishedScheduleProof == context.planner.publishedScheduleProof)
    }

    @Test("restored, old, and revision-mismatched local schedules are not actionable")
    func localFreshnessProtectsMutationAndExecution() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let context = try Self.makePlanner(now: now, item: item)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = Self.makeStore(
            planner: context.planner,
            composer: RecordingLocalComposer(),
            now: now
        )
        #expect(await store.recomposeLocally())
        let freshBlock = try #require(context.planner.blocks.first { $0.sourceItemID == item.id })
        #expect(context.planner.canMutate(freshBlock))
        #expect(context.planner.canonicalScheduleBlockActionabilityIssue(freshBlock) != nil)

        let restoredPlanner = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        let restoredBlock = try #require(restoredPlanner.blocks.first { $0.sourceItemID == item.id })
        #expect(!restoredPlanner.canMutate(restoredBlock))
        #expect(restoredPlanner.canonicalScheduleBlockActionabilityIssue(restoredBlock) != nil)

        context.planner.upsertCanonicalItem(try LocalCompositionFixture.item(revision: 2))
        #expect(!context.planner.canMutate(freshBlock))
        #expect(context.planner.canonicalScheduleBlockActionabilityIssue(freshBlock) != nil)

        let oldProvenance = LocalCompositionFixture.provenance(
            configurationIdentifier: Self.configurationIdentifier,
            item: item,
            generatedAt: now.addingTimeInterval(-7 * 3_600),
            asOf: now
        )
        let oldBlock = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .localComposition
        )
        let oldPlanner = PlannerStore(
            blocks: [oldBlock],
            canonicalItems: [item],
            canonicalDeltaCursor: "cursor-old",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            localScheduleCompositionProvenance: oldProvenance,
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(!oldPlanner.canMutate(oldBlock))
        #expect(oldPlanner.canonicalScheduleBlockActionabilityIssue(oldBlock) != nil)
    }

    @Test("schema 11 migrates without inventing local evidence and mixed origins fail closed")
    func snapshotMigrationAndMixedOriginValidation() throws {
        let now = Date(timeIntervalSince1970: 1_788_112_000)
        let item = try LocalCompositionFixture.item(revision: 1)
        let schemaEleven = Self.snapshot(
            schemaVersion: 11,
            now: now,
            item: item,
            blocks: [],
            serverProvenance: nil,
            localProvenance: nil
        )
        let migrated = try schemaEleven.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.localScheduleCompositionProvenance == nil)

        let localProvenance = LocalCompositionFixture.provenance(
            configurationIdentifier: Self.configurationIdentifier,
            item: item,
            generatedAt: now,
            asOf: now
        )
        let localBlock = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .localComposition
        )
        let serverProvenance = LocalCompositionFixture.serverProvenance(
            configurationIdentifier: Self.configurationIdentifier,
            generatedAt: now,
            asOf: now
        )
        let serverWithLocalBlock = Self.snapshot(
            now: now,
            item: item,
            blocks: [localBlock],
            serverProvenance: serverProvenance,
            localProvenance: nil
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try serverWithLocalBlock.migratedToCurrentSchema()
        }

        var canonicalBlock = localBlock
        canonicalBlock.syncOrigin = .canonicalPreview
        let localWithServerBlock = Self.snapshot(
            now: now,
            item: item,
            blocks: [canonicalBlock],
            serverProvenance: nil,
            localProvenance: localProvenance
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try localWithServerBlock.migratedToCurrentSchema()
        }

        let mismatchedConfiguration = LocalCompositionFixture.provenance(
            configurationIdentifier: "https://other.example.invalid",
            item: item,
            generatedAt: now,
            asOf: now
        )
        let mismatchedSnapshot = Self.snapshot(
            now: now,
            item: item,
            blocks: [localBlock],
            serverProvenance: nil,
            localProvenance: mismatchedConfiguration
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try mismatchedSnapshot.migratedToCurrentSchema()
        }
    }

    private static func makeStore(
        planner: PlannerStore,
        composer: any LocalScheduleComposing,
        now: Date,
        habitProvider: (any HabitCompositionCheckpointProviding)? = nil
    ) -> CanonicalSyncStore {
        CanonicalSyncStore(
            planner: planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: "local-composition-test-token"),
            session: URLProtocolStub.makeSession(),
            localComposer: composer,
            habitCompositionProvider: habitProvider,
            now: { now }
        )
    }

    private static let completedPlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000001")!
    private static let partialPlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000002")!
    private static let skippedPlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000003")!
    private static let missedSourcePlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000004")!
    private static let missedTargetPlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000005")!
    private static let missedChainTargetPlannerOccurrenceID =
        UUID(uuidString: "c1000000-0000-5000-8000-000000000006")!

    private static func habitCheckpoint(
        item: DayWeaveCanonicalItem,
        now: Date,
        configurationIdentifier: String? = Self.configurationIdentifier,
        deltaCursor: String? = "habit-cursor",
        deltaCaughtUp: Bool = true,
        pendingMutationIDs: [UUID] = [],
        hasActiveOperation: Bool = false,
        operationGeneration: UInt64 = 1,
        sourceItemRevision: UInt64 = 1
    ) -> HabitCompositionCheckpoint {
        let windowStart = now.addingTimeInterval(-1_800)
        let windowEnd = now.addingTimeInterval(7_200)
        func occurrence(
            id: UUID,
            plannerID: UUID,
            status: DayWeaveHabitOutcomeStatus,
            progress: UInt16,
            duration: UInt64?
        ) -> HabitCompositionCheckpoint.Occurrence {
            .init(
                id: id,
                habitID: item.id,
                plannerOccurrenceID: plannerID,
                sourceItemRevision: sourceItemRevision,
                nominalStart: now,
                windowStart: windowStart,
                windowEnd: windowEnd,
                expectedDurationSeconds: duration,
                outcome: .init(
                    revision: 1,
                    status: status,
                    progressBasisPoints: progress,
                    occurredAt: now
                )
            )
        }
        return .init(
            configurationIdentifier: configurationIdentifier,
            deltaCursor: deltaCursor,
            deltaCaughtUp: deltaCaughtUp,
            occurrences: [
                occurrence(
                    id: UUID(uuidString: "c2000000-0000-4000-8000-000000000001")!,
                    plannerID: completedPlannerOccurrenceID,
                    status: .completed,
                    progress: 10_000,
                    duration: 1_800
                ),
                occurrence(
                    id: UUID(uuidString: "c2000000-0000-4000-8000-000000000002")!,
                    plannerID: partialPlannerOccurrenceID,
                    status: .partial,
                    progress: 2_500,
                    duration: 1_800
                ),
                occurrence(
                    id: UUID(uuidString: "c2000000-0000-4000-8000-000000000003")!,
                    plannerID: skippedPlannerOccurrenceID,
                    status: .skipped,
                    progress: 0,
                    duration: 1_800
                ),
            ],
            pauses: [.init(
                id: UUID(uuidString: "c3000000-0000-4000-8000-000000000001")!,
                habitID: item.id,
                revision: 1,
                startedAt: now.addingTimeInterval(-900),
                endedAt: now.addingTimeInterval(900)
            )],
            pendingMutationIDs: pendingMutationIDs,
            hasActiveOperation: hasActiveOperation,
            operationGeneration: operationGeneration
        )
    }

    private static func missedCheckpoint(
        item: DayWeaveCanonicalItem,
        now: Date,
        action: MissedCompositionAction,
        sourceItemRevision: UInt64,
        sourceOutcomeStatus: DayWeaveHabitOutcomeStatus? = nil,
        pauseWindow: (start: Date, end: Date)? = nil,
        sourceNominalStart: Date? = nil,
        targetOutcomeStatus: DayWeaveHabitOutcomeStatus? = .partial,
        includeSourceIdentity: Bool = true,
        useMatchingPolicyFingerprint: Bool = true,
        targetMissedAction: DayWeaveHabitMissedResolutionAction? = nil,
        targetNominalStart: Date? = nil,
        additionalOccurrences: [HabitCompositionCheckpoint.Occurrence] = []
    ) -> HabitCompositionCheckpoint {
        let sourceEvidenceID = UUID(uuidString: "c2000000-0000-4000-8000-000000000004")!
        let targetEvidenceID = UUID(uuidString: "c2000000-0000-4000-8000-000000000005")!
        let nominalStart = sourceNominalStart ?? (action == .skipSource
            ? now.addingTimeInterval(-3_600)
            : now.addingTimeInterval(-86_400))
        let localDate = DayWeaveLocalDate.containing(
            nominalStart,
            timezoneName: item.timezoneName
        )!
        let resolutionAction: DayWeaveHabitMissedResolutionAction
        switch action {
        case .skipSource:
            resolutionAction = .skip
        case let .carry(start, end):
            resolutionAction = .carry(windowStart: start, windowEnd: end)
        case .reducePartialTarget:
            resolutionAction = .reduceFrequency(
                suppressedPlannerOccurrenceIDs: [missedTargetPlannerOccurrenceID]
            )
        }
        let updatedAt: Date
        if case let .carry(start, _) = action { updatedAt = start } else { updatedAt = now }
        let resolution = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: sourceEvidenceID,
            habitID: item.id,
            sourcePlannerOccurrenceID: missedSourcePlannerOccurrenceID,
            revision: 2,
            configuredPolicy: .ask,
            action: resolutionAction,
            createdAt: min(updatedAt, now).addingTimeInterval(-60),
            updatedAt: updatedAt
        )
        let effectiveSourceOutcomeStatus = sourceOutcomeStatus
            ?? (action == .skipSource ? .partial : nil)
        let sourceOutcome: HabitCompositionCheckpoint.Outcome? = effectiveSourceOutcomeStatus.map {
            status in
            let progressBasisPoints: UInt16
            switch status {
            case .completed: progressBasisPoints = 10_000
            case .partial: progressBasisPoints = 2_500
            case .skipped, .unresolved: progressBasisPoints = 0
            }
            return .init(
                revision: 1,
                status: status,
                progressBasisPoints: progressBasisPoints,
                occurredAt: now
            )
        }
        let source = HabitCompositionCheckpoint.Occurrence(
            id: sourceEvidenceID,
            habitID: item.id,
            plannerOccurrenceID: missedSourcePlannerOccurrenceID,
            sourceItemRevision: sourceItemRevision,
            policyFingerprint: useMatchingPolicyFingerprint
                ? item.habitPolicyFingerprint
                : "sha256:\(String(repeating: "f", count: 64))",
            nominalStart: nominalStart,
            windowStart: nominalStart.addingTimeInterval(-1_800),
            windowEnd: nominalStart.addingTimeInterval(3_600),
            expectedDurationSeconds: 1_800,
            outcome: sourceOutcome,
            missedResolution: resolution,
            identity: includeSourceIdentity
                ? .object([
                    "type": .string("calendar_day"),
                    "date": .string(localDate.rawValue),
                    "bucket_ordinal": .number(.init(UInt64(0))),
                ])
                : nil,
            nominalEnd: nominalStart.addingTimeInterval(1_800),
            localDate: localDate
        )
        var occurrences = [source]
        if action == .reducePartialTarget {
            let targetStart = targetNominalStart ?? now.addingTimeInterval(86_400)
            let targetDate = DayWeaveLocalDate.containing(
                targetStart,
                timezoneName: item.timezoneName
            )!
            let targetOutcome = targetOutcomeStatus.map { status in
                let progressBasisPoints: UInt16
                switch status {
                case .completed: progressBasisPoints = 10_000
                case .partial: progressBasisPoints = 5_000
                case .skipped, .unresolved: progressBasisPoints = 0
                }
                return HabitCompositionCheckpoint.Outcome(
                    revision: 1,
                    status: status,
                    progressBasisPoints: progressBasisPoints,
                    occurredAt: now
                )
            }
            let targetResolution = targetMissedAction.map { action in
                let targetUpdatedAt: Date
                if case let .carry(windowStart, _) = action {
                    targetUpdatedAt = windowStart
                } else {
                    targetUpdatedAt = now
                }
                return DayWeaveHabitMissedResolution(
                    occurrenceEvidenceID: targetEvidenceID,
                    habitID: item.id,
                    sourcePlannerOccurrenceID: missedTargetPlannerOccurrenceID,
                    revision: 2,
                    configuredPolicy: .ask,
                    action: action,
                    createdAt: targetUpdatedAt.addingTimeInterval(-60),
                    updatedAt: targetUpdatedAt
                )
            }
            occurrences.append(.init(
                id: targetEvidenceID,
                habitID: item.id,
                plannerOccurrenceID: missedTargetPlannerOccurrenceID,
                sourceItemRevision: sourceItemRevision,
                policyFingerprint: item.habitPolicyFingerprint,
                nominalStart: targetStart,
                windowStart: targetStart.addingTimeInterval(-1_800),
                windowEnd: targetStart.addingTimeInterval(3_600),
                expectedDurationSeconds: 1_800,
                outcome: targetOutcome,
                missedResolution: targetResolution,
                identity: .object([
                    "type": .string("calendar_day"),
                    "date": .string(targetDate.rawValue),
                    "bucket_ordinal": .number(.init(UInt64(0))),
                ]),
                nominalEnd: targetStart.addingTimeInterval(1_800),
                localDate: targetDate
            ))
        }
        occurrences.append(contentsOf: additionalOccurrences)
        return .init(
            configurationIdentifier: configurationIdentifier,
            deltaCursor: "habit-missed-cursor",
            deltaCaughtUp: true,
            occurrences: occurrences,
            pauses: pauseWindow.map { window in
                [.init(
                    id: UUID(uuidString: "c3000000-0000-4000-8000-000000000002")!,
                    habitID: item.id,
                    revision: 1,
                    startedAt: window.start,
                    endedAt: window.end
                )]
            } ?? [],
            pendingMutationIDs: [],
            hasActiveOperation: false,
            operationGeneration: 1
        )
    }

    private static func skipException(itemID: UUID, occurrenceID: UUID) -> JSONValue {
        .object([
            "item_id": .string(itemID.uuidString.lowercased()),
            "selector": .object([
                "type": .string("occurrence"),
                "id": .string(occurrenceID.uuidString.lowercased()),
            ]),
            "action": .object(["type": .string("skip")]),
        ])
    }

    private static func publishedOccurrences(
        for checkpoint: HabitCompositionCheckpoint,
        stateOverrides: [UUID: String] = [:],
        omitting omittedIDs: Set<UUID> = []
    ) -> [DayWeavePublishedScheduleOccurrenceProof] {
        checkpoint.occurrences.compactMap { occurrence in
            guard !omittedIDs.contains(occurrence.plannerOccurrenceID) else {
                return nil
            }
            return .init(
                plannerOccurrenceID: occurrence.plannerOccurrenceID,
                seriesItemID: occurrence.habitID,
                state: stateOverrides[occurrence.plannerOccurrenceID] ?? "generated"
            )
        }
        .sorted { $0.plannerOccurrenceID.uuidString < $1.plannerOccurrenceID.uuidString }
    }

    private static func isMissedSourceMove(_ exception: JSONValue) -> Bool {
        guard case let .object(fields) = exception,
              case let .object(selector)? = fields["selector"],
              selector["id"] == .string(missedSourcePlannerOccurrenceID.uuidString.lowercased()),
              case let .object(action)? = fields["action"] else { return false }
        return action["type"] == .string("move")
    }

    private static func exceptionAction(
        _ exception: JSONValue,
        occurrenceID: UUID
    ) -> String? {
        guard case let .object(fields) = exception,
              case let .object(selector)? = fields["selector"],
              selector["id"] == .string(occurrenceID.uuidString.lowercased()),
              case let .object(action)? = fields["action"],
              case let .string(type)? = action["type"] else { return nil }
        return type
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private static func makePlanner(
        now: Date,
        item: DayWeaveCanonicalItem,
        additionalItems: [DayWeaveCanonicalItem] = [],
        blocks: [ScheduleBlock] = [],
        cursor: String? = "complete-cursor",
        configurationIdentifier: String? = Self.configurationIdentifier,
        publishedOccurrences: [DayWeavePublishedScheduleOccurrenceProof]? = nil,
        publishedProofVersion: Int = DayWeavePublishedScheduleProof.currentVersion,
        recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove] = [],
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? = nil,
        pendingCanonicalMutations: [PendingCanonicalMutation] = [],
        pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation] = [],
        pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = [],
        googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal? = nil
    ) throws -> (directory: URL, persistence: EncryptedPlannerPersistence, planner: PlannerStore) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveLocalComposition-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let schedulePreviewProvenance = publishedOccurrences.flatMap { _ in
            configurationIdentifier.map { configurationIdentifier in
                SchedulePreviewProvenance(
                    configurationIdentifier: configurationIdentifier,
                    generatedAt: now,
                    asOf: now,
                    horizonStart: now.addingTimeInterval(-7 * 86_400),
                    horizonEnd: now.addingTimeInterval(8 * 86_400),
                    timezoneName: item.timezoneName
                )
            }
        }
        let publishedScheduleProof = publishedOccurrences.flatMap { occurrences in
            schedulePreviewProvenance.map { provenance in
                let revisionID = UUID()
                let publishedBlocks = blocks
                    .filter {
                        $0.syncOrigin == .canonicalPreview
                            || $0.syncOrigin == .externalPreview
                    }
                    .compactMap { DayWeavePublishedScheduleBlockProof(block: $0) }
                    .sorted { $0.id.uuidString < $1.id.uuidString }
                return DayWeavePublishedScheduleProof(
                    version: publishedProofVersion,
                    configurationIdentifier: provenance.configurationIdentifier,
                    revisionID: revisionID,
                    revision: "1:\(revisionID.uuidString.lowercased())",
                    revisionNumber: 1,
                    inputDigest: "sha256:\(String(repeating: "d", count: 64))",
                    asOf: provenance.asOf,
                    horizonStart: provenance.horizonStart,
                    horizonEnd: provenance.horizonEnd,
                    timezoneName: provenance.timezoneName,
                    publishedAt: now,
                    publishedBlocks: publishedBlocks,
                    publishedOccurrences: publishedProofVersion
                        == DayWeavePublishedScheduleProof.currentVersion
                        ? occurrences
                        : nil
                )
            }
        }
        let planner = PlannerStore(
            blocks: blocks,
            canonicalItems: [item] + additionalItems,
            canonicalDeltaCursor: cursor,
            pendingCanonicalMutations: pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
            recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
            canonicalConfigurationIdentifier: configurationIdentifier,
            schedulePreviewProvenance: schedulePreviewProvenance,
            publishedScheduleProof: publishedScheduleProof,
            pendingSchedulePublication: pendingSchedulePublication,
            pendingProposalApplicationMutation: pendingProposalApplicationMutation,
            pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
            googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        return (directory, persistence, planner)
    }

    private static func pendingState(
        _ variant: PendingJournalVariant,
        item: DayWeaveCanonicalItem,
        now: Date
    ) throws -> PendingJournalState {
        var state = PendingJournalState()
        switch variant {
        case .schedulePublication:
            state.schedulePublication = try pendingSchedulePublication(item: item, now: now)
        case .proposalApplication:
            let reviewHash = "sha256:\(String(repeating: "b", count: 64))"
            let body = try JSONEncoder().encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: reviewHash
            ))
            state.proposalApplication = .apply(
                configurationIdentifier: configurationIdentifier,
                proposalIDs: [UUID()],
                proposalRevisions: [1],
                expectedCommandIDs: [UUID()],
                previewID: UUID(),
                expectedReviewHash: reviewHash,
                requestBody: body,
                idempotencyKey: "local-composition-proposal-gate",
                createdAt: now
            )
        case .googleRecovery:
            state.googleRecovery = try GoogleOutboundRecoveryJournal(
                operationGeneration: 1,
                configurationIdentifier: "google-local-composition-gate",
                accountID: UUID(),
                collectionID: UUID(),
                itemID: item.id,
                expectedItemRevision: item.revision,
                operation: .upsert,
                intentExpiresAt: now.addingTimeInterval(30 * 60),
                createdAt: now
            )
        case .statusMutation:
            state.statusMutations = [PendingCanonicalMutation(
                id: UUID(),
                itemID: item.id,
                occurrenceID: nil,
                sessionIndex: 0,
                desiredStatus: .completed,
                baseRevision: item.revision,
                createdAt: now,
                disposition: .pending,
                diagnostic: nil
            )]
        case .sensitivityMutation:
            state.sensitivityMutations = [PendingCanonicalSensitivityMutation(
                id: UUID(),
                itemID: item.id,
                desiredIsSensitive: true,
                baseRevision: item.revision,
                createdAt: now,
                disposition: .pending,
                diagnostic: nil
            )]
        case .authoringMutation:
            let draftID = UUID()
            state.authoringMutations = [DayWeavePendingCanonicalAuthoringMutation(
                itemID: draftID,
                operation: .create,
                draft: DayWeaveCanonicalItemDraft(
                    title: "Queued local draft",
                    timezoneName: "Europe/Madrid",
                    durationSeconds: 1_800
                ),
                createdAt: now
            )]
        }
        return state
    }

    private static func pendingSchedulePublication(
        item: DayWeaveCanonicalItem,
        now: Date
    ) throws -> PendingSchedulePublication {
        let request = LocalCompositionFixture.scheduleRequest(asOf: now)
        let digest = "sha256:\(String(repeating: "c", count: 64))"
        let preview = try LocalCompositionFixture.serverPreview(
            item: item,
            request: request,
            inputDigest: digest
        )
        let publish = DayWeaveSchedulePublishRequest(
            idempotencyKey: UUID(),
            expectedInputDigest: digest,
            schedule: request
        )
        return PendingSchedulePublication(
            configurationIdentifier: configurationIdentifier,
            preparedRequest: .init(
                request: publish,
                body: Data("{}".utf8),
                bodySHA256: String(repeating: "d", count: 64)
            ),
            preview: preview,
            message: "Pending server publication",
            provenance: LocalCompositionFixture.serverProvenance(
                configurationIdentifier: configurationIdentifier,
                generatedAt: now,
                asOf: now
            ),
            preparedAt: now
        )
    }

    private static func snapshot(
        schemaVersion: Int = PlannerSnapshot.currentSchemaVersion,
        now: Date,
        item: DayWeaveCanonicalItem,
        blocks: [ScheduleBlock],
        serverProvenance: SchedulePreviewProvenance?,
        localProvenance: LocalScheduleCompositionProvenance?
    ) -> PlannerSnapshot {
        PlannerSnapshot(
            schemaVersion: schemaVersion,
            savedAt: now,
            destination: .today,
            selectedBlockID: blocks.first?.id,
            blocks: blocks,
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "fixture",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true,
            canonicalItems: [item],
            canonicalDeltaCursor: "complete-cursor",
            canonicalTombstoneRevisions: [:],
            completedOccurrenceIDs: [],
            pendingCanonicalMutations: [],
            pendingCanonicalSensitivityMutations: [],
            recurrenceSessionOutcomes: [],
            canonicalConfigurationIdentifier: configurationIdentifier,
            schedulePreviewProvenance: serverProvenance,
            localScheduleCompositionProvenance: localProvenance,
            proposalApplicationReceipts: [],
            pendingCanonicalAuthoringMutations: [],
            canonicalTrash: [],
            localCaptureDiagnostics: [:],
            executionState: .empty
        )
    }
}

private enum MissedCompositionAction: Equatable {
    case skipSource
    case carry(start: Date, end: Date)
    case reducePartialTarget
}

private enum PendingJournalVariant: String, CaseIterable {
    case schedulePublication
    case proposalApplication
    case googleRecovery
    case statusMutation
    case sensitivityMutation
    case authoringMutation
}

private struct PendingJournalState {
    var schedulePublication: PendingSchedulePublication?
    var proposalApplication: DayWeavePendingProposalApplicationMutation?
    var googleRecovery: GoogleOutboundRecoveryJournal?
    var statusMutations: [PendingCanonicalMutation] = []
    var sensitivityMutations: [PendingCanonicalSensitivityMutation] = []
    var authoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = []
}

private struct LocalCompositionConfigurationStore: SuggestionAPIConfigurationStoring {
    func loadBaseURL() -> String? { "https://api.example.com/gateway" }
    func saveBaseURL(_ value: String) {}
}

private enum LocalCompositionFixture {
    static let itemID = UUID(uuidString: "b1000000-0000-4000-8000-000000000001")!
    static let plannedBlockID = UUID(uuidString: "b2000000-0000-4000-8000-000000000002")!

    static func item(
        revision: UInt64,
        kind: String = "task",
        isExecutable: Bool = true,
        id: UUID = itemID,
        parentID: UUID? = nil
    ) throws -> DayWeaveCanonicalItem {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let encodedParentID = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data("""
        {"id":"\(id.uuidString.lowercased())","is_sensitive":false,"kind":"\(kind)",
        "status":"scheduled","title":"Compose locally","notes":"private notes",
        "timezone_name":"Europe/Madrid","duration_seconds":1800,"deadline_at":null,
        "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
        "parent_id":\(encodedParentID),"sibling_order":0,"is_executable":\(isExecutable),"revision":\(revision),
        "created_at":"2026-08-30T08:00:00Z","updated_at":"2026-08-30T08:00:00Z",
        "completed_at":null,"deleted_at":null}
        """.utf8))
    }

    static func makeComposition(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) -> LocalScheduleComposition {
        let item = canonicalItems[0]
        guard let placement = schedule.availability.first(where: {
            $0.end.timeIntervalSince($0.start) >= 1_800
        }) else {
            preconditionFailure("The local-composition fixture needs a 30-minute availability window")
        }
        let start = placement.start
        let end = start.addingTimeInterval(1_800)
        let fixedBlocks = externalBlocks(for: schedule)
        let plan = DayWeaveSchedulePreview.Plan(
            asOf: schedule.asOf,
            horizonStart: schedule.horizonStart,
            horizonEnd: schedule.horizonEnd,
            blocks: (fixedBlocks + [.init(
                id: plannedBlockID,
                isSensitive: false,
                itemID: item.id,
                occurrenceID: nil,
                externalBlockID: nil,
                title: item.title,
                start: start,
                end: end,
                sessionIndex: 0,
                kind: "planned",
                explanations: [.init(code: "local", message: "Composed on this Mac.")]
            )]).sorted {
                if $0.start != $1.start { return $0.start < $1.start }
                return $0.id.uuidString < $1.id.uuidString
            },
            unscheduled: [],
            decisions: [],
            violations: [],
            score: .init(
                scheduledMinutes: 30,
                unscheduledMinutes: 0,
                softPenalty: 0,
                movedMinutes: 0
            ),
            occurrences: []
        )
        return LocalScheduleComposition(
            localInputFingerprint: "local-sha256:\(String(repeating: "e", count: 64))",
            sourceItemCount: canonicalItems.count,
            sourceItemRevisions: Dictionary(
                uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
            ),
            acceptedItemCount: canonicalItems.count,
            rejectedItems: [],
            ignoredPreviousAssignments: [],
            plan: plan
        )
    }

    static func externalBlocks(
        for schedule: DayWeaveSchedulePreviewRequest
    ) -> [DayWeaveSchedulePreview.Plan.Block] {
        schedule.fixedBlocks
            .filter { $0.end > schedule.horizonStart && $0.start < schedule.horizonEnd }
            .map { fixed in
                .init(
                    id: fixed.id,
                    isSensitive: fixed.isSensitive,
                    itemID: nil,
                    occurrenceID: nil,
                    externalBlockID: fixed.id,
                    title: fixed.title,
                    start: fixed.start,
                    end: fixed.end,
                    sessionIndex: 0,
                    kind: "external_fixed",
                    explanations: [.init(
                        code: fixed.source,
                        message: "Protected by the local schedule profile."
                    )]
                )
            }
    }

    static func renderedBlock(
        item: DayWeaveCanonicalItem,
        start: Date,
        origin: ScheduleBlockOrigin
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: plannedBlockID,
            title: item.title,
            kind: .task,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: item.notes ?? "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: item.id,
            sourceItemRevision: item.revision,
            sessionIndex: 0,
            syncOrigin: origin,
            previewKind: "planned"
        )
    }

    static func scheduleRequest(asOf: Date) -> DayWeaveSchedulePreviewRequest {
        guard let profile = try? ScheduleProfile.legacyDefault(
            timezoneName: planningTimezone,
            protectedFreeMinutes: 90
        ), let expanded = try? profile.expanded(asOf: asOf) else {
            preconditionFailure("The built-in local-composition profile must expand")
        }
        return .init(
            asOf: asOf,
            horizonStart: expanded.horizonStart,
            horizonEnd: expanded.horizonEnd,
            timezoneName: expanded.timezoneName,
            availability: expanded.availability,
            fixedBlocks: expanded.fixedBlocks,
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
    }

    static func serverPreview(
        item: DayWeaveCanonicalItem,
        request: DayWeaveSchedulePreviewRequest,
        inputDigest: String
    ) throws -> DayWeaveSchedulePreview {
        let composition = makeComposition(canonicalItems: [item], schedule: request)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let plan = try JSONSerialization.jsonObject(with: encoder.encode(composition.plan))
        let object: [String: Any] = [
            "input_digest": inputDigest,
            "source_item_count": 1,
            "accepted_item_count": 1,
            "source_item_revisions": [item.id.uuidString.lowercased(): item.revision],
            "rejected_items": [],
            "ignored_previous_assignments": [],
            "plan": plan,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveSchedulePreview.self, from: data)
    }

    static func serverRejectedPreview(
        item: DayWeaveCanonicalItem,
        request: DayWeaveSchedulePreviewRequest
    ) throws -> DayWeaveSchedulePreview {
        let plan = DayWeaveSchedulePreview.Plan(
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            blocks: externalBlocks(for: request),
            unscheduled: [],
            decisions: [],
            violations: [],
            score: .init(
                scheduledMinutes: 0,
                unscheduledMinutes: 0,
                softPenalty: 0,
                movedMinutes: 0
            ),
            occurrences: []
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let planObject = try JSONSerialization.jsonObject(with: encoder.encode(plan))
        let object: [String: Any] = [
            "input_digest": "sha256:\(String(repeating: "f", count: 64))",
            "source_item_count": 1,
            "accepted_item_count": 0,
            "source_item_revisions": [item.id.uuidString.lowercased(): item.revision],
            "rejected_items": [[
                "item_id": item.id.uuidString.lowercased(),
                "is_sensitive": false,
                "title": item.title,
                "reason": "fixture warning",
            ]],
            "ignored_previous_assignments": [],
            "plan": planObject,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveSchedulePreview.self, from: data)
    }

    static func provenance(
        configurationIdentifier: String,
        item: DayWeaveCanonicalItem,
        generatedAt: Date,
        asOf: Date
    ) -> LocalScheduleCompositionProvenance {
        let request = scheduleRequest(asOf: asOf)
        return .init(
            configurationIdentifier: configurationIdentifier,
            localInputFingerprint: "local-sha256:\(String(repeating: "e", count: 64))",
            generatedAt: generatedAt,
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName,
            sourceItemRevisions: [item.id: item.revision]
        )
    }

    static func serverProvenance(
        configurationIdentifier: String,
        generatedAt: Date,
        asOf: Date
    ) -> SchedulePreviewProvenance {
        let request = scheduleRequest(asOf: asOf)
        return .init(
            configurationIdentifier: configurationIdentifier,
            generatedAt: generatedAt,
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName
        )
    }

    private static var planningTimezone: String {
        let identifier = TimeZone.autoupdatingCurrent.identifier
        return identifier == "GMT" ? "UTC" : identifier
    }
}

@MainActor
private final class HabitCheckpointStub: HabitCompositionCheckpointProviding {
    private(set) var habitCompositionCheckpoint: HabitCompositionCheckpoint
    private var observers: [@MainActor () -> Void] = []

    init(_ checkpoint: HabitCompositionCheckpoint) {
        habitCompositionCheckpoint = checkpoint
    }

    func observeHabitCompositionCheckpointChanges(
        _ observer: @escaping @MainActor () -> Void
    ) {
        observers.append(observer)
    }

    func update(_ checkpoint: HabitCompositionCheckpoint) {
        habitCompositionCheckpoint = checkpoint
        observers.forEach { $0() }
    }

    func replaceWithoutNotification(_ checkpoint: HabitCompositionCheckpoint) {
        habitCompositionCheckpoint = checkpoint
    }
}

private actor RecordingLocalComposer: LocalScheduleComposing {
    private var callCount = 0
    private var request: DayWeaveSchedulePreviewRequest?

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        callCount += 1
        request = schedule
        return LocalCompositionFixture.makeComposition(
            canonicalItems: canonicalItems,
            schedule: schedule
        )
    }

    func calls() -> Int { callCount }
    func lastRequest() -> DayWeaveSchedulePreviewRequest? { request }
}

private actor BlockingLocalComposer: LocalScheduleComposing {
    private var started = false
    private var released = false
    private var continuation: CheckedContinuation<Void, Never>?

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        started = true
        await withCheckedContinuation { continuation in
            if released {
                continuation.resume()
            } else {
                self.continuation = continuation
            }
        }
        return LocalCompositionFixture.makeComposition(
            canonicalItems: canonicalItems,
            schedule: schedule
        )
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        released = true
        continuation?.resume()
        continuation = nil
    }
}

private enum LocalCompositionTestError: Error {
    case helperFailed
}

private struct ThrowingLocalComposer: LocalScheduleComposing {
    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        throw LocalCompositionTestError.helperFailed
    }
}
#endif
