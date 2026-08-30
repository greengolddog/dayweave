import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Google integration store", .serialized)
@MainActor
struct GoogleIntegrationStoreTests {
    @Test("privacy suspension redacts trusted data and rejects a late response")
    func privacySuspensionRejectsLateResponse() async throws {
        let account = try Self.account(label: "private-owner@example.com")
        let collection = try Self.collection(accountID: account.id)
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let late = SuspendedGoogleValue<GoogleAccountsSnapshot>()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot), .suspended(late)],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()

        #expect(store.accounts == [account])
        #expect(store.collectionsByAccount[account.id] == [collection])

        let reload = Task { @MainActor in await store.reload() }
        await Self.waitUntil { await transport.accountRequestCount() == 2 }
        store.suspendForPrivacyBoundary()

        #expect(store.accounts.isEmpty)
        #expect(store.collectionsByAccount.isEmpty)
        #expect(store.syncStatusByAccount.isEmpty)
        #expect(store.cleanupStatus == nil)
        #expect(store.status == .privacyProtected)
        #expect(store.sidebarMessage == "Google · locked")

        await late.resume(returning: snapshot)
        await reload.value

        #expect(store.accounts.isEmpty)
        #expect(store.collectionsByAccount.isEmpty)
        #expect(store.syncStatusByAccount.isEmpty)
        #expect(store.status == .privacyProtected)
    }

    @Test("ambiguous OAuth start reuses the exact journal request and idempotency key")
    func ambiguousOAuthStartRetriesExactJournal() async throws {
        let authorization = try Self.authorization()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: []))],
            oauthStarts: [
                .failure(.transport(.timedOut)),
                .value(authorization),
            ]
        )
        let journalStore = InMemoryGoogleOAuthStartJournalStore()
        let store = Self.store(transport: transport, journalStore: journalStore)
        store.activate(automaticallyReload: false)

        await store.connectGoogleAccount()

        let durable = try #require(journalStore.journal)
        let first = try #require((await transport.oauthStartRecords()).first)
        #expect(durable.request.services.isEmpty)
        #expect(durable.request.forceConsent == false)
        #expect(durable.request.loginHint == nil)
        #expect(durable.request.accountID == nil)
        #expect(durable.request.connectNew == false)
        #expect(durable.request.makeDefault == true)
        #expect(durable.idempotencyKey == first.idempotencyKey)
        #expect(durable.request == first.request)
        #expect(store.canRetryAuthorization)
        if case .authorizationOutcomeUnknown = store.status {
            // Expected: the POST may have committed despite the lost response.
        } else {
            Issue.record("An ambiguous OAuth start was not retained as an unknown outcome")
        }

        await store.retryExactAuthorizationRequest()

        let records = await transport.oauthStartRecords()
        #expect(records.count == 2)
        #expect(records[1] == records[0])
        #expect(journalStore.journal?.request == durable.request)
        #expect(journalStore.journal?.idempotencyKey == durable.idempotencyKey)
        #expect(store.canOpenAuthorization)
        #expect(!store.canRetryAuthorization)
    }

    @Test("maximum OAuth lifetime remains valid after request latency")
    func maximumOAuthLifetimeAllowsRequestLatency() {
        let responseAt = Self.now
        let journal = GoogleOAuthStartJournal(
            request: GoogleOAuthStartRequest(),
            idempotencyKey: "mac-google-oauth-11111111-2222-4333-8444-555555555555",
            configurationIdentifier: "google-store-test-configuration",
            baselineAccountRevisions: [:],
            createdAt: responseAt.addingTimeInterval(-2),
            expiresAt: responseAt.addingTimeInterval(
                GoogleOAuthStartJournal.maximumLifetime
            )
        )

        #expect(journal.isValid(now: responseAt))
    }

    @Test("authorization URL stays private and the injected opener consumes it once")
    func authorizationURLIsConsumedByInjectedOpener() async throws {
        let authorization = try Self.authorization()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(try Self.accountsSnapshot(accounts: [])),
                .value(try Self.accountsSnapshot(accounts: [])),
            ],
            oauthStarts: [.value(authorization)]
        )
        let journalStore = InMemoryGoogleOAuthStartJournalStore()
        let opener = GoogleAuthorizationOpenerRecorder()
        let store = Self.store(
            transport: transport,
            journalStore: journalStore,
            opener: { opener.open($0) },
            pollLimit: 1
        )
        store.activate(automaticallyReload: false)

        await store.connectGoogleAccount()

        #expect(store.canOpenAuthorization)
        #expect(opener.urls.isEmpty)
        #expect(!store.status.message.contains("accounts.google.com"))
        #expect(!store.status.message.contains("opaque-state"))

        #expect(store.openAuthorizationPage())
        #expect(store.isBusy)
        #expect(opener.urls.map(\.absoluteString) == [authorization.authorizationURL])
        #expect(!store.canOpenAuthorization)
        #expect(journalStore.journal?.browserOpenedAt != nil)

        #expect(!store.openAuthorizationPage())
        #expect(opener.urls.count == 1)
        await Self.waitUntil { await transport.accountRequestCount() == 2 }
    }

    @Test("failed browser open restores the exact authorization retry")
    func failedBrowserOpenRestoresExactRetry() async throws {
        let authorization = try Self.authorization()
        let empty = try Self.accountsSnapshot(accounts: [])
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(empty), .value(empty)],
            oauthStarts: [.value(authorization), .value(authorization)]
        )
        let journalStore = InMemoryGoogleOAuthStartJournalStore()
        let store = Self.store(
            transport: transport,
            journalStore: journalStore,
            opener: { _ in false },
            pollLimit: 1
        )
        store.activate(automaticallyReload: false)

        await store.connectGoogleAccount()
        let original = try #require(journalStore.journal)
        #expect(!store.openAuthorizationPage())
        #expect(!store.isBusy)
        #expect(store.canRetryAuthorization)
        #expect(!store.canCheckAuthorization)
        #expect(journalStore.journal?.browserOpenedAt == nil)

        await store.retryExactAuthorizationRequest()

        let records = await transport.oauthStartRecords()
        #expect(records.count == 2)
        #expect(records[0] == records[1])
        #expect(journalStore.journal?.idempotencyKey == original.idempotencyKey)
        #expect(store.canOpenAuthorization)
    }

    @Test("unrelated account changes never prove the exact OAuth attempt")
    func accountChangesDoNotCompleteAuthorizationRecovery() async throws {
        let existing = try Self.account(revision: 4)
        let concurrentlyChanged = try Self.account(revision: 5)
        let collection = try Self.collection(accountID: existing.id)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(try Self.accountsSnapshot(accounts: [existing])),
                .value(try Self.accountsSnapshot(accounts: [concurrentlyChanged])),
            ],
            oauthStarts: [.value(try Self.authorization())],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: existing.id))]
        )
        let journalStore = InMemoryGoogleOAuthStartJournalStore()
        let store = Self.store(
            transport: transport,
            journalStore: journalStore,
            pollLimit: 1
        )
        store.activate(automaticallyReload: false)

        await store.connectGoogleAccount()
        #expect(journalStore.journal?.baselineAccountRevisions[existing.id] == 4)
        #expect(store.openAuthorizationPage())
        await store.waitForCurrentOperation()

        #expect(journalStore.journal != nil)
        #expect(store.authorizationRecoveryRequiresAttention)
        #expect(store.canCheckAuthorization)
        if case .authorizationOutcomeUnknown = store.status {
            // Accounts changed, but the endpoint cannot bind that change to this browser attempt.
        } else {
            Issue.record("An inferred account change was incorrectly treated as OAuth proof")
        }
    }

    @Test("configuration rotation rejects the old transport response")
    func configurationRotationRejectsOldResponse() async throws {
        let oldAccount = try Self.account(label: "old-session@example.com")
        let oldSnapshot = try Self.accountsSnapshot(accounts: [oldAccount])
        let oldGate = SuspendedGoogleValue<GoogleAccountsSnapshot>()
        let oldTransport = FakeGoogleIntegrationTransport(
            configurationIdentifier: "configuration-old",
            accounts: [.suspended(oldGate)]
        )
        let newTransport = FakeGoogleIntegrationTransport(
            configurationIdentifier: "configuration-new",
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let provider = RotatingGoogleTransportProvider(current: oldTransport)
        let store = GoogleIntegrationStore(
            transportProvider: { provider.current },
            journalStore: InMemoryGoogleOAuthStartJournalStore(),
            disconnectJournalStore: InMemoryGoogleDisconnectRetryJournalStore(),
            refreshCompletionJournalStore:
                InMemoryGooglePendingRefreshCompletionJournalStore(),
            authorizationPollLimit: 1,
            now: { Self.now },
            sleep: { _ in }
        )
        store.activate(automaticallyReload: false)

        let oldReload = Task { @MainActor in await store.reload() }
        await Self.waitUntil { await oldTransport.accountRequestCount() == 1 }
        provider.current = newTransport
        store.configurationDidChange()
        await oldGate.resume(returning: oldSnapshot)

        await Self.waitUntil { await newTransport.accountRequestCount() == 1 }
        await oldReload.value
        await store.waitForCurrentOperation()

        #expect(store.accounts.isEmpty)
        #expect(!store.sidebarMessage.contains("old-session"))
        #expect(await oldTransport.collectionRequestCount() == 0)
        #expect(store.status != .privacyProtected)
    }

    @Test("task blocking and writable roles fail before transport")
    func unsafeTaskRolesFailBeforeTransport() async throws {
        let account = try Self.account()
        let taskList = try Self.collection(accountID: account.id, kind: .taskList)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([taskList])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.configureSource(taskList, selected: true, visible: true, role: .blocking)
        await store.configureSource(taskList, selected: true, visible: true, role: .writable)

        #expect(await transport.configureRecords().isEmpty)
        #expect(store.collectionsByAccount[account.id] == [taskList])
        #expect(store.status.isFailure)
    }

    @Test("Calendar publishing scope upgrade is explicit and crash recoverable")
    func calendarPublishingScopeUpgradeIsExplicit() async throws {
        let account = try Self.account()
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let authorization = try Self.authorization()
        let journalStore = InMemoryGoogleOAuthStartJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot), .value(snapshot)],
            oauthStarts: [.value(authorization)],
            collections: [.value([])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport, journalStore: journalStore)
        store.activate(automaticallyReload: false)
        await store.reload()

        #expect(store.canEnableCalendarPublishing(for: account))
        await store.enableCalendarPublishing(for: account)

        let record = try #require((await transport.oauthStartRecords()).last)
        #expect(record.request.services == [.calendar])
        #expect(record.request.forceConsent)
        #expect(record.request.accountID == account.id)
        #expect(!record.request.connectNew)
        #expect(record.request.makeDefault)
        #expect(journalStore.journal?.request == record.request)
        #expect(store.canOpenAuthorization)
    }

    @Test("owner Calendar with full scope can become writable with exact policy")
    func writableCalendarConfigurationPreservesPublicationPolicy() async throws {
        let account = try Self.account(calendarWrite: true)
        let initial = try Self.collection(accountID: account.id)
        let policy = GoogleCalendarPolicy(
            publishAllDay: true,
            publishTentative: false,
            publishFree: true
        )
        let updated = try Self.collection(
            accountID: account.id,
            selected: true,
            role: .writable,
            revision: 2,
            publishAllDay: true,
            publishFree: true
        )
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([initial])],
            configurations: [.value(updated)],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.configureSource(
            initial,
            selected: true,
            visible: true,
            role: .writable,
            calendarPolicy: policy
        )

        let record = try #require((await transport.configureRecords()).last)
        #expect(record.role == .writable)
        #expect(record.selected)
        #expect(record.calendarPolicy == policy)
        #expect(store.collectionsByAccount[account.id] == [updated])
        #expect(!store.status.isFailure)
    }

    @Test("Calendar publishing rejects missing scope and non-writer roles locally")
    func writableCalendarRequiresScopeAndProviderRole() async throws {
        let readOnlyAccount = try Self.account()
        let owner = try Self.collection(accountID: readOnlyAccount.id)
        let missingScopeTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [readOnlyAccount]))],
            collections: [.value([owner])],
            syncStatuses: [.value(try Self.syncStatus(accountID: readOnlyAccount.id))]
        )
        let missingScopeStore = Self.store(transport: missingScopeTransport)
        missingScopeStore.activate(automaticallyReload: false)
        await missingScopeStore.reload()
        await missingScopeStore.configureSource(
            owner,
            selected: true,
            visible: true,
            role: .writable
        )
        #expect(await missingScopeTransport.configureRecords().isEmpty)

        let writableAccount = try Self.account(calendarWrite: true)
        let reader = try Self.collection(
            accountID: writableAccount.id,
            providerAccessRole: "reader"
        )
        let readerTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [writableAccount]))],
            collections: [.value([reader])],
            syncStatuses: [.value(try Self.syncStatus(accountID: writableAccount.id))]
        )
        let readerStore = Self.store(transport: readerTransport)
        readerStore.activate(automaticallyReload: false)
        await readerStore.reload()
        await readerStore.configureSource(
            reader,
            selected: true,
            visible: true,
            role: .writable
        )
        #expect(await readerTransport.configureRecords().isEmpty)
    }

    @Test("failed discovery refreshes inventory without claiming completion")
    func failedDiscoveryKeepsUncertainOutcome() async throws {
        let account = try Self.account()
        let initial = try Self.collection(accountID: account.id, revision: 1)
        let partial = try Self.collection(accountID: account.id, revision: 2)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([initial]), .value([partial])],
            discoveries: [.failure(.server(
                statusCode: 503,
                code: "provider_temporary",
                message: "provider detail",
                requestID: nil
            ))],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.discoverSources(for: account)

        #expect(store.collectionsByAccount[account.id] == [partial])
        #expect(store.status.isFailure)
        #expect(store.status.message.contains("may be incomplete"))
        #expect(!store.status.message.contains("provider detail"))
    }

    @Test("credential transitions and Google mutations reserve one atomic lane")
    func credentialTransitionAndMutationFencesBothStartOrders() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let transitionFirstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let transitionFirst = Self.store(transport: transitionFirstTransport)
        transitionFirst.activate(automaticallyReload: false)
        await transitionFirst.reload()

        #expect(transitionFirst.beginCredentialTransition())
        await transitionFirst.discoverSources(for: account)
        #expect(await transitionFirstTransport.discoveryRequestCount() == 0)
        transitionFirst.endCredentialTransition()

        let discoveryGate = SuspendedGoogleValue<[GoogleSyncCollection]>()
        let mutationFirstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            discoveries: [.suspended(discoveryGate)],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let mutationFirst = Self.store(transport: mutationFirstTransport)
        mutationFirst.activate(automaticallyReload: false)
        await mutationFirst.reload()

        let discovery = Task { @MainActor in
            await mutationFirst.discoverSources(for: account)
        }
        await Self.waitUntil {
            await mutationFirstTransport.discoveryRequestCount() == 1
        }
        #expect(!mutationFirst.beginCredentialTransition())
        await discoveryGate.resume(returning: [collection])
        await discovery.value
    }

    @Test("pending disconnect permits only same-API-base authentication repair")
    func pendingDisconnectAllowsSameAPIBaseCredentialRepair() throws {
        let sameBase = try DayWeaveAPIBaseURL("https://api.example.com/gateway")
        let differentBase = try DayWeaveAPIBaseURL("https://api.example.com/other-gateway")
        let configurationIdentifier =
            "\(sameBase.canonicalConfigurationIdentifier)|auth=device-v1:test-binding"
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let journal = try GoogleDisconnectRetryJournal(
            accountID: Self.accountID,
            expectedRevision: 1,
            idempotencyKey: "mac-google-disconnect-repair-test",
            configurationIdentifier: configurationIdentifier,
            createdAt: Self.now
        )
        try disconnectJournalStore.save(journal, now: Self.now)
        let transport = FakeGoogleIntegrationTransport(
            configurationIdentifier: configurationIdentifier
        )
        let store = Self.store(
            transport: transport,
            disconnectJournalStore: disconnectJournalStore
        )
        store.activate(automaticallyReload: false)

        #expect(store.hasPendingRecovery)
        #expect(!store.beginCredentialTransition())
        #expect(!store.beginCredentialRepairTransition(boundTo: differentBase))
        #expect(store.canRepairAuthentication(boundTo: sameBase))
        #expect(store.beginCredentialRepairTransition(boundTo: sameBase))
        store.endCredentialTransition()
    }

    @Test("ambiguous collection update reconciles through an authoritative GET")
    func ambiguousCollectionUpdateReconcilesThroughGet() async throws {
        let account = try Self.account()
        let initial = try Self.collection(accountID: account.id, revision: 1)
        let updated = try Self.collection(
            accountID: account.id,
            selected: true,
            visible: true,
            revision: 2
        )
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([initial]), .value([updated])],
            configurations: [.failure(.transport(.networkConnectionLost))],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.configureSource(initial, selected: true, visible: true, role: .readOnly)

        let record = try #require((await transport.configureRecords()).first)
        #expect(record.accountID == account.id)
        #expect(record.collectionID == initial.id)
        #expect(record.expectedRevision == 1)
        #expect(record.selected)
        #expect(record.visible)
        #expect(record.role == .readOnly)
        #expect(await transport.collectionRequestCount() == 2)
        #expect(store.collectionsByAccount[account.id] == [updated])
        #expect(!store.status.isFailure)
    }

    @Test("post-send decoding and authentication changes retain mutation recovery")
    func postSendFailuresRequireAuthoritativeRecovery() async throws {
        for failure in [
            DayWeaveAPIError.responseDecodingFailed,
            DayWeaveAPIError.durableAuthentication(.concurrentStateChange),
        ] {
            let account = try Self.account()
            let initial = try Self.collection(accountID: account.id, revision: 1)
            let transport = FakeGoogleIntegrationTransport(
                accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
                collections: [.value([initial]), .value([initial])],
                configurations: [.failure(failure)],
                syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
            )
            let store = Self.store(transport: transport)
            store.activate(automaticallyReload: false)
            await store.reload()

            await store.configureSource(
                initial,
                selected: true,
                visible: true,
                role: .readOnly
            )

            #expect(store.mutationRecoveryRequired)
            #expect(store.status.isFailure || store.accounts.isEmpty)
        }
    }

    @Test("refresh composes once only after a matching completed idle run")
    func refreshComposesOnlyAfterMatchingCompletion() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let requestedAt = Self.now
        let completedGate = SuspendedGoogleValue<GoogleSyncStatus>()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [
                .value(try Self.syncStatus(
                    accountID: account.id,
                    state: .idle,
                    requestedAt: Self.now.addingTimeInterval(-120),
                    completedAt: Self.now.addingTimeInterval(-60),
                    revision: 1
                )),
                .value(try Self.syncStatus(
                    accountID: account.id,
                    state: .idle,
                    requestedAt: requestedAt.addingTimeInterval(-30),
                    startedAt: requestedAt.addingTimeInterval(-20),
                    completedAt: requestedAt.addingTimeInterval(1),
                    revision: 2
                )),
                .suspended(completedGate),
            ],
            refreshes: [.value(try Self.refreshAccepted(
                accountID: account.id,
                requestedAt: requestedAt
            ))]
        )
        let completions = GoogleImportCompletionCounter()
        let store = Self.store(transport: transport, pollLimit: 3)
        store.installImportCompletionVerifier {
            await completions.increment()
            return true
        }
        store.activate(automaticallyReload: false)
        await store.reload()

        let refresh = Task { @MainActor in await store.refreshImports(for: account) }
        await Self.waitUntil { await transport.syncStatusRequestCount() == 3 }
        #expect(await completions.value() == 0)

        await completedGate.resume(returning: try Self.syncStatus(
            accountID: account.id,
            state: .idle,
            requestedAt: requestedAt,
            startedAt: requestedAt,
            completedAt: requestedAt.addingTimeInterval(1),
            importedCount: UInt64(Int64.max),
            updatedCount: UInt64(Int64.max),
            deletedCount: UInt64(Int64.max),
            refreshGeneration: 1,
            claimedRefreshGeneration: 1,
            completedRefreshGeneration: 1,
            revision: 3
        ))
        await refresh.value

        #expect(await completions.value() == 1)
        #expect(await transport.refreshRequestCount() == 1)
        #expect(store.status.message.contains("very large change set"))
        if case .connected = store.status {
            // Matching authoritative completion accepted.
        } else {
            Issue.record("A matching completed import did not transition to connected")
        }
    }

    @Test("definite retry failure retains the earlier ambiguous refresh marker")
    func definiteRefreshRetryFailureRetainsAmbiguousMarker() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let marker = try GooglePendingRefreshCompletionJournal(
            accountID: account.id,
            localRequestStartedAt: Self.now.addingTimeInterval(-60),
            configurationIdentifier: "google-store-test-configuration",
            createdAt: Self.now.addingTimeInterval(-60)
        )
        let journalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        try journalStore.save(marker, now: Self.now)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))],
            refreshes: [.failure(.server(
                statusCode: 400,
                code: "invalid_request",
                message: "redacted",
                requestID: nil
            ))]
        )
        let store = Self.store(
            transport: transport,
            refreshCompletionJournalStore: journalStore
        )
        store.activate(automaticallyReload: false)
        await store.reload()

        #expect(store.canRetryPendingRefresh(for: account))
        await store.refreshImports(for: account)

        #expect(await transport.refreshRequestCount() == 1)
        #expect(await transport.recordedRefreshRequestIDs() == [marker.requestID])
        #expect(journalStore.journals == [marker])
        #expect(journalStore.deleteCount == 0)
        #expect(store.hasPendingRefreshCompletion(for: account))
        #expect(store.canRetryPendingRefresh(for: account))
        #expect(store.hasPendingRecovery)
        #expect(!store.mutationRecoveryRequired)
    }

    @Test("an interrupted refresh response replays the exact durable request")
    func ambiguousRefreshResponseReplaysExactRequest() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let completed = try Self.syncStatus(
            accountID: account.id,
            state: .idle,
            requestedAt: Self.now.addingTimeInterval(-3_600),
            startedAt: Self.now.addingTimeInterval(3_600),
            completedAt: Self.now.addingTimeInterval(3_601),
            refreshGeneration: 1,
            claimedRefreshGeneration: 1,
            completedRefreshGeneration: 1,
            revision: 2
        )
        let journalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [
                .value(try Self.syncStatus(accountID: account.id)),
                .value(completed),
            ],
            refreshes: [
                .failure(.transport(.networkConnectionLost)),
                .value(try Self.refreshAccepted(
                    accountID: account.id,
                    requestedAt: Self.now.addingTimeInterval(-3_600)
                )),
            ]
        )
        let completions = GoogleImportCompletionCounter()
        let store = Self.store(
            transport: transport,
            refreshCompletionJournalStore: journalStore,
            pollLimit: 1
        )
        store.installImportCompletionVerifier {
            await completions.increment()
            return true
        }
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.refreshImports(for: account)
        let pending = try #require(journalStore.journals.first)
        #expect(pending.serverRequestedAt == nil)
        #expect(pending.targetRefreshGeneration == nil)
        #expect(store.canRetryPendingRefresh(for: account))

        await store.refreshImports(for: account)

        let requestIDs = await transport.recordedRefreshRequestIDs()
        #expect(requestIDs == [pending.requestID, pending.requestID])
        #expect(await completions.value() == 1)
        #expect(journalStore.journals.isEmpty)
        #expect(!store.hasPendingRefreshCompletion(for: account))
    }

    @Test("privacy cancellation cannot delete an in-flight refresh identity")
    func privacyCancellationRetainsInflightRefreshIdentity() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let gate = SuspendedGoogleResult<GoogleSyncRefreshAccepted>()
        let journalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))],
            refreshes: [.suspendedResult(gate)]
        )
        let store = Self.store(
            transport: transport,
            refreshCompletionJournalStore: journalStore,
            pollLimit: 1
        )
        store.activate(automaticallyReload: false)
        await store.reload()

        let refresh = Task { @MainActor in await store.refreshImports(for: account) }
        await Self.waitUntil { await gate.requestCount() == 1 }
        let durable = try #require(journalStore.journals.first)
        store.suspendForPrivacyBoundary()
        await gate.resume(returning: .failure(.transport(.cancelled)))
        await refresh.value

        #expect(journalStore.journals == [durable])
        #expect(journalStore.deleteCount == 0)
        #expect(await transport.recordedRefreshRequestIDs() == [durable.requestID])
        #expect(store.status == .privacyProtected)
    }

    @Test("terminal imports retry from a new durable boundary and reject old completion")
    func terminalRefreshRetryUsesNewDurableBoundary() async throws {
        for terminalState in [
            GoogleSyncRunState.failed,
            GoogleSyncRunState.reauthorizationRequired,
        ] {
            let account = try Self.account()
            let collection = try Self.collection(accountID: account.id)
            let oldRequestedAt = Date(
                timeIntervalSince1970: floor(Self.now.timeIntervalSince1970) - 120
            )
            let marker = try GooglePendingRefreshCompletionJournal(
                accountID: account.id,
                localRequestStartedAt: oldRequestedAt,
                serverRequestedAt: oldRequestedAt,
                targetRefreshGeneration: 2,
                configurationIdentifier: "google-store-test-configuration",
                createdAt: oldRequestedAt
            )
            let journalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
            try journalStore.save(marker, now: Self.now)
            let terminalClaimGeneration: UInt64 = terminalState == .failed ? 1 : 2
            let terminalCompletedGeneration: UInt64 = terminalState == .failed ? 0 : 2
            let terminal = try Self.syncStatus(
                accountID: account.id,
                state: terminalState,
                requestedAt: oldRequestedAt,
                startedAt: oldRequestedAt.addingTimeInterval(1),
                completedAt: oldRequestedAt.addingTimeInterval(2),
                refreshGeneration: 2,
                claimedRefreshGeneration: terminalClaimGeneration,
                completedRefreshGeneration: terminalCompletedGeneration,
                revision: 2
            )
            let staleIdle = try Self.syncStatus(
                accountID: account.id,
                state: .idle,
                requestedAt: oldRequestedAt,
                startedAt: oldRequestedAt.addingTimeInterval(1),
                completedAt: oldRequestedAt.addingTimeInterval(2),
                refreshGeneration: 3,
                claimedRefreshGeneration: 1,
                revision: 3
            )
            let freshIdle = try Self.syncStatus(
                accountID: account.id,
                state: .idle,
                requestedAt: Self.now,
                startedAt: Self.now.addingTimeInterval(1),
                completedAt: Self.now.addingTimeInterval(2),
                refreshGeneration: 3,
                claimedRefreshGeneration: 3,
                completedRefreshGeneration: 3,
                revision: 4
            )
            let transport = FakeGoogleIntegrationTransport(
                accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
                collections: [.value([collection])],
                syncStatuses: [.value(terminal), .value(staleIdle), .value(freshIdle)],
                refreshes: [.value(try Self.refreshAccepted(
                    accountID: account.id,
                    requestedAt: Self.now,
                    refreshGeneration: 3
                ))]
            )
            let completions = GoogleImportCompletionCounter()
            let store = Self.store(
                transport: transport,
                refreshCompletionJournalStore: journalStore,
                pollLimit: 2
            )
            store.installImportCompletionVerifier {
                await completions.increment()
                return true
            }
            store.activate(automaticallyReload: false)
            await store.reload()

            #expect(store.canRetryPendingRefresh(for: account))
            await store.refreshImports(for: account)

            #expect(await transport.refreshRequestCount() == 1)
            #expect(await transport.syncStatusRequestCount() == 3)
            #expect(await completions.value() == 1)
            #expect(journalStore.journals.isEmpty)
            #expect(journalStore.saved.contains { saved in
                saved.localRequestStartedAt == Self.now && saved.serverRequestedAt == nil
            })
            #expect(!store.hasPendingRefreshCompletion(for: account))
        }
    }

    @Test("trusted stale disconnect revision retires the obsolete exact request")
    func trustedDisconnectNoEffectRetiresObsoleteJournal() async throws {
        let account = try Self.account(revision: 1)
        let revised = try Self.account(revision: 2)
        let collection = try Self.collection(accountID: account.id)
        let initialSnapshot = try Self.accountsSnapshot(accounts: [account])
        let revisedSnapshot = try Self.accountsSnapshot(accounts: [revised])
        let journalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(initialSnapshot), .value(revisedSnapshot)],
            collections: [.value([collection]), .value([collection])],
            disconnects: [.failure(.trustedGoogleDisconnectNoEffect)],
            syncStatuses: [
                .value(try Self.syncStatus(accountID: account.id, revision: 1)),
                .value(try Self.syncStatus(accountID: account.id, revision: 2)),
            ]
        )
        let store = Self.store(
            transport: transport,
            disconnectJournalStore: journalStore
        )
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.disconnectGoogleAccount(account)

        let request = try #require((await transport.disconnectRecords()).first)
        #expect(request.expectedRevision == 1)
        #expect(journalStore.journal == nil)
        #expect(journalStore.deleteCount == 1)
        #expect(store.accounts == [revised])
        #expect(!store.hasPendingDisconnectRecovery(for: revised))
        #expect(!store.hasPendingRecovery)
    }

    @Test("trusted stale disconnect retains composition proof when the account disappeared")
    func trustedDisconnectNoEffectWaitsForAbsentAccountComposition() async throws {
        let account = try Self.account(revision: 1)
        let collection = try Self.collection(accountID: account.id)
        let initialSnapshot = try Self.accountsSnapshot(accounts: [account])
        let emptySnapshot = try Self.accountsSnapshot(accounts: [])
        let journalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let rejectedCompositions = GoogleImportCompletionCounter()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(initialSnapshot), .value(emptySnapshot)],
            collections: [.value([collection])],
            disconnects: [.failure(.trustedGoogleDisconnectNoEffect)],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(
            transport: transport,
            disconnectJournalStore: journalStore
        )
        store.installImportCompletionVerifier {
            await rejectedCompositions.increment()
            return false
        }
        store.activate(automaticallyReload: false)
        await store.reload()

        await store.disconnectGoogleAccount(account)

        let retained = try #require(journalStore.journal)
        #expect(retained.accountID == account.id)
        #expect(journalStore.deleteCount == 0)
        #expect(await rejectedCompositions.value() == 1)
        #expect(store.hasPendingRecovery)

        let acceptedCompositions = GoogleImportCompletionCounter()
        let relaunched = Self.store(
            transport: FakeGoogleIntegrationTransport(accounts: [.value(emptySnapshot)]),
            disconnectJournalStore: journalStore
        )
        relaunched.installImportCompletionVerifier {
            await acceptedCompositions.increment()
            return true
        }
        relaunched.activate(automaticallyReload: false)
        await relaunched.reload()

        #expect(await acceptedCompositions.value() == 1)
        #expect(journalStore.journal == nil)
        #expect(journalStore.deleteCount == 1)
        #expect(!relaunched.hasPendingRecovery)
    }

    @Test("ambiguous disconnect survives relaunch and retries the exact request identity")
    func disconnectRecoverySurvivesRelaunch() async throws {
        let account = try Self.account(revision: 1)
        let collection = try Self.collection(accountID: account.id)
        let activeSnapshot = try Self.accountsSnapshot(accounts: [account])
        let initialStatus = try Self.syncStatus(accountID: account.id)
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        let firstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(activeSnapshot), .value(activeSnapshot)],
            collections: [.value([collection]), .value([collection])],
            disconnects: [.failure(.transport(.networkConnectionLost))],
            syncStatuses: [.value(initialStatus), .value(initialStatus)]
        )
        let firstStore = Self.store(
            transport: firstTransport,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshJournalStore,
            pollLimit: 1
        )
        firstStore.activate(automaticallyReload: false)
        await firstStore.reload()

        await firstStore.disconnectGoogleAccount(account)

        let durable = try #require(disconnectJournalStore.journal)
        let firstRequest = try #require((await firstTransport.disconnectRecords()).first)
        #expect(firstRequest.accountID == durable.accountID)
        #expect(firstRequest.expectedRevision == durable.expectedRevision)
        #expect(firstRequest.idempotencyKey == durable.idempotencyKey)
        #expect(firstStore.hasPendingDisconnectRecovery(for: account))
        #expect(firstStore.hasPendingRecovery)
        #expect(
            try disconnectJournalStore.load(
                now: Self.now.addingTimeInterval(365 * 24 * 60 * 60)
            ) == durable
        )

        let revoked = try Self.account(revision: 3, status: .revoked)
        let secondTransport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(activeSnapshot),
                .value(try Self.accountsSnapshot(accounts: [revoked])),
            ],
            collections: [.value([collection])],
            disconnects: [.value(revoked)],
            syncStatuses: [.value(initialStatus)]
        )
        let relaunched = Self.store(
            transport: secondTransport,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshJournalStore,
            pollLimit: 1
        )
        relaunched.activate(automaticallyReload: false)
        await relaunched.reload()
        #expect(relaunched.hasPendingDisconnectRecovery(for: account))

        await relaunched.disconnectGoogleAccount(account)

        let replay = try #require((await secondTransport.disconnectRecords()).first)
        #expect(replay == firstRequest)
        #expect(disconnectJournalStore.journal == nil)
        #expect(!relaunched.hasPendingDisconnectRecovery(for: account))
        #expect(!relaunched.hasPendingRecovery)
    }

    @Test("refresh recovery ignores an older run after relaunch and composes once")
    func refreshRecoveryRejectsOlderRunAfterRelaunch() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let oldRun = try Self.syncStatus(
            accountID: account.id,
            state: .idle,
            requestedAt: Self.now.addingTimeInterval(-120),
            startedAt: Self.now.addingTimeInterval(-110),
            completedAt: Self.now.addingTimeInterval(-100),
            revision: 1
        )
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        let acceptedRefresh = try Self.refreshAccepted(
            accountID: account.id,
            requestedAt: Self.now
        )
        let firstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot)],
            collections: [.value([collection])],
            syncStatuses: [.value(oldRun), .value(oldRun)],
            refreshes: [.value(acceptedRefresh)]
        )
        let firstCompletions = GoogleImportCompletionCounter()
        let firstStore = Self.store(
            transport: firstTransport,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshJournalStore,
            pollLimit: 1
        )
        firstStore.installImportCompletionVerifier {
            await firstCompletions.increment()
            return true
        }
        firstStore.activate(automaticallyReload: false)
        await firstStore.reload()

        await firstStore.refreshImports(for: account)

        #expect(await firstCompletions.value() == 0)
        #expect(firstStore.hasPendingRefreshCompletion(for: account))
        #expect(refreshJournalStore.journals.count == 1)
        #expect(
            refreshJournalStore.journals[0].serverRequestedAt
                == acceptedRefresh.requestedAt
        )
        #expect(
            try refreshJournalStore.load(
                now: Self.now.addingTimeInterval(365 * 24 * 60 * 60)
            ).count == 1
        )

        let matchingRun = try Self.syncStatus(
            accountID: account.id,
            state: .idle,
            requestedAt: acceptedRefresh.requestedAt,
            startedAt: acceptedRefresh.requestedAt.addingTimeInterval(1),
            completedAt: acceptedRefresh.requestedAt.addingTimeInterval(2),
            refreshGeneration: 1,
            claimedRefreshGeneration: 1,
            completedRefreshGeneration: 1,
            revision: 2
        )
        let secondTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot), .value(snapshot), .value(snapshot)],
            collections: [
                .value([collection]), .value([collection]), .value([collection]),
            ],
            syncStatuses: [.value(oldRun), .value(matchingRun), .value(matchingRun)]
        )
        let relaunchedCompletions = GoogleImportCompletionCounter()
        let relaunched = Self.store(
            transport: secondTransport,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshJournalStore,
            pollLimit: 1
        )
        relaunched.installImportCompletionVerifier {
            await relaunchedCompletions.increment()
            return true
        }
        relaunched.activate(automaticallyReload: false)

        await relaunched.reload()
        #expect(await relaunchedCompletions.value() == 0)
        #expect(relaunched.hasPendingRefreshCompletion(for: account))
        #expect(refreshJournalStore.journals.count == 1)

        await relaunched.reload()
        #expect(await relaunchedCompletions.value() == 1)
        #expect(refreshJournalStore.journals.isEmpty)
        #expect(!relaunched.hasPendingRefreshCompletion(for: account))

        await relaunched.reload()
        #expect(await relaunchedCompletions.value() == 1)
    }

    @Test("failed composition verification retains the marker for a later store")
    func refreshVerifierFailureSurvivesRelaunch() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let marker = try GooglePendingRefreshCompletionJournal(
            accountID: account.id,
            localRequestStartedAt: Self.now,
            serverRequestedAt: Self.now,
            targetRefreshGeneration: 1,
            configurationIdentifier: "google-store-test-configuration",
            createdAt: Self.now
        )
        let matchingRun = try Self.syncStatus(
            accountID: account.id,
            state: .idle,
            requestedAt: Self.now,
            startedAt: Self.now.addingTimeInterval(1),
            completedAt: Self.now.addingTimeInterval(2),
            refreshGeneration: 1,
            claimedRefreshGeneration: 1,
            completedRefreshGeneration: 1,
            revision: 2
        )
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        try refreshJournalStore.save(marker, now: Self.now)
        let rejectedAttempts = GoogleImportCompletionCounter()
        let firstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot)],
            collections: [.value([collection])],
            syncStatuses: [.value(matchingRun)]
        )
        let firstStore = Self.store(
            transport: firstTransport,
            refreshCompletionJournalStore: refreshJournalStore
        )
        firstStore.installImportCompletionVerifier {
            await rejectedAttempts.increment()
            return false
        }
        firstStore.activate(automaticallyReload: false)

        await firstStore.reload()

        #expect(await rejectedAttempts.value() == 1)
        #expect(refreshJournalStore.journals == [marker])
        #expect(refreshJournalStore.deleteCount == 0)
        #expect(firstStore.hasPendingRefreshCompletion(for: account))

        let acceptedAttempts = GoogleImportCompletionCounter()
        let secondTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot)],
            collections: [.value([collection])],
            syncStatuses: [.value(matchingRun)]
        )
        let relaunched = Self.store(
            transport: secondTransport,
            refreshCompletionJournalStore: refreshJournalStore
        )
        relaunched.installImportCompletionVerifier {
            await acceptedAttempts.increment()
            return true
        }
        relaunched.activate(automaticallyReload: false)

        await relaunched.reload()

        #expect(await acceptedAttempts.value() == 1)
        #expect(refreshJournalStore.journals.isEmpty)
        #expect(refreshJournalStore.deleteCount == 1)
        #expect(!relaunched.hasPendingRefreshCompletion(for: account))
    }

    @Test("privacy cancellation retains a refresh marker after suspended verification")
    func privacyCancellationRetainsRefreshMarker() async throws {
        let account = try Self.account()
        let marker = try GooglePendingRefreshCompletionJournal(
            accountID: account.id,
            localRequestStartedAt: Self.now,
            serverRequestedAt: Self.now,
            targetRefreshGeneration: 1,
            configurationIdentifier: "google-store-test-configuration",
            createdAt: Self.now
        )
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        try refreshJournalStore.save(marker, now: Self.now)
        let verifier = SuspendedGoogleValue<Bool>()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let store = Self.store(
            transport: transport,
            refreshCompletionJournalStore: refreshJournalStore
        )
        store.installImportCompletionVerifier { await verifier.value() }
        store.activate(automaticallyReload: false)

        let reload = Task { @MainActor in await store.reload() }
        await Self.waitUntil { await verifier.requestCount() == 1 }
        store.suspendForPrivacyBoundary()
        await verifier.resume(returning: true)
        await reload.value

        #expect(refreshJournalStore.journals == [marker])
        #expect(refreshJournalStore.deleteCount == 0)
        #expect(store.hasPendingRefreshCompletion(for: account))
        #expect(store.accounts.isEmpty)
        #expect(store.status == .privacyProtected)
    }

    @Test("configuration cancellation cannot clear a disconnect with a stale verifier")
    func configurationCancellationRetainsDisconnectMarker() async throws {
        let originalConfiguration = "google-store-test-configuration"
        let rotatedConfiguration = "google-store-rotated-configuration"
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let journal = try GoogleDisconnectRetryJournal(
            accountID: account.id,
            expectedRevision: account.revision,
            idempotencyKey: "mac-google-disconnect-configuration-race",
            configurationIdentifier: originalConfiguration,
            createdAt: Self.now
        )
        try disconnectJournalStore.save(journal, now: Self.now)
        let verifier = SuspendedGoogleValue<Bool>()
        let originalTransport = FakeGoogleIntegrationTransport(
            configurationIdentifier: originalConfiguration,
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let rotatedTransport = FakeGoogleIntegrationTransport(
            configurationIdentifier: rotatedConfiguration,
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let provider = RotatingGoogleTransportProvider(current: originalTransport)
        let store = GoogleIntegrationStore(
            transportProvider: { provider.current },
            journalStore: InMemoryGoogleOAuthStartJournalStore(),
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore:
                InMemoryGooglePendingRefreshCompletionJournalStore(),
            authorizationPollLimit: 1,
            now: { Self.now },
            sleep: { _ in }
        )
        store.installImportCompletionVerifier { await verifier.value() }
        store.activate(automaticallyReload: false)

        let staleReload = Task { @MainActor in await store.reload() }
        await Self.waitUntil { await verifier.requestCount() == 1 }
        provider.current = rotatedTransport
        store.configurationDidChange()
        await verifier.resume(returning: true)
        await staleReload.value
        await Self.waitUntil { await rotatedTransport.accountRequestCount() == 1 }
        await store.waitForCurrentOperation()

        let retained = try #require(disconnectJournalStore.journal)
        #expect(retained.accountID == journal.accountID)
        #expect(retained.expectedRevision == journal.expectedRevision)
        #expect(retained.idempotencyKey == journal.idempotencyKey)
        #expect(retained.configurationIdentifier == rotatedConfiguration)
        #expect(disconnectJournalStore.deleteCount == 0)
        #expect(store.accounts == [account])
    }

    @Test("proven disconnect waits for verified composition across relaunch")
    func provenDisconnectWaitsForCompositionAcrossRelaunch() async throws {
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let journal = try GoogleDisconnectRetryJournal(
            accountID: Self.accountID,
            expectedRevision: 1,
            idempotencyKey: "mac-google-disconnect-composition-proof",
            configurationIdentifier: "google-store-test-configuration",
            createdAt: Self.now
        )
        try disconnectJournalStore.save(journal, now: Self.now)
        let rejectedAttempts = GoogleImportCompletionCounter()
        let firstTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let firstStore = Self.store(
            transport: firstTransport,
            disconnectJournalStore: disconnectJournalStore
        )
        firstStore.installImportCompletionVerifier {
            await rejectedAttempts.increment()
            return false
        }
        firstStore.activate(automaticallyReload: false)

        await firstStore.reload()

        #expect(await rejectedAttempts.value() == 1)
        #expect(disconnectJournalStore.journal == journal)
        #expect(disconnectJournalStore.deleteCount == 0)
        #expect(firstStore.hasPendingRecovery)

        let acceptedAttempts = GoogleImportCompletionCounter()
        let relaunchedTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let relaunched = Self.store(
            transport: relaunchedTransport,
            disconnectJournalStore: disconnectJournalStore
        )
        relaunched.installImportCompletionVerifier {
            await acceptedAttempts.increment()
            return true
        }
        relaunched.activate(automaticallyReload: false)

        await relaunched.reload()

        #expect(await acceptedAttempts.value() == 1)
        #expect(disconnectJournalStore.journal == nil)
        #expect(disconnectJournalStore.deleteCount == 1)
        #expect(!relaunched.hasPendingRecovery)
    }

    @Test("orphan abandonment removes only mismatched absent-account recovery")
    func orphanAbandonmentIsConfigurationSelective() async throws {
        let currentConfiguration = "google-store-test-configuration"
        let previousConfiguration = "google-store-previous-configuration"
        let disconnectJournalStore = InMemoryGoogleDisconnectRetryJournalStore()
        let orphanedDisconnect = try GoogleDisconnectRetryJournal(
            accountID: Self.accountID,
            expectedRevision: 1,
            idempotencyKey: "mac-google-disconnect-orphaned-recovery",
            configurationIdentifier: previousConfiguration,
            createdAt: Self.now
        )
        try disconnectJournalStore.save(orphanedDisconnect, now: Self.now)
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        let orphanedRefresh = try GooglePendingRefreshCompletionJournal(
            accountID: Self.accountID,
            localRequestStartedAt: Self.now,
            serverRequestedAt: Self.now,
            targetRefreshGeneration: 1,
            configurationIdentifier: previousConfiguration,
            createdAt: Self.now
        )
        let retainedRefresh = try GooglePendingRefreshCompletionJournal(
            accountID: Self.retainedRecoveryAccountID,
            localRequestStartedAt: Self.now,
            serverRequestedAt: Self.now,
            targetRefreshGeneration: 1,
            configurationIdentifier: currentConfiguration,
            createdAt: Self.now
        )
        try refreshJournalStore.save(orphanedRefresh, now: Self.now)
        try refreshJournalStore.save(retainedRefresh, now: Self.now)
        let emptySnapshot = try Self.accountsSnapshot(accounts: [])
        let transport = FakeGoogleIntegrationTransport(
            configurationIdentifier: currentConfiguration,
            accounts: [.value(emptySnapshot), .value(emptySnapshot)]
        )
        let store = Self.store(
            transport: transport,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshJournalStore
        )
        store.installImportCompletionVerifier { false }
        store.activate(automaticallyReload: false)

        await store.reload()

        #expect(store.orphanedRecoveryRequiresConfirmation)
        #expect(disconnectJournalStore.journal == orphanedDisconnect)
        #expect(refreshJournalStore.journals == [orphanedRefresh, retainedRefresh])
        #expect(disconnectJournalStore.deleteCount == 0)
        #expect(refreshJournalStore.deleteCount == 0)

        await store.abandonOrphanedRecovery()

        #expect(await transport.accountRequestCount() == 2)
        #expect(!store.orphanedRecoveryRequiresConfirmation)
        #expect(disconnectJournalStore.journal == nil)
        #expect(disconnectJournalStore.deleteCount == 1)
        #expect(refreshJournalStore.journals == [retainedRefresh])
        #expect(refreshJournalStore.deleteCount == 1)
        #expect(store.hasPendingRecovery)
    }

    @Test("cleanup fences block connect and reauthorization before OAuth persistence")
    func cleanupFencesBlockAllAuthorizationStarts() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        for (revocationFenced, operatorRecoveryRequired) in [
            (true, false),
            (false, true),
        ] {
            let snapshot = try Self.accountsSnapshot(
                accounts: [account],
                revocationFenced: revocationFenced,
                operatorRecoveryRequired: operatorRecoveryRequired
            )
            let transport = FakeGoogleIntegrationTransport(
                accounts: [.value(snapshot)],
                collections: [.value([collection])],
                syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
            )
            let journalStore = InMemoryGoogleOAuthStartJournalStore()
            let store = Self.store(transport: transport, journalStore: journalStore)
            store.activate(automaticallyReload: false)
            await store.reload()

            #expect(store.authorizationStartIsFenced)
            await store.connectGoogleAccount()
            await store.reauthorizeGoogleAccount(account)

            #expect((await transport.oauthStartRecords()).isEmpty)
            #expect(journalStore.journal == nil)
            #expect(journalStore.saved.isEmpty)
        }
    }

    @Test("run-level authorization failure permits repair without dropping refresh recovery")
    func activeAccountRunReauthorizationRetainsRefreshMarker() async throws {
        let account = try Self.account(status: .active)
        let collection = try Self.collection(accountID: account.id)
        let marker = try GooglePendingRefreshCompletionJournal(
            accountID: account.id,
            localRequestStartedAt: Self.now,
            serverRequestedAt: Self.now.addingTimeInterval(-3_600),
            targetRefreshGeneration: 1,
            configurationIdentifier: "google-store-test-configuration",
            createdAt: Self.now
        )
        let refreshJournalStore = InMemoryGooglePendingRefreshCompletionJournalStore()
        try refreshJournalStore.save(marker, now: Self.now)
        let run = try Self.syncStatus(
            accountID: account.id,
            state: .reauthorizationRequired,
            requestedAt: Self.now.addingTimeInterval(-3_600),
            startedAt: Self.now.addingTimeInterval(-3_599),
            refreshGeneration: 1,
            claimedRefreshGeneration: 1,
            revision: 2
        )
        let oauthJournalStore = InMemoryGoogleOAuthStartJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [account]))],
            oauthStarts: [.value(try Self.authorization())],
            collections: [.value([collection])],
            syncStatuses: [.value(run)]
        )
        let store = Self.store(
            transport: transport,
            journalStore: oauthJournalStore,
            refreshCompletionJournalStore: refreshJournalStore
        )
        store.activate(automaticallyReload: false)
        await store.reload()

        #expect(store.requiresReauthorization(for: account))
        await store.reauthorizeGoogleAccount(account)

        let start = try #require((await transport.oauthStartRecords()).first)
        #expect(start.request.accountID == account.id)
        #expect(start.request.services.isEmpty)
        #expect(oauthJournalStore.journal != nil)
        #expect(refreshJournalStore.journals == [marker])
        #expect(refreshJournalStore.deleteCount == 0)
        #expect(store.hasPendingRefreshCompletion(for: account))
    }

    @Test("offline reload preserves trusted cache for the same binding")
    func offlineReloadPreservesSameBindingCache() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(try Self.accountsSnapshot(accounts: [account])),
                .failure(.transport(.notConnectedToInternet)),
            ],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()
        await store.reload()

        #expect(store.accounts == [account])
        #expect(store.collectionsByAccount[account.id] == [collection])
        #expect(store.syncStatusByAccount[account.id] != nil)
        #expect(store.sidebarMessage == "Google · offline")
        #expect(store.sidebarSymbol == "wifi.slash")
        if case .offline = store.status {
            // Trusted data remains visible but explicitly stale.
        } else {
            Issue.record("Offline reload did not expose the offline state")
        }
    }

    @Test("authentication failure quarantines all trusted Google presentation")
    func authenticationFailureQuarantinesTrustedCache() async throws {
        let account = try Self.account()
        let collection = try Self.collection(accountID: account.id)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(try Self.accountsSnapshot(accounts: [account])),
                .failure(.server(
                    statusCode: 401,
                    code: "unauthorized",
                    message: "redacted",
                    requestID: nil
                )),
            ],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()
        await store.reload()

        #expect(store.accounts.isEmpty)
        #expect(store.collectionsByAccount.isEmpty)
        #expect(store.syncStatusByAccount.isEmpty)
        #expect(store.cleanupStatus == nil)
        #expect(!store.canOpenAuthorization)
        #expect(!store.canRetryAuthorization)
        #expect(!store.canCheckAuthorization)
        if case .configurationRequired = store.status {
            // First-party authentication is required before private data returns.
        } else {
            Issue.record("Authentication failure did not quarantine integration state")
        }
    }

    @Test("server diagnostics never enter Google status text")
    func serverDiagnosticsStayOutOfStatus() async throws {
        let account = try Self.account()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [
                .value(try Self.accountsSnapshot(accounts: [account])),
                .failure(.server(
                    statusCode: 500,
                    code: "PROVIDER-CODE-CANARY",
                    message: "PROVIDER-MESSAGE-CANARY",
                    requestID: "REQUEST-ID-CANARY"
                )),
            ],
            collections: [.value([])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let store = Self.store(transport: transport)
        store.activate(automaticallyReload: false)
        await store.reload()
        await store.reload()

        #expect(store.status.isFailure)
        #expect(!store.status.message.contains("PROVIDER-CODE-CANARY"))
        #expect(!store.status.message.contains("PROVIDER-MESSAGE-CANARY"))
        #expect(!store.status.message.contains("REQUEST-ID-CANARY"))
    }

    @Test("OAuth start and polling authentication failures quarantine private cache")
    func oauthAuthenticationFailuresQuarantineCache() async throws {
        let account = try Self.account(label: "private-owner@example.com")
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let collection = try Self.collection(accountID: account.id)
        let unauthorized = DayWeaveAPIError.server(
            statusCode: 401,
            code: "unauthorized",
            message: "private provider detail",
            requestID: nil
        )

        let startFailureTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot)],
            oauthStarts: [.failure(unauthorized)],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let startFailureStore = Self.store(transport: startFailureTransport)
        startFailureStore.activate(automaticallyReload: false)
        await startFailureStore.reload()
        await startFailureStore.connectGoogleAccount()

        #expect(startFailureStore.accounts.isEmpty)
        #expect(startFailureStore.collectionsByAccount.isEmpty)
        #expect(startFailureStore.syncStatusByAccount.isEmpty)

        let pollingFailureTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot), .value(snapshot), .failure(unauthorized)],
            oauthStarts: [.value(try Self.authorization())],
            collections: [.value([collection])],
            syncStatuses: [.value(try Self.syncStatus(accountID: account.id))]
        )
        let pollingFailureStore = Self.store(
            transport: pollingFailureTransport,
            pollLimit: 1
        )
        pollingFailureStore.activate(automaticallyReload: false)
        await pollingFailureStore.reload()
        await pollingFailureStore.connectGoogleAccount()
        #expect(pollingFailureStore.openAuthorizationPage())
        await pollingFailureStore.waitForCurrentOperation()

        #expect(pollingFailureStore.accounts.isEmpty)
        #expect(pollingFailureStore.collectionsByAccount.isEmpty)
        #expect(pollingFailureStore.syncStatusByAccount.isEmpty)
    }

    @Test("revocation failure and disconnecting snapshots never look connected")
    func nonActiveAccountStatesHaveAccurateSeverity() async throws {
        let revocationFailed = try Self.account(status: .revocationFailed)
        let failedTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [revocationFailed]))]
        )
        let failedStore = Self.store(transport: failedTransport)
        failedStore.activate(automaticallyReload: false)
        await failedStore.reload()

        #expect(failedStore.status.isFailure)
        #expect(failedStore.sidebarMessage == "Google · needs attention")
        #expect(failedStore.sidebarSymbol == "exclamationmark.triangle")

        let disconnecting = try Self.account(status: .disconnecting)
        let disconnectingTransport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: [disconnecting]))]
        )
        let disconnectingStore = Self.store(transport: disconnectingTransport)
        disconnectingStore.activate(automaticallyReload: false)
        await disconnectingStore.reload()

        if case .loading = disconnectingStore.status {
            // Authoritative disconnect remains in progress.
        } else {
            Issue.record("A disconnecting-only snapshot was presented as connected")
        }
        #expect(disconnectingStore.sidebarMessage == "Google · disconnecting")
        #expect(disconnectingStore.sidebarSymbol == "clock")
    }

    @Test("corrupt authorization recovery stays fail closed until explicit reset")
    func corruptAuthorizationRecoveryRequiresReset() async throws {
        let account = try Self.account()
        let snapshot = try Self.accountsSnapshot(accounts: [account])
        let collection = try Self.collection(accountID: account.id)
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(snapshot), .value(snapshot)],
            collections: [.value([collection]), .value([collection])],
            syncStatuses: [
                .value(try Self.syncStatus(accountID: account.id)),
                .value(try Self.syncStatus(accountID: account.id)),
            ]
        )
        let journalStore = ThrowingGoogleOAuthStartJournalStore()
        let store = Self.store(transport: transport, journalStore: journalStore)

        store.activate(automaticallyReload: false)
        await store.reload()

        #expect(store.authorizationRecoveryRequiresAttention)
        #expect(store.authorizationRecoveryResetRequired)
        #expect(store.hasPendingAuthorizationRecovery)
        #expect(store.status.isFailure)
        #expect(!store.beginCredentialTransition())

        await store.resetUnreadableAuthorizationRecovery()

        #expect(!store.authorizationRecoveryRequiresAttention)
        #expect(!store.authorizationRecoveryResetRequired)
        #expect(!store.hasPendingAuthorizationRecovery)
        #expect(journalStore.deleteCount == 1)
    }

    @Test("unreadable completion recovery resets only after verified composition")
    func unreadableCompletionResetRequiresComposition() async throws {
        let refreshJournalStore = ThrowingGooglePendingRefreshCompletionJournalStore()
        let transport = FakeGoogleIntegrationTransport(
            accounts: [.value(try Self.accountsSnapshot(accounts: []))]
        )
        let store = Self.store(
            transport: transport,
            refreshCompletionJournalStore: refreshJournalStore
        )
        var compositionIsVerified = false
        store.installImportCompletionVerifier { compositionIsVerified }
        store.activate(automaticallyReload: false)

        #expect(store.refreshCompletionRecoveryResetRequired)
        await store.resetUnreadableRecovery()

        #expect(refreshJournalStore.deleteAllCount == 0)
        #expect(store.refreshCompletionRecoveryResetRequired)

        compositionIsVerified = true
        await store.resetUnreadableRecovery()

        #expect(refreshJournalStore.deleteAllCount == 1)
        #expect(!store.refreshCompletionRecoveryResetRequired)
        #expect(!store.hasPendingRecovery)
    }

    nonisolated private static let now = Date()
    nonisolated private static let accountID = UUID(
        uuidString: "11111111-aaaa-4aaa-8aaa-111111111111"
    )!
    nonisolated private static let collectionID = UUID(
        uuidString: "22222222-bbbb-4bbb-8bbb-222222222222"
    )!
    nonisolated private static let retainedRecoveryAccountID = UUID(
        uuidString: "33333333-cccc-4ccc-8ccc-333333333333"
    )!

    private static func store(
        transport: FakeGoogleIntegrationTransport,
        journalStore: any GoogleOAuthStartJournalStoring =
            InMemoryGoogleOAuthStartJournalStore(),
        disconnectJournalStore: any GoogleDisconnectRetryJournalStoring =
            InMemoryGoogleDisconnectRetryJournalStore(),
        refreshCompletionJournalStore:
            any GooglePendingRefreshCompletionJournalStoring =
            InMemoryGooglePendingRefreshCompletionJournalStore(),
        opener: @escaping GoogleIntegrationStore.AuthorizationOpener = { _ in true },
        pollLimit: Int = 3
    ) -> GoogleIntegrationStore {
        let store = GoogleIntegrationStore(
            transportProvider: { transport },
            journalStore: journalStore,
            disconnectJournalStore: disconnectJournalStore,
            refreshCompletionJournalStore: refreshCompletionJournalStore,
            authorizationOpener: opener,
            authorizationPollLimit: pollLimit,
            now: { now },
            sleep: { _ in }
        )
        store.installImportCompletionVerifier { true }
        return store
    }

    private static func authorization() throws -> GoogleOAuthAuthorization {
        return try decode(
            GoogleOAuthAuthorization.self,
            """
            {
              "authorization_url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=dayweave&state=opaque-state",
              "expires_at":"\(timestamp(now.addingTimeInterval(10 * 60)))"
            }
            """
        )
    }

    private static func account(
        label: String = "owner@example.com",
        revision: UInt64 = 1,
        status: GoogleAccountStatus = .active,
        calendarWrite: Bool = false
    ) throws -> GoogleAccount {
        let syncEnabled = status == .active ? "true" : "false"
        let isDefault = status == .revoked ? "false" : "true"
        let calendarScope = calendarWrite
            ? "https://www.googleapis.com/auth/calendar"
            : "https://www.googleapis.com/auth/calendar.readonly"
        let scopes = status == .revoked
            ? "[]"
            : "[\"openid\",\"\(calendarScope)\",\"https://www.googleapis.com/auth/tasks.readonly\"]"
        let tokenExpiresAt = status == .revoked
            ? "null"
            : "\"\(timestamp(now.addingTimeInterval(3_600)))\""
        return try decode(
            GoogleAccount.self,
            """
            {
              "id":"\(accountID.uuidString.lowercased())",
              "external_account_id":"google-subject-1",
              "display_label":"\(label)",
              "status":"\(status.rawValue)",
              "sync_enabled":\(syncEnabled),
              "is_default":\(isDefault),
              "granted_scopes":\(scopes),
              "token_expires_at":\(tokenExpiresAt),
              "revision":\(revision),
              "created_at":"\(timestamp(now.addingTimeInterval(-3_600)))",
              "updated_at":"\(timestamp(now.addingTimeInterval(-60)))"
            }
            """
        )
    }

    private static func accountsSnapshot(
        accounts: [GoogleAccount],
        revocationFenced: Bool = false,
        operatorRecoveryRequired: Bool = false
    ) throws -> GoogleAccountsSnapshot {
        let encodedAccounts = try accounts.map { account -> String in
            let data = try encoder().encode(account)
            return String(decoding: data, as: UTF8.self)
        }.joined(separator: ",")
        return try decode(
            GoogleAccountsSnapshot.self,
            """
            {
              "accounts":[\(encodedAccounts)],
              "cleanup":{
                "held":0,
                "pending":0,
                "retrying":0,
                "exhausted":0,
                "volatile_guardians":0,
                "durability_degraded":false,
                "revocation_fenced":\(revocationFenced),
                "operator_recovery_required":\(operatorRecoveryRequired),
                "uncertain_authorizations":0,
                "legacy_recovery_required":0,
                "next_attempt_at":null,
                "last_failure_at":null
              }
            }
            """
        )
    }

    private static func collection(
        accountID: UUID,
        kind: GoogleCollectionKind = .calendar,
        selected: Bool = false,
        visible: Bool = true,
        role: GoogleSyncRole = .readOnly,
        revision: UInt64 = 1,
        providerAccessRole: String? = "owner",
        publishAllDay: Bool = false,
        publishTentative: Bool = false,
        publishFree: Bool = false
    ) throws -> GoogleSyncCollection {
        let encodedProviderAccessRole = providerAccessRole.map { "\"\($0)\"" } ?? "null"
        return try decode(
            GoogleSyncCollection.self,
            """
            {
              "id":"\(collectionID.uuidString.lowercased())",
              "account_id":"\(accountID.uuidString.lowercased())",
              "kind":"\(kind.rawValue)",
              "remote_collection_id":"primary",
              "display_name":"Primary source",
              "provider_access_role":\(encodedProviderAccessRole),
              "provider_primary":true,
              "provider_selected":true,
              "provider_hidden":false,
              "provider_deleted":false,
              "selected":\(selected),
              "visible":\(visible),
              "sync_role":"\(role.rawValue)",
              "calendar_policy":{
                "confirmed_busy":"blocking",
                "tentative":"visible_nonblocking",
                "free":"visible_nonblocking",
                "all_day":"visible_nonblocking",
                "publish_all_day":\(publishAllDay),
                "publish_tentative":\(publishTentative),
                "publish_free":\(publishFree)
              },
              "revision":\(revision),
              "discovered_at":"\(timestamp(now.addingTimeInterval(-3_600)))",
              "configured_at":null,
              "last_import_at":null,
              "planning_projection_state":"uninitialized",
              "planning_generation":0,
              "planning_collection_revision":null,
              "planning_window_start":null,
              "planning_window_end":null,
              "planning_window_refreshed_at":null,
              "created_at":"\(timestamp(now.addingTimeInterval(-3_600)))",
              "updated_at":"\(timestamp(now.addingTimeInterval(-60)))"
            }
            """
        )
    }

    private static func syncStatus(
        accountID: UUID,
        state: GoogleSyncRunState = .idle,
        requestedAt: Date? = nil,
        startedAt: Date? = nil,
        completedAt: Date? = nil,
        importedCount: UInt64 = 1,
        updatedCount: UInt64 = 0,
        deletedCount: UInt64 = 0,
        refreshGeneration: UInt64 = 0,
        claimedRefreshGeneration: UInt64 = 0,
        completedRefreshGeneration: UInt64 = 0,
        revision: UInt64 = 1
    ) throws -> GoogleSyncStatus {
        let requested = requestedAt.map { "\"\(timestamp($0))\"" } ?? "null"
        let started = startedAt.map { "\"\(timestamp($0))\"" } ?? "null"
        let completed = completedAt.map { "\"\(timestamp($0))\"" } ?? "null"
        return try decode(
            GoogleSyncStatus.self,
            """
            {
              "run":{
                "account_id":"\(accountID.uuidString.lowercased())",
                "state":"\(state.rawValue)",
                "requested_at":\(requested),
                "started_at":\(started),
                "completed_at":\(completed),
                "next_attempt_at":"\(timestamp(now.addingTimeInterval(300)))",
                "consecutive_failures":0,
                "last_error_code":null,
                "last_error_at":null,
                "imported_count":\(importedCount),
                "updated_count":\(updatedCount),
                "deleted_count":\(deletedCount),
                "conflict_count":0,
                "rejected_count":0,
                "refresh_generation":\(refreshGeneration),
                "claimed_refresh_generation":\(claimedRefreshGeneration),
                "completed_refresh_generation":\(completedRefreshGeneration),
                "revision":\(revision)
              },
              "import_conflicts":0,
              "pending_outbound":0,
              "conflicted_outbound":0,
              "failed_outbound":0,
              "last_outbound_error_code":null,
              "last_outbound_error_at":null,
              "next_outbound_attempt_at":null
            }
            """
        )
    }

    private static func refreshAccepted(
        accountID: UUID,
        requestedAt: Date,
        refreshGeneration: UInt64 = 1
    ) throws -> GoogleSyncRefreshAccepted {
        try decode(
            GoogleSyncRefreshAccepted.self,
            """
            {
              "account_id":"\(accountID.uuidString.lowercased())",
              "request_id":"11111111-bbbb-4bbb-8bbb-111111111111",
              "refresh_generation":\(refreshGeneration),
              "requested_at":"\(timestamp(requestedAt))"
            }
            """
        )
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            let fractional = ISO8601DateFormatter()
            fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let date = fractional.date(from: value) { return date }
            let whole = ISO8601DateFormatter()
            whole.formatOptions = [.withInternetDateTime]
            guard let date = whole.date(from: value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Expected RFC 3339 timestamp"
                )
            }
            return date
        }
        return decoder
    }

    private static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(timestamp(date))
        }
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }

    private static func decode<Value: Decodable>(
        _ type: Value.Type,
        _ json: String
    ) throws -> Value {
        try decoder().decode(type, from: Data(json.utf8))
    }

    nonisolated private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private static func waitUntil(
        _ condition: @escaping @Sendable () async -> Bool
    ) async {
        for _ in 0..<10_000 {
            if await condition() { return }
            await Task.yield()
        }
        Issue.record("Timed out waiting for asynchronous Google test state")
    }
}

@MainActor
private final class InMemoryGoogleOAuthStartJournalStore: GoogleOAuthStartJournalStoring {
    private(set) var journal: GoogleOAuthStartJournal?
    private(set) var saved: [GoogleOAuthStartJournal] = []
    private(set) var deleteCount = 0

    func load() throws -> GoogleOAuthStartJournal? { journal }

    func save(_ journal: GoogleOAuthStartJournal) throws {
        self.journal = journal
        saved.append(journal)
    }

    func delete() throws {
        journal = nil
        deleteCount += 1
    }
}

@MainActor
private final class InMemoryGoogleDisconnectRetryJournalStore:
    GoogleDisconnectRetryJournalStoring
{
    private(set) var journal: GoogleDisconnectRetryJournal?
    private(set) var saved: [GoogleDisconnectRetryJournal] = []
    private(set) var deleteCount = 0

    func load(now: Date) throws -> GoogleDisconnectRetryJournal? {
        guard let journal else { return nil }
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        return journal
    }

    func save(_ journal: GoogleDisconnectRetryJournal, now: Date) throws {
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        self.journal = journal
        saved.append(journal)
    }

    func delete() throws {
        journal = nil
        deleteCount += 1
    }
}

@MainActor
private final class InMemoryGooglePendingRefreshCompletionJournalStore:
    GooglePendingRefreshCompletionJournalStoring
{
    private(set) var journals: [GooglePendingRefreshCompletionJournal] = []
    private(set) var saved: [GooglePendingRefreshCompletionJournal] = []
    private(set) var deleteCount = 0

    func load(now: Date) throws -> [GooglePendingRefreshCompletionJournal] {
        guard journals.allSatisfy({ $0.isValid(now: now) }) else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        return journals
    }

    func save(_ journal: GooglePendingRefreshCompletionJournal, now: Date) throws {
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        if let index = journals.firstIndex(where: { $0.accountID == journal.accountID }) {
            journals[index] = journal
        } else {
            journals.append(journal)
        }
        saved.append(journal)
    }

    func delete(accountID: UUID, configurationIdentifier: String) throws {
        journals.removeAll {
            $0.accountID == accountID
                && $0.configurationIdentifier == configurationIdentifier
        }
        deleteCount += 1
    }

    func deleteAll() throws {
        journals = []
        deleteCount += 1
    }
}

@MainActor
private final class ThrowingGoogleOAuthStartJournalStore: GoogleOAuthStartJournalStoring {
    private(set) var deleteCount = 0
    private var isCorrupt = true

    func load() throws -> GoogleOAuthStartJournal? {
        if isCorrupt { throw GoogleOAuthStartJournalStoreError.invalidStoredJournal }
        return nil
    }

    func save(_ journal: GoogleOAuthStartJournal) throws {}

    func delete() throws {
        isCorrupt = false
        deleteCount += 1
    }
}

@MainActor
private final class ThrowingGooglePendingRefreshCompletionJournalStore:
    GooglePendingRefreshCompletionJournalStoring
{
    private var isCorrupt = true
    private(set) var deleteAllCount = 0

    func load(now: Date) throws -> [GooglePendingRefreshCompletionJournal] {
        if isCorrupt { throw GoogleIntegrationJournalStoreError.invalidStoredJournal }
        return []
    }

    func save(_ journal: GooglePendingRefreshCompletionJournal, now: Date) throws {
        throw GoogleIntegrationJournalStoreError.invalidStoredJournal
    }

    func delete(accountID: UUID, configurationIdentifier: String) throws {
        throw GoogleIntegrationJournalStoreError.invalidStoredJournal
    }

    func deleteAll() throws {
        isCorrupt = false
        deleteAllCount += 1
    }
}

private enum FakeGoogleReply<Value: Sendable>: Sendable {
    case value(Value)
    case failure(DayWeaveAPIError)
    case suspended(SuspendedGoogleValue<Value>)
    case suspendedResult(SuspendedGoogleResult<Value>)
}

private actor SuspendedGoogleValue<Value: Sendable> {
    private var continuation: CheckedContinuation<Value, Never>?
    private var buffered: Value?
    private var requests = 0

    func value() async -> Value {
        requests += 1
        if let buffered {
            self.buffered = nil
            return buffered
        }
        return await withCheckedContinuation { continuation = $0 }
    }

    func resume(returning value: Value) {
        if let continuation {
            self.continuation = nil
            continuation.resume(returning: value)
        } else {
            buffered = value
        }
    }

    func requestCount() -> Int { requests }
}

private actor SuspendedGoogleResult<Value: Sendable> {
    private var continuation: CheckedContinuation<Result<Value, DayWeaveAPIError>, Never>?
    private var buffered: Result<Value, DayWeaveAPIError>?
    private var requests = 0

    func value() async throws -> Value {
        requests += 1
        let result: Result<Value, DayWeaveAPIError>
        if let buffered {
            self.buffered = nil
            result = buffered
        } else {
            result = await withCheckedContinuation { continuation = $0 }
        }
        return try result.get()
    }

    func resume(returning result: Result<Value, DayWeaveAPIError>) {
        if let continuation {
            self.continuation = nil
            continuation.resume(returning: result)
        } else {
            buffered = result
        }
    }

    func requestCount() -> Int { requests }
}

private actor FakeGoogleIntegrationTransport: GoogleIntegrationTransport {
    struct OAuthStartRecord: Equatable, Sendable {
        let request: GoogleOAuthStartRequest
        let idempotencyKey: String
    }

    struct ConfigureRecord: Equatable, Sendable {
        let accountID: UUID
        let collectionID: UUID
        let expectedRevision: UInt64
        let selected: Bool
        let visible: Bool
        let role: GoogleSyncRole
        let calendarPolicy: GoogleCalendarPolicy
    }

    struct DisconnectRecord: Equatable, Sendable {
        let accountID: UUID
        let expectedRevision: UInt64
        let idempotencyKey: String
    }

    nonisolated let configurationIdentifier: String

    private var accountReplies: [FakeGoogleReply<GoogleAccountsSnapshot>]
    private var oauthStartReplies: [FakeGoogleReply<GoogleOAuthAuthorization>]
    private var collectionReplies: [FakeGoogleReply<[GoogleSyncCollection]>]
    private var discoveryReplies: [FakeGoogleReply<[GoogleSyncCollection]>]
    private var configurationReplies: [FakeGoogleReply<GoogleSyncCollection>]
    private var disconnectReplies: [FakeGoogleReply<GoogleAccount>]
    private var syncStatusReplies: [FakeGoogleReply<GoogleSyncStatus>]
    private var refreshReplies: [FakeGoogleReply<GoogleSyncRefreshAccepted>]
    private var oauthRecords: [OAuthStartRecord] = []
    private var configurations: [ConfigureRecord] = []
    private var disconnects: [DisconnectRecord] = []
    private var accountRequests = 0
    private var collectionRequests = 0
    private var discoveryRequests = 0
    private var syncStatusRequests = 0
    private var refreshRequests = 0
    private var refreshRequestIDs: [UUID] = []

    init(
        configurationIdentifier: String = "google-store-test-configuration",
        accounts: [FakeGoogleReply<GoogleAccountsSnapshot>] = [],
        oauthStarts: [FakeGoogleReply<GoogleOAuthAuthorization>] = [],
        collections: [FakeGoogleReply<[GoogleSyncCollection]>] = [],
        discoveries: [FakeGoogleReply<[GoogleSyncCollection]>] = [],
        configurations: [FakeGoogleReply<GoogleSyncCollection>] = [],
        disconnects: [FakeGoogleReply<GoogleAccount>] = [],
        syncStatuses: [FakeGoogleReply<GoogleSyncStatus>] = [],
        refreshes: [FakeGoogleReply<GoogleSyncRefreshAccepted>] = []
    ) {
        self.configurationIdentifier = configurationIdentifier
        accountReplies = accounts
        oauthStartReplies = oauthStarts
        collectionReplies = collections
        discoveryReplies = discoveries
        configurationReplies = configurations
        disconnectReplies = disconnects
        syncStatusReplies = syncStatuses
        refreshReplies = refreshes
    }

    func googleAccounts() async throws -> GoogleAccountsSnapshot {
        accountRequests += 1
        return try await resolve(take(&accountReplies))
    }

    func startGoogleOAuth(
        _ request: GoogleOAuthStartRequest,
        idempotencyKey: String
    ) async throws -> GoogleOAuthAuthorization {
        oauthRecords.append(.init(request: request, idempotencyKey: idempotencyKey))
        return try await resolve(take(&oauthStartReplies))
    }

    func pauseGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        throw DayWeaveAPIError.responseDecodingFailed
    }

    func resumeGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        throw DayWeaveAPIError.responseDecodingFailed
    }

    func disconnectGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        disconnects.append(.init(
            accountID: id,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        ))
        return try await resolve(take(&disconnectReplies))
    }

    func googleCollections(accountID: UUID) async throws -> [GoogleSyncCollection] {
        collectionRequests += 1
        return try await resolve(take(&collectionReplies))
    }

    func discoverGoogleCollections(accountID: UUID) async throws -> [GoogleSyncCollection] {
        discoveryRequests += 1
        return try await resolve(take(&discoveryReplies))
    }

    func configureGoogleCollection(
        accountID: UUID,
        collectionID: UUID,
        expectedRevision: UInt64,
        selected: Bool,
        visible: Bool,
        role: GoogleSyncRole,
        calendarPolicy: GoogleCalendarPolicy
    ) async throws -> GoogleSyncCollection {
        configurations.append(.init(
            accountID: accountID,
            collectionID: collectionID,
            expectedRevision: expectedRevision,
            selected: selected,
            visible: visible,
            role: role,
            calendarPolicy: calendarPolicy
        ))
        return try await resolve(take(&configurationReplies))
    }

    func googleSyncStatus(accountID: UUID) async throws -> GoogleSyncStatus {
        syncStatusRequests += 1
        return try await resolve(take(&syncStatusReplies))
    }

    func requestGoogleSyncRefresh(
        accountID: UUID,
        requestID: UUID
    ) async throws -> GoogleSyncRefreshAccepted {
        refreshRequests += 1
        refreshRequestIDs.append(requestID)
        let accepted = try await resolve(take(&refreshReplies))
        return GoogleSyncRefreshAccepted(
            accountID: accepted.accountID,
            requestID: requestID,
            refreshGeneration: accepted.refreshGeneration,
            requestedAt: accepted.requestedAt
        )
    }

    func oauthStartRecords() -> [OAuthStartRecord] { oauthRecords }
    func configureRecords() -> [ConfigureRecord] { configurations }
    func disconnectRecords() -> [DisconnectRecord] { disconnects }
    func accountRequestCount() -> Int { accountRequests }
    func collectionRequestCount() -> Int { collectionRequests }
    func discoveryRequestCount() -> Int { discoveryRequests }
    func syncStatusRequestCount() -> Int { syncStatusRequests }
    func refreshRequestCount() -> Int { refreshRequests }
    func recordedRefreshRequestIDs() -> [UUID] { refreshRequestIDs }

    private func take<Value: Sendable>(
        _ replies: inout [FakeGoogleReply<Value>]
    ) -> FakeGoogleReply<Value> {
        guard !replies.isEmpty else { return .failure(.responseDecodingFailed) }
        return replies.removeFirst()
    }

    private func resolve<Value: Sendable>(
        _ reply: FakeGoogleReply<Value>
    ) async throws -> Value {
        switch reply {
        case let .value(value): value
        case let .failure(error): throw error
        case let .suspended(gate): await gate.value()
        case let .suspendedResult(gate): try await gate.value()
        }
    }
}

@MainActor
private final class RotatingGoogleTransportProvider {
    var current: any GoogleIntegrationTransport

    init(current: any GoogleIntegrationTransport) {
        self.current = current
    }
}

@MainActor
private final class GoogleAuthorizationOpenerRecorder {
    private(set) var urls: [URL] = []

    func open(_ url: URL) -> Bool {
        urls.append(url)
        return true
    }
}

private actor GoogleImportCompletionCounter {
    private var count = 0

    func increment() { count += 1 }
    func value() -> Int { count }
}
#endif
