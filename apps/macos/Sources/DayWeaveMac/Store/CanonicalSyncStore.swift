import Foundation

enum CanonicalSyncStatus: Equatable, Sendable {
    case configurationRequired(String)
    case ready
    case syncing(String)
    case online(updatedAt: Date, message: String)
    case failed(String)

    var message: String {
        switch self {
        case let .configurationRequired(message), let .syncing(message), let .failed(message): message
        case .ready: "Canonical sync is configured."
        case let .online(updatedAt, message):
            "\(message) · \(updatedAt.formatted(date: .omitted, time: .shortened))"
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

@MainActor
final class CanonicalSyncStore: ObservableObject {
    static let maximumDeltaChanges = 20_000
    static let maximumRetainedDeltaBytes = 32 * 1_048_576
    static let maximumDeltaCursorBytes = 4_096
    static let maximumCreatePushesPerSync = 100
    static let maximumAuthoringPushesPerSync = 100
    static let maximumStatusPushesPerSync = 100
    static let maximumPreviousAssignments = 10_000
    static let maximumPreviousAssignmentBlocks = 50_000
    @Published private(set) var status: CanonicalSyncStatus
    @Published private(set) var isSyncing = false
    @Published private(set) var lastPreview: DayWeaveSchedulePreview?
    @Published private(set) var warnings: [String] = []

    private let planner: PlannerStore
    private let configurationStore: any SuggestionAPIConfigurationStoring
    private let tokenStore: any BearerTokenStoring
    private let authCoordinator: DurableAuthCoordinator?
    private let session: URLSession
    private let now: @Sendable () -> Date
    private let createPushLimit: Int
    private let authoringPushLimit: Int
    private let statusPushLimit: Int
    private let previousAssignmentLimit: Int
    private let previousAssignmentBlockLimit: Int
    private var configurationGeneration: UInt64 = 0
    private var activeSyncID: UUID?
    private var activeSyncTask: Task<Void, Never>?
    private var lastSuccessfulSyncID: UUID?
    private var lastFreshCompositionSyncID: UUID?

    init(
        planner: PlannerStore,
        configurationStore: any SuggestionAPIConfigurationStoring = UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        authCoordinator: DurableAuthCoordinator? = nil,
        session: URLSession = makeDayWeaveEphemeralSession(),
        createPushLimit: Int = CanonicalSyncStore.maximumCreatePushesPerSync,
        authoringPushLimit: Int = CanonicalSyncStore.maximumAuthoringPushesPerSync,
        statusPushLimit: Int = CanonicalSyncStore.maximumStatusPushesPerSync,
        previousAssignmentLimit: Int = CanonicalSyncStore.maximumPreviousAssignments,
        previousAssignmentBlockLimit: Int = CanonicalSyncStore.maximumPreviousAssignmentBlocks,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.planner = planner
        self.configurationStore = configurationStore
        self.tokenStore = tokenStore
        self.authCoordinator = authCoordinator
        self.session = session
        self.createPushLimit = max(0, createPushLimit)
        self.authoringPushLimit = max(0, authoringPushLimit)
        self.statusPushLimit = max(0, statusPushLimit)
        self.previousAssignmentLimit = max(0, previousAssignmentLimit)
        self.previousAssignmentBlockLimit = max(0, previousAssignmentBlockLimit)
        self.now = now
        status = .ready
        reloadConfigurationStatus()
    }

    var isConfigured: Bool {
        makeClient(reportFailure: false) != nil
    }

    func configurationDidChange() {
        configurationGeneration &+= 1
        activeSyncTask?.cancel()
        planner.invalidateCanonicalPreview()
        lastPreview = nil
        warnings = []
        reloadConfigurationStatus()
        if planner.pendingSchedulePublication != nil {
            status = .failed(
                "A schedule publication is awaiting exact recovery. Restore its original API configuration and authentication, then sync before replacing or resetting this connection."
            )
        }
    }

    func resetCanonicalSyncState() {
        guard activeSyncID == nil else { return }
        guard planner.pendingSchedulePublication == nil else {
            status = .failed(
                "An exact schedule publication may already be committed remotely. Restore its original API configuration and authentication, then sync to recover it before resetting local state."
            )
            return
        }
        planner.resetCanonicalSyncState()
        lastPreview = nil
        warnings = []
        reloadConfigurationStatus()
    }

    func sync() async {
        _ = await syncReportingSuccess()
    }

    private func syncReportingSuccess() async -> Bool {
        guard await waitForCanonicalMutationFence() else { return false }
        // Invalidate before reading configuration or Keychain state. An
        // out-of-process change must not leave an old preview actionable just
        // because client construction fails.
        planner.invalidateCanonicalPreview()
        lastPreview = nil
        warnings = []
        guard let client = makeClient(reportFailure: true) else {
            planner.endCanonicalSync()
            return false
        }
        if let pending = planner.pendingSchedulePublication,
           pending.configurationIdentifier != client.configurationIdentifier {
            planner.endCanonicalSync()
            status = .failed(
                "The exact schedule publication belongs to another API URL or credential session. Restore that original configuration and authentication, then sync to recover it; it was not discarded."
            )
            return false
        }
        do {
            try planner.prepareCanonicalSync(
                configurationIdentifier: client.configurationIdentifier
            )
        } catch {
            planner.endCanonicalSync()
            status = .failed(error.localizedDescription)
            return false
        }

        let operationID = UUID()
        let generation = configurationGeneration
        activeSyncID = operationID
        isSyncing = true
        status = .syncing("Pulling canonical item revisions…")
        let task = Task<Void, Never> { @MainActor [weak self] in
            guard let self else { return }
            await self.performSync(
                client: client,
                operationID: operationID,
                generation: generation
            )
        }
        activeSyncTask = task
        await task.value
        return lastSuccessfulSyncID == operationID
    }

    /// Import completion must survive an unrelated pending publication
    /// recovery. A recovered old receipt is successful work, but it does not
    /// prove that newly imported canonical items were pulled and recomposed.
    @discardableResult
    func syncThroughFreshComposition() async -> Bool {
        for _ in 0..<2 {
            guard await syncReportingSuccess(),
                  let successfulID = lastSuccessfulSyncID else { return false }
            if lastFreshCompositionSyncID == successfulID { return true }
        }
        return false
    }

    /// A proposal completion must not lose its required pull/recomposition just
    /// because execution reconciliation owns the shared canonical mutation
    /// fence for a moment. Existing canonical work is also awaited and followed
    /// by this request, so every caller is either cancelled explicitly or gets a
    /// synchronization attempt after the work that caused contention.
    private func waitForCanonicalMutationFence() async -> Bool {
        while !Task.isCancelled {
            guard planner.pendingProposalApplicationMutation == nil else {
                status = .failed(
                    "Recover the exact pending proposal application or undo before synchronizing canonical items."
                )
                return false
            }
            guard planner.canPersistPlan else { return false }
            if let activeSyncTask {
                await activeSyncTask.value
                continue
            }
            if planner.beginCanonicalSync() {
                return true
            }
            guard planner.isCanonicalSyncLocked else { return false }
            do {
                try await Task.sleep(for: .milliseconds(25))
            } catch {
                return false
            }
        }
        return false
    }

    private func performSync(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async {
        defer {
            if activeSyncID == operationID {
                activeSyncTask = nil
                activeSyncID = nil
                isSyncing = false
                planner.endCanonicalSync()
                if generation != configurationGeneration {
                    reloadConfigurationStatus()
                    if planner.pendingSchedulePublication != nil {
                        status = .failed(
                            "A schedule publication is awaiting exact recovery. Restore its original API configuration and authentication, then sync before replacing or resetting this connection."
                        )
                    }
                }
            }
        }

        do {
            var freshPublicationRetryBudget = 1
            if let pending = planner.pendingSchedulePublication {
                status = .syncing("Recovering an exact schedule publication…")
                let recovery = try await recoverPendingSchedulePublication(
                    pending,
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                switch recovery {
                case let .installed(revisionNumber, blockCount):
                    lastPreview = pending.preview
                    lastSuccessfulSyncID = operationID
                    status = .online(
                        updatedAt: now(),
                        message: "Recovered published revision \(revisionNumber); composed \(blockCount) blocks"
                    )
                    return
                case .requiresFreshComposition:
                    // The recovered receipt may name a superseded revision, or
                    // the server proved that the old composition was never
                    // published. Permit exactly one newly composed attempt in
                    // this sync; it must not recursively retry again.
                    freshPublicationRetryBudget = 0
                }
            }
            planner.capturePendingCanonicalMutations()
            planner.flushPersistence()
            if let persistenceError = planner.persistenceError { throw persistenceError }
            try await pullCanonicalItems(
                client: client,
                operationID: operationID,
                generation: generation
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            status = .syncing("Publishing encrypted Inbox edits…")
            let authored = try await publishCanonicalAuthoringMutations(
                client: client,
                operationID: operationID,
                generation: generation
            )
            if authored > 0 {
                status = .syncing("Refreshing authored item revisions…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
            }
            planner.capturePendingCanonicalMutations()
            planner.flushPersistence()
            if let persistenceError = planner.persistenceError { throw persistenceError }
            warnings.append(contentsOf: planner.canonicalItems
                .filter { !$0.supportsLosslessReplacement }
                .map {
                    "“\($0.title)” cannot be full-replaced without normalizing a server timestamp, number, or unsupported field, so it remains read-only."
                })
            status = .syncing("Publishing local captures…")
            let created = try await publishLocalCaptures(
                client: client,
                operationID: operationID,
                generation: generation
            )
            status = .syncing("Publishing privacy changes…")
            let privacyUpdated = try await publishSensitivityChanges(
                client: client,
                operationID: operationID,
                generation: generation
            )
            status = .syncing("Reconciling status changes…")
            let updated = try await publishSafeStatusChanges(
                client: client,
                operationID: operationID,
                generation: generation
            )
            let installed = try await composeAndPublishFreshSchedule(
                client: client,
                operationID: operationID,
                generation: generation,
                created: created,
                privacyUpdated: privacyUpdated,
                updated: updated,
                retryBudget: freshPublicationRetryBudget
            )
            lastPreview = installed.preview
            lastSuccessfulSyncID = operationID
            lastFreshCompositionSyncID = operationID
            status = .online(
                updatedAt: now(),
                message: "Synced \(planner.canonicalItems.count) items"
                    + (authored > 0 ? "; applied \(authored) Inbox edit\(authored == 1 ? "" : "s")" : "")
                    + "; composed \(installed.blockCount) blocks"
            )
        } catch {
            planner.flushPersistence()
            guard activeSyncID == operationID,
                  generation == configurationGeneration,
                  !Task.isCancelled else { return }
            status = .failed((planner.persistenceError ?? error).localizedDescription)
        }
    }

    private func pullCanonicalItems(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws {
        do {
            let result = try await loadDelta(client: client, from: planner.canonicalDeltaCursor)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            planner.applyCanonicalDelta(result.changes, nextCursor: result.cursor)
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error, statusCode == 422,
                  planner.canonicalDeltaCursor != nil else {
                throw error
            }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            // A server restart can rotate the cursor scope. Build a complete replacement
            // in memory first so an interrupted recovery never erases the offline cache.
            let result = try await loadDelta(client: client, from: nil)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            planner.replaceCanonicalState(changes: result.changes, nextCursor: result.cursor)
            warnings.append("The server item stream changed; the encrypted cache was rebuilt safely.")
        }
    }

    private func loadDelta(
        client: DayWeaveAPIClient,
        from initialCursor: String?
    ) async throws -> (changes: [DayWeaveItemDeltaChange], cursor: String) {
        var cursor = initialCursor
        var changes: [DayWeaveItemDeltaChange] = []
        var retainedBytes = 0
        var seen = Set<String>()
        for _ in 0..<100 {
            if let cursor,
               cursor.utf8.count > Self.maximumDeltaCursorBytes {
                throw CanonicalSyncError.deltaResourceLimit
            }
            let page = try await client.itemDelta(cursor: cursor, limit: 200)
            guard page.changes.count <= Self.maximumDeltaChanges - changes.count else {
                throw CanonicalSyncError.deltaResourceLimit
            }
            for change in page.changes {
                let (nextRetainedBytes, overflow) = retainedBytes.addingReportingOverflow(
                    change.retainedByteEstimate
                )
                guard !overflow, nextRetainedBytes <= Self.maximumRetainedDeltaBytes else {
                    throw CanonicalSyncError.deltaResourceLimit
                }
                retainedBytes = nextRetainedBytes
            }
            let cursorBytes = page.nextCursor.utf8.count
            let (withCursorBytes, cursorOverflow) = retainedBytes.addingReportingOverflow(cursorBytes)
            guard cursorBytes <= Self.maximumDeltaCursorBytes,
                  !cursorOverflow,
                  withCursorBytes <= Self.maximumRetainedDeltaBytes else {
                throw CanonicalSyncError.deltaResourceLimit
            }
            retainedBytes = withCursorBytes
            changes.append(contentsOf: page.changes)
            guard !page.nextCursor.isEmpty else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            if !page.hasMore { return (changes, page.nextCursor) }
            guard page.nextCursor != cursor,
                  seen.insert(page.nextCursor).inserted else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            cursor = page.nextCursor
        }
        throw CanonicalSyncError.tooManyDeltaPages
    }

    private func loadConsistentPreview(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> (preview: DayWeaveSchedulePreview, request: DayWeaveSchedulePreviewRequest) {
        for attempt in 1...3 {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            let request = makePreviewRequest()
            let preview = try await client.previewSchedule(request)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            let localRevisions = Dictionary(
                uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
            )
            if preview.sourceItemRevisions == localRevisions {
                try validate(preview: preview, against: request)
                return (preview, request)
            }
            guard attempt < 3 else { break }
            warnings.append(
                "The preview used different item revisions; refreshed the delta stream before retry \(attempt + 1)."
            )
            try await pullCanonicalItems(
                client: client,
                operationID: operationID,
                generation: generation
            )
        }
        throw CanonicalSyncError.sourceRevisionMismatch
    }

    private func recoverPendingSchedulePublication(
        _ publication: PendingSchedulePublication,
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> PendingSchedulePublicationRecovery {
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        try validatePublicationJournal(publication, client: client)
        let rendered = render(publication.preview)
        let published: DayWeaveSchedulePublishResponse
        do {
            published = try await client.publishSchedule(publication.preparedRequest)
        } catch {
            guard Self.isStaleSchedulePublication(error) else { throw error }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            try planner.clearPendingSchedulePublicationWithoutApplying(publication)
            warnings.append(
                "The server proved that the retained schedule was stale before publication; it was cleared safely and will be composed once from current items."
            )
            return .requiresFreshComposition
        }
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        try validatePublicationResponse(published, for: publication)
        if published.replayed {
            // An exact receipt can identify a revision that another device has
            // already superseded. Clear the now-acknowledged write boundary but
            // leave the prior local plan invalid until one fresh composition.
            try planner.clearPendingSchedulePublicationWithoutApplying(publication)
            warnings.append(
                "Recovered the exact publication receipt; recomposing once before making any schedule actionable because that receipt may be superseded."
            )
            return .requiresFreshComposition
        }
        try planner.commitPendingSchedulePublication(publication, blocks: rendered)
        return .installed(
            revisionNumber: published.revision.revisionNumber,
            blockCount: rendered.count
        )
    }

    private func composeAndPublishFreshSchedule(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64,
        created: Int,
        privacyUpdated: Int,
        updated: Int,
        retryBudget: Int
    ) async throws -> InstalledSchedulePublication {
        var retriesRemaining = max(0, retryBudget)
        while true {
            status = .syncing("Composing a read-only schedule preview…")
            let consistentPreview = try await loadConsistentPreview(
                client: client,
                operationID: operationID,
                generation: generation
            )
            let preview = consistentPreview.preview
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            warnings.append(contentsOf: preview.rejectedItems.map {
                "“\($0.title)” was excluded from the preview: \($0.reason)"
            })
            let rendered = render(preview)
            let preparedAt = now()
            let provenance = SchedulePreviewProvenance(
                configurationIdentifier: client.configurationIdentifier,
                generatedAt: preparedAt,
                asOf: preview.plan.asOf,
                horizonStart: preview.plan.horizonStart,
                horizonEnd: preview.plan.horizonEnd,
                timezoneName: consistentPreview.request.timezoneName
            )
            let publicationRequest = DayWeaveSchedulePublishRequest(
                idempotencyKey: UUID(),
                expectedInputDigest: preview.inputDigest,
                schedule: consistentPreview.request
            )
            let publication = PendingSchedulePublication(
                configurationIdentifier: client.configurationIdentifier,
                preparedRequest: try client.prepareSchedulePublication(publicationRequest),
                preview: preview,
                message: previewMessage(
                    preview,
                    created: created,
                    privacyUpdated: privacyUpdated,
                    updated: updated
                ),
                provenance: provenance,
                preparedAt: preparedAt
            )
            try validatePublicationJournal(publication, client: client)
            try planner.persistPendingSchedulePublication(publication)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            status = .syncing("Publishing the validated schedule…")

            let published: DayWeaveSchedulePublishResponse
            do {
                published = try await client.publishSchedule(publication.preparedRequest)
            } catch {
                guard Self.isStaleSchedulePublication(error) else { throw error }
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                try planner.clearPendingSchedulePublicationWithoutApplying(publication)
                guard retriesRemaining > 0 else {
                    throw CanonicalSyncError.schedulePublicationStayedStale
                }
                retriesRemaining -= 1
                warnings.append(
                    "Canonical items changed during publication; discarded that non-published candidate and recomposed once."
                )
                status = .syncing("Refreshing canonical items after a publication race…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                continue
            }

            try ensureOperationCurrent(operationID: operationID, generation: generation)
            try validatePublicationResponse(published, for: publication)
            if published.replayed {
                try planner.clearPendingSchedulePublicationWithoutApplying(publication)
                guard retriesRemaining > 0 else {
                    throw CanonicalSyncError.schedulePublicationReplayNeedsFreshComposition
                }
                retriesRemaining -= 1
                warnings.append(
                    "The publication returned an existing exact receipt; recomposed once before making a schedule actionable."
                )
                status = .syncing("Refreshing canonical items after an exact receipt…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                continue
            }

            try planner.commitPendingSchedulePublication(publication, blocks: rendered)
            return .init(preview: preview, blockCount: rendered.count)
        }
    }

    private static func isStaleSchedulePublication(_ error: any Error) -> Bool {
        (error as? DayWeaveAPIError) == .trustedSchedulePublicationStale
    }

    private func validatePublicationJournal(
        _ publication: PendingSchedulePublication,
        client: DayWeaveAPIClient
    ) throws {
        let request = publication.preparedRequest.request
        guard publication.version == PendingSchedulePublication.currentVersion,
              publication.isWithinEncodedSizeLimit,
              publication.configurationIdentifier == client.configurationIdentifier,
              publication.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              publication.provenance.configurationIdentifier == publication.configurationIdentifier,
              Self.isValidInputDigest(request.expectedInputDigest),
              request.expectedInputDigest == publication.preview.inputDigest,
              sameInstant(publication.provenance.asOf, request.schedule.asOf),
              sameInstant(publication.provenance.horizonStart, request.schedule.horizonStart),
              sameInstant(publication.provenance.horizonEnd, request.schedule.horizonEnd),
              publication.provenance.timezoneName == request.schedule.timezoneName,
              publication.preparedAt.timeIntervalSinceReferenceDate.isFinite,
              publication.provenance.generatedAt.timeIntervalSinceReferenceDate.isFinite,
              publication.provenance.generatedAt >= publication.preparedAt.addingTimeInterval(-0.002),
              publication.provenance.generatedAt <= publication.preparedAt.addingTimeInterval(0.002),
              publication.message.utf8.count <= PendingSchedulePublication.maximumMessageBytes,
              !publication.message.unicodeScalars.contains(
                  where: CharacterSet.controlCharacters.contains
              ) else {
            throw CanonicalSyncError.invalidSchedulePublication
        }
        try client.validatePreparedSchedulePublication(publication.preparedRequest)
        let localRevisions = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        guard publication.preview.sourceItemRevisions == localRevisions else {
            throw CanonicalSyncError.sourceRevisionMismatch
        }
        try validate(preview: publication.preview, against: request.schedule)
    }

    private func validatePublicationResponse(
        _ response: DayWeaveSchedulePublishResponse,
        for publication: PendingSchedulePublication
    ) throws {
        let revision = response.revision
        let request = publication.preparedRequest.request
        let components = revision.revision.split(
            separator: ":",
            maxSplits: 1,
            omittingEmptySubsequences: false
        )
        let currentTime = now()
        guard components.count == 2,
              revision.revisionNumber > 0,
              components[0] == Substring(String(revision.revisionNumber)),
              components[1] == Substring(revision.id.uuidString.lowercased()),
              revision.inputDigest == request.expectedInputDigest,
              sameInstant(revision.horizonStart, request.schedule.horizonStart),
              sameInstant(revision.horizonEnd, request.schedule.horizonEnd),
              revision.timezoneName == request.schedule.timezoneName,
              revision.publishedAt.timeIntervalSinceReferenceDate.isFinite,
              revision.publishedAt <= currentTime.addingTimeInterval(5 * 60) else {
            throw CanonicalSyncError.invalidSchedulePublication
        }
    }

    private func sameInstant(_ left: Date, _ right: Date) -> Bool {
        abs(left.timeIntervalSince(right)) <= 0.002
    }

    private static func isValidInputDigest(_ value: String) -> Bool {
        let prefix = "sha256:"
        guard value.hasPrefix(prefix), value.utf8.count == prefix.utf8.count + 64 else {
            return false
        }
        return value.utf8.dropFirst(prefix.utf8.count).allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }

    private func validate(
        preview: DayWeaveSchedulePreview,
        against request: DayWeaveSchedulePreviewRequest
    ) throws {
        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        func sameInstant(_ left: Date, _ right: Date) -> Bool {
            abs(left.timeIntervalSince(right)) <= 0.002
        }
        func effectiveSensitivity(for itemID: UUID) -> Bool {
            var currentID: UUID? = itemID
            var visited = Set<UUID>()
            while let identifier = currentID {
                guard visited.insert(identifier).inserted,
                      let item = itemByID[identifier] else { return true }
                if item.isSensitive { return true }
                currentID = item.parentID
            }
            return false
        }
        let fixedByID = try Self.validatedFixedBlocksByID(request.fixedBlocks)
        guard !preview.inputDigest.isEmpty,
              sameInstant(preview.plan.asOf, request.asOf),
              sameInstant(preview.plan.horizonStart, request.horizonStart),
              sameInstant(preview.plan.horizonEnd, request.horizonEnd),
              preview.plan.horizonStart < preview.plan.horizonEnd else {
            throw CanonicalSyncError.invalidPreview(
                "The response clock or horizon does not match the preview request."
            )
        }
        for rejected in preview.rejectedItems {
            guard let item = itemByID[rejected.itemID],
                  item.title == rejected.title,
                  rejected.isSensitive == effectiveSensitivity(for: rejected.itemID) else {
                throw CanonicalSyncError.invalidPreview(
                    "A rejected item has inconsistent canonical sensitivity or identity."
                )
            }
        }

        var blockIDs = Set<UUID>()
        var sessionIdentities = Set<PreviewSessionIdentity>()
        var externalBlockIDs = Set<UUID>()
        var scheduledMinutes: UInt32 = 0
        let orderedBlocks = preview.plan.blocks.sorted {
            if $0.start != $1.start { return $0.start < $1.start }
            if $0.end != $1.end { return $0.end < $1.end }
            return $0.id.uuidString < $1.id.uuidString
        }
        var latestAnyEnd: Date?
        var latestPlannableEnd: Date?
        for block in orderedBlocks {
            guard blockIDs.insert(block.id).inserted else {
                throw CanonicalSyncError.invalidPreview("The response contains a duplicate block identifier.")
            }
            guard block.start < block.end,
                  block.end > preview.plan.horizonStart,
                  block.start < preview.plan.horizonEnd else {
                throw CanonicalSyncError.invalidPreview(
                    "A response block has an empty interval or does not intersect the response horizon."
                )
            }
            if let itemID = block.itemID {
                guard itemByID[itemID] != nil,
                      block.isSensitive == effectiveSensitivity(for: itemID) else {
                    throw CanonicalSyncError.invalidPreview(
                        "A canonical block has inconsistent effective sensitivity."
                    )
                }
            }
            switch block.kind {
            case "planned", "pinned":
                if let latestAnyEnd, latestAnyEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block overlaps another block."
                    )
                }
                guard block.start >= preview.plan.horizonStart,
                      block.end <= preview.plan.horizonEnd else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block lies outside the response horizon."
                    )
                }
                guard let itemID = block.itemID,
                      preview.sourceItemRevisions[itemID] != nil,
                      block.externalBlockID == nil,
                      itemByID[itemID]?.isExecutable == true else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned block does not reference an executable canonical leaf item."
                    )
                }
                let identity = PreviewSessionIdentity(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )
                guard sessionIdentities.insert(identity).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "The response contains a duplicate item/occurrence/session identity."
                    )
                }
                let seconds = block.end.timeIntervalSince(block.start)
                let rawMinutes = seconds / 60
                guard seconds.isFinite,
                      rawMinutes.isFinite,
                      abs(rawMinutes.rounded() - rawMinutes) < 0.000_001,
                      rawMinutes >= 0,
                      rawMinutes <= Double(UInt32.max),
                      let minutes = UInt32(exactly: rawMinutes.rounded()) else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block has a non-finite or non-minute duration."
                    )
                }
                scheduledMinutes = scheduledMinutes.addingReportingOverflow(minutes).overflow
                    ? UInt32.max
                    : scheduledMinutes &+ minutes
            case "calendar_event":
                if let latestPlannableEnd, latestPlannableEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "A calendar block overlaps planned work."
                    )
                }
                guard let itemID = block.itemID,
                      preview.sourceItemRevisions[itemID] != nil,
                      block.externalBlockID == nil,
                      itemByID[itemID]?.kind == .event else {
                    throw CanonicalSyncError.invalidPreview(
                        "A calendar block does not reference a canonical event item."
                    )
                }
                let identity = PreviewSessionIdentity(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )
                guard sessionIdentities.insert(identity).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "The response contains a duplicate item/occurrence/session identity."
                    )
                }
            case "external_fixed":
                if let latestPlannableEnd, latestPlannableEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "An external fixed block overlaps planned work."
                    )
                }
                guard block.itemID == nil,
                      block.occurrenceID == nil,
                      let externalBlockID = block.externalBlockID,
                      let fixed = fixedByID[externalBlockID],
                      fixed.title == block.title,
                      fixed.isSensitive == block.isSensitive,
                      sameInstant(fixed.start, block.start),
                      sameInstant(fixed.end, block.end),
                      externalBlockIDs.insert(externalBlockID).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "An external fixed block has an invalid or duplicate source identity."
                    )
                }
            default:
                throw CanonicalSyncError.invalidPreview(
                    "The response contains an unsupported schedule block kind."
                )
            }
            if latestAnyEnd == nil || block.end > latestAnyEnd! {
                latestAnyEnd = block.end
            }
            if block.kind == "planned" || block.kind == "pinned",
               latestPlannableEnd == nil || block.end > latestPlannableEnd! {
                latestPlannableEnd = block.end
            }
        }
        try Self.validateFixedBlockCoverage(
            returnedExternalBlockIDs: externalBlockIDs,
            request: request
        )
        var unscheduledMinutes: UInt32 = 0
        var unscheduledIdentities = Set<PreviewOccurrenceIdentity>()
        for unscheduled in preview.plan.unscheduled {
            guard preview.sourceItemRevisions[unscheduled.itemID] != nil,
                  unscheduledIdentities.insert(.init(
                      itemID: unscheduled.itemID,
                      occurrenceID: unscheduled.occurrenceID
                  )).inserted else {
                throw CanonicalSyncError.invalidPreview(
                    "The response reports duplicate unscheduled work or an unknown source item."
                )
            }
            unscheduledMinutes = unscheduledMinutes.addingReportingOverflow(unscheduled.remaining).overflow
                ? UInt32.max
                : unscheduledMinutes &+ unscheduled.remaining
        }
        guard preview.plan.score.scheduledMinutes == scheduledMinutes,
              preview.plan.score.unscheduledMinutes == unscheduledMinutes else {
            throw CanonicalSyncError.invalidPreview(
                "The response score does not match its scheduled and unscheduled work."
            )
        }
    }

    static func validateFixedBlockCoverage(
        returnedExternalBlockIDs: Set<UUID>,
        request: DayWeaveSchedulePreviewRequest
    ) throws {
        let fixedByID = try validatedFixedBlocksByID(request.fixedBlocks)
        let expectedExternalBlockIDs = Set(fixedByID.values.compactMap { fixed in
            fixed.end > request.horizonStart && fixed.start < request.horizonEnd ? fixed.id : nil
        })
        guard returnedExternalBlockIDs == expectedExternalBlockIDs else {
            throw CanonicalSyncError.invalidPreview(
                "The response omitted or invented an intersecting external fixed block."
            )
        }
    }

    private static func validatedFixedBlocksByID(
        _ fixedBlocks: [DayWeaveSchedulePreviewRequest.FixedBlock]
    ) throws -> [UUID: DayWeaveSchedulePreviewRequest.FixedBlock] {
        var fixedByID: [UUID: DayWeaveSchedulePreviewRequest.FixedBlock] = [:]
        for fixed in fixedBlocks {
            guard fixedByID[fixed.id] == nil else {
                throw CanonicalSyncError.invalidPreview(
                    "The preview request contains a duplicate external fixed-block identifier."
                )
            }
            fixedByID[fixed.id] = fixed
        }
        return fixedByID
    }

    private func publishCanonicalAuthoringMutations(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let ordered = orderedCanonicalAuthoringMutations(
            planner.sortedPendingCanonicalAuthoringMutations.filter {
                $0.disposition == .pending
            }
        )
        var appliedCount = 0
        var attemptedCount = 0

        for (offset, original) in ordered.enumerated() {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard attemptedCount < authoringPushLimit else {
                let deferred = ordered.count - offset
                warnings.append(
                    "Deferred \(deferred) encrypted Inbox edit\(deferred == 1 ? "" : "s") after reaching the \(authoringPushLimit)-request safety cap."
                )
                break
            }
            guard var mutation = planner.canonicalAuthoringMutation(id: original.id),
                  mutation.disposition == .pending else { continue }

            if mutation.hasBeenSubmitted,
               let observed = try reconcileCanonicalAuthoringFromCache(mutation) {
                if observed { appliedCount += 1 }
                continue
            }
            guard try canonicalAuthoringPreflightIsCurrent(mutation) else { continue }
            if !mutation.hasBeenSubmitted {
                mutation = try planner.bindCanonicalAuthoringMutation(
                    mutation.id,
                    configurationIdentifier: client.configurationIdentifier
                )
                mutation = try planner.markCanonicalAuthoringMutationSubmitted(mutation.id)
            }
            guard mutation.configurationIdentifier == client.configurationIdentifier else {
                throw PlannerCanonicalAuthoringError.invalidConfiguration
            }

            attemptedCount += 1
            do {
                let response = try await sendCanonicalAuthoringMutation(
                    mutation,
                    client: client
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                try planner.applyCanonicalAuthoringResponse(mutation.id, item: response)
                appliedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                if let reconciled = try await reconcileCanonicalAuthoringAfterServerError(
                    mutation,
                    error: error,
                    client: client,
                    operationID: operationID,
                    generation: generation
                ) {
                    if reconciled { appliedCount += 1 }
                    continue
                }
                throw error
            }
        }
        return appliedCount
    }

    /// Every mutation of a child may refresh both its old and new canonical
    /// parents. Publish any queued parent operation first so that the child's
    /// hierarchy side effect cannot make the parent's exact base revision stale
    /// inside the same offline batch. Submitted work otherwise retains priority
    /// among unrelated nodes so uncertain requests are reconciled first.
    private func orderedCanonicalAuthoringMutations(
        _ mutations: [DayWeavePendingCanonicalAuthoringMutation]
    ) -> [DayWeavePendingCanonicalAuthoringMutation] {
        let stable = mutations.sorted {
            if $0.hasBeenSubmitted != $1.hasBeenSubmitted {
                return $0.hasBeenSubmitted && !$1.hasBeenSubmitted
            }
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.id.uuidString < $1.id.uuidString
        }
        let byItemID = Dictionary(
            stable.map { ($0.itemID, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        let stableRank = Dictionary(uniqueKeysWithValues: stable.enumerated().map {
            ($0.element.id, $0.offset)
        })
        var childrenByParentMutationID: [UUID: Set<UUID>] = [:]
        var dependencyCount = Dictionary(uniqueKeysWithValues: stable.map { ($0.id, 0) })
        for child in stable {
            for parentItemID in canonicalAuthoringAffectedParentIDs(child) {
                guard let parent = byItemID[parentItemID], parent.id != child.id else { continue }
                if childrenByParentMutationID[parent.id, default: []].insert(child.id).inserted {
                    dependencyCount[child.id, default: 0] += 1
                }
            }
        }

        func stableOrder(_ left: DayWeavePendingCanonicalAuthoringMutation,
                         _ right: DayWeavePendingCanonicalAuthoringMutation) -> Bool {
            (stableRank[left.id] ?? .max) < (stableRank[right.id] ?? .max)
        }

        var ready = stable.filter { dependencyCount[$0.id] == 0 }.sorted(by: stableOrder)
        var emitted = Set<UUID>()
        var result: [DayWeavePendingCanonicalAuthoringMutation] = []
        result.reserveCapacity(stable.count)

        while !ready.isEmpty {
            let mutation = ready.removeFirst()
            guard emitted.insert(mutation.id).inserted else { continue }
            result.append(mutation)
            for childID in childrenByParentMutationID[mutation.id] ?? [] {
                let remaining = max(0, (dependencyCount[childID] ?? 0) - 1)
                dependencyCount[childID] = remaining
                if remaining == 0, let child = stable.first(where: { $0.id == childID }) {
                    ready.append(child)
                    ready.sort(by: stableOrder)
                }
            }
        }
        // A cycle across old and new ancestry cannot be serialized with exact
        // per-item revisions. Keep deterministic order; normal preflight and
        // conflict recovery retain every request rather than dropping it.
        result.append(contentsOf: stable.filter { !emitted.contains($0.id) })
        return result
    }

    private func canonicalAuthoringAffectedParentIDs(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) -> Set<UUID> {
        var parentIDs = Set<UUID>()
        if let parentID = mutation.draft?.parentID { parentIDs.insert(parentID) }
        if let parentID = mutation.baseItem?.parentID { parentIDs.insert(parentID) }
        if mutation.operation == .restore,
           let parentID = planner.canonicalTrashEntry(id: mutation.itemID)?.parentID {
            parentIDs.insert(parentID)
        }
        parentIDs.remove(mutation.itemID)
        return parentIDs
    }

    private func canonicalAuthoringPreflightIsCurrent(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        let diagnostic: String?
        switch mutation.operation {
        case .create:
            diagnostic = if planner.canonicalItem(id: mutation.itemID) != nil
                || planner.canonicalTombstoneRevisions[mutation.itemID] != nil {
                "This identifier already belongs to canonical history. Keep the latest item or create a new draft."
            } else if let draft = mutation.draft,
                      !planner.canonicalAuthoringDraftHierarchyIsCurrent(
                          draft,
                          itemID: mutation.itemID,
                          requiresCommittedParent: true
                      ) {
                "The selected parent is no longer available for this item. Choose an active Inbox or Planned parent before retrying."
            } else {
                nil
            }
        case .replace, .trash:
            if let expected = mutation.expectedRevision,
               planner.canonicalItem(id: mutation.itemID)?.revision == expected {
                if mutation.operation == .replace,
                   let draft = mutation.draft,
                   !planner.canonicalAuthoringDraftHierarchyIsCurrent(
                       draft,
                       itemID: mutation.itemID,
                       requiresCommittedParent: true
                   ) {
                    diagnostic = "The selected parent is no longer available for this item. Review the latest hierarchy before retrying."
                } else if mutation.operation == .trash,
                          planner.canonicalItems.contains(where: {
                              $0.parentID == mutation.itemID && $0.deletedAt == nil
                          }) {
                    diagnostic = "This item now has active children and cannot be deleted until they are moved or deleted."
                } else {
                    diagnostic = nil
                }
            } else if mutation.operation == .trash,
                      mutation.hasBeenSubmitted,
                      let expected = mutation.expectedRevision,
                      let entry = planner.canonicalTrashEntry(id: mutation.itemID),
                      entry.revision > expected {
                // A pulled tombstone proves the item left the active set, but
                // delta deliberately retains only the pre-delete body. Replay
                // the immutable request to recover the full deleted response;
                // do not misclassify response loss as a revision conflict.
                diagnostic = nil
            } else {
                diagnostic = "The item changed after this edit was saved. Review the latest revision before retrying."
            }
        case .restore:
            if let expected = mutation.expectedRevision,
               let entry = planner.canonicalTrashEntry(id: mutation.itemID),
               entry.revision == expected {
                if let parentID = entry.parentID,
                   planner.canonicalItem(id: parentID) == nil {
                    diagnostic = "The deleted item's parent is no longer active, so it cannot be restored in place."
                } else {
                    diagnostic = nil
                }
            } else {
                diagnostic = "The deleted item changed after restore was requested. Review the latest revision before retrying."
            }
        }
        guard let diagnostic else { return true }
        _ = try planner.markCanonicalAuthoringMutationConflicted(
            mutation.id,
            diagnostic: diagnostic
        )
        warnings.append("An Inbox edit needs conflict review before it can be published.")
        return false
    }

    private func sendCanonicalAuthoringMutation(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation,
        client: DayWeaveAPIClient
    ) async throws -> DayWeaveCanonicalItem {
        switch mutation.operation {
        case .create:
            guard let draft = mutation.draft else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.createCanonicalItem(
                DayWeaveNewCanonicalItem(id: mutation.itemID, fields: draft.requestFields),
                idempotencyKey: mutation.idempotencyKey
            )
        case .replace:
            guard let draft = mutation.draft,
                  let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.replaceCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                item: draft.requestFields,
                idempotencyKey: mutation.idempotencyKey
            )
        case .trash:
            guard let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.trashCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                idempotencyKey: mutation.idempotencyKey
            )
        case .restore:
            guard let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.restoreCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                idempotencyKey: mutation.idempotencyKey
            )
        }
    }

    /// Returns `true` when the exact journal was committed, `false` when it was
    /// moved to explicit conflict review, and `nil` when no conclusive local
    /// observation exists and the exact request still needs replay.
    private func reconcileCanonicalAuthoringFromCache(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool? {
        let candidate: DayWeaveCanonicalItem?
        switch mutation.operation {
        case .create:
            candidate = planner.canonicalItem(id: mutation.itemID)
        case .replace, .restore:
            guard let expected = mutation.expectedRevision,
                  let observed = planner.canonicalItem(id: mutation.itemID),
                  observed.revision > expected else { return nil }
            candidate = observed
        case .trash:
            guard let expected = mutation.expectedRevision,
                  let entry = planner.canonicalTrashEntry(id: mutation.itemID),
                  entry.revision > expected,
                  entry.lastKnownItem?.deletedAt != nil else { return nil }
            candidate = entry.lastKnownItem
        }
        guard let candidate else { return nil }
        return try reconcileCanonicalAuthoringCandidate(candidate, mutation: mutation)
    }

    private func reconcileCanonicalAuthoringCandidate(
        _ candidate: DayWeaveCanonicalItem,
        mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        do {
            try planner.applyCanonicalAuthoringResponse(mutation.id, item: candidate)
            return true
        } catch PlannerCanonicalAuthoringError.invalidRemoteResponse {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The canonical item now has different content or revision state. Review both versions before deciding which to keep."
            )
            warnings.append("An Inbox edit resolved to different canonical content and needs review.")
            return false
        }
    }

    private func reconcileCanonicalAuthoringAfterServerError(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation,
        error: DayWeaveAPIError,
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Bool? {
        let statusCode: Int
        let trustedNoEffect: Bool
        switch error {
        case .trustedCanonicalMutationInProgress:
            return nil
        case .trustedCanonicalMutationNoEffect:
            statusCode = 409
            trustedNoEffect = true
        case let .server(code, _, _, _):
            statusCode = code
            trustedNoEffect = false
        default:
            return nil
        }
        if statusCode == 400 || statusCode == 422 {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The server rejected this saved item contract. Edit the retained draft before retrying."
            )
            warnings.append("An Inbox edit was rejected by the canonical contract and remains encrypted for review.")
            return false
        }
        guard statusCode == 404 || statusCode == 409 else { return nil }

        let observed: [DayWeaveCanonicalItem]
        do {
            observed = try await client.listCanonicalItems(
                includeDeleted: true,
                limit: DayWeaveAPIClient.maximumCanonicalItemListLimit
            )
        } catch {
            if trustedNoEffect {
                return try markCanonicalAuthoringNoEffectConflict(mutation)
            }
            // The original exact mutation remains submitted. Failure to obtain
            // independent canonical evidence must never turn ambiguity into a
            // destructive conflict decision.
            return nil
        }
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        if let candidate = observed.first(where: { $0.id == mutation.itemID }) {
            if mutation.operation != .create,
               let expected = mutation.expectedRevision,
               candidate.revision <= expected {
                // A matching idempotent request can return 409 while it still
                // owns the mutation. During that window the list endpoint
                // legitimately exposes the unchanged base item (or deleted
                // restore base). It is not evidence of a conflicting result.
                return trustedNoEffect
                    ? try markCanonicalAuthoringNoEffectConflict(mutation)
                    : nil
            }
            return try reconcileCanonicalAuthoringCandidate(candidate, mutation: mutation)
        }
        if trustedNoEffect {
            return try markCanonicalAuthoringNoEffectConflict(mutation)
        }
        if statusCode == 404,
           observed.count < DayWeaveAPIClient.maximumCanonicalItemListLimit {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The authenticated server confirmed that this item is absent. Keep the saved draft or discard this operation."
            )
            warnings.append("An Inbox edit references an item that is no longer available.")
            return false
        }
        // A 409 with no observed item can mean the matching idempotent request
        // is still in progress. A full 200-item list can also be truncated.
        // Preserve the exact submitted journal and retry later.
        return nil
    }

    private func markCanonicalAuthoringNoEffectConflict(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        _ = try planner.markCanonicalAuthoringMutationConflicted(
            mutation.id,
            diagnostic: "The authenticated server proved this exact request made no change. Review the latest canonical hierarchy and revision, then keep or copy the retained draft."
        )
        warnings.append("An Inbox edit made no canonical change and needs conflict review.")
        return false
    }

    private func publishLocalCaptures(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let captures = planner.blocks
            .filter { $0.isLocallyAuthored && $0.sourceItemID == nil }
            .sorted {
                if $0.start != $1.start { return $0.start < $1.start }
                return $0.id.uuidString < $1.id.uuidString
            }
        var publishable: [ScheduleBlock] = []
        for var block in captures {
            guard let normalizedTitle = PlannerStore.normalizedCanonicalTitle(block.title) else {
                let diagnostic = "Not published: the title must contain 1–\(PlannerStore.maximumCanonicalTitleScalars) Unicode characters. Edit or delete this local capture in the inspector."
                planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                warnings.append("A legacy local capture was skipped safely because its title is outside the server contract.")
                continue
            }
            if normalizedTitle != block.title {
                planner.normalizeLocalCaptureForSync(block.id, title: normalizedTitle)
                block.title = normalizedTitle
            }
            guard canonicalStatus(block.status) != nil else {
                let diagnostic = "Not published: this legacy local status is not representable by the canonical API. Delete the capture or change it through a supported planner action."
                planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                warnings.append("“\(block.title)” was skipped safely because its local status is unsupported.")
                continue
            }
            if let existing = planner.canonicalItem(id: block.id) {
                guard let intended = makeNewItem(from: block),
                      DayWeaveCanonicalItemFields(item: existing) == intended.fields else {
                    let diagnostic = "Not published: this identifier already belongs to a server item with different canonical fields or privacy. Edit or delete the local capture."
                    planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                    warnings.append("“\(block.title)” was not published because its identifier already exists remotely.")
                    continue
                }
                planner.bindLocalBlock(block.id, to: existing)
                continue
            }
            planner.clearLocalCaptureDiagnostic(block.id)
            publishable.append(block)
        }
        // Persist normalization/quarantine before the first remote side effect.
        planner.flushPersistence()
        if let persistenceError = planner.persistenceError { throw persistenceError }

        var createdCount = 0
        var networkPushes = 0
        for (offset, block) in publishable.enumerated() {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard let newItem = makeNewItem(from: block) else {
                planner.quarantineLocalCapture(
                    block.id,
                    diagnostic: "Not published because this legacy capture cannot be represented losslessly. Edit or delete it in the inspector."
                )
                warnings.append("“\(block.title)” could not be represented by the canonical API and was retained locally.")
                continue
            }
            guard networkPushes < createPushLimit else {
                warnings.append(
                    "Deferred \(publishable.count - offset) local capture push\(publishable.count - offset == 1 ? "" : "es") to a later sync after reaching the \(createPushLimit)-request safety cap."
                )
                break
            }
            networkPushes += 1
            let created = try await client.createCanonicalItem(
                newItem,
                idempotencyKey: "mac-create-\(block.id.uuidString.lowercased())"
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard created.id == block.id,
                  created.revision > (planner.canonicalTombstoneRevisions[block.id] ?? 0),
                  created.isSensitive == newItem.fields.isSensitive,
                  created.status == newItem.fields.status,
                  created.deletedAt == nil else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            planner.bindLocalBlock(block.id, to: created)
            createdCount += 1
        }
        return createdCount
    }

    private func publishSensitivityChanges(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        var mutations = planner.pendingCanonicalSensitivityMutations.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.id.uuidString < $1.id.uuidString
        }
        var updatedCount = 0
        var networkPushes = 0

        while !mutations.isEmpty {
            let mutation = mutations.removeFirst()
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard mutation.disposition == .pending else {
                warnings.append("A conflicted privacy edit remains encrypted for review.")
                continue
            }
            guard let item = planner.canonicalItem(id: mutation.itemID) else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: mutation.itemID,
                    diagnostic: "The source item is no longer present in the canonical cache."
                )
                warnings.append("A local privacy edit targets an item that was removed remotely.")
                continue
            }
            if item.isSensitive == mutation.desiredIsSensitive {
                if let next = planner.reconcileCanonicalSensitivityObservation(item) {
                    mutations.append(next)
                }
                continue
            }
            guard mutation.baseRevision == item.revision else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "Remote revision \(item.revision) differs from local privacy-edit base revision \(mutation.baseRevision)."
                )
                warnings.append("“\(item.title)” changed remotely; its privacy edit was retained as a conflict.")
                continue
            }
            guard item.supportsLosslessReplacement else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "A full replacement cannot losslessly preserve this item's fields."
                )
                warnings.append("“\(item.title)” has fields this app will not overwrite; its privacy edit remains recoverable.")
                continue
            }
            guard planner.executionState.activeSession?.itemID != item.id,
                  planner.executionState.pendingCommand == nil else {
                warnings.append("“\(item.title)” has active execution state; its privacy edit was deferred safely.")
                continue
            }
            guard networkPushes < statusPushLimit else {
                warnings.append(
                    "Deferred remaining privacy pushes to a later sync after reaching the \(statusPushLimit)-request safety cap."
                )
                break
            }
            let (expectedRevision, revisionOverflow) = item.revision.addingReportingOverflow(1)
            guard !revisionOverflow else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            var replacement = DayWeaveCanonicalItemFields(item: item)
            replacement.isSensitive = mutation.desiredIsSensitive

            do {
                guard planner.markCanonicalSensitivityMutationSubmitted(mutation.id) else {
                    throw CanonicalSyncError.localPersistenceUnavailable
                }
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                networkPushes += 1
                let updated = try await client.replaceCanonicalItem(
                    item.id,
                    expectedRevision: item.revision,
                    item: replacement,
                    idempotencyKey: "mac-sensitive-\(item.id.uuidString.lowercased())-r\(item.revision)-\(mutation.desiredIsSensitive ? "private" : "standard")"
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard updated.id == item.id,
                      updated.revision == expectedRevision,
                      updated.deletedAt == nil,
                      DayWeaveCanonicalItemFields(item: updated) == replacement else {
                    throw CanonicalSyncError.invalidMutationResponse
                }
                if let next = planner.applyCanonicalSensitivityMutationResponse(
                    updated,
                    replacingBaseRevision: item.revision
                ) {
                    mutations.append(next)
                }
                updatedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard case let .server(statusCode, _, _, _) = error, statusCode == 409 else {
                    throw error
                }
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "The server rejected privacy-edit base revision \(item.revision) as stale."
                )
                warnings.append("“\(item.title)” conflicted remotely; its privacy edit was retained.")
            }
        }
        return updatedCount
    }

    private func publishSafeStatusChanges(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let grouped = Dictionary(grouping: planner.pendingCanonicalMutations, by: \.itemID)
        var updatedCount = 0
        var networkPushes = 0

        for itemID in grouped.keys.sorted(by: { $0.uuidString < $1.uuidString }) {
            guard let mutations = grouped[itemID] else { continue }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard let item = planner.canonicalItem(id: itemID) else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "The source item is no longer present in the canonical cache."
                )
                warnings.append("A local status edit targets an item that was removed remotely.")
                continue
            }
            guard planner.canonicalSensitivityMutation(itemID: itemID) == nil else {
                // Privacy replacements own this item's revision until their
                // complete submitted/follow-up chain is reconciled. Advancing
                // the item here would strand the final privacy choice on a
                // stale base revision when the privacy push cap is reached.
                warnings.append(
                    "“\(item.title)” has a privacy edit in progress; its status edit was deferred safely."
                )
                continue
            }
            guard mutations.count == 1,
                  let mutation = mutations.first,
                  mutation.occurrenceID == nil,
                  mutation.disposition == .pending else {
                warnings.append("“\(item.title)” has split-session or conflicted status intent retained for review.")
                continue
            }
            guard mutation.baseRevision == item.revision else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "Remote revision \(item.revision) differs from local base revision \(mutation.baseRevision)."
                )
                warnings.append("“\(item.title)” changed remotely; its local status edit was retained as a conflict.")
                continue
            }
            let itemBlocks = planner.blocks.filter {
                $0.sourceItemID == itemID && $0.occurrenceID == nil
            }
            guard item.recurrence == nil,
                  case .indivisible = item.splitPolicy,
                  itemBlocks.count == 1,
                  itemBlocks[0].occurrenceFullyScheduled else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "A session-level edit cannot safely replace a recurring, split, partial, or multi-block item."
                )
                warnings.append(
                    "“\(item.title)” has a session-level status edit retained locally; the item was not full-replaced."
                )
                continue
            }
            guard item.supportsLosslessReplacement,
                  let desiredStatus = canonicalStatus(mutation.desiredStatus) else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "A full replacement cannot losslessly preserve this item's fields or status."
                )
                warnings.append("“\(item.title)” has fields or a status this app will not overwrite; the edit remains recoverable.")
                continue
            }
            let (expectedRevision, revisionOverflow) = item.revision.addingReportingOverflow(1)
            guard !revisionOverflow else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            let replacement = DayWeaveCanonicalItemFields(
                item: item,
                status: desiredStatus
            )

            do {
                guard networkPushes < statusPushLimit else {
                    warnings.append(
                        "Deferred remaining status pushes to a later sync after reaching the \(statusPushLimit)-request safety cap."
                    )
                    break
                }
                networkPushes += 1
                let updated = try await client.replaceCanonicalItem(
                    item.id,
                    expectedRevision: item.revision,
                    item: replacement,
                    idempotencyKey: "mac-status-\(item.id.uuidString.lowercased())-r\(item.revision)-\(desiredStatus.wireValue)"
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard updated.id == item.id,
                      updated.revision == expectedRevision,
                      DayWeaveCanonicalItemFields(item: updated) == replacement,
                      updated.deletedAt == nil else {
                    throw CanonicalSyncError.invalidMutationResponse
                }
                planner.upsertCanonicalItem(updated)
                planner.clearCanonicalMutations(itemID: item.id)
                updatedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard case let .server(statusCode, _, _, _) = error, statusCode == 409 else {
                    throw error
                }
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "The server rejected base revision \(item.revision) as stale."
                )
                warnings.append("“\(item.title)” conflicted remotely; its local status edit was retained.")
            }
        }
        return updatedCount
    }

    private func makeNewItem(from block: ScheduleBlock) -> DayWeaveNewCanonicalItem? {
        guard let status = canonicalStatus(block.status) else { return nil }
        let kind = canonicalKind(block.kind)
        var constraints: [String: JSONValue] = ["energy": .string(block.energy.rawValue)]
        var recurrence: JSONValue?
        if block.kind == .event {
            constraints["calendar_event"] = .object([
                "start": .string(format(block.start)),
                "end": .string(format(block.end)),
                "immutable": .bool(true),
                "all_day": .bool(false),
            ])
        } else if block.kind == .habit {
            recurrence = .object(["type": .string("daily"), "times_per_day": .number(1)])
        } else if block.kind == .goal {
            constraints["has_own_effort"] = .bool(true)
        }

        return DayWeaveNewCanonicalItem(
            id: block.id,
            fields: DayWeaveCanonicalItemFields(
                isSensitive: block.isSensitive,
                kind: kind,
                status: status,
                title: block.title,
                notes: block.notes.isEmpty ? nil : block.notes,
                timezoneName: planningTimezone,
                durationSeconds: UInt32(clamping: block.durationMinutes * 60),
                recurrence: recurrence,
                flexibleConstraints: .object(constraints),
                splitPolicy: .indivisible
            )
        )
    }

    private func makePreviewRequest() -> DayWeaveSchedulePreviewRequest {
        let calendar = Calendar.autoupdatingCurrent
        let asOf = now()
        let start = calendar.startOfDay(for: asOf)
        let end = calendar.date(byAdding: .day, value: 7, to: start) ?? start.addingTimeInterval(7 * 86_400)
        let availability = (0..<7).compactMap { offset -> DayWeaveSchedulePreviewRequest.Availability? in
            guard let day = calendar.date(byAdding: .day, value: offset, to: start),
                  let availableStart = calendar.date(bySettingHour: 6, minute: 0, second: 0, of: day),
                  let dayEnd = calendar.date(bySettingHour: 23, minute: 0, second: 0, of: day),
                  let availableEnd = calendar.date(
                      byAdding: .minute,
                      value: -planner.protectedFreeMinutes,
                      to: dayEnd
                  ),
                  availableEnd > availableStart else { return nil }
            return .init(
                start: max(availableStart, asOf),
                end: availableEnd,
                contexts: [],
                location: nil,
                energy: "medium"
            )
        }.filter { $0.end > $0.start }
        let activeItemIDs = Set(planner.canonicalItems.map(\.id))
        let activeOutcomes = planner.recurrenceSessionOutcomes.filter {
            activeItemIDs.contains($0.itemID)
        }
        let completedOccurrenceIDs: [JSONValue] = Dictionary(
            grouping: activeOutcomes.filter {
                $0.disposition == .completed
                    && $0.occurrenceFullyScheduled
                    && planner.completedOccurrenceIDs.contains($0.occurrenceID)
            },
            by: \.occurrenceID
        )
            .compactMap { occurrenceID, outcomes in
                outcomes.map(\.occurredAt).max().map { (occurrenceID, $0) }
            }
            .sorted {
                if $0.1 != $1.1 { return $0.1 > $1.1 }
                return $0.0.uuidString < $1.0.uuidString
            }
            .prefix(3_000)
            .map { JSONValue.string($0.0.uuidString.lowercased()) }
        let completionAnchors = planner.recurrenceCompletionAnchors()
            .filter { activeItemIDs.contains($0.key) }
            .sorted {
                if $0.value != $1.value { return $0.value > $1.value }
                return $0.key.uuidString < $1.key.uuidString
            }
            .prefix(3_000)
            .reduce(into: [String: JSONValue]()) { result, entry in
                result[entry.key.uuidString.lowercased()] = .string(format(entry.value))
            }
        let skipExceptions = activeOutcomes
            .filter { $0.disposition == .skipped }
            .reduce(into: [UUID: RecurrenceSessionOutcome]()) { latest, outcome in
                if latest[outcome.occurrenceID]?.occurredAt ?? .distantPast < outcome.occurredAt {
                    latest[outcome.occurrenceID] = outcome
                }
            }
            .values
            .sorted {
                if $0.occurredAt != $1.occurredAt { return $0.occurredAt > $1.occurredAt }
                return $0.occurrenceID.uuidString < $1.occurrenceID.uuidString
            }
            .prefix(3_000)
            .map { outcome in
                JSONValue.object([
                    "item_id": .string(outcome.itemID.uuidString.lowercased()),
                    "selector": .object([
                        "type": .string("occurrence"),
                        "id": .string(outcome.occurrenceID.uuidString.lowercased()),
                    ]),
                    "action": .object(["type": .string("skip")]),
                ])
            }
        return .init(
            asOf: asOf,
            horizonStart: start,
            horizonEnd: end,
            timezoneName: planningTimezone,
            availability: availability,
            fixedBlocks: [],
            previousAssignments: previousAssignments(stabilityStart: asOf, horizonEnd: end),
            config: .init(slotGranularityMinutes: 5, stabilityWeight: 4, defaultSoftWeight: 100),
            recurrenceContext: [
                "completed_occurrence_ids": .array(completedOccurrenceIDs),
                "completion_anchors": .object(completionAnchors),
                "exceptions": .array(skipExceptions),
            ]
        )
    }

    private func previousAssignments(
        stabilityStart: Date,
        horizonEnd: Date
    ) -> [DayWeaveSchedulePreviewRequest.PreviousAssignment] {
        struct AssignmentKey: Hashable {
            let itemID: UUID
            let itemRevision: UInt64
            let occurrenceID: UUID?
        }
        let frozenUntil = now().addingTimeInterval(TimeInterval(planner.freezeHours * 3_600))
        let currentRevisionByItem = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        let candidates = planner.blocks.compactMap { block -> (AssignmentKey, ScheduleBlock)? in
            guard block.syncOrigin == .canonicalPreview,
                  (block.previewKind == "planned" || block.previewKind == "pinned"),
                  block.status != .completed,
                  block.status != .skipped,
                  block.status != .canceled,
                  let itemID = block.sourceItemID,
                  let revision = block.sourceItemRevision,
                  currentRevisionByItem[itemID] == revision,
                  block.end > stabilityStart,
                  block.start < horizonEnd else { return nil }
            return (.init(itemID: itemID, itemRevision: revision, occurrenceID: block.occurrenceID), block)
        }
        let assignments: [DayWeaveSchedulePreviewRequest.PreviousAssignment] = Dictionary(
            grouping: candidates,
            by: \.0
        )
            .compactMap { key, entries -> DayWeaveSchedulePreviewRequest.PreviousAssignment? in
                let blocks = entries.map(\.1)
                let sessionIndices = blocks.map { $0.sessionIndex ?? 0 }
                guard Set(sessionIndices).count == sessionIndices.count else {
                    warnings.append(
                        "A previous assignment for \(key.itemID.uuidString) had duplicate session indices and was omitted."
                    )
                    return nil
                }
                let allInsideFreeze = blocks.allSatisfy {
                    $0.start >= stabilityStart && $0.end <= frozenUntil
                }
                return .init(
                    itemID: key.itemID,
                    itemRevision: key.itemRevision,
                    occurrenceID: key.occurrenceID,
                    blocks: entries.map(\.1).sorted {
                        if $0.start != $1.start { return $0.start < $1.start }
                        if $0.end != $1.end { return $0.end < $1.end }
                        return ($0.sessionIndex ?? 0) < ($1.sessionIndex ?? 0)
                    }.map {
                        .init(
                            start: $0.start,
                            end: $0.end,
                            sessionIndex: $0.sessionIndex ?? 0
                        )
                    },
                    pinned: allInsideFreeze
                )
            }
            .sorted {
                if $0.itemID != $1.itemID { return $0.itemID.uuidString < $1.itemID.uuidString }
                if $0.occurrenceID != $1.occurrenceID {
                    return ($0.occurrenceID?.uuidString ?? "") < ($1.occurrenceID?.uuidString ?? "")
                }
                return $0.itemRevision < $1.itemRevision
            }
        var retained: [DayWeaveSchedulePreviewRequest.PreviousAssignment] = []
        retained.reserveCapacity(min(assignments.count, previousAssignmentLimit))
        var retainedBlockCount = 0
        var omittedAssignmentCount = 0
        var omittedBlockCount = 0
        for assignment in assignments {
            let blockCount = assignment.blocks.count
            let (nextBlockCount, overflow) = retainedBlockCount.addingReportingOverflow(blockCount)
            guard retained.count < previousAssignmentLimit,
                  !overflow,
                  nextBlockCount <= previousAssignmentBlockLimit else {
                omittedAssignmentCount += 1
                omittedBlockCount += blockCount
                continue
            }
            retained.append(assignment)
            retainedBlockCount = nextBlockCount
        }
        if omittedAssignmentCount > 0 {
            warnings.append(
                "Omitted \(omittedAssignmentCount) previous assignment\(omittedAssignmentCount == 1 ? "" : "s") containing \(omittedBlockCount) block\(omittedBlockCount == 1 ? "" : "s") to stay within the API's \(previousAssignmentLimit)-assignment/\(previousAssignmentBlockLimit)-block limits."
            )
        }
        return retained
    }

    private func render(_ preview: DayWeaveSchedulePreview) -> [ScheduleBlock] {
        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        var previousBySession: [PreviewSessionIdentity: ScheduleBlock] = [:]
        var duplicatePreviousSessions = Set<PreviewSessionIdentity>()
        for previous in planner.blocks {
            guard let itemID = previous.sourceItemID else { continue }
            let identity = PreviewSessionIdentity(
                itemID: itemID,
                occurrenceID: previous.occurrenceID,
                sessionIndex: previous.sessionIndex ?? 0
            )
            if previousBySession.updateValue(previous, forKey: identity) != nil {
                duplicatePreviousSessions.insert(identity)
            }
        }
        for identity in duplicatePreviousSessions {
            previousBySession.removeValue(forKey: identity)
        }
        if !duplicatePreviousSessions.isEmpty {
            warnings.append(
                "\(duplicatePreviousSessions.count) duplicate local session identities were not used to restore status."
            )
        }
        let unscheduledOccurrences = Set(preview.plan.unscheduled.map {
            PreviewOccurrenceIdentity(itemID: $0.itemID, occurrenceID: $0.occurrenceID)
        })
        return preview.plan.blocks.map { block in
            let item = block.itemID.flatMap { itemByID[$0] }
            let previous = block.itemID.flatMap { itemID -> ScheduleBlock? in
                let candidate = previousBySession[.init(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )]
                return candidate?.sourceItemRevision == item?.revision ? candidate : nil
            }
            let explanation = block.explanations.map(\.message).joined(separator: " ")
            let occurrenceFullyScheduled = block.itemID.map {
                !unscheduledOccurrences.contains(.init(
                    itemID: $0,
                    occurrenceID: block.occurrenceID
                ))
            } ?? true
            let isPlannable = block.kind == "planned" || block.kind == "pinned"
            return ScheduleBlock(
                id: block.id,
                isSensitive: block.isSensitive,
                title: block.title,
                kind: item.map { plannerKind($0.kind) } ?? (block.kind == "calendar_event" ? .event : .breakTime),
                start: block.start,
                end: block.end,
                status: previous?.status ?? item.map { plannerStatus($0.status) } ?? .scheduled,
                project: item?.parentID.flatMap { itemByID[$0]?.title },
                notes: item?.notes ?? "",
                energy: item.map(energyLevel) ?? .medium,
                isFlexible: isPlannable && item?.kind != .event,
                isHardConstraint: !isPlannable,
                actualMinutes: previous?.actualMinutes,
                sourceItemID: block.itemID,
                sourceItemRevision: item?.revision,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex,
                syncOrigin: item == nil ? .externalPreview : .canonicalPreview,
                placementReason: explanation.isEmpty ? nil : explanation,
                previewKind: block.kind,
                occurrenceFullyScheduled: occurrenceFullyScheduled
            )
        }
    }

    private func previewMessage(
        _ preview: DayWeaveSchedulePreview,
        created: Int,
        privacyUpdated: Int,
        updated: Int
    ) -> String {
        var parts = [
            "Composed \(preview.plan.blocks.count) blocks",
            "\(preview.plan.score.unscheduledMinutes)m unscheduled",
        ]
        if created > 0 { parts.append("published \(created) new") }
        if privacyUpdated > 0 { parts.append("updated \(privacyUpdated) privacy") }
        if updated > 0 { parts.append("updated \(updated)") }
        if !preview.rejectedItems.isEmpty { parts.append("\(preview.rejectedItems.count) need review") }
        if !warnings.isEmpty { parts.append("\(warnings.count) sync warning\(warnings.count == 1 ? "" : "s")") }
        return parts.joined(separator: " · ")
    }

    private var planningTimezone: String {
        let identifier = TimeZone.autoupdatingCurrent.identifier
        return identifier == "GMT" ? "UTC" : identifier
    }

    private func energyLevel(_ item: DayWeaveCanonicalItem) -> EnergyLevel {
        guard case let .object(constraints) = item.flexibleConstraints,
              let energy = constraints["energy"] else { return .medium }
        let value: String?
        switch energy {
        case let .string(raw): value = raw
        case let .object(object):
            if case let .string(raw)? = object["value"] { value = raw } else { value = nil }
        default: value = nil
        }
        return value.flatMap(EnergyLevel.init(rawValue:)) ?? .medium
    }

    private func reloadConfigurationStatus() {
        status = makeClient(reportFailure: false) == nil
            ? .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
            : .ready
    }

    private func ensureOperationCurrent(
        operationID: UUID,
        generation: UInt64
    ) throws {
        guard !Task.isCancelled,
              activeSyncID == operationID,
              configurationGeneration == generation else {
            throw CanonicalSyncError.operationSuperseded
        }
        guard planner.canPersistPlan else {
            if let persistenceError = planner.persistenceError { throw persistenceError }
            throw CanonicalSyncError.localPersistenceUnavailable
        }
    }

    private func makeClient(reportFailure: Bool) -> DayWeaveAPIClient? {
        do {
            guard let configuredURL = configurationStore.loadBaseURL() else {
                if reportFailure {
                    status = .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
                }
                return nil
            }
            let baseURL = try DayWeaveAPIBaseURL(configuredURL)
            if let authCoordinator {
                guard authCoordinator.hasUsableCredential(boundTo: baseURL) else {
                    if reportFailure {
                        status = .configurationRequired("Authenticate this Mac in Settings before syncing.")
                    }
                    return nil
                }
                return DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    authCoordinator: authCoordinator
                )
            }
            guard let token = try tokenStore.loadToken(boundTo: baseURL),
                  !token.isEmpty else {
                if reportFailure {
                    status = .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
                }
                return nil
            }
            return DayWeaveAPIClient(
                baseURL: baseURL,
                session: session,
                bearerToken: token
            )
        } catch {
            if reportFailure { status = .configurationRequired(error.localizedDescription) }
            return nil
        }
    }

    private func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

private enum PendingSchedulePublicationRecovery {
    case installed(revisionNumber: UInt64, blockCount: Int)
    case requiresFreshComposition
}

private struct InstalledSchedulePublication {
    let preview: DayWeaveSchedulePreview
    let blockCount: Int
}

private struct PreviewSessionIdentity: Hashable {
    let itemID: UUID
    let occurrenceID: UUID?
    let sessionIndex: UInt16
}

private struct PreviewOccurrenceIdentity: Hashable {
    let itemID: UUID
    let occurrenceID: UUID?
}

private enum CanonicalSyncError: LocalizedError {
    case invalidDeltaSequence
    case tooManyDeltaPages
    case localPersistenceUnavailable
    case operationSuperseded
    case sourceRevisionMismatch
    case invalidPreview(String)
    case invalidSchedulePublication
    case schedulePublicationStayedStale
    case schedulePublicationReplayNeedsFreshComposition
    case deltaResourceLimit
    case invalidMutationResponse

    var errorDescription: String? {
        switch self {
        case .invalidDeltaSequence: "The canonical item stream returned a non-progressing cursor."
        case .tooManyDeltaPages: "Canonical sync exceeded the safe page limit."
        case .localPersistenceUnavailable: "Canonical sync stopped because encrypted local storage is unavailable."
        case .operationSuperseded: "Canonical sync was cancelled because its configuration changed."
        case .sourceRevisionMismatch: "The scheduler preview never matched the local canonical revision map after three bounded retries."
        case let .invalidPreview(diagnostic): "The scheduler preview was rejected safely: \(diagnostic)"
        case .invalidSchedulePublication: "The schedule publication journal or server acknowledgment was rejected safely. The exact request remains available for recovery."
        case .schedulePublicationStayedStale: "Canonical items changed during both bounded publication attempts. No candidate was applied or left pending; sync again to retry."
        case .schedulePublicationReplayNeedsFreshComposition: "The bounded fresh publication also returned an older exact receipt. No candidate was applied or left pending; sync again to recompose."
        case .deltaResourceLimit: "Canonical sync exceeded the safe 20,000-change or 32 MiB retained-delta limit."
        case .invalidMutationResponse: "The canonical API returned a mutation result with the wrong identity, status, or revision. Local state was not changed."
        }
    }
}

private func canonicalKind(_ kind: PlannerItemKind) -> DayWeaveCanonicalItemKind {
    switch kind {
    case .event: .event
    case .task: .task
    case .habit: .habit
    case .routine: .routine
    case .goal: .goal
    case .breakTime: .breakTime
    }
}

private func plannerKind(_ kind: DayWeaveCanonicalItemKind) -> PlannerItemKind {
    switch kind {
    case .event: .event
    case .task: .task
    case .habit: .habit
    case .routine: .routine
    case .goal: .goal
    case .breakTime: .breakTime
    case .unknown: .task
    }
}

private func canonicalStatus(_ status: PlannerItemStatus) -> DayWeaveCanonicalItemStatus? {
    switch status {
    case .notStarted: .planned
    case .scheduled: .scheduled
    case .active: .inProgress
    case .paused: .paused
    case .completed: .completed
    case .skipped: .skipped
    case .canceled: .cancelled
    case .blocked: nil
    }
}

private func plannerStatus(_ status: DayWeaveCanonicalItemStatus) -> PlannerItemStatus {
    switch status {
    case .inbox, .planned: .notStarted
    case .scheduled: .scheduled
    case .inProgress: .active
    case .paused: .paused
    case .completed: .completed
    case .skipped: .skipped
    case .cancelled: .canceled
    case .unknown: .blocked
    }
}
