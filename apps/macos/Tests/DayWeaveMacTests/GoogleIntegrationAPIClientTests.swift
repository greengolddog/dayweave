import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Google integration API client", .serialized)
@MainActor
struct GoogleIntegrationAPIClientTests {
    init() {
        URLProtocolStub.storage.reset(key: Self.apiToken)
    }

    @Test("OAuth start sends the explicit read-only service sentinel and accepts only 201")
    func oauthStartUsesExactReadOnlyContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 201, body: Self.authorizationEnvelope())
        )
        let client = makeClient()
        let request = GoogleOAuthStartRequest(
            forceConsent: true,
            loginHint: "owner@example.com",
            makeDefault: true
        )

        let authorization = try await client.startGoogleOAuth(
            request,
            idempotencyKey: "google.oauth-start_01"
        )

        #expect(authorization.authorizationURL.hasPrefix(
            "https://accounts.google.com/o/oauth2/v2/auth?"
        ))
        #expect(authorization.expiresAt > Date())
        let recorded = try #require(URLProtocolStub.storage.requests(for: Self.apiToken).first)
        #expect(recorded.method == "POST")
        #expect(recorded.url.path == "/gateway/v1/integrations/google/oauth/start")
        #expect(recorded.headers["Idempotency-Key"] == "google.oauth-start_01")
        let body = try #require(recorded.jsonBody)
        #expect(Set(body.keys) == Set([
            "services", "force_consent", "login_hint", "account_id", "connect_new", "make_default",
        ]))
        #expect((body["services"] as? [String]) == [])
        #expect(body["force_consent"] as? Bool == true)
        #expect(body["login_hint"] as? String == "owner@example.com")
        #expect(body["account_id"] is NSNull)
        #expect(body["connect_new"] as? Bool == false)
        #expect(body["make_default"] as? Bool == true)

        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.authorizationEnvelope())
        )
        do {
            _ = try await client.startGoogleOAuth(
                .init(),
                idempotencyKey: "google.oauth-start_02"
            )
            Issue.record("OAuth start must require HTTP 201")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(statusCode: 200, code: nil, message: nil, requestID: nil))
        }
    }

    @Test("account lifecycle binds identity, revision, status, and retry headers")
    func accountLifecycleUsesRevisionGuardedEndpoints() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.accountsEnvelope(revision: 4)),
            .init(statusCode: 200, body: Self.accountBody(status: "paused", revision: 5)),
            .init(statusCode: 200, body: Self.accountBody(status: "active", revision: 6)),
            .init(statusCode: 200, body: Self.accountBody(status: "revoked", revision: 8))
        )
        let client = makeClient()

        let snapshot = try await client.googleAccounts()
        #expect(snapshot.accounts.map(\.id) == [Self.accountID])
        #expect(snapshot.accounts[0].revision == 4)
        #expect(snapshot.cleanup.pending == 1)
        let paused = try await client.pauseGoogleAccount(
            Self.accountID,
            expectedRevision: 4,
            idempotencyKey: "google.pause_01"
        )
        let resumed = try await client.resumeGoogleAccount(
            Self.accountID,
            expectedRevision: 5,
            idempotencyKey: "google.resume_01"
        )
        let disconnected = try await client.disconnectGoogleAccount(
            Self.accountID,
            expectedRevision: 6,
            idempotencyKey: "google.disconnect_01"
        )
        #expect(paused.status == .paused && paused.revision == 5)
        #expect(resumed.status == .active && resumed.revision == 6)
        #expect(disconnected.status == .revoked && disconnected.revision == 8)

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        #expect(requests.map(\.method) == ["GET", "POST", "POST", "DELETE"])
        let accountPath = "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())"
        #expect(requests[0].url.path == "/gateway/v1/integrations/google/accounts")
        #expect(requests[1].url.path == accountPath + "/pause")
        #expect(requests[2].url.path == accountPath + "/resume")
        #expect(requests[3].url.path == accountPath)
        #expect((requests[1].jsonBody?["expected_revision"] as? NSNumber)?.uint64Value == 4)
        #expect((requests[2].jsonBody?["expected_revision"] as? NSNumber)?.uint64Value == 5)
        #expect(requests[3].body == nil)
        let disconnectQuery = try #require(
            URLComponents(url: requests[3].url, resolvingAgainstBaseURL: false)
        )
        #expect(disconnectQuery.queryItems == [
            URLQueryItem(name: "expected_revision", value: "6"),
        ])
        #expect(requests[1].headers["Idempotency-Key"] == "google.pause_01")
        #expect(requests[2].headers["Idempotency-Key"] == "google.resume_01")
        #expect(requests[3].headers["Idempotency-Key"] == "google.disconnect_01")
    }

    @Test("only the strict disconnect revision conflict proves no effect")
    func disconnectRevisionConflictRequiresStrictTrustedEnvelope() async throws {
        let body = Data(
            """
            {"error":{"code":"conflict","message":"Google account changed on another device","details":{"expected_revision":6,"actual_revision":7}}}
            """.utf8
        )
        let strictHeaders = [
            "content-type": "application/json",
            "cache-control": "no-store, max-age=0",
            "pragma": "no-cache",
        ]
        let duplicateKeyBody = Data(
            #"{"error":{"code":"conflict","message":"Google account changed on another device","details":{"expected_revision":6,"\u0065xpected_revision":6,"actual_revision":7}}}"#.utf8
        )
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 409, headers: strictHeaders, body: body),
            .init(
                statusCode: 409,
                headers: ["content-type": "application/json"],
                body: body
            ),
            .init(statusCode: 409, headers: strictHeaders, body: duplicateKeyBody)
        )
        let client = makeClient()

        do {
            _ = try await client.disconnectGoogleAccount(
                Self.accountID,
                expectedRevision: 6,
                idempotencyKey: "google.disconnect_conflict_01"
            )
            Issue.record("A strict revision conflict was not surfaced as trusted no-effect")
        } catch let error as DayWeaveAPIError {
            #expect(error == .trustedGoogleDisconnectNoEffect)
        }

        do {
            _ = try await client.disconnectGoogleAccount(
                Self.accountID,
                expectedRevision: 6,
                idempotencyKey: "google.disconnect_conflict_02"
            )
            Issue.record("An untrusted revision conflict was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(
                statusCode: 409,
                code: "conflict",
                message: "Google account changed on another device",
                requestID: nil
            ))
        }

        do {
            _ = try await client.disconnectGoogleAccount(
                Self.accountID,
                expectedRevision: 6,
                idempotencyKey: "google.disconnect_conflict_03"
            )
            Issue.record("A duplicate-key revision conflict was promoted to trusted no-effect")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(
                statusCode: 409,
                code: "conflict",
                message: "Google account changed on another device",
                requestID: nil
            ))
        }
    }

    @Test("OAuth capabilities and Google identity details stay out of diagnostics")
    func providerDetailsAreRedactedFromDiagnosticsAndReflection() async throws {
        let authorizationCanary = "SENSITIVE-OAUTH-STATE-CANARY"
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 201,
                body: Self.authorizationEnvelope(
                    url: "https://accounts.google.com/o/oauth2/v2/auth?state=\(authorizationCanary)"
                )
            ),
            .init(statusCode: 200, body: Self.accountsEnvelope(revision: 4))
        )
        let client = makeClient()
        let authorization = try await client.startGoogleOAuth(
            .init(),
            idempotencyKey: "google.redaction_01"
        )
        let snapshot = try await client.googleAccounts()
        let account = try #require(snapshot.accounts.first)

        let authorizationDiagnostics = [
            String(describing: authorization),
            String(reflecting: authorization),
            Self.reflectedChildren(authorization),
        ].joined(separator: " ")
        #expect(!authorizationDiagnostics.contains(authorizationCanary))
        #expect(!authorizationDiagnostics.contains("accounts.google.com"))
        #expect(authorizationDiagnostics.contains("authorization pending"))

        let accountDiagnostics = [
            String(describing: account),
            String(reflecting: account),
            Self.reflectedChildren(account),
        ].joined(separator: " ")
        for privateValue in [
            "google-subject-1",
            "owner@example.com",
            "calendar.readonly",
            "2026-08-30T12:00:00",
        ] {
            #expect(!accountDiagnostics.contains(privateValue))
        }
        #expect(accountDiagnostics.contains("Google account active"))
    }

    @Test("collection discovery, read-only configuration, status, and refresh use exact routes")
    func collectionAndSyncMethodsUseExactContracts() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.collectionsEnvelope(revision: 3)),
            .init(statusCode: 200, body: Self.collectionsEnvelope(revision: 3)),
            .init(
                statusCode: 200,
                body: Self.collectionEnvelope(
                    revision: 4,
                    selected: true,
                    visible: true,
                    role: "blocking"
                )
            ),
            .init(statusCode: 200, body: Self.syncStatusEnvelope()),
            .init(statusCode: 202, body: Self.refreshEnvelope())
        )
        let client = makeClient()
        let policy = GoogleCalendarPolicy()

        let listed = try await client.googleCollections(accountID: Self.accountID)
        let discovered = try await client.discoverGoogleCollections(accountID: Self.accountID)
        let configured = try await client.configureGoogleCollection(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedRevision: 3,
            selected: true,
            visible: true,
            role: .blocking,
            calendarPolicy: policy
        )
        let status = try await client.googleSyncStatus(accountID: Self.accountID)
        let refresh = try await client.requestGoogleSyncRefresh(
            accountID: Self.accountID,
            requestID: Self.refreshRequestID
        )

        #expect(listed.count == 1 && discovered.count == 1)
        #expect(configured.kind == .calendar)
        #expect(configured.syncRole == .blocking)
        #expect(configured.revision == 4)
        #expect(status.run?.accountID == Self.accountID)
        #expect(status.run?.importedCount == 2)
        #expect(status.importConflicts == 1)
        #expect(refresh.accountID == Self.accountID)
        #expect(refresh.requestID == Self.refreshRequestID)
        #expect(refresh.refreshGeneration == 7)

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        let accountPath = "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())"
        #expect(requests.map(\.method) == ["GET", "POST", "PUT", "GET", "POST"])
        #expect(requests[0].url.path == accountPath + "/collections")
        #expect(requests[1].url.path == accountPath + "/collections/discover")
        #expect(requests[1].body == nil)
        #expect(requests[2].url.path == accountPath + "/collections/\(Self.collectionID.uuidString.lowercased())")
        #expect(requests[3].url.path == accountPath + "/sync")
        #expect(requests[4].url.path == accountPath + "/sync/refresh")
        let refreshBody = try #require(requests[4].jsonBody)
        #expect(Set(refreshBody.keys) == ["request_id"])
        #expect(refreshBody["request_id"] as? String == Self.refreshRequestID.uuidString)
        let configuration = try #require(requests[2].jsonBody)
        #expect(Set(configuration.keys) == Set([
            "expected_revision", "selected", "visible", "sync_role", "calendar_policy",
        ]))
        #expect((configuration["expected_revision"] as? NSNumber)?.uint64Value == 3)
        #expect(configuration["selected"] as? Bool == true)
        #expect(configuration["visible"] as? Bool == true)
        #expect(configuration["sync_role"] as? String == "blocking")
        let encodedPolicy = try #require(configuration["calendar_policy"] as? [String: Any])
        #expect(Set(encodedPolicy.keys) == Set([
            "confirmed_busy", "tentative", "free", "all_day",
            "publish_all_day", "publish_tentative", "publish_free",
        ]))
        #expect(encodedPolicy["publish_all_day"] as? Bool == false)
        #expect(encodedPolicy["publish_tentative"] as? Bool == false)
        #expect(encodedPolicy["publish_free"] as? Bool == false)
    }

    @Test("read-only and idempotency gates fail before transport")
    func unsafeRequestsFailLocally() async throws {
        let client = makeClient()
        let invalidKeys = [
            "short", "contains:colon", "contains~tilde", "contains space",
            String(repeating: "a", count: 129), "unicode-éééééééé",
        ]
        for key in invalidKeys {
            do {
                _ = try await client.startGoogleOAuth(.init(), idempotencyKey: key)
                Issue.record("Invalid Google idempotency key was accepted: \(key)")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            }
        }
        for request in [
            GoogleOAuthStartRequest(services: [.calendarReadOnly]),
            GoogleOAuthStartRequest(loginHint: ""),
            GoogleOAuthStartRequest(accountID: Self.accountID, connectNew: true),
        ] {
            do {
                _ = try await client.startGoogleOAuth(
                    request,
                    idempotencyKey: "google.invalid_01"
                )
                Issue.record("Unsafe Google OAuth request was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            }
        }
        do {
            _ = try await client.pauseGoogleAccount(
                Self.accountID,
                expectedRevision: 0,
                idempotencyKey: "google.pause_00"
            )
            Issue.record("Zero account revision was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.configureGoogleCollection(
                accountID: Self.accountID,
                collectionID: Self.collectionID,
                expectedRevision: 3,
                selected: true,
                visible: true,
                role: .writable,
                calendarPolicy: .init()
            )
            Issue.record("Writable collection role was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.configureGoogleCollection(
                accountID: Self.accountID,
                collectionID: Self.collectionID,
                expectedRevision: 3,
                selected: true,
                visible: true,
                role: .readOnly,
                calendarPolicy: .init(publishAllDay: true)
            )
            Issue.record("Outbound publication policy was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        #expect(URLProtocolStub.storage.requests(for: Self.apiToken).isEmpty)
    }

    @Test("static and legacy authorization never reach Google transport")
    func nonDurableAuthorizationFailsBeforeTransport() async throws {
        let baseURL = Self.baseURL
        let staticClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            bearerToken: Self.apiToken
        )
        do {
            _ = try await staticClient.googleAccounts()
            Issue.record("A static bearer reached a Google endpoint")
        } catch let error as DayWeaveAPIError {
            #expect(error == .durableAuthentication(.enrollmentRequired))
        }

        let legacyEnvelope = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: UUID(uuidString: "44444444-dddd-4ddd-8ddd-444444444444")!,
            state: .legacy(.init(bearerToken: Self.apiToken))
        )
        let legacyCoordinator = DurableAuthCoordinator(
            stateStore: GoogleAPITestDurableAuthStateStore(initial: legacyEnvelope),
            legacyStore: GoogleAPITestBearerTokenStore()
        )
        let legacyClient = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: legacyCoordinator
        )
        do {
            _ = try await legacyClient.requestGoogleSyncRefresh(
                accountID: Self.accountID,
                requestID: Self.refreshRequestID
            )
            Issue.record("A legacy coordinator bearer reached a Google endpoint")
        } catch let error as DayWeaveAPIError {
            #expect(error == .durableAuthentication(.enrollmentRequired))
        }

        #expect(URLProtocolStub.storage.requests(for: Self.apiToken).isEmpty)
    }

    @Test("authorization URLs, unknown credential fields, counts, and identities fail closed")
    func malformedResponsesFailClosed() async throws {
        let client = makeClient()
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 201,
                body: Self.authorizationEnvelope(
                    url: "https://evil.example/o/oauth2/v2/auth?state=opaque"
                )
            ),
            .init(
                statusCode: 201,
                body: Self.authorizationEnvelope(expiry: Date().addingTimeInterval(31 * 60))
            ),
            .init(
                statusCode: 200,
                body: Self.accountsEnvelope(
                    revision: 4,
                    extraAccountField: ",\"access_token\":\"never\""
                )
            ),
            .init(
                statusCode: 200,
                body: Self.accountsEnvelope(
                    revision: 4,
                    cleanupPending: "9223372036854775808"
                )
            ),
            .init(
                statusCode: 200,
                body: Self.collectionsEnvelope(revision: 3, accountID: Self.otherAccountID)
            ),
            .init(statusCode: 200, body: Self.syncStatusEnvelope(accountID: Self.otherAccountID)),
            .init(statusCode: 202, body: Self.refreshEnvelope(accountID: Self.otherAccountID))
        )

        for key in ["bad-host", "bad-expiry"] {
            do {
                _ = try await client.startGoogleOAuth(
                    .init(),
                    idempotencyKey: "google.\(key)_01"
                )
                Issue.record("Malformed authorization response was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            }
        }
        for _ in 0..<2 {
            do {
                _ = try await client.googleAccounts()
                Issue.record("Malformed account response was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            }
        }
        do {
            _ = try await client.googleCollections(accountID: Self.accountID)
            Issue.record("Cross-account collection response was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
        do {
            _ = try await client.googleSyncStatus(accountID: Self.accountID)
            Issue.record("Cross-account sync response was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
        do {
            _ = try await client.requestGoogleSyncRefresh(
                accountID: Self.accountID,
                requestID: Self.refreshRequestID
            )
            Issue.record("Cross-account refresh response was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
    }

    @Test("task lists reject blocking while existing writable inventory remains representable")
    func collectionRoleResponsesFailClosed() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 200,
                body: Self.collectionEnvelope(
                    revision: 4,
                    selected: true,
                    visible: true,
                    role: "blocking",
                    kind: "task_list"
                )
            ),
            .init(
                statusCode: 200,
                body: Self.collectionsEnvelope(revision: 3, role: "writable")
            )
        )
        let client = makeClient()
        do {
            _ = try await client.configureGoogleCollection(
                accountID: Self.accountID,
                collectionID: Self.collectionID,
                expectedRevision: 3,
                selected: true,
                visible: true,
                role: .blocking,
                calendarPolicy: .init()
            )
            Issue.record("A blocking task list response was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
        let existingWritable = try await client.googleCollections(accountID: Self.accountID)
        #expect(existingWritable.first?.syncRole == .writable)
    }

    private func makeClient() -> DayWeaveAPIClient {
        let baseURL = Self.baseURL
        let issuedAt = Date()
        let clientInstanceID = UUID(uuidString: "55555555-eeee-4eee-8eee-555555555555")!
        let session = DurableDeviceSessionMetadata(
            id: UUID(uuidString: "66666666-ffff-4fff-8fff-666666666666")!,
            clientInstanceID: clientInstanceID,
            clientKind: "macos",
            deviceLabel: "Google API test Mac",
            scopes: DayWeaveAuthScope.deviceDefaults,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: "test",
            clientCapabilities: DurableAuthClientDescriptor.capabilities,
            createdAt: issuedAt,
            lastSeenAt: issuedAt,
            credentialIssuedAt: issuedAt,
            accessExpiresAt: issuedAt.addingTimeInterval(10 * 60),
            refreshIdleExpiresAt: issuedAt.addingTimeInterval(29 * 24 * 60 * 60),
            absoluteExpiresAt: issuedAt.addingTimeInterval(179 * 24 * 60 * 60),
            revision: 1
        )
        let envelope = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientInstanceID,
            state: .active(.init(
                session: session,
                credentials: .init(
                    accessToken: Self.apiToken,
                    refreshToken: "google-integration-test-refresh-token"
                )
            ))
        )
        let coordinator = DurableAuthCoordinator(
            stateStore: GoogleAPITestDurableAuthStateStore(initial: envelope),
            legacyStore: GoogleAPITestBearerTokenStore()
        )
        return DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            authCoordinator: coordinator
        )
    }

    private static let baseURL = try! DayWeaveAPIBaseURL("https://api.example.com/gateway")
    private static let apiToken = "google-integration-test-token"
    private static let accountID = UUID(uuidString: "11111111-aaaa-4aaa-8aaa-111111111111")!
    private static let refreshRequestID = UUID(
        uuidString: "11111111-bbbb-4bbb-8bbb-111111111111"
    )!
    private static let otherAccountID = UUID(uuidString: "22222222-bbbb-4bbb-8bbb-222222222222")!
    private static let collectionID = UUID(uuidString: "33333333-cccc-4ccc-8ccc-333333333333")!

    private static func reflectedChildren(_ value: Any) -> String {
        Mirror(reflecting: value).children
            .map { child in "\(child.label ?? "")=\(String(reflecting: child.value))" }
            .joined(separator: ",")
    }

    private static func authorizationEnvelope(
        url: String = "https://accounts.google.com/o/oauth2/v2/auth?client_id=dayweave&state=opaque",
        expiry: Date = Date().addingTimeInterval(10 * 60)
    ) -> Data {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return Data(
            #"{"authorization_url":"\#(url)","expires_at":"\#(formatter.string(from: expiry))"}"#.utf8
        )
    }

    private static func accountsEnvelope(
        revision: UInt64,
        cleanupPending: String = "1",
        extraAccountField: String = ""
    ) -> Data {
        let account = accountObject(
            status: "active",
            revision: revision,
            extraField: extraAccountField
        )
        return Data(
            "{\"accounts\":[\(account)],\"cleanup\":\(cleanupObject(pending: cleanupPending))}".utf8
        )
    }

    private static func accountBody(status: String, revision: UInt64) -> Data {
        Data(accountObject(status: status, revision: revision).utf8)
    }

    private static func accountObject(
        status: String,
        revision: UInt64,
        extraField: String = ""
    ) -> String {
        let syncEnabled = status == "active" ? "true" : "false"
        let isDefault = status == "revoked" ? "false" : "true"
        let grantedScopes = status == "revoked"
            ? "[]"
            : "[\"email\",\"https://www.googleapis.com/auth/calendar.readonly\",\"https://www.googleapis.com/auth/tasks.readonly\",\"openid\"]"
        let tokenExpiresAt = status == "revoked" ? "null" : "\"2026-08-30T12:00:00Z\""
        return """
        {
          "id":"\(accountID.uuidString.lowercased())",
          "external_account_id":"google-subject-1",
          "display_label":"owner@example.com",
          "status":"\(status)",
          "sync_enabled":\(syncEnabled),
          "is_default":\(isDefault),
          "granted_scopes":\(grantedScopes),
          "token_expires_at":\(tokenExpiresAt),
          "revision":\(revision),
          "created_at":"2026-08-30T09:00:00Z",
          "updated_at":"2026-08-30T10:00:00Z"\(extraField)
        }
        """
    }

    private static func cleanupObject(pending: String) -> String {
        """
        {
          "held":0,
          "pending":\(pending),
          "retrying":0,
          "exhausted":0,
          "volatile_guardians":0,
          "durability_degraded":false,
          "revocation_fenced":false,
          "operator_recovery_required":false,
          "uncertain_authorizations":0,
          "legacy_recovery_required":0,
          "next_attempt_at":"2026-08-30T10:05:00Z",
          "last_failure_at":null
        }
        """
    }

    private static func collectionsEnvelope(
        revision: UInt64,
        accountID: UUID = accountID,
        role: String = "read_only"
    ) -> Data {
        Data(
            "{\"collections\":[\(collectionObject(revision: revision, accountID: accountID, role: role))]}".utf8
        )
    }

    private static func collectionEnvelope(
        revision: UInt64,
        selected: Bool,
        visible: Bool,
        role: String,
        kind: String = "calendar"
    ) -> Data {
        Data(
            "{\"collection\":\(collectionObject(revision: revision, selected: selected, visible: visible, role: role, kind: kind))}".utf8
        )
    }

    private static func collectionObject(
        revision: UInt64,
        accountID: UUID = accountID,
        selected: Bool = false,
        visible: Bool = true,
        role: String = "read_only",
        kind: String = "calendar"
    ) -> String {
        """
        {
          "id":"\(collectionID.uuidString.lowercased())",
          "account_id":"\(accountID.uuidString.lowercased())",
          "kind":"\(kind)",
          "remote_collection_id":"primary",
          "display_name":"Primary calendar",
          "provider_access_role":"owner",
          "provider_primary":true,
          "provider_selected":true,
          "provider_hidden":false,
          "provider_deleted":false,
          "selected":\(selected),
          "visible":\(visible),
          "sync_role":"\(role)",
          "calendar_policy":\(calendarPolicyObject()),
          "revision":\(revision),
          "discovered_at":"2026-08-30T09:00:00Z",
          "configured_at":null,
          "last_import_at":null,
          "planning_projection_state":"uninitialized",
          "planning_generation":0,
          "planning_collection_revision":null,
          "planning_window_start":null,
          "planning_window_end":null,
          "planning_window_refreshed_at":null,
          "created_at":"2026-08-30T09:00:00Z",
          "updated_at":"2026-08-30T10:00:00Z"
        }
        """
    }

    private static func calendarPolicyObject() -> String {
        """
        {
          "confirmed_busy":"blocking",
          "tentative":"visible_nonblocking",
          "free":"visible_nonblocking",
          "all_day":"visible_nonblocking",
          "publish_all_day":false,
          "publish_tentative":false,
          "publish_free":false
        }
        """
    }

    private static func syncStatusEnvelope(accountID: UUID = accountID) -> Data {
        Data(
            """
            {
              "sync":{
                "run":{
                  "account_id":"\(accountID.uuidString.lowercased())",
                  "state":"idle",
                  "requested_at":"2026-08-30T09:00:00Z",
                  "started_at":"2026-08-30T09:01:00Z",
                  "completed_at":"2026-08-30T09:02:00Z",
                  "next_attempt_at":"2026-08-30T10:00:00Z",
                  "consecutive_failures":0,
                  "last_error_code":null,
                  "last_error_at":null,
                  "imported_count":2,
                  "updated_count":3,
                  "deleted_count":0,
                  "conflict_count":1,
                  "rejected_count":0,
                  "refresh_generation":7,
                  "claimed_refresh_generation":7,
                  "completed_refresh_generation":7,
                  "revision":4
                },
                "import_conflicts":1,
                "pending_outbound":0,
                "conflicted_outbound":0,
                "failed_outbound":0,
                "last_outbound_error_code":null,
                "last_outbound_error_at":null,
                "next_outbound_attempt_at":null
              }
            }
            """.utf8
        )
    }

    private static func refreshEnvelope(
        accountID: UUID = accountID,
        requestID: UUID = refreshRequestID
    ) -> Data {
        Data(
            "{\"refresh\":{\"account_id\":\"\(accountID.uuidString.lowercased())\",\"request_id\":\"\(requestID.uuidString.lowercased())\",\"refresh_generation\":7,\"requested_at\":\"2026-08-30T10:00:00Z\"}}".utf8
        )
    }
}

private final class GoogleAPITestDurableAuthStateStore: DurableAuthStateStoring,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var envelope: DurableAuthEnvelope?

    init(initial: DurableAuthEnvelope?) {
        envelope = initial
    }

    func loadEnvelope() -> DurableAuthEnvelope? {
        lock.withLock { envelope }
    }

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
}

private struct GoogleAPITestBearerTokenStore: BearerTokenStoring {
    func loadCredential() -> OriginBoundBearerCredential? { nil }
    func saveCredential(_ credential: OriginBoundBearerCredential) {}
    func deleteCredential() {}
}
#endif
