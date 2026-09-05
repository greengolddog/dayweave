import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("macOS account recovery", .serialized)
struct AccountRecoveryCoordinatorTests {
    private let baseURL = try! DayWeaveAPIBaseURL("https://api.example.com/gateway")
    private let now = Date(timeIntervalSince1970: 1_788_000_000)
    private let descriptor = DurableAuthClientDescriptor(
        deviceLabel: "Recovery Test Mac",
        clientVersion: "9.0-test"
    )

    @Test("issue is journaled before send and exact replay reveals only after success")
    func issueLostResponseAndReplay() async throws {
        let proposedID = uuid("11111111-1111-4111-8111-111111111111")
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .response(currentResponse(nil)),
                .failure(.transport(.timedOut)),
                .response(issueResponse(id: proposedID, replayed: true, status: 200)),
            ]
        )
        let generator = RecoveryGenerator(markers: [31], uuids: [proposedID])
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: generator
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)

        do {
            _ = try await coordinator.issueAccountRecoveryCode(
                replacing: snapshot,
                boundTo: baseURL
            )
            Issue.record("Expected synthetic lost response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        let pendingEnvelope = try #require(try journal.loadEnvelope())
        guard case let .issuePending(pending) = pendingEnvelope.state else {
            Issue.record("Expected issue journal")
            return
        }
        let firstRecords = await transport.records()
        #expect(firstRecords.count == 2)
        #expect(firstRecords[1].journalAtSend == pendingEnvelope)
        #expect(firstRecords[1].body == pending.request.body)
        #expect(try await coordinator.recoveryCodeAwaitingAcknowledgement(boundTo: baseURL) == nil)

        let relaunched = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: generator
        )
        try await relaunched.resumeAccountRecoveryWork(boundTo: baseURL)
        let records = await transport.records()
        #expect(records[1].body == records[2].body)
        #expect(records[1].authorization == records[2].authorization)
        #expect(records[1].path == records[2].path)
        #expect(
            try await relaunched.recoveryCodeAwaitingAcknowledgement(boundTo: baseURL)
                == pending.recoveryCode
        )
        #expect(!String(describing: pending).contains(pending.recoveryCode))
    }

    @Test("ambiguous issue can only be abandoned by explicit pending-only confirmation")
    func issuePendingExplicitDiscard() async throws {
        let proposedID = uuid("19191919-1919-4191-8191-191919191919")
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: RecoveryTransport(
                recoveryStore: journal,
                plans: [
                    .response(currentResponse(nil)),
                    .failure(.transport(.timedOut)),
                ]
            ),
            generator: RecoveryGenerator(markers: [34], uuids: [proposedID])
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        do {
            _ = try await coordinator.issueAccountRecoveryCode(
                replacing: snapshot,
                boundTo: baseURL
            )
        } catch {}
        guard case .issuePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Expected retained issue request")
            return
        }
        let model = await MainActor.run {
            DurableAuthSettingsModel(
                coordinator: coordinator,
                configurationStore: RecoveryConfigurationStore(
                    baseURL: baseURL.canonicalConfigurationIdentifier
                ),
                descriptor: descriptor
            )
        }
        #expect(await model.discardPendingAccountRecoveryIssue())
        #expect(try journal.loadEnvelope() == nil)
    }

    @Test("issuance authorization rejection retains the generated code journal")
    func issue401RetainsJournal() async throws {
        let proposedID = uuid("39393939-3939-4393-8393-393939393939")
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .response(currentResponse(nil)),
                .response(unauthorizedResponse()),
                .response(unauthorizedResponse()),
            ]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [104, 105, 106],
                uuids: [proposedID]
            )
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        do {
            _ = try await coordinator.issueAccountRecoveryCode(
                replacing: snapshot,
                boundTo: baseURL
            )
            Issue.record("Expected authorization rejection")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case let .issuePending(pending) = try #require(
            try journal.loadEnvelope()
        ).state else {
            Issue.record("Issuance 401 must retain the generated secret and exact request")
            return
        }
        #expect(pending.proposedID == proposedID)
        #expect(await transport.records().count == 3)
    }

    @Test("issue permits one trusted refresh and journals the rebased stable fence before retry")
    func issueTrusted401Refresh() async throws {
        let proposedID = uuid("29292929-2929-4292-8292-292929292929")
        let initial = activeEnvelope()
        guard case let .active(previous) = initial.state else { return }
        let refreshedSession = DurableDeviceSessionMetadata(
            id: previous.session.id,
            clientInstanceID: previous.session.clientInstanceID,
            clientKind: previous.session.clientKind,
            deviceLabel: previous.session.deviceLabel,
            scopes: previous.session.scopes,
            clientContractVersion: previous.session.clientContractVersion,
            clientVersion: previous.session.clientVersion,
            clientCapabilities: previous.session.clientCapabilities,
            createdAt: previous.session.createdAt,
            lastSeenAt: now,
            credentialIssuedAt: now,
            accessExpiresAt: now.addingTimeInterval(DurableAuthCoordinator.accessLifetime),
            refreshIdleExpiresAt: now.addingTimeInterval(
                DurableAuthCoordinator.refreshIdleLifetime
            ),
            absoluteExpiresAt: previous.session.absoluteExpiresAt,
            revision: 2
        )
        let auth = RecoveryAuthStore(initial: initial)
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .response(currentResponse(nil)),
                .response(unauthorizedResponse()),
                .response(sessionMutationResponse(
                    refreshedSession,
                    replayed: false,
                    status: 200
                )),
                .response(issueResponse(id: proposedID, replayed: false, status: 201)),
            ]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [101, 102, 103],
                uuids: [proposedID]
            )
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        _ = try await coordinator.issueAccountRecoveryCode(
            replacing: snapshot,
            boundTo: baseURL
        )
        let records = await transport.records()
        #expect(records.count == 4)
        #expect(records[1].body == records[3].body)
        #expect(records[3].authorization == "Bearer \(credential(prefix: "dw_da1_", marker: 102))")
        guard case let .issuePending(rebased) = records[3].journalAtSend?.state else {
            Issue.record("Retry must observe durable rebased issue fence")
            return
        }
        #expect(rebased.authorizationFence.envelopeRevision == 2)
        #expect(
            rebased.authorizationFence.authorizationBindingIdentifier
                == snapshot.fence.authorizationBindingIdentifier
        )
    }

    @Test("definitive issue retry rejection retires only the exact rebased session")
    func issueRetry401RetiresExactSession() async throws {
        let proposedID = uuid("49494949-4949-4494-8494-494949494949")
        let initial = activeEnvelope()
        guard case let .active(previous) = initial.state else { return }
        let refreshedSession = DurableDeviceSessionMetadata(
            id: previous.session.id,
            clientInstanceID: previous.session.clientInstanceID,
            clientKind: previous.session.clientKind,
            deviceLabel: previous.session.deviceLabel,
            scopes: previous.session.scopes,
            clientContractVersion: previous.session.clientContractVersion,
            clientVersion: previous.session.clientVersion,
            clientCapabilities: previous.session.clientCapabilities,
            createdAt: previous.session.createdAt,
            lastSeenAt: now,
            credentialIssuedAt: now,
            accessExpiresAt: now.addingTimeInterval(DurableAuthCoordinator.accessLifetime),
            refreshIdleExpiresAt: now.addingTimeInterval(
                DurableAuthCoordinator.refreshIdleLifetime
            ),
            absoluteExpiresAt: previous.session.absoluteExpiresAt,
            revision: 2
        )
        let auth = RecoveryAuthStore(initial: initial)
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .response(currentResponse(nil)),
                .response(unauthorizedResponse()),
                .response(sessionMutationResponse(
                    refreshedSession,
                    replayed: false,
                    status: 200
                )),
                .response(unauthorizedResponse()),
            ]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [121, 122, 123],
                uuids: [proposedID]
            )
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        do {
            _ = try await coordinator.issueAccountRecoveryCode(
                replacing: snapshot,
                boundTo: baseURL
            )
            Issue.record("Expected definitive retry rejection")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case let .reauthenticationRequired(value) = try #require(
            try auth.loadEnvelope()
        ).state else {
            Issue.record("Rejected rebased session must be retired")
            return
        }
        #expect(value.previousSessionID == refreshedSession.id)
        guard case .issuePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Generated recovery authority must remain journaled")
            return
        }
        #expect(await transport.records().count == 4)
    }

    @Test("local expiry retires an exact session while its issue journal remains")
    func issuePendingSessionExpiryRetiresAuth() async throws {
        let proposedID = uuid("59595959-5959-4595-8595-595959595959")
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: RecoveryTransport(
                recoveryStore: journal,
                plans: [
                    .response(currentResponse(nil)),
                    .failure(.transport(.timedOut)),
                ]
            ),
            generator: RecoveryGenerator(markers: [124], uuids: [proposedID])
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        do {
            _ = try await coordinator.issueAccountRecoveryCode(
                replacing: snapshot,
                boundTo: baseURL
            )
        } catch {}
        auth.force(activeEnvelope(
            issuedAt: now.addingTimeInterval(-DurableAuthCoordinator.absoluteLifetime)
        ))

        do {
            _ = try await coordinator.authorization(boundTo: baseURL)
            Issue.record("Expected locally expired authorization")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case .reauthenticationRequired = try #require(
            try auth.loadEnvelope()
        ).state else {
            Issue.record("Expired exact issue session must be retired")
            return
        }
        guard case .issuePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Expiry must not destroy the generated issue tuple")
            return
        }
    }

    @Test("rotation request carries the exact reviewed predecessor CAS")
    func rotationUsesExactSnapshot() async throws {
        let current = DurableAccountRecoveryCodeMetadata(
            id: uuid("22222222-2222-4222-8222-222222222222"),
            createdAt: now.addingTimeInterval(-86_400),
            revision: 1
        )
        let nextID = uuid("33333333-3333-4333-8333-333333333333")
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .response(currentResponse(current)),
                .response(issueResponse(id: nextID, replayed: false, status: 201)),
            ]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(markers: [32], uuids: [nextID])
        )
        let snapshot = try await coordinator.listCurrentAccountRecoveryCode(boundTo: baseURL)
        _ = try await coordinator.issueAccountRecoveryCode(
            replacing: snapshot,
            boundTo: baseURL
        )
        let request = try #require(await transport.records().last?.body)
        let object = try #require(JSONSerialization.jsonObject(with: request) as? [String: Any])
        #expect(object["replaces_recovery_code_id"] as? String == current.id.uuidString.lowercased())
        #expect((object["replaces_recovery_code_revision"] as? NSNumber)?.uint64Value == 1)
        #expect(object["id"] as? String == nextID.uuidString.lowercased())
    }

    @Test("consume commits journal before auth install and quarantines binding until handoff")
    func consumeInstallAndHandoff() async throws {
        let ids = recoveryIDs(offset: 0)
        let auth = RecoveryAuthStore(initial: nil)
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [.response(consumeResponse(ids: ids, replayed: false, status: 201))]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [41, 42, 43],
                uuids: [ids.session, ids.client, ids.successor]
            )
        )

        let session = try await coordinator.consumeAccountRecoveryCode(
            credential(prefix: "dw_rc1_", marker: 40),
            boundTo: baseURL,
            descriptor: descriptor
        )
        #expect(session.id == ids.session)
        let installed = try #require(try auth.loadEnvelope())
        guard case let .active(active) = installed.state else {
            Issue.record("Expected installed recovered session")
            return
        }
        #expect(active.session.id == ids.session)
        let installedJournal = try #require(try journal.loadEnvelope())
        guard case .consumeInstalledAwaitingHandoff = installedJournal.state else {
            Issue.record("Expected handoff quarantine")
            return
        }
        do {
            _ = try await coordinator.authorization(boundTo: baseURL)
            Issue.record("Recovered auth must remain quarantined")
        } catch {
            #expect(error as? DurableAuthError == .accountRecoveryPending)
        }

        try await coordinator.completeAccountRecoveryCredentialHandoff(boundTo: baseURL)
        let successor = try await coordinator.recoveryCodeAwaitingAcknowledgement(
            boundTo: baseURL
        )
        #expect(successor == credential(prefix: "dw_rc1_", marker: 43))
        let authorization = try await coordinator.authorization(boundTo: baseURL)
        #expect(authorization.bearerToken == credential(prefix: "dw_da1_", marker: 41))
    }

    @Test("consume lost response retries byte-identically after restart")
    func consumeLostResponseExactReplay() async throws {
        let ids = recoveryIDs(offset: 10)
        let auth = RecoveryAuthStore(initial: nil)
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .failure(.transport(.networkConnectionLost)),
                .response(consumeResponse(ids: ids, replayed: true, status: 200)),
            ]
        )
        let generator = RecoveryGenerator(
            markers: [51, 52, 53],
            uuids: [ids.session, ids.client, ids.successor]
        )
        let code = credential(prefix: "dw_rc1_", marker: 50)
        let first = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: generator
        )
        do {
            _ = try await first.consumeAccountRecoveryCode(
                code,
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("Expected lost response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.networkConnectionLost))
        }
        guard case .consumePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Expected retained consume tuple")
            return
        }
        let second = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: generator
        )
        try await second.resumeAccountRecoveryWork(boundTo: baseURL)
        let records = await transport.records()
        #expect(records.count == 2)
        #expect(records[0].body == records[1].body)
        #expect(records[0].authorization == records[1].authorization)
        #expect(records[0].path == records[1].path)
    }

    @Test("definitive consume 401 retains exact tuple until owner abandons it")
    func consume401RetainsJournal() async throws {
        let ids = recoveryIDs(offset: 20)
        let auth = RecoveryAuthStore(initial: nil)
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [.response(unauthorizedResponse())]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [61, 62, 63],
                uuids: [ids.session, ids.client, ids.successor]
            )
        )
        do {
            _ = try await coordinator.consumeAccountRecoveryCode(
                credential(prefix: "dw_rc1_", marker: 60),
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("Expected rejection")
        } catch {
            #expect(error as? DurableAuthError == .invalidAccountRecoveryCode)
        }
        guard case .consumePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("401 must retain exact tuple")
            return
        }
        try await coordinator.confirmDiscardPendingAccountRecoveryConsumption()
        #expect(try journal.loadEnvelope() == nil)
    }

    @Test("malformed success remains replayable and never changes auth")
    func malformedConsumeResponseRetainsJournal() async throws {
        let ids = recoveryIDs(offset: 30)
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let original = try #require(try auth.loadEnvelope())
        let journal = RecoveryJournalStore()
        let malformed = RecoveryHTTPResponse(
            statusCode: 201,
            headers: successHeaders,
            body: Data("{\"session\":{},\"successor_recovery_code\":{},\"replayed\":false}".utf8)
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: RecoveryTransport(recoveryStore: journal, plans: [.response(malformed)]),
            generator: RecoveryGenerator(
                markers: [71, 72, 73],
                uuids: [ids.session, ids.client, ids.successor]
            )
        )
        do {
            _ = try await coordinator.consumeAccountRecoveryCode(
                credential(prefix: "dw_rc1_", marker: 70),
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("Expected strict response rejection")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        #expect(try auth.loadEnvelope() == original)
        guard case .consumePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Ambiguous malformed response must retain tuple")
            return
        }
    }

    @Test("privacy boundary rejects a late consume success and keeps auth quarantined")
    func lateConsumeSuccessAfterPrivacyBoundary() async throws {
        let ids = recoveryIDs(offset: 70)
        let auth = RecoveryAuthStore(initial: nil)
        let journal = RecoveryJournalStore()
        let responseGate = RecoverySuspension()
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: RecoveryTransport(
                recoveryStore: journal,
                plans: [.suspendedResponse(
                    consumeResponse(ids: ids, replayed: false, status: 201),
                    responseGate
                )]
            ),
            generator: RecoveryGenerator(
                markers: [131, 132, 133],
                uuids: [ids.session, ids.client, ids.successor]
            )
        )
        let model = await MainActor.run {
            DurableAuthSettingsModel(
                coordinator: coordinator,
                configurationStore: RecoveryConfigurationStore(
                    baseURL: baseURL.canonicalConfigurationIdentifier
                ),
                descriptor: descriptor
            )
        }
        let operation = Task { @MainActor in
            await model.consumeAccountRecoveryCode(
                baseURL: baseURL,
                code: credential(prefix: "dw_rc1_", marker: 130)
            )
        }
        await responseGate.waitUntilStarted()
        await model.suspendForPrivacyBoundary()
        await responseGate.release()
        #expect(await operation.value == false)
        #expect(await model.revealedAccountRecoveryCode == nil)
        guard case .consumeInstalledAwaitingHandoff = try #require(
            try journal.loadEnvelope()
        ).state else {
            Issue.record("Late success must remain quarantined for explicit resume")
            return
        }
        #expect(coordinator.hasUsableCredential(boundTo: baseURL) == false)
    }

    @Test("plaintext request DTO diagnostics are fully redacted")
    func requestDTOsAreRedacted() {
        let issueSecret = credential(prefix: "dw_rc1_", marker: 140)
        let consumeSecrets = [
            credential(prefix: "dw_da1_", marker: 141),
            credential(prefix: "dw_dr1_", marker: 142),
            credential(prefix: "dw_rc1_", marker: 143),
        ]
        let issue = AccountRecoveryIssueRequest(
            id: uuid("69696969-6969-4696-8696-696969696969"),
            recoveryCode: issueSecret,
            replacesRecoveryCodeID: nil,
            replacesRecoveryCodeRevision: nil
        )
        let consume = AccountRecoveryConsumeRequest(
            sessionID: uuid("79797979-7979-4797-8797-797979797979"),
            accessToken: consumeSecrets[0],
            refreshToken: consumeSecrets[1],
            clientInstanceID: uuid("89898989-8989-4898-8898-898989898989"),
            clientKind: "macos",
            deviceLabel: descriptor.deviceLabel,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: descriptor.clientVersion,
            clientCapabilities: descriptor.clientCapabilities,
            successorRecoveryCodeID: uuid("99999999-9999-4999-8999-999999999999"),
            successorRecoveryCode: consumeSecrets[2]
        )
        for rendered in [
            String(describing: issue), String(reflecting: issue),
            String(describing: consume), String(reflecting: consume),
        ] {
            #expect(rendered == "<redacted durable authentication state>")
            #expect(!rendered.contains(issueSecret))
            #expect(consumeSecrets.allSatisfy { !rendered.contains($0) })
        }
        let mirrors = [Mirror(reflecting: issue), Mirror(reflecting: consume)]
        for mirror in mirrors {
            let values = mirror.children.map { String(describing: $0.value) }
            #expect(values == ["<redacted durable authentication state>"])
        }
    }

    @Test("committed response survives failed auth CAS and installs on resume")
    func committedBeforeAuthInstall() async throws {
        let ids = recoveryIDs(offset: 40)
        let auth = RecoveryAuthStore(initial: nil, failNextCAS: true)
        let journal = RecoveryJournalStore()
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: RecoveryTransport(
                recoveryStore: journal,
                plans: [.response(consumeResponse(ids: ids, replayed: false, status: 201))]
            ),
            generator: RecoveryGenerator(
                markers: [81, 82, 83],
                uuids: [ids.session, ids.client, ids.successor]
            )
        )
        do {
            _ = try await coordinator.consumeAccountRecoveryCode(
                credential(prefix: "dw_rc1_", marker: 80),
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("Expected injected CAS race")
        } catch {
            #expect(error as? DurableAuthError == .concurrentStateChange)
        }
        guard case .consumeCommittedAwaitingInstallation = try #require(
            try journal.loadEnvelope()
        ).state else {
            Issue.record("Validated response must be durable before auth CAS")
            return
        }
        #expect(try auth.loadEnvelope() == nil)
        try await coordinator.resumeAccountRecoveryWork(boundTo: baseURL)
        guard case .consumeInstalledAwaitingHandoff = try #require(
            try journal.loadEnvelope()
        ).state else {
            Issue.record("Expected installed handoff quarantine")
            return
        }
        #expect(try auth.loadEnvelope() != nil)
    }

    @Test("binding replacement prevents replay dispatch")
    func authReplacementBeforeResumeFailsClosed() async throws {
        let ids = recoveryIDs(offset: 50)
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let journal = RecoveryJournalStore()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [.failure(.transport(.timedOut)), .response(
                consumeResponse(ids: ids, replayed: true, status: 200)
            )]
        )
        let generator = RecoveryGenerator(
            markers: [91, 92, 93],
            uuids: [ids.session, ids.client, ids.successor]
        )
        let coordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: generator
        )
        do {
            _ = try await coordinator.consumeAccountRecoveryCode(
                credential(prefix: "dw_rc1_", marker: 90),
                boundTo: baseURL,
                descriptor: descriptor
            )
        } catch {}
        auth.force(activeEnvelope(sessionMarker: 9, revision: 9))
        do {
            try await coordinator.resumeAccountRecoveryWork(boundTo: baseURL)
            Issue.record("Expected exact auth fence rejection")
        } catch {
            #expect(error as? DurableAuthError == .concurrentStateChange)
        }
        #expect(await transport.records().count == 1)
    }

    @Test("pending recovery prevents an in-flight current-session revoke from clearing auth")
    func currentSessionRevokeCannotStrandRecovery() async throws {
        let ids = recoveryIDs(offset: 60)
        let auth = RecoveryAuthStore(initial: activeEnvelope())
        let original = try #require(try auth.loadEnvelope())
        let journal = RecoveryJournalStore()
        let deleteGate = RecoverySuspension()
        let consumeGate = RecoverySuspension()
        let transport = RecoveryTransport(
            recoveryStore: journal,
            plans: [
                .suspendedResponse(.init(
                    statusCode: 204,
                    headers: successHeaders,
                    body: Data()
                ), deleteGate),
                .suspendedResponse(
                    consumeResponse(ids: ids, replayed: false, status: 201),
                    consumeGate
                ),
            ]
        )
        let transactionGate = RecoveryTransactionGate()
        let revokingCoordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(markers: [], uuids: []),
            transactionGate: transactionGate
        )
        let recoveringCoordinator = makeCoordinator(
            auth: auth,
            journal: journal,
            transport: transport,
            generator: RecoveryGenerator(
                markers: [111, 112, 113],
                uuids: [ids.session, ids.client, ids.successor]
            ),
            transactionGate: transactionGate
        )
        let approval = try await revokingCoordinator.prepareCurrentSessionRevocationApproval(
            boundTo: baseURL
        )
        let revoke = Task {
            try await revokingCoordinator.revokeAndForget(
                boundTo: baseURL,
                approvedBy: approval
            )
        }
        await deleteGate.waitUntilStarted()

        let consume = Task {
            try await recoveringCoordinator.consumeAccountRecoveryCode(
                credential(prefix: "dw_rc1_", marker: 110),
                boundTo: baseURL,
                descriptor: descriptor
            )
        }
        await consumeGate.waitUntilStarted()
        guard case .consumePending = try #require(try journal.loadEnvelope()).state else {
            Issue.record("Recovery must own its exact auth fence before DELETE resumes")
            await deleteGate.release()
            await consumeGate.release()
            _ = try? await revoke.value
            _ = try? await consume.value
            return
        }

        await deleteGate.release()
        do {
            try await revoke.value
            Issue.record("In-flight revoke must not clear recovery's fenced auth state")
        } catch {
            #expect(error as? DurableAuthError == .accountRecoveryPending)
        }
        #expect(try auth.loadEnvelope() == original)

        await consumeGate.release()
        let recovered = try await consume.value
        #expect(recovered.id == ids.session)
        guard case let .active(active) = try #require(try auth.loadEnvelope()).state else {
            Issue.record("Expected recovered auth installation")
            return
        }
        #expect(active.session.id == ids.session)
        guard case .consumeInstalledAwaitingHandoff = try #require(
            try journal.loadEnvelope()
        ).state else {
            Issue.record("Expected protected handoff state")
            return
        }
    }

    @Test("invalid Keychain bytes quarantine and require exact-item repair confirmation")
    func invalidJournalQuarantineRepair() throws {
        let keychain = RecoveryKeychain()
        try keychain.save(
            Data("{\"schema_version\":999,\"revision\":0,\"state\":{}}".utf8),
            service: KeychainDurableAccountRecoveryStateStore.defaultService,
            account: KeychainDurableAccountRecoveryStateStore.defaultAccount
        )
        let store = KeychainDurableAccountRecoveryStateStore(
            keychain: keychain,
            interprocessLockURL: nil
        )
        let quarantined = try #require(try store.loadEnvelope())
        guard case .incompatible = quarantined.state else {
            Issue.record("Expected quarantined state")
            return
        }
        #expect(throws: DurableAuthStateStoreError.invalidStoredState) {
            _ = try store.compareAndSwap(expected: quarantined, replacement: nil)
        }
        #expect(try store.discardIncompatibleEnvelope(expected: quarantined))
        #expect(try store.loadEnvelope() == nil)
    }

    private func makeCoordinator(
        auth: RecoveryAuthStore,
        journal: RecoveryJournalStore,
        transport: RecoveryTransport,
        generator: RecoveryGenerator,
        transactionGate: RecoveryTransactionGate = .shared
    ) -> DurableAuthCoordinator {
        DurableAuthCoordinator(
            stateStore: auth,
            legacyStore: RecoveryLegacyStore(),
            recoveryStore: journal,
            authRecoveryTransactionGate: transactionGate,
            transport: transport,
            generator: generator,
            now: { now }
        )
    }

    private func activeEnvelope(
        sessionMarker: UInt8 = 1,
        revision: UInt64 = 0,
        issuedAt: Date? = nil
    ) -> DurableAuthEnvelope {
        let sessionID = uuid(String(format: "aaaaaaaa-aaaa-4aaa-8aaa-%012d", sessionMarker))
        let clientID = uuid(String(format: "bbbbbbbb-bbbb-4bbb-8bbb-%012d", sessionMarker))
        let session = deviceSession(
            id: sessionID,
            clientID: clientID,
            issuedAt: issuedAt ?? now.addingTimeInterval(-30)
        )
        return .init(
            revision: revision,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .active(.init(
                session: session,
                credentials: .init(
                    accessToken: credential(prefix: "dw_da1_", marker: 1),
                    refreshToken: credential(prefix: "dw_dr1_", marker: 2)
                )
            ))
        )
    }

    private func deviceSession(
        id: UUID,
        clientID: UUID,
        issuedAt: Date
    ) -> DurableDeviceSessionMetadata {
        .init(
            id: id,
            clientInstanceID: clientID,
            clientKind: "macos",
            deviceLabel: descriptor.deviceLabel,
            scopes: descriptor.scopes,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: descriptor.clientVersion,
            clientCapabilities: descriptor.clientCapabilities,
            createdAt: issuedAt,
            lastSeenAt: issuedAt,
            credentialIssuedAt: issuedAt,
            accessExpiresAt: issuedAt.addingTimeInterval(DurableAuthCoordinator.accessLifetime),
            refreshIdleExpiresAt: issuedAt.addingTimeInterval(
                DurableAuthCoordinator.refreshIdleLifetime
            ),
            absoluteExpiresAt: issuedAt.addingTimeInterval(
                DurableAuthCoordinator.absoluteLifetime
            ),
            revision: 1
        )
    }

    private func currentResponse(
        _ metadata: DurableAccountRecoveryCodeMetadata?
    ) -> RecoveryHTTPResponse {
        var object: [String: Any] = ["recovery_code": NSNull()]
        if let metadata { object["recovery_code"] = metadataObject(metadata) }
        return jsonResponse(status: 200, object: object)
    }

    private func issueResponse(id: UUID, replayed: Bool, status: Int) -> RecoveryHTTPResponse {
        jsonResponse(status: status, object: [
            "recovery_code": metadataObject(.init(id: id, createdAt: now, revision: 1)),
            "replayed": replayed,
        ])
    }

    private func consumeResponse(
        ids: (session: UUID, client: UUID, successor: UUID),
        replayed: Bool,
        status: Int
    ) -> RecoveryHTTPResponse {
        let session = deviceSession(id: ids.session, clientID: ids.client, issuedAt: now)
        return jsonResponse(status: status, object: [
            "session": sessionObject(session),
            "successor_recovery_code": metadataObject(.init(
                id: ids.successor,
                createdAt: now,
                revision: 1
            )),
            "replayed": replayed,
        ])
    }

    private func sessionMutationResponse(
        _ session: DurableDeviceSessionMetadata,
        replayed: Bool,
        status: Int
    ) -> RecoveryHTTPResponse {
        jsonResponse(status: status, object: [
            "session": sessionObject(session),
            "replayed": replayed,
        ])
    }

    private func metadataObject(_ value: DurableAccountRecoveryCodeMetadata) -> [String: Any] {
        [
            "id": value.id.uuidString.lowercased(),
            "created_at": Self.timestamp(value.createdAt),
            "revision": value.revision,
        ]
    }

    private func sessionObject(_ value: DurableDeviceSessionMetadata) -> [String: Any] {
        [
            "id": value.id.uuidString.lowercased(),
            "client_instance_id": value.clientInstanceID.uuidString.lowercased(),
            "client_kind": value.clientKind,
            "device_label": value.deviceLabel,
            "scopes": value.scopes.map(\.rawValue),
            "client_contract_version": value.clientContractVersion,
            "client_version": value.clientVersion,
            "client_capabilities": value.clientCapabilities,
            "created_at": Self.timestamp(value.createdAt),
            "last_seen_at": Self.timestamp(value.lastSeenAt),
            "credential_issued_at": Self.timestamp(value.credentialIssuedAt),
            "access_expires_at": Self.timestamp(value.accessExpiresAt),
            "refresh_idle_expires_at": Self.timestamp(value.refreshIdleExpiresAt),
            "absolute_expires_at": Self.timestamp(value.absoluteExpiresAt),
            "revision": value.revision,
        ]
    }

    private func jsonResponse(status: Int, object: Any) -> RecoveryHTTPResponse {
        .init(
            statusCode: status,
            headers: successHeaders,
            body: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        )
    }

    private func unauthorizedResponse() -> RecoveryHTTPResponse {
        .init(
            statusCode: 401,
            headers: successHeaders.merging([
                "www-authenticate": DayWeaveAuthResponseContract.bearerChallenge,
            ]) { _, replacement in replacement },
            body: try! JSONSerialization.data(withJSONObject: [
                "error": ["code": "unauthorized", "message": "Unauthorized"],
            ], options: [.sortedKeys])
        )
    }

    private var successHeaders: [String: String] {
        [
            "cache-control": "no-store, max-age=0",
            "pragma": "no-cache",
            "content-type": "application/json; charset=utf-8",
        ]
    }

    private func recoveryIDs(offset: Int) -> (session: UUID, client: UUID, successor: UUID) {
        (
            uuid(String(format: "cccccccc-cccc-4ccc-8ccc-%012d", offset + 1)),
            uuid(String(format: "dddddddd-dddd-4ddd-8ddd-%012d", offset + 2)),
            uuid(String(format: "eeeeeeee-eeee-4eee-8eee-%012d", offset + 3))
        )
    }

    private func uuid(_ value: String) -> UUID { UUID(uuidString: value)! }

    private static func timestamp(_ value: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: value)
    }
}

private struct RecoveryHTTPResponse: Sendable {
    let statusCode: Int
    let headers: [String: String]
    let body: Data

    var durable: DurableAuthHTTPResponse {
        .init(statusCode: statusCode, headers: headers, body: body)
    }
}

private final class RecoveryAuthStore: DurableAuthStateStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var envelope: DurableAuthEnvelope?
    private var failNextCAS: Bool

    init(initial: DurableAuthEnvelope?, failNextCAS: Bool = false) {
        envelope = initial
        self.failNextCAS = failNextCAS
    }

    func loadEnvelope() throws -> DurableAuthEnvelope? { lock.withLock { envelope } }

    func compareAndSwap(
        expected: DurableAuthEnvelope?,
        replacement: DurableAuthEnvelope?
    ) throws -> Bool {
        lock.withLock {
            guard envelope == expected else { return false }
            if failNextCAS {
                failNextCAS = false
                return false
            }
            envelope = replacement
            return true
        }
    }

    func force(_ value: DurableAuthEnvelope?) { lock.withLock { envelope = value } }
}

private final class RecoveryJournalStore: DurableAccountRecoveryStateStoring,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var envelope: DurableAccountRecoveryEnvelope?

    init(initial: DurableAccountRecoveryEnvelope? = nil) { envelope = initial }
    func loadEnvelope() throws -> DurableAccountRecoveryEnvelope? { lock.withLock { envelope } }
    func compareAndSwap(
        expected: DurableAccountRecoveryEnvelope?,
        replacement: DurableAccountRecoveryEnvelope?
    ) throws -> Bool {
        lock.withLock {
            guard envelope == expected else { return false }
            envelope = replacement
            return true
        }
    }
}

private final class RecoveryTransactionGate:
    DurableAuthRecoveryTransactionGating, @unchecked Sendable
{
    static let shared = RecoveryTransactionGate()
    private let lock = NSLock()

    func withTransaction(_ operation: () throws -> Void) throws {
        lock.lock()
        defer { lock.unlock() }
        try operation()
    }
}

private final class RecoveryLegacyStore: BearerTokenStoring, @unchecked Sendable {
    func loadCredential() throws -> OriginBoundBearerCredential? { nil }
    func saveCredential(_ credential: OriginBoundBearerCredential) throws {}
    func deleteCredential() throws {}
}

private final class RecoveryConfigurationStore: SuggestionAPIConfigurationStoring,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var baseURL: String?
    init(baseURL: String?) { self.baseURL = baseURL }
    func loadBaseURL() -> String? { lock.withLock { baseURL } }
    func saveBaseURL(_ value: String) { lock.withLock { baseURL = value } }
}

private final class RecoveryGenerator: DurableCredentialGenerating, @unchecked Sendable {
    private let lock = NSLock()
    private var markers: [UInt8]
    private var uuids: [UUID]

    init(markers: [UInt8], uuids: [UUID]) {
        self.markers = markers
        self.uuids = uuids
    }

    func makeCredential(prefix: String) throws -> String {
        try lock.withLock {
            guard !markers.isEmpty else { throw DurableAuthError.randomnessUnavailable }
            return credential(prefix: prefix, marker: markers.removeFirst())
        }
    }

    func makeUUID() throws -> UUID {
        try lock.withLock {
            guard !uuids.isEmpty else { throw DurableAuthError.randomnessUnavailable }
            return uuids.removeFirst()
        }
    }
}

private actor RecoveryTransport: DurableAuthHTTPTransport {
    enum Plan: Sendable {
        case response(RecoveryHTTPResponse)
        case suspendedResponse(RecoveryHTTPResponse, RecoverySuspension)
        case failure(DurableAuthError)
    }

    struct Record: Sendable {
        let path: String
        let authorization: String?
        let body: Data?
        let journalAtSend: DurableAccountRecoveryEnvelope?
    }

    private let recoveryStore: RecoveryJournalStore
    private var plans: [Plan]
    private var recorded: [Record] = []

    init(recoveryStore: RecoveryJournalStore, plans: [Plan]) {
        self.recoveryStore = recoveryStore
        self.plans = plans
    }

    func records() -> [Record] { recorded }

    func send(_ request: URLRequest) async throws -> DurableAuthHTTPResponse {
        recorded.append(.init(
            path: request.url?.path ?? "",
            authorization: request.value(forHTTPHeaderField: "Authorization"),
            body: request.httpBody,
            journalAtSend: try recoveryStore.loadEnvelope()
        ))
        guard !plans.isEmpty else { throw DurableAuthError.transport(.badServerResponse) }
        switch plans.removeFirst() {
        case let .response(value): return value.durable
        case let .suspendedResponse(value, suspension):
            await suspension.suspendUntilReleased()
            return value.durable
        case let .failure(error): throw error
        }
    }
}

private actor RecoverySuspension {
    private var started = false
    private var released = false
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func waitUntilStarted() async {
        guard !started else { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }

    func suspendUntilReleased() async {
        if !started {
            started = true
            let waiters = startWaiters
            startWaiters.removeAll()
            waiters.forEach { $0.resume() }
        }
        guard !released else { return }
        await withCheckedContinuation { releaseWaiters.append($0) }
    }

    func release() {
        guard !released else { return }
        released = true
        let waiters = releaseWaiters
        releaseWaiters.removeAll()
        waiters.forEach { $0.resume() }
    }
}

private final class RecoveryKeychain: KeychainSecretAccessing, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    func read(service: String, account: String) throws -> Data? {
        lock.withLock { values["\(service)\u{0}\(account)"] }
    }
    func save(_ data: Data, service: String, account: String) throws {
        lock.withLock { values["\(service)\u{0}\(account)"] = data }
    }
    func delete(service: String, account: String) throws {
        _ = lock.withLock { values.removeValue(forKey: "\(service)\u{0}\(account)") }
    }
}

private func credential(prefix: String, marker: UInt8) -> String {
    prefix + Data(repeating: marker, count: 32).base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}
#endif
