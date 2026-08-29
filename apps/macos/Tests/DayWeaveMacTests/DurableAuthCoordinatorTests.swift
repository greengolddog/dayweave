import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Durable macOS authentication", .serialized)
struct DurableAuthCoordinatorTests {
    private let baseURL = try! DayWeaveAPIBaseURL("https://api.example.com/gateway")
    private let descriptor = DurableAuthClientDescriptor(
        deviceLabel: "Synthetic Test Mac",
        clientVersion: "1.0-test"
    )
    private let instant = Date(timeIntervalSince1970: 1_788_000_000)

    @Test("contract v2 requires REST schedule publication authority and rejects v1 sessions")
    func schedulePublicationScopeRequiresReenrollment() {
        #expect(DurableAuthClientDescriptor.contractVersion == 2)
        #expect(DayWeaveAuthScope.deviceDefaults.contains(.schedulePublish))
        #expect(descriptor.isValid)
        let current = makeActive(issuedAt: instant, accessMarker: 201, refreshMarker: 202)
        #expect(DurableAuthCoordinator.isStoredSessionValid(current.session))
        let legacy = DurableDeviceSessionMetadata(
            id: current.session.id,
            clientInstanceID: current.session.clientInstanceID,
            clientKind: current.session.clientKind,
            deviceLabel: current.session.deviceLabel,
            scopes: current.session.scopes.filter { $0 != .schedulePublish },
            clientContractVersion: 1,
            clientVersion: current.session.clientVersion,
            clientCapabilities: current.session.clientCapabilities,
            createdAt: current.session.createdAt,
            lastSeenAt: current.session.lastSeenAt,
            credentialIssuedAt: current.session.credentialIssuedAt,
            accessExpiresAt: current.session.accessExpiresAt,
            refreshIdleExpiresAt: current.session.refreshIdleExpiresAt,
            absoluteExpiresAt: current.session.absoluteExpiresAt,
            revision: current.session.revision
        )
        #expect(!DurableAuthCoordinator.isStoredSessionValid(legacy))
    }

    @Test("hybrid upgrade journals enrollment tuple before send and retries exact bytes after restart")
    func hybridEnrollmentExactRetry() async throws {
        let clientID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
        let sessionID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
        let enrollmentID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
        let enrollmentCode = syntheticCredential(prefix: "dw_en1_", marker: 91)
        let bootstrap = "synthetic-bootstrap-for-hybrid-upgrade"
        let initial = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .legacy(.init(bearerToken: bootstrap))
        )
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .enrollment(
                    id: enrollmentID,
                    code: enrollmentCode,
                    expiresAt: instant.addingTimeInterval(600)
                ),
                .failure(.transport(.timedOut)),
                .session(issuedAt: instant, statusCode: 200, replayed: true),
            ]
        )
        let generator = TestDurableCredentialGenerator(
            markers: [91, 11, 12],
            uuids: [enrollmentID, sessionID]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )

        do {
            _ = try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
            Issue.record("Expected the synthetic lost response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
            #expect(!error.localizedDescription.contains(bootstrap))
            #expect(!error.localizedDescription.contains(enrollmentCode))
        }

        let pendingEnvelope = try #require(state.loadEnvelope())
        let pending: EnrollmentPendingAuthState
        if case let .enrollmentPending(value) = pendingEnvelope.state {
            pending = value
        } else {
            Issue.record("Expected a durable enrollment journal")
            return
        }
        #expect(pending.proposedSessionID == sessionID)
        #expect(pending.enrollmentID == enrollmentID)
        let firstRecords = await transport.records()
        #expect(firstRecords.count == 2)
        #expect(firstRecords[0].path.hasSuffix("/v1/auth/device-enrollments"))
        #expect(firstRecords[1].path.hasSuffix("/v1/auth/device-enrollments/consume"))
        #expect(firstRecords[1].stateAtSend == pendingEnvelope)

        let relaunched = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )
        try await relaunched.resumePendingWork(boundTo: baseURL)
        let records = await transport.records()
        #expect(records.count == 3)
        #expect(records[1].authorization == records[2].authorization)
        #expect(records[1].body == records[2].body)
        #expect(records[1].method == records[2].method)
        #expect(records[1].path == records[2].path)

        let activeEnvelope = try #require(state.loadEnvelope())
        guard case let .active(active) = activeEnvelope.state else {
            Issue.record("Expected enrollment recovery to commit active state")
            return
        }
        #expect(active.session.id == sessionID)
        #expect(active.credentials == pending.proposedCredentials)
        #expect(activeEnvelope.clientInstanceID == clientID)
    }

    @Test("cold bootstrap journals exact creation authority before send and replays after restart")
    func coldBootstrapCreationExactRetryAfterRestart() async throws {
        let journalURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway-a")
        let bootstrap = "synthetic-cold-bootstrap-authority"
        let clientID = UUID(uuidString: "34343434-3434-4434-8434-343434343434")!
        let enrollmentID = UUID(uuidString: "35353535-3535-4535-8535-353535353535")!
        let sessionID = UUID(uuidString: "36363636-3636-4636-8636-363636363636")!
        let enrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: 221)
        let state = TestDurableAuthStateStore()
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .failure(.transport(.networkConnectionLost)),
                .enrollment(
                    id: enrollmentID,
                    code: enrollmentToken,
                    expiresAt: instant.addingTimeInterval(600),
                    statusCode: 200,
                    replayed: true
                ),
                .session(issuedAt: instant, statusCode: 201, replayed: false),
            ]
        )
        let generator = TestDurableCredentialGenerator(
            markers: [221, 222, 223],
            uuids: [clientID, enrollmentID, sessionID]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )

        do {
            _ = try await coordinator.enroll(
                boundTo: journalURL,
                descriptor: descriptor,
                bootstrapToken: bootstrap
            )
            Issue.record("Expected the synthetic lost creation response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.networkConnectionLost))
        }
        let creationEnvelope = try #require(state.loadEnvelope())
        guard case let .enrollmentCreationPending(pending) = creationEnvelope.state else {
            Issue.record("Creation authority must be journaled before its first send")
            return
        }
        #expect(creationEnvelope.clientInstanceID == clientID)
        #expect(pending.proposedEnrollmentID == enrollmentID)
        #expect(pending.proposedEnrollmentToken == enrollmentToken)
        #expect(pending.proposedSessionID == sessionID)
        #expect(pending.creationRequest.bodySHA256.count == 64)
        #expect(coordinator.presentation(boundTo: journalURL).phase == .enrollmentCreationPending)
        #expect(!coordinator.presentation(boundTo: journalURL).canConsumeEnrollmentCode)

        let relaunched = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )
        for mismatchedURL in [
            try DayWeaveAPIBaseURL("https://api.example.com/gateway-b"),
            try DayWeaveAPIBaseURL("https://api.example.com:444/gateway-a"),
            try DayWeaveAPIBaseURL("https://other.example.com/gateway-a"),
        ] {
            do {
                try await relaunched.resumePendingWork(boundTo: mismatchedURL)
                Issue.record("A creation journal must never be retargeted")
            } catch {
                #expect(error as? DurableAuthError == .originMismatch)
            }
            #expect(await transport.records().count == 1)
            #expect(state.loadEnvelope() == creationEnvelope)
            let mismatch = relaunched.presentation(boundTo: mismatchedURL)
            #expect(mismatch.phase == .incompatible)
            #expect(!mismatch.canUpgrade)
            #expect(!mismatch.canReenroll)
            #expect(!mismatch.canConsumeEnrollmentCode)
        }
        try await relaunched.resumePendingWork(boundTo: journalURL)

        let records = await transport.records()
        #expect(records.count == 3)
        #expect(records[0].url == "https://api.example.com/gateway-a/v1/auth/device-enrollments")
        #expect(records[0].url == records[1].url)
        #expect(records[0].method == records[1].method)
        #expect(records[0].headers == records[1].headers)
        #expect(records[0].authorization == "Bearer \(bootstrap)")
        #expect(records[0].authorization == records[1].authorization)
        #expect(records[0].body == records[1].body)
        #expect(records[0].stateAtSend == creationEnvelope)
        #expect(records[1].stateAtSend == creationEnvelope)
        let encodedCreation = try #require(records[0].body)
        let body = try #require(
            try JSONSerialization.jsonObject(with: encodedCreation) as? [String: Any]
        )
        #expect(body["id"] as? String == enrollmentID.uuidString.lowercased())
        #expect(body["enrollment_token"] as? String == enrollmentToken)
        #expect(body["client_instance_id"] as? String == clientID.uuidString.lowercased())
        #expect(records[2].url.hasSuffix("/gateway-a/v1/auth/device-enrollments/consume"))
        guard case let .active(active)? = state.loadEnvelope()?.state else {
            Issue.record("Exact creation replay should complete enrollment")
            return
        }
        #expect(active.session.id == sessionID)
        #expect(active.session.clientInstanceID == clientID)
    }

    @Test("a local-only tombstone cannot downgrade while bootstrap creation is ambiguous")
    func tombstoneBootstrapCreationExactRetryAfterRestart() async throws {
        let oldURL = try DayWeaveAPIBaseURL("https://old.example.com/root")
        let journalURL = try DayWeaveAPIBaseURL("https://replacement.example.com/gateway-a")
        let oldClientID = UUID(uuidString: "37373737-3737-4737-8737-373737373737")!
        let oldSessionID = UUID(uuidString: "38383838-3838-4838-8838-383838383838")!
        let newClientID = UUID(uuidString: "39393939-3939-4939-8939-393939393939")!
        let enrollmentID = UUID(uuidString: "40404040-4040-4040-8040-404040404040")!
        let sessionID = UUID(uuidString: "41414141-4141-4141-8141-414141414141")!
        let enrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: 224)
        let bootstrap = "synthetic-replacement-bootstrap-authority"
        let tombstone = DurableAuthEnvelope(
            revision: 90,
            origin: oldURL.credentialOriginIdentifier,
            clientInstanceID: oldClientID,
            state: .reauthenticationRequired(.init(
                clientInstanceID: oldClientID,
                previousSessionID: oldSessionID,
                reason: .explicitlyDisconnected,
                detectedAt: instant
            ))
        )
        let state = TestDurableAuthStateStore(initial: tombstone)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .failure(.transport(.timedOut)),
                .enrollment(
                    id: enrollmentID,
                    code: enrollmentToken,
                    expiresAt: instant.addingTimeInterval(600),
                    statusCode: 200,
                    replayed: true
                ),
                .session(issuedAt: instant, statusCode: 201, replayed: false),
            ]
        )
        let generator = TestDurableCredentialGenerator(
            markers: [224, 225, 226],
            uuids: [newClientID, enrollmentID, sessionID]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )

        do {
            _ = try await coordinator.enroll(
                boundTo: journalURL,
                descriptor: descriptor,
                bootstrapToken: bootstrap
            )
            Issue.record("Expected the synthetic lost replacement response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        let journal = try #require(state.loadEnvelope())
        guard case let .enrollmentCreationPending(pending) = journal.state else {
            Issue.record("A tombstone replacement must retain creation authority")
            return
        }
        #expect(pending.durableWasPreviouslyActivated)
        #expect(journal.clientInstanceID == newClientID)
        #expect(journal.origin == journalURL.credentialOriginIdentifier)
        if case .legacy = journal.state {
            Issue.record("A previously durable client must never downgrade to legacy state")
        }

        let relaunched = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )
        try await relaunched.resumePendingWork(boundTo: journalURL)
        let records = await transport.records()
        #expect(records.count == 3)
        #expect(records[0].url == records[1].url)
        #expect(records[0].headers == records[1].headers)
        #expect(records[0].body == records[1].body)
        #expect(records[0].authorization == records[1].authorization)
        guard case let .active(active)? = state.loadEnvelope()?.state else {
            Issue.record("Replacement bootstrap replay should activate")
            return
        }
        #expect(active.session.clientInstanceID == newClientID)
        #expect(active.session.id == sessionID)
    }

    @Test("creation CAS cannot overwrite a newer durable state")
    func enrollmentCreationStaleCASPreservesNewerState() async throws {
        let clientID = UUID(uuidString: "42424242-4242-4242-8242-424242424242")!
        let enrollmentID = UUID(uuidString: "43434343-4343-4343-8343-434343434343")!
        let proposedSessionID = UUID(uuidString: "45454545-4545-4545-8545-454545454545")!
        let enrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: 227)
        let initial = DurableAuthEnvelope(
            revision: 12,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .legacy(.init(bearerToken: "synthetic-stale-cas-bootstrap"))
        )
        let newerActive = makeActive(
            issuedAt: instant,
            accessMarker: 230,
            refreshMarker: 231,
            sessionID: UUID(uuidString: "46464646-4646-4646-8646-464646464646")!,
            clientInstanceID: clientID
        )
        let newer = DurableAuthEnvelope(
            revision: 99,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .active(newerActive)
        )
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.enrollmentAndReplaceState(
                id: enrollmentID,
                code: enrollmentToken,
                expiresAt: instant.addingTimeInterval(600),
                replacement: newer
            )]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(
                markers: [227, 228, 229],
                uuids: [enrollmentID, proposedSessionID]
            ),
            now: { instant }
        )

        do {
            _ = try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
            Issue.record("A stale creation response must not overwrite newer state")
        } catch {
            #expect(error as? DurableAuthError == .concurrentStateChange)
        }
        #expect(state.loadEnvelope() == newer)
        #expect(await transport.records().count == 1)
    }

    @Test("creation expiry is measured from server receive time after a delayed first send")
    func enrollmentCreationDelayedFirstSendUsesReceiveTime() async throws {
        let clientID = UUID(uuidString: "47474747-4747-4747-8747-474747474747")!
        let enrollmentID = UUID(uuidString: "48484848-4848-4848-8848-484848484848")!
        let sessionID = UUID(uuidString: "49494949-4949-4949-8949-494949494949")!
        let enrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: 232)
        let delayedReceive = instant.addingTimeInterval(3_600)
        let state = TestDurableAuthStateStore(initial: DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .legacy(.init(bearerToken: "synthetic-delayed-bootstrap"))
        ))
        let clock = TestAuthClock(instant)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .delayedEnrollment(
                    id: enrollmentID,
                    code: enrollmentToken,
                    expiresAt: delayedReceive.addingTimeInterval(600),
                    nanoseconds: 50_000_000
                ),
                .session(issuedAt: delayedReceive, statusCode: 201, replayed: false),
            ]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(
                markers: [232, 233, 234],
                uuids: [enrollmentID, sessionID]
            ),
            now: { clock.value }
        )
        let enrollment = Task {
            try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
        }
        while await transport.records().isEmpty { await Task.yield() }
        clock.value = delayedReceive
        let session = try await enrollment.value
        #expect(session.id == sessionID)
        #expect(await transport.records().count == 2)
    }

    @Test("a pre-minted one-time code is consumed directly and exact-retried after restart")
    func directEnrollmentCodeExactRetry() async throws {
        let journalURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway-a")
        let sessionID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
        let serverClientID = UUID(uuidString: "55555555-5555-4555-8555-555555555555")!
        let code = syntheticCredential(prefix: "dw_en1_", marker: 92)
        let state = TestDurableAuthStateStore()
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .failure(.transport(.networkConnectionLost)),
                .session(
                    issuedAt: instant,
                    statusCode: 200,
                    replayed: true,
                    serverClientInstanceID: serverClientID
                ),
            ]
        )
        let generator = TestDurableCredentialGenerator(markers: [21, 22], uuids: [sessionID])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )

        do {
            _ = try await coordinator.consumeOneTimeEnrollmentCode(
                code,
                boundTo: journalURL,
                descriptor: descriptor
            )
            Issue.record("Expected the synthetic lost response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.networkConnectionLost))
        }
        let pending = try #require(state.loadEnvelope())
        #expect(pending.clientInstanceID == nil)
        guard case .enrollmentPending = pending.state else {
            Issue.record("Expected direct-code tuple to be journaled")
            return
        }
        let relaunched = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant }
        )
        for mismatchedURL in [
            try DayWeaveAPIBaseURL("https://api.example.com/gateway-b"),
            try DayWeaveAPIBaseURL("https://api.example.com:444/gateway-a"),
            try DayWeaveAPIBaseURL("https://other.example.com/gateway-a"),
        ] {
            do {
                try await relaunched.resumePendingWork(boundTo: mismatchedURL)
                Issue.record("A journal must not be retargeted after restart")
            } catch {
                #expect(error as? DurableAuthError == .originMismatch)
            }
            #expect(await transport.records().count == 1)
            #expect(!relaunched.hasUsableCredential(boundTo: mismatchedURL))
            #expect(relaunched.presentation(boundTo: mismatchedURL).phase == .incompatible)
        }
        try await relaunched.resumePendingWork(boundTo: journalURL)

        let records = await transport.records()
        #expect(records.count == 2)
        #expect(records.allSatisfy {
            $0.url == "https://api.example.com/gateway-a/v1/auth/device-enrollments/consume"
        })
        #expect(records[0].authorization == "Bearer \(code)")
        #expect(records[0].authorization == records[1].authorization)
        #expect(records[0].method == records[1].method)
        #expect(records[0].headers == records[1].headers)
        #expect(records[0].body == records[1].body)
        let activeEnvelope = try #require(state.loadEnvelope())
        #expect(activeEnvelope.clientInstanceID == serverClientID)
        guard case let .active(active) = activeEnvelope.state else {
            Issue.record("Expected direct enrollment to activate")
            return
        }
        #expect(active.session.clientInstanceID == serverClientID)
        #expect(active.session.id == sessionID)
    }

    @Test("bootstrap and direct-code enrollment cannot replace a live durable session")
    func liveSessionReplacementRequiresRemoteRevocation() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 23, refreshMarker: 24)
        let initial = envelope(active: active, revision: 5)
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(stateStore: state, plans: [])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        let code = syntheticCredential(prefix: "dw_en1_", marker: 25)

        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.enroll(
                boundTo: baseURL,
                descriptor: descriptor,
                bootstrapToken: "synthetic-bootstrap-replacement"
            )
        }
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.consumeOneTimeEnrollmentCode(
                code,
                boundTo: baseURL,
                descriptor: descriptor
            )
        }
        #expect(state.loadEnvelope() == initial)
        #expect(!coordinator.presentation(boundTo: baseURL).canReenroll)
        #expect(!coordinator.presentation(boundTo: baseURL).canConsumeEnrollmentCode)

        let pending = makeRefreshPending(
            previous: active,
            accessMarker: 26,
            refreshMarker: 27
        )
        let refreshEnvelope = DurableAuthEnvelope(
            revision: initial.revision + 1,
            origin: initial.origin,
            clientInstanceID: initial.clientInstanceID,
            state: .refreshPending(pending)
        )
        state.forceReplace(refreshEnvelope)
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.enroll(
                boundTo: baseURL,
                descriptor: descriptor,
                bootstrapToken: "synthetic-bootstrap-replacement"
            )
        }
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.consumeOneTimeEnrollmentCode(
                code,
                boundTo: baseURL,
                descriptor: descriptor
            )
        }
        #expect(state.loadEnvelope() == refreshEnvelope)
        #expect(!coordinator.presentation(boundTo: baseURL).canReenroll)
        #expect(!coordinator.presentation(boundTo: baseURL).canConsumeEnrollmentCode)

        let differentOrigin = try DayWeaveAPIBaseURL("https://other.example.com")
        #expect(!coordinator.presentation(boundTo: differentOrigin).canReenroll)
        #expect(await transport.records().isEmpty)
    }

    @Test("lost refresh response retains exact pair even after its access token expires")
    func refreshExactRetryAfterAccessExpiry() async throws {
        let journalURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway-a")
        let active = makeActive(issuedAt: instant, accessMarker: 31, refreshMarker: 32)
        let initial = envelope(active: active, revision: 7)
        let state = TestDurableAuthStateStore(initial: initial)
        let clock = TestAuthClock(instant.addingTimeInterval(850))
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .failure(.transport(.timedOut)),
                .session(
                    issuedAt: instant.addingTimeInterval(850),
                    statusCode: 200,
                    replayed: true
                ),
            ]
        )
        let generator = TestDurableCredentialGenerator(markers: [33, 34])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { clock.value }
        )
        let stableBinding = try coordinator.bindingIdentifier(boundTo: journalURL)

        do {
            _ = try await coordinator.authorization(boundTo: journalURL)
            Issue.record("Expected the synthetic lost refresh response")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        let pending = try #require(state.loadEnvelope())
        guard case .refreshPending = pending.state else {
            Issue.record("Expected a refresh journal")
            return
        }
        clock.value = instant.addingTimeInterval(4_000)
        let relaunched = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { clock.value }
        )
        for mismatchedURL in [
            try DayWeaveAPIBaseURL("https://api.example.com/gateway-b"),
            try DayWeaveAPIBaseURL("https://api.example.com:444/gateway-a"),
            try DayWeaveAPIBaseURL("https://other.example.com/gateway-a"),
        ] {
            do {
                try await relaunched.resumePendingWork(boundTo: mismatchedURL)
                Issue.record("A refresh journal must not be retargeted after restart")
            } catch {
                #expect(error as? DurableAuthError == .originMismatch)
            }
            #expect(await transport.records().count == 1)
        }
        try await relaunched.resumePendingWork(boundTo: journalURL)
        let records = await transport.records()
        #expect(records.count == 2)
        #expect(records.allSatisfy {
            $0.url == "https://api.example.com/gateway-a/v1/auth/sessions/refresh"
        })
        #expect(records[0].authorization == records[1].authorization)
        #expect(records[0].method == records[1].method)
        #expect(records[0].headers == records[1].headers)
        #expect(records[0].body == records[1].body)
        #expect(try relaunched.bindingIdentifier(boundTo: journalURL) == stableBinding)
        guard case let .active(recovered)? = state.loadEnvelope()?.state else {
            Issue.record("Expected exact replay to recover committed rotation")
            return
        }
        #expect(recovered.session.accessExpiresAt < clock.value)
    }

    @Test("concurrent proactive refresh callers share one rotation and stable binding")
    func concurrentProactiveRefreshIsSingleFlight() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 41, refreshMarker: 42)
        let state = TestDurableAuthStateStore(initial: envelope(active: active, revision: 10))
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .session(
                    issuedAt: instant.addingTimeInterval(850),
                    statusCode: 200,
                    replayed: false
                ),
            ]
        )
        let generator = TestDurableCredentialGenerator(markers: [43, 44])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant.addingTimeInterval(850) }
        )
        let priorBinding = try coordinator.bindingIdentifier(boundTo: baseURL)
        let authorizations = try await withThrowingTaskGroup(
            of: DurableAuthorization.self,
            returning: [DurableAuthorization].self
        ) { group in
            for _ in 0..<12 {
                group.addTask { try await coordinator.authorization(boundTo: self.baseURL) }
            }
            var values: [DurableAuthorization] = []
            for try await value in group { values.append(value) }
            return values
        }
        #expect(authorizations.count == 12)
        #expect(Set(authorizations.map(\.bearerToken)).count == 1)
        #expect(Set(authorizations.map(\.bindingIdentifier)) == [priorBinding])
        #expect(await transport.records().count == 1)
    }

    @Test("401 refresh replays the exact prepared schedule publication body")
    @MainActor
    func unauthorizedRequestRefreshesAndReplaysExactly() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 51, refreshMarker: 52)
        let state = TestDurableAuthStateStore(initial: envelope(active: active, revision: 2))
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .session(
                    issuedAt: instant.addingTimeInterval(60),
                    statusCode: 200,
                    replayed: false
                ),
            ]
        )
        let nextAccess = syntheticCredential(prefix: "dw_da1_", marker: 53)
        let generator = TestDurableCredentialGenerator(markers: [53, 54])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: generator,
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: active.credentials.accessToken)
        URLProtocolStub.storage.reset(key: nextAccess)
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: active.credentials.accessToken,
            .init(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: Data(#"{"error":{"code":"unauthorized","message":"rejected"}}"#.utf8)
            )
        )
        let client = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
        let schedule = DayWeaveSchedulePreviewRequest(
            asOf: instant,
            horizonStart: instant,
            horizonEnd: instant.addingTimeInterval(86_400),
            timezoneName: "UTC",
            availability: [],
            fixedBlocks: [],
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
        let prepared = try client.prepareSchedulePublication(.init(
            idempotencyKey: UUID(uuidString: "51515151-5151-4151-8151-515151515151")!,
            expectedInputDigest: "sha256:5151515151515151515151515151515151515151515151515151515151515151",
            schedule: schedule
        ))
        _ = try await client.publishSchedule(prepared)

        let first = try #require(
            URLProtocolStub.storage.requests(
                for: active.credentials.accessToken,
                includingSchedulePublication: true
            ).first
        )
        let replay = try #require(URLProtocolStub.storage.requests(
            for: nextAccess,
            includingSchedulePublication: true
        ).first)
        #expect(first.method == replay.method)
        #expect(first.url == replay.url)
        #expect(first.body == replay.body)
        var firstHeaders = first.headers
        var replayHeaders = replay.headers
        firstHeaders.removeValue(forKey: "Authorization")
        replayHeaders.removeValue(forKey: "Authorization")
        #expect(firstHeaders == replayHeaders)
        #expect(await transport.records().count == 1)
    }

    @Test("durable rejection never falls back to a residual legacy bearer")
    func durableRejectionNeverFallsBack() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 61, refreshMarker: 62)
        let initial = envelope(active: active, revision: 3)
        let state = TestDurableAuthStateStore(initial: initial)
        let legacy = TestBearerTokenStore(token: "synthetic-residual-static")
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.response(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: trustedUnauthorizedBody
            )]
        )
        let coordinator = DurableAuthCoordinator(
            stateStore: state,
            legacyStore: legacy,
            transport: transport,
            generator: TestDurableCredentialGenerator(markers: [63, 64]),
            now: { instant.addingTimeInterval(60) }
        )

        do {
            _ = try await coordinator.recoverFromUnauthorized(
                rejectedBearer: active.credentials.accessToken,
                boundTo: baseURL
            )
            Issue.record("Expected refresh rejection")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case .reauthenticationRequired? = state.loadEnvelope()?.state else {
            Issue.record("Expected durable reauthentication state")
            return
        }
        do {
            _ = try await coordinator.authorization(boundTo: baseURL)
            Issue.record("Residual static bearer must never be selected")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        do {
            try await coordinator.installLegacyCredential(
                "synthetic-replacement-static",
                boundTo: baseURL
            )
            Issue.record("Durable state must require direct re-enrollment")
        } catch {
            #expect(error as? DurableAuthError == .durableSessionRequiresExplicitReenrollment)
        }
        #expect(await transport.records().count == 1)
    }

    @Test("a Keychain read failure never exposes or selects a residual legacy bearer")
    @MainActor
    func keychainReadFailureNeverFallsBack() async throws {
        let transportState = TestDurableAuthStateStore()
        let transport = TestDurableAuthTransport(stateStore: transportState, plans: [])
        let legacy = TestBearerTokenStore(token: "synthetic-residual-static")
        let coordinator = DurableAuthCoordinator(
            stateStore: FailingDurableAuthStateStore(),
            legacyStore: legacy,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )

        #expect(!coordinator.hasUsableCredential(boundTo: baseURL))
        let presentation = coordinator.presentation(boundTo: baseURL)
        #expect(presentation.phase == .incompatible)
        #expect(!presentation.canUpgrade)
        #expect(!presentation.canReenroll)
        #expect(!presentation.canConsumeEnrollmentCode)
        #expect(!presentation.canRevokeRemotely)
        #expect(!presentation.canForget)

        do {
            _ = try await coordinator.authorization(boundTo: baseURL)
            Issue.record("A residual static bearer must not be selected after a Keychain failure")
        } catch {
            #expect(error as? DurableAuthError == .localStateUnavailable)
        }
        #expect(await transport.records().isEmpty)

        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: legacy,
            authCoordinator: coordinator,
            session: URLProtocolStub.makeSession(),
            now: { self.instant }
        )
        #expect(!sync.tokenConfigured)
        #expect(!sync.applyConfiguration(
            baseURL: baseURL.url.absoluteString,
            newToken: ""
        ))
    }

    @Test("strict auth response keys quarantine recoverable pending material")
    func strictResponseValidationQuarantinesPendingMaterial() async throws {
        let clientID = UUID(uuidString: "66666666-6666-4666-8666-666666666666")!
        let state = TestDurableAuthStateStore(initial: DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .legacy(.init(bearerToken: "synthetic-bootstrap-strict"))
        ))
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .enrollment(
                    id: UUID(uuidString: "77777777-7777-4777-8777-777777777777")!,
                    code: syntheticCredential(prefix: "dw_en1_", marker: 71),
                    expiresAt: instant.addingTimeInterval(600)
                ),
                .session(
                    issuedAt: instant,
                    statusCode: 201,
                    replayed: false,
                    extraTopLevelKey: true
                ),
            ]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(
                markers: [71, 72, 73],
                uuids: [
                    UUID(uuidString: "77777777-7777-4777-8777-777777777777")!,
                    UUID(uuidString: "88888888-8888-4888-8888-888888888888")!,
                ]
            ),
            now: { instant }
        )
        do {
            _ = try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
            Issue.record("Expected strict response rejection")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        guard case let .incompatible(value)? = state.loadEnvelope()?.state else {
            Issue.record("Expected explicit incompatible quarantine")
            return
        }
        guard case .enrollment? = value.recovery else {
            Issue.record("Expected exact enrollment material to remain recoverable after update")
            return
        }
    }

    @Test("creation response requires exact echo and status-to-replay consistency")
    func strictEnrollmentCreationResponseContract() async throws {
        let clientID = UUID(uuidString: "50505050-5050-4050-8050-505050505050")!
        let enrollmentID = UUID(uuidString: "51515151-5151-4151-8151-515151515151")!
        let sessionID = UUID(uuidString: "52525252-5252-4252-8252-525252525252")!
        let proposedToken = syntheticCredential(prefix: "dw_en1_", marker: 235)
        let cases: [(Int, Bool, UUID, String)] = [
            (200, false, enrollmentID, proposedToken),
            (201, true, enrollmentID, proposedToken),
            (
                201,
                false,
                UUID(uuidString: "53535353-5353-4353-8353-535353535353")!,
                proposedToken
            ),
            (
                201,
                false,
                enrollmentID,
                syntheticCredential(prefix: "dw_en1_", marker: 238)
            ),
        ]

        for (statusCode, replayed, responseID, responseToken) in cases {
            let initial = DurableAuthEnvelope(
                revision: 4,
                origin: baseURL.credentialOriginIdentifier,
                clientInstanceID: clientID,
                state: .legacy(.init(bearerToken: "synthetic-strict-creation-bootstrap"))
            )
            let state = TestDurableAuthStateStore(initial: initial)
            let transport = TestDurableAuthTransport(
                stateStore: state,
                plans: [.enrollment(
                    id: responseID,
                    code: responseToken,
                    expiresAt: instant.addingTimeInterval(600),
                    statusCode: statusCode,
                    replayed: replayed
                )]
            )
            let coordinator = makeCoordinator(
                state: state,
                transport: transport,
                generator: TestDurableCredentialGenerator(
                    markers: [235, 236, 237],
                    uuids: [enrollmentID, sessionID]
                ),
                now: { instant }
            )
            do {
                _ = try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
                Issue.record("Mismatched creation response must fail closed")
            } catch {
                #expect(error as? DurableAuthError == .invalidResponse)
            }
            guard case let .incompatible(quarantine)? = state.loadEnvelope()?.state,
                  case .enrollmentCreation? = quarantine.recovery else {
                Issue.record("Exact creation recovery evidence must be retained")
                continue
            }
            #expect(await transport.records().count == 1)
        }

        let deterministicState = TestDurableAuthStateStore(initial: DurableAuthEnvelope(
            revision: 8,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientID,
            state: .legacy(.init(bearerToken: "synthetic-conflict-bootstrap"))
        ))
        let deterministicTransport = TestDurableAuthTransport(
            stateStore: deterministicState,
            plans: [.response(
                statusCode: 409,
                headers: [
                    "Cache-Control": "no-store, max-age=0",
                    "Pragma": "no-cache",
                    "Content-Type": "application/json",
                ],
                body: Data(
                    #"{"error":{"code":"conflict","message":"Exact tuple differs"}}"#.utf8
                )
            )]
        )
        let deterministicCoordinator = makeCoordinator(
            state: deterministicState,
            transport: deterministicTransport,
            generator: TestDurableCredentialGenerator(
                markers: [235, 236, 237],
                uuids: [enrollmentID, sessionID]
            ),
            now: { instant }
        )
        do {
            _ = try await deterministicCoordinator.enroll(
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("A strict deterministic creation rejection must fail closed")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        guard case let .incompatible(quarantine)? = deterministicState.loadEnvelope()?.state,
              case .enrollmentCreation? = quarantine.recovery else {
            Issue.record("Deterministic rejection must retain exact creation recovery")
            return
        }
    }

    @Test("revoke-first sign-out validates 204 before exact CAS deletion")
    func revokeFirstSignOut() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 81, refreshMarker: 82)
        let initial = envelope(active: active, revision: 4)
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(stateStore: state, plans: [.noContent])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        try await coordinator.revokeAndForget(boundTo: baseURL)
        #expect(state.loadEnvelope() == nil)
        let record = try #require(await transport.records().first)
        #expect(record.method == "DELETE")
        #expect(record.path.hasSuffix("/v1/auth/sessions/\(active.session.id.uuidString.lowercased())"))
        #expect(record.body == nil)
        #expect(record.stateAtSend == initial)
    }

    @Test("failed remote revoke retains state; stale success cannot delete newer CAS state")
    func revokeFailureAndStaleCASRetention() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 83, refreshMarker: 84)
        let initial = envelope(active: active, revision: 8)
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.failure(.transport(.notConnectedToInternet))]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        do {
            try await coordinator.revokeAndForget(boundTo: baseURL)
            Issue.record("Expected offline revoke failure")
        } catch {
            #expect(error as? DurableAuthError == .transport(.notConnectedToInternet))
        }
        #expect(state.loadEnvelope() == initial)

        let rejectedTransport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.raw(statusCode: 403, body: Data())]
        )
        let rejectedCoordinator = makeCoordinator(
            state: state,
            transport: rejectedTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        do {
            try await rejectedCoordinator.revokeAndForget(boundTo: baseURL)
            Issue.record("Expected rejected revoke failure")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        #expect(state.loadEnvelope() == initial)

        let newer = DurableAuthEnvelope(
            revision: initial.revision + 1,
            origin: initial.origin,
            clientInstanceID: initial.clientInstanceID,
            state: initial.state
        )
        let staleTransport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.noContentAndReplaceState(newer)]
        )
        let staleCoordinator = makeCoordinator(
            state: state,
            transport: staleTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        do {
            try await staleCoordinator.revokeAndForget(boundTo: baseURL)
            Issue.record("Expected stale CAS failure")
        } catch {
            #expect(error as? DurableAuthError == .concurrentStateChange)
        }
        #expect(state.loadEnvelope() == newer)
    }

    @Test("revoke retry retires only an exactly and definitively rejected refreshed lease")
    func revokeSecondUnauthorizedIsStrictAndExact() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 230, refreshMarker: 231)
        let initial = envelope(active: active, revision: 160)
        let strictState = TestDurableAuthStateStore(initial: initial)
        let strictTransport = TestDurableAuthTransport(
            stateStore: strictState,
            plans: [
                .response(
                    statusCode: 401,
                    headers: trustedUnauthorizedHeaders,
                    body: trustedUnauthorizedBody
                ),
                .session(
                    issuedAt: instant.addingTimeInterval(60),
                    statusCode: 200,
                    replayed: false
                ),
                .response(
                    statusCode: 401,
                    headers: trustedUnauthorizedHeaders,
                    body: trustedUnauthorizedBody
                ),
            ]
        )
        let strictCoordinator = makeCoordinator(
            state: strictState,
            transport: strictTransport,
            generator: TestDurableCredentialGenerator(markers: [232, 233]),
            now: { instant.addingTimeInterval(60) }
        )
        do {
            try await strictCoordinator.revokeAndForget(boundTo: baseURL)
            Issue.record("A twice-rejected revoke must require reauthentication")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case .reauthenticationRequired? = strictState.loadEnvelope()?.state else {
            Issue.record("The exact refreshed revoke lease was not retired")
            return
        }

        let arbitraryState = TestDurableAuthStateStore(initial: initial)
        let arbitraryTransport = TestDurableAuthTransport(
            stateStore: arbitraryState,
            plans: [
                .response(
                    statusCode: 401,
                    headers: trustedUnauthorizedHeaders,
                    body: trustedUnauthorizedBody
                ),
                .session(
                    issuedAt: instant.addingTimeInterval(60),
                    statusCode: 200,
                    replayed: false
                ),
                .raw(statusCode: 401, body: trustedUnauthorizedBody),
            ]
        )
        let arbitraryCoordinator = makeCoordinator(
            state: arbitraryState,
            transport: arbitraryTransport,
            generator: TestDurableCredentialGenerator(markers: [234, 235]),
            now: { instant.addingTimeInterval(60) }
        )
        do {
            try await arbitraryCoordinator.revokeAndForget(boundTo: baseURL)
            Issue.record("An arbitrary second revoke 401 must fail closed")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        guard case .active? = arbitraryState.loadEnvelope()?.state else {
            Issue.record("An arbitrary revoke 401 destroyed the refreshed lease")
            return
        }
    }

    @Test("confirmed local-only destruction is separate and leaves a no-secret tombstone")
    func confirmedLocalOnlyForget() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 85, refreshMarker: 86)
        let state = TestDurableAuthStateStore(initial: envelope(active: active, revision: 9))
        let transport = TestDurableAuthTransport(stateStore: state, plans: [])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        try await coordinator.confirmLocalOnlyForget()
        let tombstone = try #require(state.loadEnvelope())
        guard case let .reauthenticationRequired(value) = tombstone.state else {
            Issue.record("Expected durable no-fallback tombstone")
            return
        }
        #expect(value.reason == .explicitlyDisconnected)
        #expect(value.previousSessionID == active.session.id)
        let encoded = try JSONEncoder().encode(tombstone)
        #expect(encoded.range(of: Data(active.credentials.accessToken.utf8)) == nil)
        #expect(encoded.range(of: Data(active.credentials.refreshToken.utf8)) == nil)
        #expect(await transport.records().isEmpty)
    }

    @Test("Keychain envelope rejects stale CAS and surfaces future schemas deterministically")
    func keychainEnvelopeCASAndFutureSchema() async throws {
        let keychain = TestDurableKeychainAccess()
        let store = KeychainDurableAuthStateStore(
            service: "synthetic.auth.service",
            account: "synthetic.auth.account",
            keychain: keychain,
            interprocessLockURL: nil
        )
        let future = DurableAuthEnvelope(
            revision: 17,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: UUID(uuidString: "99999999-9999-4999-8999-999999999999")!,
            state: .legacy(.init(bearerToken: "synthetic-future-bootstrap")),
            schemaVersion: 99
        )
        let data = try JSONEncoder().encode(future)
        keychain.save(
            data,
            service: "synthetic.auth.service",
            account: "synthetic.auth.account"
        )
        let firstOptional = try store.loadEnvelope()
        let secondOptional = try store.loadEnvelope()
        let first = try #require(firstOptional)
        let second = try #require(secondOptional)
        #expect(first == second)
        guard case let .incompatible(incompatible) = first.state else {
            Issue.record("Expected future schema quarantine")
            return
        }
        #expect(incompatible.storedSchemaVersion == 99)

        let wrongExpected = DurableAuthEnvelope(
            revision: first.revision,
            origin: first.origin,
            clientInstanceID: first.clientInstanceID,
            state: .reauthenticationRequired(.init(
                clientInstanceID: first.clientInstanceID,
                previousSessionID: nil,
                reason: .expired,
                detectedAt: instant
            ))
        )
        #expect(try !store.compareAndSwap(expected: wrongExpected, replacement: nil))
        #expect(try store.loadEnvelope() == first)
        #expect(try store.compareAndSwap(expected: first, replacement: nil))
        #expect(try store.loadEnvelope() == nil)
    }

    @Test("Keychain CAS verifies save and deletion readback before reporting success")
    func keychainCASReadbackFailures() throws {
        let keychain = TestDurableKeychainAccess()
        let store = KeychainDurableAuthStateStore(
            service: "synthetic.readback.service",
            account: "synthetic.readback.account",
            keychain: keychain,
            interprocessLockURL: nil
        )
        let envelope = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: UUID(uuidString: "cccccccc-cccc-4ccc-8ccc-cccccccccccc")!,
            state: .legacy(.init(bearerToken: "synthetic-readback-bootstrap"))
        )

        keychain.corruptReadbackAfterNextSave()
        do {
            _ = try store.compareAndSwap(expected: nil, replacement: envelope)
            Issue.record("A mismatched save readback must not report success")
        } catch {
            #expect(error as? DurableAuthStateStoreError == .writeVerificationFailed)
        }
        #expect(try store.loadEnvelope() == envelope)

        keychain.retainValueOnNextDelete()
        do {
            _ = try store.compareAndSwap(expected: envelope, replacement: nil)
            Issue.record("A mismatched deletion readback must not report success")
        } catch {
            #expect(error as? DurableAuthStateStoreError == .writeVerificationFailed)
        }
        #expect(try store.loadEnvelope() == envelope)
        #expect(try store.compareAndSwap(expected: envelope, replacement: nil))
        #expect(try store.loadEnvelope() == nil)
    }

    @Test("verified envelope retries failed legacy-copy cleanup before using authority")
    func legacyDuplicateDeletionFailureIsRetryable() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 87, refreshMarker: 88)
        let initial = envelope(active: active, revision: 11)
        let state = TestDurableAuthStateStore(initial: initial)
        let legacy = FailOnceBearerTokenStore(
            credential: OriginBoundBearerCredential(
                token: "synthetic-residual-copy",
                origin: baseURL.credentialOriginIdentifier
            )
        )
        let transport = TestDurableAuthTransport(stateStore: state, plans: [])
        let coordinator = DurableAuthCoordinator(
            stateStore: state,
            legacyStore: legacy,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )

        do {
            _ = try await coordinator.authorization(boundTo: baseURL)
            Issue.record("Authority must remain unavailable until duplicate cleanup succeeds")
        } catch {
            #expect(error as? DurableAuthError == .localStateUnavailable)
        }
        #expect(state.loadEnvelope() == initial)
        #expect(legacy.deleteAttempts == 1)
        #expect(legacy.savedCredential != nil)

        let authorization = try await coordinator.authorization(boundTo: baseURL)
        #expect(authorization.bearerToken == active.credentials.accessToken)
        #expect(legacy.deleteAttempts == 2)
        #expect(legacy.savedCredential == nil)
        #expect(await transport.records().isEmpty)
    }

    @Test("schema-v1 bytes are canonical and malformed CAS is bound to exact raw identity")
    func keychainCanonicalBytesAndRawIdentity() throws {
        let service = "synthetic.canonical.service"
        let account = "synthetic.canonical.account"
        let keychain = TestDurableKeychainAccess()
        let store = KeychainDurableAuthStateStore(
            service: service,
            account: account,
            keychain: keychain,
            interprocessLockURL: nil
        )
        let canonicalEnvelope = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: UUID(uuidString: "10101010-1010-4010-8010-101010101010")!,
            state: .legacy(.init(bearerToken: "synthetic-canonical-bootstrap"))
        )
        #expect(try store.compareAndSwap(expected: nil, replacement: canonicalEnvelope))
        let canonical = try #require(keychain.storedData(service: service, account: account))

        var unknownObject = try #require(
            try JSONSerialization.jsonObject(with: canonical) as? [String: Any]
        )
        unknownObject["unexpected"] = "first"
        let firstRaw = try JSONSerialization.data(
            withJSONObject: unknownObject,
            options: [.sortedKeys]
        )
        keychain.save(firstRaw, service: service, account: account)
        let firstQuarantine = try #require(try store.loadEnvelope())
        guard case let .incompatible(firstIncompatible) = firstQuarantine.state else {
            Issue.record("Unknown schema-v1 keys must be quarantined")
            return
        }
        #expect(firstIncompatible.reasonCode == "stored_state_noncanonical")
        #expect(firstIncompatible.storedStateSHA256?.count == 64)

        unknownObject["unexpected"] = "second"
        let secondRaw = try JSONSerialization.data(
            withJSONObject: unknownObject,
            options: [.sortedKeys]
        )
        keychain.save(secondRaw, service: service, account: account)
        let secondQuarantine = try #require(try store.loadEnvelope())
        #expect(secondQuarantine != firstQuarantine)
        #expect(try !store.compareAndSwap(expected: firstQuarantine, replacement: nil))
        #expect(try store.compareAndSwap(expected: secondQuarantine, replacement: nil))

        keychain.save(Data([0x20]) + canonical, service: service, account: account)
        guard case let .incompatible(noncanonical)? = try store.loadEnvelope()?.state else {
            Issue.record("Whitespace aliases must not decode as authoritative schema-v1 state")
            return
        }
        #expect(noncanonical.reasonCode == "stored_state_noncanonical")
        let noncanonicalEnvelope = try #require(try store.loadEnvelope())
        #expect(try store.compareAndSwap(expected: noncanonicalEnvelope, replacement: nil))

        let invalidActive = makeActive(
            issuedAt: instant,
            accessMarker: 111,
            refreshMarker: 111
        )
        let invalidRecovery = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: invalidActive.session.clientInstanceID,
            state: .incompatible(.init(
                reasonCode: "synthetic_invalid_recovery",
                storedSchemaVersion: DurableAuthEnvelope.currentSchemaVersion,
                detectedAt: instant,
                recovery: .active(invalidActive)
            ))
        )
        do {
            _ = try store.compareAndSwap(expected: nil, replacement: invalidRecovery)
            Issue.record("Invalid incompatible recovery material must be rejected")
        } catch {
            #expect(error as? DurableAuthStateStoreError == .invalidStoredState)
        }
        #expect(try store.loadEnvelope() == nil)

        let hybridEnrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: 112)
        let hybridCredentials = DurableAuthCredentialPair(
            accessToken: syntheticCredential(prefix: "dw_da1_", marker: 113),
            refreshToken: syntheticCredential(prefix: "dw_dr1_", marker: 114)
        )
        let hybridSessionID = UUID(uuidString: "55555555-4444-4333-8222-111111111111")!
        let hybridPending = EnrollmentPendingAuthState(
            enrollmentID: UUID(uuidString: "11111111-2222-4333-8444-555555555555")!,
            enrollmentToken: hybridEnrollmentToken,
            enrollmentExpiresAt: instant.addingTimeInterval(600),
            proposedSessionID: hybridSessionID,
            proposedCredentials: hybridCredentials,
            descriptor: descriptor,
            preparedAt: instant,
            consumeRequest: try! DurableAuthJournaledRequest.make(
                kind: .consumeEnrollment,
                baseURL: baseURL,
                bearer: hybridEnrollmentToken,
                body: canonicalBody(ConsumeEnrollmentRequest(
                    sessionID: hybridSessionID,
                    accessToken: hybridCredentials.accessToken,
                    refreshToken: hybridCredentials.refreshToken
                ))
            ),
            durableWasPreviouslyActivated: false
        )
        let identitylessHybrid = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: nil,
            state: .enrollmentPending(hybridPending)
        )
        #expect(throws: DurableAuthStateStoreError.self) {
            _ = try store.compareAndSwap(expected: nil, replacement: identitylessHybrid)
        }
        #expect(try store.loadEnvelope() == nil)

        // Records written by an older client before full request journaling
        // cannot acquire authority under a new caller-supplied URL.
        let oldPendingEnvelope = enrollmentEnvelope(
            makeEnrollmentPending(
                enrollmentMarker: 115,
                accessMarker: 116,
                refreshMarker: 117,
                sessionID: UUID(uuidString: "56565656-5656-4656-8656-565656565656")!
            ),
            revision: 3
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        var oldObject = try #require(
            try JSONSerialization.jsonObject(with: encoder.encode(oldPendingEnvelope))
                as? [String: Any]
        )
        var oldState = try #require(oldObject["state"] as? [String: Any])
        var oldPayload = try #require(oldState["payload"] as? [String: Any])
        oldPayload.removeValue(forKey: "consume_request")
        oldState["payload"] = oldPayload
        oldObject["state"] = oldState
        let oldRaw = try JSONSerialization.data(
            withJSONObject: oldObject,
            options: [.sortedKeys]
        )
        keychain.save(oldRaw, service: service, account: account)
        let oldQuarantine = try #require(try store.loadEnvelope())
        guard case let .incompatible(oldIncompatible) = oldQuarantine.state else {
            Issue.record("An unbound old pending record must fail closed")
            return
        }
        #expect(oldIncompatible.reasonCode == "stored_state_invalid")
        #expect(oldIncompatible.storedStateSHA256?.count == 64)
        #expect(oldIncompatible.recovery == nil)
        #expect(try store.compareAndSwap(expected: oldQuarantine, replacement: nil))
    }

    @Test("single-flight keys include the exact envelope and tuple, not only revision")
    func singleFlightRejectsSameRevisionAliasing() async throws {
        let clientID = UUID(uuidString: "12121212-1212-4212-8212-121212121212")!
        let firstPending = makeEnrollmentPending(
            enrollmentMarker: 112,
            accessMarker: 113,
            refreshMarker: 114,
            sessionID: UUID(uuidString: "13131313-1313-4313-8313-131313131313")!
        )
        let secondPending = makeEnrollmentPending(
            enrollmentMarker: 115,
            accessMarker: 116,
            refreshMarker: 117,
            sessionID: UUID(uuidString: "14141414-1414-4414-8414-141414141414")!
        )
        let firstEnvelope = enrollmentEnvelope(
            firstPending,
            revision: 19,
            clientInstanceID: clientID
        )
        let secondEnvelope = enrollmentEnvelope(
            secondPending,
            revision: 19,
            clientInstanceID: clientID
        )
        let state = TestDurableAuthStateStore(initial: firstEnvelope)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [
                .delayedFailure(.transport(.timedOut), nanoseconds: 50_000_000),
                .failure(.transport(.timedOut)),
            ]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        let first = Task { () -> DurableAuthError? in
            do {
                try await coordinator.resumePendingWork(boundTo: baseURL)
                return nil
            } catch {
                return error as? DurableAuthError
            }
        }
        while await transport.records().isEmpty { await Task.yield() }
        state.forceReplace(secondEnvelope)
        do {
            try await coordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Expected the synthetic second transport failure")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        #expect(await first.value == .transport(.timedOut))
        let enrollmentRecords = await transport.records()
        #expect(enrollmentRecords.count == 2)
        #expect(enrollmentRecords[0].authorization != enrollmentRecords[1].authorization)
        #expect(enrollmentRecords[0].body != enrollmentRecords[1].body)

        let firstActive = makeActive(issuedAt: instant, accessMarker: 118, refreshMarker: 119)
        let firstRefresh = makeRefreshPending(
            previous: firstActive,
            accessMarker: 120,
            refreshMarker: 121
        )
        let secondRefresh = makeRefreshPending(
            previous: firstActive,
            accessMarker: 122,
            refreshMarker: 123
        )
        let firstRefreshEnvelope = DurableAuthEnvelope(
            revision: 27,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: firstActive.session.clientInstanceID,
            state: .refreshPending(firstRefresh)
        )
        let secondRefreshEnvelope = DurableAuthEnvelope(
            revision: 27,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: firstActive.session.clientInstanceID,
            state: .refreshPending(secondRefresh)
        )
        let refreshState = TestDurableAuthStateStore(initial: firstRefreshEnvelope)
        let refreshTransport = TestDurableAuthTransport(
            stateStore: refreshState,
            plans: [
                .delayedFailure(.transport(.timedOut), nanoseconds: 50_000_000),
                .failure(.transport(.timedOut)),
            ]
        )
        let refreshCoordinator = makeCoordinator(
            state: refreshState,
            transport: refreshTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        let firstRefreshTask = Task { () -> DurableAuthError? in
            do {
                try await refreshCoordinator.resumePendingWork(boundTo: baseURL)
                return nil
            } catch {
                return error as? DurableAuthError
            }
        }
        while await refreshTransport.records().isEmpty { await Task.yield() }
        refreshState.forceReplace(secondRefreshEnvelope)
        do {
            try await refreshCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Expected the synthetic second refresh failure")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        #expect(await firstRefreshTask.value == .transport(.timedOut))
        let refreshRecords = await refreshTransport.records()
        #expect(refreshRecords.count == 2)
        #expect(refreshRecords[0].body != refreshRecords[1].body)
    }

    @Test("cross-origin replacement requires an explicit local-only tombstone")
    func crossOriginReplacementRequiresConfirmedTombstone() async throws {
        let otherURL = try DayWeaveAPIBaseURL("https://other.example.com/api")
        let clientID = UUID(uuidString: "15151515-1515-4515-8515-151515151515")!
        let pending = makeEnrollmentPending(
            enrollmentMarker: 124,
            accessMarker: 125,
            refreshMarker: 126,
            sessionID: UUID(uuidString: "16161616-1616-4616-8616-161616161616")!
        )
        let pendingEnvelope = enrollmentEnvelope(
            pending,
            revision: 31,
            clientInstanceID: clientID
        )
        let state = TestDurableAuthStateStore(initial: pendingEnvelope)
        let transport = TestDurableAuthTransport(stateStore: state, plans: [])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        let replacementCode = syntheticCredential(prefix: "dw_en1_", marker: 127)

        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.enroll(
                boundTo: otherURL,
                descriptor: descriptor,
                bootstrapToken: "synthetic-other-bootstrap"
            )
        }
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.consumeOneTimeEnrollmentCode(
                replacementCode,
                boundTo: otherURL,
                descriptor: descriptor
            )
        }
        #expect(!coordinator.presentation(boundTo: otherURL).canReenroll)
        #expect(!coordinator.presentation(boundTo: otherURL).canConsumeEnrollmentCode)

        let active = makeActive(issuedAt: instant, accessMarker: 128, refreshMarker: 129)
        let incompatibleEnvelope = DurableAuthEnvelope(
            revision: 32,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: active.session.clientInstanceID,
            state: .incompatible(.init(
                reasonCode: "synthetic_ambiguous_commit",
                storedSchemaVersion: DurableAuthEnvelope.currentSchemaVersion,
                detectedAt: instant,
                recovery: .active(active)
            ))
        )
        state.forceReplace(incompatibleEnvelope)
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.consumeOneTimeEnrollmentCode(
                replacementCode,
                boundTo: otherURL,
                descriptor: descriptor
            )
        }

        let rejectedEnvelope = DurableAuthEnvelope(
            revision: 33,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: active.session.clientInstanceID,
            state: .reauthenticationRequired(.init(
                clientInstanceID: active.session.clientInstanceID,
                previousSessionID: active.session.id,
                reason: .rejected,
                detectedAt: instant
            ))
        )
        state.forceReplace(rejectedEnvelope)
        await expectLiveSessionReplacementRejection {
            _ = try await coordinator.enroll(
                boundTo: otherURL,
                descriptor: descriptor,
                bootstrapToken: "synthetic-other-bootstrap"
            )
        }

        state.forceReplace(incompatibleEnvelope)
        try await coordinator.confirmLocalOnlyForget()
        let tombstone = try #require(state.loadEnvelope())
        guard case let .reauthenticationRequired(tombstoneState) = tombstone.state else {
            Issue.record("Expected an explicit local-only tombstone")
            return
        }
        #expect(tombstoneState.reason == .explicitlyDisconnected)
        #expect(coordinator.presentation(boundTo: otherURL).canReenroll)
        #expect(coordinator.presentation(boundTo: otherURL).canConsumeEnrollmentCode)

        let directTransport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.failure(.transport(.timedOut))]
        )
        let directCoordinator = makeCoordinator(
            state: state,
            transport: directTransport,
            generator: TestDurableCredentialGenerator(
                markers: [130, 131],
                uuids: [UUID(uuidString: "17171717-1717-4717-8717-171717171717")!]
            ),
            now: { instant }
        )
        do {
            _ = try await directCoordinator.consumeOneTimeEnrollmentCode(
                replacementCode,
                boundTo: otherURL,
                descriptor: descriptor
            )
            Issue.record("Expected the synthetic direct enrollment failure")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        #expect(state.loadEnvelope()?.origin == otherURL.credentialOriginIdentifier)
        guard case .enrollmentPending? = state.loadEnvelope()?.state else {
            Issue.record("Confirmed local-only removal should authorize direct cross-origin setup")
            return
        }

        let bootstrapState = TestDurableAuthStateStore(initial: envelope(active: active, revision: 40))
        let bootstrapInitialTransport = TestDurableAuthTransport(
            stateStore: bootstrapState,
            plans: []
        )
        let bootstrapInitialCoordinator = makeCoordinator(
            state: bootstrapState,
            transport: bootstrapInitialTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        try await bootstrapInitialCoordinator.confirmLocalOnlyForget()
        let bootstrapTransport = TestDurableAuthTransport(
            stateStore: bootstrapState,
            plans: [
                .enrollment(
                    id: UUID(uuidString: "18181818-1818-4818-8818-181818181818")!,
                    code: syntheticCredential(prefix: "dw_en1_", marker: 132),
                    expiresAt: instant.addingTimeInterval(600)
                ),
                .failure(.transport(.timedOut)),
            ]
        )
        let bootstrapCoordinator = makeCoordinator(
            state: bootstrapState,
            transport: bootstrapTransport,
            generator: TestDurableCredentialGenerator(
                markers: [132, 133, 134],
                uuids: [
                    UUID(uuidString: "19191919-1919-4919-8919-191919191919")!,
                    UUID(uuidString: "18181818-1818-4818-8818-181818181818")!,
                    UUID(uuidString: "20202020-2020-4020-8020-202020202020")!,
                ]
            ),
            now: { instant }
        )
        do {
            _ = try await bootstrapCoordinator.enroll(
                boundTo: otherURL,
                descriptor: descriptor,
                bootstrapToken: "synthetic-other-bootstrap"
            )
            Issue.record("Expected the synthetic bootstrap consume failure")
        } catch {
            #expect(error as? DurableAuthError == .transport(.timedOut))
        }
        #expect(bootstrapState.loadEnvelope()?.origin == otherURL.credentialOriginIdentifier)
        guard case .enrollmentPending? = bootstrapState.loadEnvelope()?.state else {
            Issue.record("Confirmed local-only removal should authorize bootstrap cross-origin setup")
            return
        }
    }

    @Test("retryable auth statuses preserve exact journals; only trusted deterministic errors quarantine")
    func retryableAndDeterministicFailureClassification() async throws {
        for statusCode in [408, 425, 429, 500, 502, 503] {
            let pending = makeEnrollmentPending(
                enrollmentMarker: UInt8(135 + statusCode % 10),
                accessMarker: UInt8(145 + statusCode % 10),
                refreshMarker: UInt8(155 + statusCode % 10),
                sessionID: UUID()
            )
            let initial = enrollmentEnvelope(pending, revision: UInt64(statusCode))
            let state = TestDurableAuthStateStore(initial: initial)
            let transport = TestDurableAuthTransport(
                stateStore: state,
                plans: [.raw(statusCode: statusCode, body: Data())]
            )
            let coordinator = makeCoordinator(
                state: state,
                transport: transport,
                generator: TestDurableCredentialGenerator(),
                now: { instant }
            )
            do {
                try await coordinator.resumePendingWork(boundTo: baseURL)
                Issue.record("Expected retryable HTTP \(statusCode)")
            } catch {
                #expect(error as? DurableAuthError == .retryableServer(statusCode: statusCode))
            }
            #expect(state.loadEnvelope() == initial)
        }

        for statusCode in [408, 425, 429, 500, 503] {
            let clientID = UUID()
            let enrollmentID = UUID()
            let sessionID = UUID()
            let initial = DurableAuthEnvelope(
                revision: UInt64(statusCode),
                origin: baseURL.credentialOriginIdentifier,
                clientInstanceID: clientID,
                state: .legacy(.init(bearerToken: "synthetic-retryable-create-bootstrap"))
            )
            let state = TestDurableAuthStateStore(initial: initial)
            let transport = TestDurableAuthTransport(
                stateStore: state,
                plans: [.raw(statusCode: statusCode, body: Data())]
            )
            let coordinator = makeCoordinator(
                state: state,
                transport: transport,
                generator: TestDurableCredentialGenerator(
                    markers: [240, 241, 242],
                    uuids: [enrollmentID, sessionID]
                ),
                now: { instant }
            )
            do {
                _ = try await coordinator.enroll(boundTo: baseURL, descriptor: descriptor)
                Issue.record("Expected retryable creation HTTP \(statusCode)")
            } catch {
                #expect(error as? DurableAuthError == .retryableServer(statusCode: statusCode))
            }
            let retained = try #require(state.loadEnvelope())
            guard case .enrollmentCreationPending = retained.state else {
                Issue.record("Retryable creation status must retain its exact journal")
                continue
            }
            #expect(await transport.records().first?.stateAtSend == retained)
        }

        let active = makeActive(issuedAt: instant, accessMarker: 166, refreshMarker: 167)
        let refreshPending = makeRefreshPending(
            previous: active,
            accessMarker: 168,
            refreshMarker: 169
        )
        let refreshInitial = DurableAuthEnvelope(
            revision: 55,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: active.session.clientInstanceID,
            state: .refreshPending(refreshPending)
        )
        let refreshState = TestDurableAuthStateStore(initial: refreshInitial)
        let refreshTransport = TestDurableAuthTransport(
            stateStore: refreshState,
            plans: [.raw(statusCode: 503, body: Data())]
        )
        let refreshCoordinator = makeCoordinator(
            state: refreshState,
            transport: refreshTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        do {
            try await refreshCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Expected retryable refresh response")
        } catch {
            #expect(error as? DurableAuthError == .retryableServer(statusCode: 503))
        }
        #expect(refreshState.loadEnvelope() == refreshInitial)

        let invalidPending = makeEnrollmentPending(
            enrollmentMarker: 170,
            accessMarker: 171,
            refreshMarker: 172,
            sessionID: UUID(uuidString: "21212121-2121-4121-8121-212121212121")!
        )
        let invalidInitial = enrollmentEnvelope(invalidPending, revision: 60)
        let invalidState = TestDurableAuthStateStore(initial: invalidInitial)
        let invalidTransport = TestDurableAuthTransport(
            stateStore: invalidState,
            plans: [.raw(statusCode: 422, body: Data())]
        )
        let invalidCoordinator = makeCoordinator(
            state: invalidState,
            transport: invalidTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        do {
            try await invalidCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Malformed deterministic errors must fail closed")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        #expect(invalidState.loadEnvelope() == invalidInitial)

        let strictHeaders = [
            "Cache-Control": "no-store, max-age=0",
            "Pragma": "no-cache",
            "Content-Type": "application/json",
        ]
        let strictBody = Data(
            #"{"error":{"code":"validation_failed","message":"Credential request is invalid"}}"#.utf8
        )
        let strictState = TestDurableAuthStateStore(initial: invalidInitial)
        let strictTransport = TestDurableAuthTransport(
            stateStore: strictState,
            plans: [.response(statusCode: 422, headers: strictHeaders, body: strictBody)]
        )
        let strictCoordinator = makeCoordinator(
            state: strictState,
            transport: strictTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        do {
            try await strictCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Trusted deterministic rejection should quarantine")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        guard case let .incompatible(strictQuarantine)? = strictState.loadEnvelope()?.state else {
            Issue.record("Expected strict deterministic quarantine")
            return
        }
        guard case .enrollment? = strictQuarantine.recovery else {
            Issue.record("Expected exact enrollment recovery material")
            return
        }
    }

    @Test("only a strict Bearer 401 can destroy an exact auth journal")
    func arbitraryUnauthorizedNeverDestroysJournal() async throws {
        let pending = makeEnrollmentPending(
            enrollmentMarker: 173,
            accessMarker: 174,
            refreshMarker: 175,
            sessionID: UUID(uuidString: "22222222-3333-4222-8222-333333333333")!
        )
        let initial = enrollmentEnvelope(pending, revision: 61)
        let arbitraryState = TestDurableAuthStateStore(initial: initial)
        let arbitraryTransport = TestDurableAuthTransport(
            stateStore: arbitraryState,
            plans: [.raw(statusCode: 401, body: trustedUnauthorizedBody)]
        )
        let arbitraryCoordinator = makeCoordinator(
            state: arbitraryState,
            transport: arbitraryTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        do {
            try await arbitraryCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("An untrusted 401 must fail closed")
        } catch {
            #expect(error as? DurableAuthError == .invalidResponse)
        }
        #expect(arbitraryState.loadEnvelope() == initial)

        let trustedState = TestDurableAuthStateStore(initial: initial)
        let trustedTransport = TestDurableAuthTransport(
            stateStore: trustedState,
            plans: [.response(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: trustedUnauthorizedBody
            )]
        )
        let trustedCoordinator = makeCoordinator(
            state: trustedState,
            transport: trustedTransport,
            generator: TestDurableCredentialGenerator(),
            now: { instant }
        )
        do {
            try await trustedCoordinator.resumePendingWork(boundTo: baseURL)
            Issue.record("Expected definitive reauthentication")
        } catch {
            #expect(error as? DurableAuthError == .reauthenticationRequired)
        }
        guard case .reauthenticationRequired? = trustedState.loadEnvelope()?.state else {
            Issue.record("Trusted 401 should retire the unusable journal")
            return
        }
    }

    @Test("strict auth headers, media type, error shape, and challenge are exact")
    func strictAuthResponseContract() throws {
        let validHeaders = [
            "cache-control": "no-store, max-age=0",
            "pragma": "no-cache",
            "content-type": "application/json; charset=utf-8",
            "www-authenticate": "Bearer realm=\"dayweave\"",
            "x-request-id": "synthetic-request-1",
        ]
        #expect(DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
            statusCode: 401,
            headers: validHeaders,
            body: trustedUnauthorizedBody
        ))
        try DayWeaveAuthResponseContract.validateNoStore(
            headers: validHeaders,
            requiresJSON: true
        )

        var extraDirective = validHeaders
        extraDirective["cache-control"] = "no-store, max-age=0, private"
        #expect(throws: DurableAuthError.self) {
            try DayWeaveAuthResponseContract.validateNoStore(
                headers: extraDirective,
                requiresJSON: true
            )
        }
        var jsonp = validHeaders
        jsonp["content-type"] = "application/jsonp"
        #expect(throws: DurableAuthError.self) {
            try DayWeaveAuthResponseContract.validateNoStore(
                headers: jsonp,
                requiresJSON: true
            )
        }
        for contentType in [
            "application/json; profile=synthetic",
            "application/json; charset",
            "application/json; charset=",
            "application/json; =utf-8",
            "application/json; charset=utf-8; charset=utf-8",
            "application/json; charset=utf-8; profile=synthetic",
            "application/json; charset=\"utf-8\"",
            "application/json-patch+json",
        ] {
            var malformed = validHeaders
            malformed["content-type"] = contentType
            #expect(throws: DurableAuthError.self) {
                try DayWeaveAuthResponseContract.validateNoStore(
                    headers: malformed,
                    requiresJSON: true
                )
            }
        }
        var spacedCharset = validHeaders
        spacedCharset["content-type"] = "Application/JSON ; Charset = UTF-8"
        try DayWeaveAuthResponseContract.validateNoStore(
            headers: spacedCharset,
            requiresJSON: true
        )
        var wrongChallenge = validHeaders
        wrongChallenge["www-authenticate"] = "Basic realm=\"dayweave\""
        #expect(!DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
            statusCode: 401,
            headers: wrongChallenge,
            body: trustedUnauthorizedBody
        ))
        let extraErrorKey = Data(
            #"{"error":{"code":"unauthorized","message":"rejected","details":null}}"#.utf8
        )
        #expect(!DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
            statusCode: 401,
            headers: validHeaders,
            body: extraErrorKey
        ))
        let controlMessage = Data(
            "{\"error\":{\"code\":\"unauthorized\",\"message\":\"bad\\nmessage\"}}".utf8
        )
        #expect(!DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
            statusCode: 401,
            headers: validHeaders,
            body: controlMessage
        ))
    }

    @Test("credential material is distinct across enrollment and every rotation generation")
    func credentialMaterialCollisionsFailBeforeAuthorityChanges() async throws {
        let directCode = syntheticCredential(prefix: "dw_en1_", marker: 176)
        let directState = TestDurableAuthStateStore()
        let directTransport = TestDurableAuthTransport(stateStore: directState, plans: [])
        let directCoordinator = makeCoordinator(
            state: directState,
            transport: directTransport,
            generator: TestDurableCredentialGenerator(
                markers: [176, 177],
                uuids: [UUID(uuidString: "23232323-2323-4323-8323-232323232323")!]
            ),
            now: { instant }
        )
        do {
            _ = try await directCoordinator.consumeOneTimeEnrollmentCode(
                directCode,
                boundTo: baseURL,
                descriptor: descriptor
            )
            Issue.record("Enrollment material reused as access material")
        } catch {
            #expect(error as? DurableAuthError == .randomnessUnavailable)
        }
        #expect(directState.loadEnvelope() == nil)
        #expect(await directTransport.records().isEmpty)

        let legacy = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: UUID(uuidString: "24242424-2424-4424-8424-242424242424")!,
            state: .legacy(.init(bearerToken: "synthetic-material-bootstrap"))
        )
        let hybridState = TestDurableAuthStateStore(initial: legacy)
        let issuedCode = syntheticCredential(prefix: "dw_en1_", marker: 178)
        let hybridTransport = TestDurableAuthTransport(
            stateStore: hybridState,
            plans: [.enrollment(
                id: UUID(uuidString: "25252525-2525-4525-8525-252525252525")!,
                code: issuedCode,
                expiresAt: instant.addingTimeInterval(600)
            )]
        )
        let hybridCoordinator = makeCoordinator(
            state: hybridState,
            transport: hybridTransport,
            generator: TestDurableCredentialGenerator(
                markers: [178, 178, 179],
                uuids: [
                    UUID(uuidString: "25252525-2525-4525-8525-252525252525")!,
                    UUID(uuidString: "26262626-2626-4626-8626-262626262626")!,
                ]
            ),
            now: { instant }
        )
        do {
            _ = try await hybridCoordinator.enroll(boundTo: baseURL, descriptor: descriptor)
            Issue.record("Proposed enrollment material reused as access material")
        } catch {
            #expect(error as? DurableAuthError == .randomnessUnavailable)
        }
        #expect(hybridState.loadEnvelope() == legacy)
        #expect(await hybridTransport.records().isEmpty)

        for (index, markers) in [[180, 182], [181, 183], [184, 184]].enumerated() {
            let active = makeActive(issuedAt: instant, accessMarker: 180, refreshMarker: 181)
            let initial = envelope(active: active, revision: UInt64(70 + index))
            let state = TestDurableAuthStateStore(initial: initial)
            let transport = TestDurableAuthTransport(stateStore: state, plans: [])
            let coordinator = makeCoordinator(
                state: state,
                transport: transport,
                generator: TestDurableCredentialGenerator(markers: markers.map(UInt8.init)),
                now: { instant.addingTimeInterval(850) }
            )
            do {
                _ = try await coordinator.authorization(boundTo: baseURL)
                Issue.record("A prior/current/next rotation material collision was accepted")
            } catch {
                #expect(error as? DurableAuthError == .randomnessUnavailable)
            }
            #expect(state.loadEnvelope() == initial)
            #expect(await transport.records().isEmpty)
        }
    }

    @Test("envelope and server-session revision overflow fail before network mutation")
    func revisionOverflowFailsClosed() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 185, refreshMarker: 186)
        let maxEnvelope = DurableAuthEnvelope(
            revision: UInt64.max,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: active.session.clientInstanceID,
            state: .active(active)
        )
        let envelopeState = TestDurableAuthStateStore(initial: maxEnvelope)
        let envelopeTransport = TestDurableAuthTransport(stateStore: envelopeState, plans: [])
        let envelopeCoordinator = makeCoordinator(
            state: envelopeState,
            transport: envelopeTransport,
            generator: TestDurableCredentialGenerator(markers: [187, 188]),
            now: { instant.addingTimeInterval(850) }
        )
        do {
            _ = try await envelopeCoordinator.authorization(boundTo: baseURL)
            Issue.record("Envelope revision overflow must fail closed")
        } catch {
            #expect(error as? DurableAuthStateStoreError == .revisionOverflow)
        }
        #expect(envelopeState.loadEnvelope() == maxEnvelope)
        #expect(await envelopeTransport.records().isEmpty)

        let maxSession = ActiveDurableAuthState(
            session: .init(
                id: active.session.id,
                clientInstanceID: active.session.clientInstanceID,
                clientKind: active.session.clientKind,
                deviceLabel: active.session.deviceLabel,
                scopes: active.session.scopes,
                clientContractVersion: active.session.clientContractVersion,
                clientVersion: active.session.clientVersion,
                clientCapabilities: active.session.clientCapabilities,
                createdAt: active.session.createdAt,
                lastSeenAt: active.session.lastSeenAt,
                credentialIssuedAt: active.session.credentialIssuedAt,
                accessExpiresAt: active.session.accessExpiresAt,
                refreshIdleExpiresAt: active.session.refreshIdleExpiresAt,
                absoluteExpiresAt: active.session.absoluteExpiresAt,
                revision: UInt64.max
            ),
            credentials: active.credentials
        )
        let sessionEnvelope = envelope(active: maxSession, revision: 80)
        let sessionState = TestDurableAuthStateStore(initial: sessionEnvelope)
        let sessionTransport = TestDurableAuthTransport(stateStore: sessionState, plans: [])
        let sessionCoordinator = makeCoordinator(
            state: sessionState,
            transport: sessionTransport,
            generator: TestDurableCredentialGenerator(markers: [189, 190]),
            now: { instant.addingTimeInterval(850) }
        )
        do {
            _ = try await sessionCoordinator.authorization(boundTo: baseURL)
            Issue.record("Session revision overflow must fail closed")
        } catch {
            #expect(error as? DurableAuthStateStoreError == .revisionOverflow)
        }
        #expect(sessionState.loadEnvelope() == sessionEnvelope)
        #expect(await sessionTransport.records().isEmpty)
    }

    @Test("session timestamps bind to the journal, receive time, and replay semantics")
    func sessionTimestampValidationIsReplayAware() async throws {
        let clientID = UUID(uuidString: "27272727-2727-4727-8727-272727272727")!
        let receive = instant.addingTimeInterval(4_000)

        func expectEnrollmentRejected(
            _ session: DurableDeviceSessionMetadata,
            replayed: Bool,
            revision: UInt64
        ) async throws {
            let pending = makeEnrollmentPending(
                enrollmentMarker: 191,
                accessMarker: 192,
                refreshMarker: 193,
                sessionID: session.id,
                preparedAt: instant
            )
            let initial = enrollmentEnvelope(
                pending,
                revision: revision,
                clientInstanceID: clientID
            )
            let state = TestDurableAuthStateStore(initial: initial)
            let transport = TestDurableAuthTransport(
                stateStore: state,
                plans: [.customSession(
                    session,
                    statusCode: replayed ? 200 : 201,
                    replayed: replayed
                )]
            )
            let coordinator = makeCoordinator(
                state: state,
                transport: transport,
                generator: TestDurableCredentialGenerator(),
                now: { receive }
            )
            do {
                try await coordinator.resumePendingWork(boundTo: baseURL)
                Issue.record("Invalid timestamp contract was accepted")
            } catch {
                #expect(error as? DurableAuthError == .invalidResponse)
            }
            guard case let .incompatible(quarantine)? = state.loadEnvelope()?.state else {
                Issue.record("Invalid timestamp response must preserve a recovery quarantine")
                return
            }
            guard case .enrollment? = quarantine.recovery else {
                Issue.record("Enrollment journal was not preserved")
                return
            }
        }

        let templatePending = makeEnrollmentPending(
            enrollmentMarker: 194,
            accessMarker: 195,
            refreshMarker: 196,
            sessionID: UUID(uuidString: "28282828-2828-4828-8828-282828282828")!,
            preparedAt: instant
        )
        let validReplay = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: instant,
            credentialIssuedAt: instant,
            accessExpiresAt: instant.addingTimeInterval(900),
            refreshIdleExpiresAt: receive.addingTimeInterval(600),
            absoluteExpiresAt: receive.addingTimeInterval(1_200)
        )

        let idleExpired = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: instant,
            credentialIssuedAt: instant,
            accessExpiresAt: instant.addingTimeInterval(900),
            refreshIdleExpiresAt: receive.addingTimeInterval(-1),
            absoluteExpiresAt: receive.addingTimeInterval(1_200)
        )
        try await expectEnrollmentRejected(idleExpired, replayed: true, revision: 90)

        let absoluteExpired = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: instant,
            credentialIssuedAt: instant,
            accessExpiresAt: instant.addingTimeInterval(900),
            refreshIdleExpiresAt: receive.addingTimeInterval(-2),
            absoluteExpiresAt: receive.addingTimeInterval(-1)
        )
        try await expectEnrollmentRejected(absoluteExpired, replayed: true, revision: 91)

        let staleNonReplay = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: instant,
            credentialIssuedAt: receive.addingTimeInterval(-60),
            accessExpiresAt: receive.addingTimeInterval(-1),
            refreshIdleExpiresAt: receive.addingTimeInterval(600),
            absoluteExpiresAt: receive.addingTimeInterval(1_200)
        )
        try await expectEnrollmentRejected(staleNonReplay, replayed: false, revision: 92)

        let tooEarlyIssue = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: instant.addingTimeInterval(-301),
            credentialIssuedAt: instant.addingTimeInterval(-301),
            accessExpiresAt: instant.addingTimeInterval(599),
            refreshIdleExpiresAt: receive.addingTimeInterval(600),
            absoluteExpiresAt: receive.addingTimeInterval(1_200)
        )
        try await expectEnrollmentRejected(tooEarlyIssue, replayed: true, revision: 93)

        let futureIssue = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: receive.addingTimeInterval(301),
            credentialIssuedAt: receive.addingTimeInterval(301),
            accessExpiresAt: receive.addingTimeInterval(1_201),
            refreshIdleExpiresAt: receive.addingTimeInterval(1_800),
            absoluteExpiresAt: receive.addingTimeInterval(2_400)
        )
        try await expectEnrollmentRejected(futureIssue, replayed: true, revision: 94)

        let wrongRevision = makeEnrollmentSession(
            pending: templatePending,
            clientInstanceID: clientID,
            createdAt: validReplay.createdAt,
            credentialIssuedAt: validReplay.credentialIssuedAt,
            accessExpiresAt: validReplay.accessExpiresAt,
            refreshIdleExpiresAt: validReplay.refreshIdleExpiresAt,
            absoluteExpiresAt: validReplay.absoluteExpiresAt,
            revision: 2
        )
        try await expectEnrollmentRejected(wrongRevision, replayed: true, revision: 95)

        // Equality is allowed by the server contract during a prompt refresh.
        let active = makeActive(issuedAt: instant, accessMarker: 197, refreshMarker: 198)
        let equalSession = DurableDeviceSessionMetadata(
            id: active.session.id,
            clientInstanceID: active.session.clientInstanceID,
            clientKind: active.session.clientKind,
            deviceLabel: active.session.deviceLabel,
            scopes: active.session.scopes,
            clientContractVersion: active.session.clientContractVersion,
            clientVersion: active.session.clientVersion,
            clientCapabilities: active.session.clientCapabilities,
            createdAt: active.session.createdAt,
            lastSeenAt: active.session.lastSeenAt,
            credentialIssuedAt: active.session.credentialIssuedAt,
            accessExpiresAt: active.session.accessExpiresAt,
            refreshIdleExpiresAt: active.session.refreshIdleExpiresAt,
            absoluteExpiresAt: active.session.absoluteExpiresAt,
            revision: 2
        )
        let equalityState = TestDurableAuthStateStore(
            initial: envelope(active: active, revision: 96)
        )
        let equalityTransport = TestDurableAuthTransport(
            stateStore: equalityState,
            plans: [.customSession(equalSession, statusCode: 200, replayed: false)]
        )
        let equalityCoordinator = makeCoordinator(
            state: equalityState,
            transport: equalityTransport,
            generator: TestDurableCredentialGenerator(markers: [199, 200]),
            now: { instant.addingTimeInterval(60) }
        )
        _ = try await equalityCoordinator.recoverFromUnauthorized(
            rejectedBearer: active.credentials.accessToken,
            boundTo: baseURL
        )
        guard case let .active(equalActive)? = equalityState.loadEnvelope()?.state else {
            Issue.record("Nondecreasing equality response should activate")
            return
        }
        #expect(equalActive.session.revision == 2)
    }

    @Test("a second definitive API 401 retires only the exact refreshed lease")
    @MainActor
    func secondDefinitiveUnauthorizedRetiresExactLease() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 201, refreshMarker: 202)
        let initial = envelope(active: active, revision: 100)
        let state = TestDurableAuthStateStore(initial: initial)
        let nextAccess = syntheticCredential(prefix: "dw_da1_", marker: 203)
        let transport = TestDurableAuthTransport(
            stateStore: state,
            plans: [.session(
                issuedAt: instant.addingTimeInterval(60),
                statusCode: 200,
                replayed: false
            )]
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(markers: [203, 204]),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: active.credentials.accessToken)
        URLProtocolStub.storage.reset(key: nextAccess)
        URLProtocolStub.storage.enqueue(
            key: active.credentials.accessToken,
            .init(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: trustedUnauthorizedBody
            )
        )
        URLProtocolStub.storage.enqueue(
            key: nextAccess,
            .init(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: trustedUnauthorizedBody
            )
        )
        let client = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
        do {
            _ = try await client.listSuggestions()
            Issue.record("A refreshed lease rejected again must require reauthentication")
        } catch {
            #expect(
                error as? DayWeaveAPIError
                    == .durableAuthentication(.reauthenticationRequired)
            )
        }
        guard case let .reauthenticationRequired(retired)? = state.loadEnvelope()?.state else {
            Issue.record("The exact twice-rejected lease was not retired")
            return
        }
        #expect(retired.previousSessionID == active.session.id)
        #expect(await transport.records().count == 1)
        #expect(URLProtocolStub.storage.requests(for: active.credentials.accessToken).count == 1)
        #expect(URLProtocolStub.storage.requests(for: nextAccess).count == 1)

        let staleState = TestDurableAuthStateStore(initial: initial)
        let staleCoordinator = makeCoordinator(
            state: staleState,
            transport: TestDurableAuthTransport(stateStore: staleState, plans: []),
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        let staleAuthorization = try await staleCoordinator.authorization(boundTo: baseURL)
        let newer = makeActive(
            issuedAt: instant.addingTimeInterval(30),
            accessMarker: 205,
            refreshMarker: 206,
            sessionID: UUID(uuidString: "29292929-2929-4929-8929-292929292929")!,
            clientInstanceID: active.session.clientInstanceID
        )
        let newerEnvelope = envelope(active: newer, revision: initial.revision + 1)
        staleState.forceReplace(newerEnvelope)
        do {
            try await staleCoordinator.retireDefinitivelyRejectedAuthorization(
                staleAuthorization,
                boundTo: baseURL
            )
            Issue.record("A stale rejected lease must not retire a replacement session")
        } catch {
            #expect(error as? DurableAuthError == .concurrentStateChange)
        }
        #expect(staleState.loadEnvelope() == newerEnvelope)
    }

    @Test("ordinary API 401 recovery requires the full trusted contract")
    @MainActor
    func ordinaryUnauthorizedContractIsFailClosed() async throws {
        let active = makeActive(issuedAt: instant, accessMarker: 207, refreshMarker: 208)
        let initial = envelope(active: active, revision: 110)
        let state = TestDurableAuthStateStore(initial: initial)
        let transport = TestDurableAuthTransport(stateStore: state, plans: [])
        let coordinator = makeCoordinator(
            state: state,
            transport: transport,
            generator: TestDurableCredentialGenerator(markers: [209, 210]),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: active.credentials.accessToken)
        URLProtocolStub.storage.enqueue(
            key: active.credentials.accessToken,
            .init(statusCode: 401, body: trustedUnauthorizedBody)
        )
        let client = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
        do {
            _ = try await client.listSuggestions()
            Issue.record("Expected ordinary untrusted 401")
        } catch {
            guard case let .server(statusCode, _, _, _)? = error as? DayWeaveAPIError else {
                Issue.record("Expected a non-mutating server error")
                return
            }
            #expect(statusCode == 401)
        }
        #expect(state.loadEnvelope() == initial)
        #expect(await transport.records().isEmpty)

        let replayState = TestDurableAuthStateStore(initial: initial)
        let nextAccess = syntheticCredential(prefix: "dw_da1_", marker: 209)
        let replayTransport = TestDurableAuthTransport(
            stateStore: replayState,
            plans: [.session(
                issuedAt: instant.addingTimeInterval(60),
                statusCode: 200,
                replayed: false
            )]
        )
        let replayCoordinator = makeCoordinator(
            state: replayState,
            transport: replayTransport,
            generator: TestDurableCredentialGenerator(markers: [209, 210]),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: active.credentials.accessToken)
        URLProtocolStub.storage.reset(key: nextAccess)
        URLProtocolStub.storage.enqueue(
            key: active.credentials.accessToken,
            .init(
                statusCode: 401,
                headers: trustedUnauthorizedHeaders,
                body: trustedUnauthorizedBody
            )
        )
        URLProtocolStub.storage.enqueue(
            key: nextAccess,
            .init(statusCode: 401, body: trustedUnauthorizedBody)
        )
        let replayClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: replayCoordinator
        )
        do {
            _ = try await replayClient.listSuggestions()
            Issue.record("Expected untrusted replay 401")
        } catch {
            guard case let .server(statusCode, _, _, _)? = error as? DayWeaveAPIError else {
                Issue.record("Expected a non-mutating replay server error")
                return
            }
            #expect(statusCode == 401)
        }
        guard case let .active(replayedActive)? = replayState.loadEnvelope()?.state else {
            Issue.record("An arbitrary replay 401 must retain the refreshed active state")
            return
        }
        #expect(replayedActive.credentials.accessToken == nextAccess)
    }

    @Test("API configuration and response acceptance are fenced to the exact session binding")
    @MainActor
    func apiBindingFencesSameOriginReplacement() async throws {
        let first = makeActive(issuedAt: instant, accessMarker: 211, refreshMarker: 212)
        let firstEnvelope = envelope(active: first, revision: 120)
        let state = TestDurableAuthStateStore(initial: firstEnvelope)
        let coordinator = makeCoordinator(
            state: state,
            transport: TestDurableAuthTransport(stateStore: state, plans: []),
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: first.credentials.accessToken)
        URLProtocolStub.storage.enqueue(
            key: first.credentials.accessToken,
            .init(
                statusCode: 200,
                body: DayWeaveAPIClientTests.listEnvelope(),
                delay: 0.05
            )
        )
        let firstClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
        let request = Task { () -> DayWeaveAPIError? in
            do {
                _ = try await firstClient.listSuggestions()
                return nil
            } catch {
                return error as? DayWeaveAPIError
            }
        }
        while URLProtocolStub.storage.requests(for: first.credentials.accessToken).isEmpty {
            await Task.yield()
        }

        let second = makeActive(
            issuedAt: instant.addingTimeInterval(30),
            accessMarker: 213,
            refreshMarker: 214,
            sessionID: UUID(uuidString: "30303030-3030-4030-8030-303030303030")!,
            clientInstanceID: first.session.clientInstanceID
        )
        state.forceReplace(envelope(active: second, revision: 121))
        let secondClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
        #expect(firstClient.configurationIdentifier != secondClient.configurationIdentifier)
        #expect(
            await request.value
                == .durableAuthentication(.concurrentStateChange)
        )
        #expect(state.loadEnvelope()?.state == .active(second))
    }

    @Test("secret-bearing state, DTOs, and diagnostics never reflect credential plaintext")
    @MainActor
    func authenticationDiagnosticsAreRedacted() async throws {
        let enrollment = syntheticCredential(prefix: "dw_en1_", marker: 215)
        let access = syntheticCredential(prefix: "dw_da1_", marker: 216)
        let refresh = syntheticCredential(prefix: "dw_dr1_", marker: 217)
        let mcp = syntheticCredential(prefix: "dw_mc1_", marker: 218)
        let bootstrap = "synthetic-creation-bootstrap-canary"
        let pair = DurableAuthCredentialPair(accessToken: access, refreshToken: refresh)
        let pending = makeEnrollmentPending(
            enrollmentMarker: 215,
            accessMarker: 216,
            refreshMarker: 217,
            sessionID: UUID(uuidString: "31313131-3131-4131-8131-313131313131")!
        )
        let active = makeActive(issuedAt: instant, accessMarker: 216, refreshMarker: 217)
        let refreshPending = makeRefreshPending(
            previous: active,
            accessMarker: 219,
            refreshMarker: 220
        )
        let creationClientID = UUID(uuidString: "57575757-5757-4757-8757-575757575757")!
        let creationEnrollmentID = UUID(uuidString: "58585858-5858-4858-8858-585858585858")!
        let creationSessionID = UUID(uuidString: "59595959-5959-4959-8959-595959595959")!
        let creationDTO = CreateEnrollmentRequest(
            id: creationEnrollmentID,
            enrollmentToken: enrollment,
            clientInstanceID: creationClientID,
            clientKind: "macos",
            deviceLabel: descriptor.deviceLabel,
            scopes: descriptor.scopes,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: descriptor.clientVersion,
            clientCapabilities: descriptor.clientCapabilities
        )
        let creationJournal = try DurableAuthJournaledRequest.make(
            kind: .createEnrollment,
            baseURL: baseURL,
            bearer: bootstrap,
            body: canonicalBody(creationDTO)
        )
        let creationPending = EnrollmentCreationPendingAuthState(
            bootstrapToken: bootstrap,
            proposedEnrollmentID: creationEnrollmentID,
            proposedEnrollmentToken: enrollment,
            proposedSessionID: creationSessionID,
            proposedCredentials: pair,
            descriptor: descriptor,
            preparedAt: instant,
            creationRequest: creationJournal,
            durableWasPreviouslyActivated: false
        )
        let creationEnvelope = DurableAuthEnvelope(
            revision: 129,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: creationClientID,
            state: .enrollmentCreationPending(creationPending)
        )
        let wrappers: [Any] = [
            pair,
            LegacyAuthState(bearerToken: "synthetic-legacy-secret-canary"),
            creationDTO,
            creationJournal,
            creationPending,
            creationEnvelope,
            pending,
            active,
            refreshPending,
            IncompatibleAuthRecovery.enrollment(pending),
            IncompatibleAuthState(
                reasonCode: "synthetic",
                storedSchemaVersion: 1,
                detectedAt: instant,
                recovery: .refresh(refreshPending)
            ),
            DurableAuthState.enrollmentPending(pending),
            enrollmentEnvelope(pending, revision: 130),
            DurableAuthorization(
                bearerToken: access,
                bindingIdentifier: "synthetic-binding",
                isDurable: true
            ),
            DeviceEnrollmentResponse(
                id: UUID(uuidString: "32323232-3232-4232-8232-323232323232")!,
                enrollmentToken: enrollment,
                expiresAt: instant.addingTimeInterval(600),
                clientContractVersion: DurableAuthClientDescriptor.contractVersion,
                replayed: false
            ),
            ConsumeEnrollmentRequest(
                sessionID: pending.proposedSessionID,
                accessToken: access,
                refreshToken: refresh
            ),
            RefreshRequest(nextAccessToken: access, nextRefreshToken: refresh),
        ]
        let secrets = [
            enrollment, access, refresh, mcp, bootstrap, "synthetic-legacy-secret-canary",
            refreshPending.nextCredentials.accessToken,
            refreshPending.nextCredentials.refreshToken,
        ]
        for wrapper in wrappers {
            let rendered = diagnosticRendering(wrapper)
            for secret in secrets {
                #expect(!rendered.contains(secret))
            }
        }

        let adversarial = DayWeaveAPIError.server(
            statusCode: 500,
            code: "failure_\(access)",
            message: "Bearer \(refresh) \(enrollment) \(mcp)",
            requestID: access
        )
        let adversarialRendering = diagnosticRendering(adversarial)
        for secret in secrets {
            #expect(!adversarialRendering.contains(secret))
        }

        let apiState = TestDurableAuthStateStore(
            initial: envelope(active: active, revision: 131)
        )
        let apiCoordinator = makeCoordinator(
            state: apiState,
            transport: TestDurableAuthTransport(stateStore: apiState, plans: []),
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: access)
        let responseBody = try JSONSerialization.data(withJSONObject: [
            "error": [
                "code": "failure_\(access)",
                "message": "Bearer \(refresh) \(enrollment) \(mcp)",
            ],
        ])
        URLProtocolStub.storage.enqueue(
            key: access,
            .init(statusCode: 500, headers: ["x-request-id": access], body: responseBody)
        )
        let apiClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: apiCoordinator
        )
        do {
            _ = try await apiClient.listSuggestions()
            Issue.record("Expected adversarial server diagnostic")
        } catch let error as DayWeaveAPIError {
            guard case let .server(_, code, message, requestID) = error else {
                Issue.record("Expected a sanitized server error")
                return
            }
            #expect(code == nil)
            #expect(requestID == nil)
            for value in [message, error.localizedDescription] {
                for secret in secrets {
                    #expect(!(value?.contains(secret) ?? false))
                }
            }
        }
    }

    @Test("settings presentation freezes during auth and invalidates stale URL affordances")
    @MainActor
    func settingsPresentationIsBoundToCapturedURL() async throws {
        let otherURL = try DayWeaveAPIBaseURL("https://other.example.com/gateway")
        let active = makeActive(issuedAt: instant, accessMarker: 221, refreshMarker: 222)
        let activeState = TestDurableAuthStateStore(
            initial: envelope(active: active, revision: 140)
        )
        let activeCoordinator = makeCoordinator(
            state: activeState,
            transport: TestDurableAuthTransport(stateStore: activeState, plans: []),
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        let activeModel = DurableAuthSettingsModel(
            coordinator: activeCoordinator,
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            descriptor: descriptor
        )
        #expect(activeModel.presentation.phase == .active)
        activeModel.reload(boundTo: otherURL)
        #expect(activeModel.presentation.phase == .incompatible)
        #expect(!activeModel.presentation.canReenroll)
        #expect(!activeModel.presentation.canConsumeEnrollmentCode)
        activeModel.reload(boundTo: baseURL)
        #expect(activeModel.presentation.phase == .active)

        let pendingState = TestDurableAuthStateStore()
        let pendingTransport = TestDurableAuthTransport(
            stateStore: pendingState,
            plans: [.delayedFailure(.transport(.timedOut), nanoseconds: 50_000_000)]
        )
        let pendingCoordinator = makeCoordinator(
            state: pendingState,
            transport: pendingTransport,
            generator: TestDurableCredentialGenerator(
                markers: [223, 224],
                uuids: [UUID(uuidString: "33333333-4444-4333-8333-444444444444")!]
            ),
            now: { instant }
        )
        let pendingModel = DurableAuthSettingsModel(
            coordinator: pendingCoordinator,
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            descriptor: descriptor
        )
        let code = syntheticCredential(prefix: "dw_en1_", marker: 225)
        let operation = Task {
            await pendingModel.consumeEnrollmentCode(baseURL: baseURL, code: code)
        }
        while !pendingModel.isBusy { await Task.yield() }
        let frozen = pendingModel.presentation
        pendingModel.reload(boundTo: otherURL)
        #expect(pendingModel.presentation == frozen)
        #expect(!(await operation.value))
        #expect(pendingModel.presentation.phase == .enrollmentPending)
        #expect(!pendingModel.presentation.canReenroll)
        #expect(!pendingModel.presentation.canConsumeEnrollmentCode)
        #expect(pendingState.loadEnvelope()?.origin == baseURL.credentialOriginIdentifier)
    }

    @Test("suggestion proposals cannot cross a same-origin durable session replacement")
    @MainActor
    func suggestionProposalIsFencedToAuthenticationBinding() async throws {
        let first = makeActive(issuedAt: instant, accessMarker: 226, refreshMarker: 227)
        let state = TestDurableAuthStateStore(
            initial: envelope(active: first, revision: 150)
        )
        let coordinator = makeCoordinator(
            state: state,
            transport: TestDurableAuthTransport(stateStore: state, plans: []),
            generator: TestDurableCredentialGenerator(),
            now: { instant.addingTimeInterval(60) }
        )
        URLProtocolStub.storage.reset(key: first.credentials.accessToken)
        URLProtocolStub.storage.enqueue(
            key: first.credentials.accessToken,
            .init(statusCode: 200, body: DayWeaveAPIClientTests.listEnvelope())
        )
        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: TestBearerTokenStore(token: nil),
            authCoordinator: coordinator,
            session: URLProtocolStub.makeSession(),
            now: { self.instant.addingTimeInterval(60) }
        )
        await sync.refresh()
        let proposal = try #require(sync.proposals.first)

        let second = makeActive(
            issuedAt: instant.addingTimeInterval(30),
            accessMarker: 228,
            refreshMarker: 229,
            sessionID: UUID(uuidString: "34343434-3434-4434-8434-343434343434")!,
            clientInstanceID: first.session.clientInstanceID
        )
        state.forceReplace(envelope(active: second, revision: 151))
        URLProtocolStub.storage.reset(key: second.credentials.accessToken)
        await sync.accept(proposal)

        #expect(sync.proposals == [proposal])
        #expect(sync.status.isFailure)
        #expect(sync.activeProposalIDs.isEmpty)
        #expect(URLProtocolStub.storage.requests(for: first.credentials.accessToken).count == 1)
        #expect(URLProtocolStub.storage.requests(for: second.credentials.accessToken).isEmpty)
    }

    @Test("system generator emits exact unique OS-CSPRNG credentials")
    func systemCredentialShape() throws {
        let generator = SystemDurableCredentialGenerator()
        var credentials: Set<String> = []
        for _ in 0..<32 {
            let value = try generator.makeCredential(prefix: "dw_da1_")
            #expect(DurableAuthCoordinator.isCredential(value, prefix: "dw_da1_"))
            credentials.insert(value)
        }
        #expect(credentials.count == 32)
    }

    private func makeCoordinator(
        state: TestDurableAuthStateStore,
        transport: TestDurableAuthTransport,
        generator: TestDurableCredentialGenerator,
        now: @escaping @Sendable () -> Date
    ) -> DurableAuthCoordinator {
        DurableAuthCoordinator(
            stateStore: state,
            legacyStore: TestBearerTokenStore(token: nil),
            transport: transport,
            generator: generator,
            now: now
        )
    }

    private func makeActive(
        issuedAt: Date,
        accessMarker: UInt8,
        refreshMarker: UInt8,
        sessionID: UUID = UUID(uuidString: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!,
        clientInstanceID: UUID = UUID(uuidString: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")!
    ) -> ActiveDurableAuthState {
        let descriptor = descriptor
        let session = DurableDeviceSessionMetadata(
            id: sessionID,
            clientInstanceID: clientInstanceID,
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
        return .init(
            session: session,
            credentials: .init(
                accessToken: syntheticCredential(prefix: "dw_da1_", marker: accessMarker),
                refreshToken: syntheticCredential(prefix: "dw_dr1_", marker: refreshMarker)
            )
        )
    }

    private func makeEnrollmentPending(
        enrollmentMarker: UInt8,
        accessMarker: UInt8,
        refreshMarker: UInt8,
        sessionID: UUID,
        preparedAt: Date? = nil,
        boundTo requestBaseURL: DayWeaveAPIBaseURL? = nil
    ) -> EnrollmentPendingAuthState {
        let enrollmentToken = syntheticCredential(prefix: "dw_en1_", marker: enrollmentMarker)
        let proposedCredentials = DurableAuthCredentialPair(
            accessToken: syntheticCredential(prefix: "dw_da1_", marker: accessMarker),
            refreshToken: syntheticCredential(prefix: "dw_dr1_", marker: refreshMarker)
        )
        let consumeBody = canonicalBody(ConsumeEnrollmentRequest(
            sessionID: sessionID,
            accessToken: proposedCredentials.accessToken,
            refreshToken: proposedCredentials.refreshToken
        ))
        let consumeRequest = try! DurableAuthJournaledRequest.make(
            kind: .consumeEnrollment,
            baseURL: requestBaseURL ?? baseURL,
            bearer: enrollmentToken,
            body: consumeBody
        )
        return .init(
            enrollmentID: nil,
            enrollmentToken: enrollmentToken,
            enrollmentExpiresAt: nil,
            proposedSessionID: sessionID,
            proposedCredentials: proposedCredentials,
            descriptor: descriptor,
            preparedAt: preparedAt ?? instant,
            consumeRequest: consumeRequest,
            durableWasPreviouslyActivated: false
        )
    }

    private func makeRefreshPending(
        previous: ActiveDurableAuthState,
        accessMarker: UInt8,
        refreshMarker: UInt8,
        preparedAt: Date? = nil,
        boundTo requestBaseURL: DayWeaveAPIBaseURL? = nil
    ) -> RefreshPendingAuthState {
        let nextCredentials = DurableAuthCredentialPair(
            accessToken: syntheticCredential(prefix: "dw_da1_", marker: accessMarker),
            refreshToken: syntheticCredential(prefix: "dw_dr1_", marker: refreshMarker)
        )
        let refreshBody = canonicalBody(RefreshRequest(
            nextAccessToken: nextCredentials.accessToken,
            nextRefreshToken: nextCredentials.refreshToken
        ))
        let refreshRequest = try! DurableAuthJournaledRequest.make(
            kind: .refreshSession,
            baseURL: requestBaseURL ?? baseURL,
            bearer: previous.credentials.refreshToken,
            body: refreshBody
        )
        return .init(
            previous: previous,
            nextCredentials: nextCredentials,
            preparedAt: preparedAt ?? instant,
            refreshRequest: refreshRequest
        )
    }

    private func canonicalBody(_ value: some Encodable) -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try! encoder.encode(value)
    }

    private func enrollmentEnvelope(
        _ pending: EnrollmentPendingAuthState,
        revision: UInt64,
        clientInstanceID: UUID? = nil,
        origin: String? = nil
    ) -> DurableAuthEnvelope {
        .init(
            revision: revision,
            origin: origin ?? baseURL.credentialOriginIdentifier,
            clientInstanceID: clientInstanceID,
            state: .enrollmentPending(pending)
        )
    }

    private func makeEnrollmentSession(
        pending: EnrollmentPendingAuthState,
        clientInstanceID: UUID,
        createdAt: Date,
        credentialIssuedAt: Date,
        lastSeenAt: Date? = nil,
        accessExpiresAt: Date? = nil,
        refreshIdleExpiresAt: Date? = nil,
        absoluteExpiresAt: Date? = nil,
        revision: UInt64 = 1
    ) -> DurableDeviceSessionMetadata {
        .init(
            id: pending.proposedSessionID,
            clientInstanceID: clientInstanceID,
            clientKind: "macos",
            deviceLabel: pending.descriptor.deviceLabel,
            scopes: pending.descriptor.scopes,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: pending.descriptor.clientVersion,
            clientCapabilities: pending.descriptor.clientCapabilities,
            createdAt: createdAt,
            lastSeenAt: lastSeenAt ?? credentialIssuedAt,
            credentialIssuedAt: credentialIssuedAt,
            accessExpiresAt: accessExpiresAt
                ?? credentialIssuedAt.addingTimeInterval(DurableAuthCoordinator.accessLifetime),
            refreshIdleExpiresAt: refreshIdleExpiresAt
                ?? credentialIssuedAt.addingTimeInterval(DurableAuthCoordinator.refreshIdleLifetime),
            absoluteExpiresAt: absoluteExpiresAt
                ?? createdAt.addingTimeInterval(DurableAuthCoordinator.absoluteLifetime),
            revision: revision
        )
    }

    private func envelope(active: ActiveDurableAuthState, revision: UInt64) -> DurableAuthEnvelope {
        .init(
            revision: revision,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: active.session.clientInstanceID,
            state: .active(active)
        )
    }

    private func expectLiveSessionReplacementRejection(
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            Issue.record("A live durable session must be revoked before replacement")
        } catch {
            #expect(error as? DurableAuthError == .activeSessionMustBeRevoked)
        }
    }
}

private func syntheticCredential(prefix: String, marker: UInt8) -> String {
    prefix + Data(repeating: marker, count: 32)
        .base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}

private func diagnosticRendering(_ value: Any) -> String {
    var dumped = ""
    dump(value, to: &dumped)
    return [String(describing: value), String(reflecting: value), dumped]
        .joined(separator: "\n")
}

private let trustedUnauthorizedHeaders = [
    "Cache-Control": "no-store, max-age=0",
    "Pragma": "no-cache",
    "Content-Type": "application/json; charset=utf-8",
    "WWW-Authenticate": "Bearer realm=\"dayweave\"",
]

private let trustedUnauthorizedBody = Data(
    #"{"error":{"code":"unauthorized","message":"A valid bearer token is required"}}"#.utf8
)

private final class TestDurableAuthStateStore: DurableAuthStateStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var envelope: DurableAuthEnvelope?

    init(initial: DurableAuthEnvelope? = nil) { envelope = initial }

    func loadEnvelope() -> DurableAuthEnvelope? { lock.withLock { envelope } }

    func compareAndSwap(
        expected: DurableAuthEnvelope?,
        replacement: DurableAuthEnvelope?
    ) -> Bool {
        lock.withLock {
            guard envelope == expected else { return false }
            envelope = replacement
            return true
        }
    }

    func forceReplace(_ replacement: DurableAuthEnvelope?) {
        lock.withLock { envelope = replacement }
    }
}

private struct FailingDurableAuthStateStore: DurableAuthStateStoring {
    func loadEnvelope() throws -> DurableAuthEnvelope? {
        throw DurableAuthStateStoreError.interprocessLockUnavailable
    }

    func compareAndSwap(
        expected: DurableAuthEnvelope?,
        replacement: DurableAuthEnvelope?
    ) throws -> Bool {
        throw DurableAuthStateStoreError.interprocessLockUnavailable
    }
}

private final class TestDurableCredentialGenerator: DurableCredentialGenerating,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var markers: [UInt8]
    private var uuids: [UUID]

    init(markers: [UInt8] = [], uuids: [UUID] = []) {
        self.markers = markers
        self.uuids = uuids
    }

    func makeCredential(prefix: String) throws -> String {
        try lock.withLock {
            guard !markers.isEmpty else { throw DurableAuthError.randomnessUnavailable }
            return syntheticCredential(prefix: prefix, marker: markers.removeFirst())
        }
    }

    func makeUUID() throws -> UUID {
        try lock.withLock {
            guard !uuids.isEmpty else { throw DurableAuthError.randomnessUnavailable }
            return uuids.removeFirst()
        }
    }
}

private final class TestAuthClock: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Date

    init(_ value: Date) { stored = value }
    var value: Date {
        get { lock.withLock { stored } }
        set { lock.withLock { stored = newValue } }
    }
}

private actor TestDurableAuthTransport: DurableAuthHTTPTransport {
    enum Plan: Sendable {
        case enrollment(
            id: UUID,
            code: String,
            expiresAt: Date,
            statusCode: Int = 201,
            replayed: Bool = false
        )
        case enrollmentAndReplaceState(
            id: UUID,
            code: String,
            expiresAt: Date,
            replacement: DurableAuthEnvelope
        )
        case delayedEnrollment(
            id: UUID,
            code: String,
            expiresAt: Date,
            nanoseconds: UInt64
        )
        case session(
            issuedAt: Date,
            statusCode: Int,
            replayed: Bool,
            serverClientInstanceID: UUID? = nil,
            extraTopLevelKey: Bool = false
        )
        case noContent
        case noContentAndReplaceState(DurableAuthEnvelope)
        case raw(statusCode: Int, body: Data)
        case response(statusCode: Int, headers: [String: String], body: Data)
        case delayedFailure(DurableAuthError, nanoseconds: UInt64)
        case customSession(
            DurableDeviceSessionMetadata,
            statusCode: Int,
            replayed: Bool
        )
        case failure(DurableAuthError)
    }

    struct Record: Equatable, Sendable {
        let method: String
        let url: String
        let path: String
        let headers: [String: String]
        let authorization: String?
        let body: Data?
        let stateAtSend: DurableAuthEnvelope?
    }

    private let stateStore: TestDurableAuthStateStore
    private var plans: [Plan]
    private var recorded: [Record] = []

    init(stateStore: TestDurableAuthStateStore, plans: [Plan]) {
        self.stateStore = stateStore
        self.plans = plans
    }

    func records() -> [Record] { recorded }

    func send(_ request: URLRequest) async throws -> DurableAuthHTTPResponse {
        let snapshot = stateStore.loadEnvelope()
        recorded.append(.init(
            method: request.httpMethod ?? "",
            url: request.url?.absoluteString ?? "",
            path: request.url?.path ?? "",
            headers: request.allHTTPHeaderFields ?? [:],
            authorization: request.value(forHTTPHeaderField: "Authorization"),
            body: request.httpBody,
            stateAtSend: snapshot
        ))
        guard !plans.isEmpty else { throw DurableAuthError.transport(.badServerResponse) }
        let plan = plans.removeFirst()
        switch plan {
        case let .enrollment(id, code, expiresAt, statusCode, replayed):
            return response(statusCode: statusCode, object: [
                "id": id.uuidString.lowercased(),
                "enrollment_token": code,
                "expires_at": Self.format(expiresAt),
                "client_contract_version": DurableAuthClientDescriptor.contractVersion,
                "replayed": replayed,
            ])
        case let .enrollmentAndReplaceState(id, code, expiresAt, replacement):
            stateStore.forceReplace(replacement)
            return response(statusCode: 201, object: [
                "id": id.uuidString.lowercased(),
                "enrollment_token": code,
                "expires_at": Self.format(expiresAt),
                "client_contract_version": DurableAuthClientDescriptor.contractVersion,
                "replayed": false,
            ])
        case let .delayedEnrollment(id, code, expiresAt, nanoseconds):
            try await Task.sleep(nanoseconds: nanoseconds)
            return response(statusCode: 201, object: [
                "id": id.uuidString.lowercased(),
                "enrollment_token": code,
                "expires_at": Self.format(expiresAt),
                "client_contract_version": DurableAuthClientDescriptor.contractVersion,
                "replayed": false,
            ])
        case let .session(
            issuedAt,
            statusCode,
            replayed,
            serverClientInstanceID,
            extraTopLevelKey
        ):
            let session = try makeSession(
                from: snapshot,
                issuedAt: issuedAt,
                serverClientInstanceID: serverClientInstanceID
            )
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .iso8601
            let sessionObject = try JSONSerialization.jsonObject(with: encoder.encode(session))
            var object: [String: Any] = ["session": sessionObject, "replayed": replayed]
            if extraTopLevelKey { object["unexpected"] = true }
            return response(statusCode: statusCode, object: object)
        case .noContent:
            return noContentResponse()
        case let .noContentAndReplaceState(replacement):
            stateStore.forceReplace(replacement)
            return noContentResponse()
        case let .raw(statusCode, body):
            return .init(statusCode: statusCode, headers: Self.jsonHeaders, body: body)
        case let .response(statusCode, headers, body):
            return .init(
                statusCode: statusCode,
                headers: Dictionary(uniqueKeysWithValues: headers.map {
                    ($0.key.lowercased(), $0.value)
                }),
                body: body
            )
        case let .delayedFailure(error, nanoseconds):
            try await Task.sleep(nanoseconds: nanoseconds)
            throw error
        case let .customSession(session, statusCode, replayed):
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .iso8601
            let sessionObject = try JSONSerialization.jsonObject(with: encoder.encode(session))
            return response(statusCode: statusCode, object: [
                "session": sessionObject,
                "replayed": replayed,
            ])
        case let .failure(error):
            throw error
        }
    }

    private func makeSession(
        from snapshot: DurableAuthEnvelope?,
        issuedAt: Date,
        serverClientInstanceID: UUID?
    ) throws -> DurableDeviceSessionMetadata {
        guard let snapshot else { throw DurableAuthError.invalidResponse }
        switch snapshot.state {
        case let .enrollmentPending(pending):
            let clientID = serverClientInstanceID ?? snapshot.clientInstanceID
            guard let clientID else { throw DurableAuthError.invalidResponse }
            return .init(
                id: pending.proposedSessionID,
                clientInstanceID: clientID,
                clientKind: "macos",
                deviceLabel: pending.descriptor.deviceLabel,
                scopes: pending.descriptor.scopes,
                clientContractVersion: DurableAuthClientDescriptor.contractVersion,
                clientVersion: pending.descriptor.clientVersion,
                clientCapabilities: pending.descriptor.clientCapabilities,
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
        case let .refreshPending(pending):
            let previous = pending.previous.session
            return .init(
                id: previous.id,
                clientInstanceID: previous.clientInstanceID,
                clientKind: previous.clientKind,
                deviceLabel: previous.deviceLabel,
                scopes: previous.scopes,
                clientContractVersion: previous.clientContractVersion,
                clientVersion: previous.clientVersion,
                clientCapabilities: previous.clientCapabilities,
                createdAt: previous.createdAt,
                lastSeenAt: max(previous.lastSeenAt, issuedAt),
                credentialIssuedAt: issuedAt,
                accessExpiresAt: min(
                    issuedAt.addingTimeInterval(DurableAuthCoordinator.accessLifetime),
                    previous.absoluteExpiresAt
                ),
                refreshIdleExpiresAt: min(
                    issuedAt.addingTimeInterval(DurableAuthCoordinator.refreshIdleLifetime),
                    previous.absoluteExpiresAt
                ),
                absoluteExpiresAt: previous.absoluteExpiresAt,
                revision: previous.revision + 1
            )
        default:
            throw DurableAuthError.invalidResponse
        }
    }

    private func response(statusCode: Int, object: [String: Any]) -> DurableAuthHTTPResponse {
        .init(
            statusCode: statusCode,
            headers: Self.jsonHeaders,
            body: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        )
    }

    private func noContentResponse() -> DurableAuthHTTPResponse {
        .init(
            statusCode: 204,
            headers: ["cache-control": "no-store, max-age=0", "pragma": "no-cache"],
            body: Data()
        )
    }

    private static let jsonHeaders = [
        "cache-control": "no-store, max-age=0",
        "pragma": "no-cache",
        "content-type": "application/json",
    ]

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }
}

private final class TestDurableKeychainAccess: KeychainSecretAccessing, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    private var shouldCorruptReadbackAfterSave = false
    private var shouldCorruptNextRead = false
    private var shouldRetainValueOnDelete = false

    func read(service: String, account: String) -> Data? {
        lock.withLock {
            if shouldCorruptNextRead {
                shouldCorruptNextRead = false
                return Data("synthetic-mismatched-readback".utf8)
            }
            return values[service + "\u{0}" + account]
        }
    }

    func save(_ data: Data, service: String, account: String) {
        lock.withLock {
            values[service + "\u{0}" + account] = data
            if shouldCorruptReadbackAfterSave {
                shouldCorruptReadbackAfterSave = false
                shouldCorruptNextRead = true
            }
        }
    }

    func delete(service: String, account: String) {
        lock.withLock {
            if shouldRetainValueOnDelete {
                shouldRetainValueOnDelete = false
            } else {
                values.removeValue(forKey: service + "\u{0}" + account)
            }
        }
    }

    func corruptReadbackAfterNextSave() {
        lock.withLock { shouldCorruptReadbackAfterSave = true }
    }

    func retainValueOnNextDelete() {
        lock.withLock { shouldRetainValueOnDelete = true }
    }

    func storedData(service: String, account: String) -> Data? {
        lock.withLock { values[service + "\u{0}" + account] }
    }
}

private final class FailOnceBearerTokenStore: BearerTokenStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var credential: OriginBoundBearerCredential?
    private var remainingDeleteFailures = 1
    private var deletionCount = 0

    init(credential: OriginBoundBearerCredential) {
        self.credential = credential
    }

    var deleteAttempts: Int { lock.withLock { deletionCount } }
    var savedCredential: OriginBoundBearerCredential? { lock.withLock { credential } }

    func loadCredential() -> OriginBoundBearerCredential? {
        lock.withLock { credential }
    }

    func saveCredential(_ credential: OriginBoundBearerCredential) {
        lock.withLock { self.credential = credential }
    }

    func deleteCredential() throws {
        try lock.withLock {
            deletionCount += 1
            if remainingDeleteFailures > 0 {
                remainingDeleteFailures -= 1
                throw BearerTokenStoreError.deleteFailed(status: -1)
            }
            credential = nil
        }
    }
}
#endif
