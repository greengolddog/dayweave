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

    @Test("OAuth permits only separate bounded Calendar or Tasks write-scope upgrades")
    func oauthWriteUpgradesUseExactServiceShapes() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 201, body: Self.authorizationEnvelope()),
            .init(statusCode: 201, body: Self.authorizationEnvelope())
        )
        let client = makeClient()

        _ = try await client.startGoogleOAuth(
            .init(services: [.calendar], forceConsent: true, accountID: Self.accountID),
            idempotencyKey: "google.calendar-upgrade_01"
        )
        _ = try await client.startGoogleOAuth(
            .init(services: [.tasks], forceConsent: true, accountID: Self.accountID),
            idempotencyKey: "google.tasks-upgrade_01"
        )

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        #expect(requests.count == 2)
        #expect(requests.allSatisfy {
            $0.url.path == "/gateway/v1/integrations/google/oauth/start"
        })
        #expect(requests[0].jsonBody?["services"] as? [String] == ["calendar"])
        #expect(requests[1].jsonBody?["services"] as? [String] == ["tasks"])
        #expect(requests.allSatisfy {
            $0.jsonBody?["account_id"] as? String == Self.accountID.uuidString
                && $0.jsonBody?["force_consent"] as? Bool == true
                && $0.jsonBody?["connect_new"] as? Bool == false
        })

        for services: [GoogleService] in [
            [.calendarReadOnly], [.tasksReadOnly], [.calendar, .tasks],
            [.calendar, .calendar], [.tasks, .tasks],
        ] {
            do {
                _ = try await client.startGoogleOAuth(
                    .init(
                        services: services,
                        forceConsent: true,
                        accountID: Self.accountID
                    ),
                    idempotencyKey: "google.calendar-upgrade_02"
                )
                Issue.record("An unsupported Google scope combination was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            }
        }
        for request in [
            GoogleOAuthStartRequest(services: [.calendar]),
            GoogleOAuthStartRequest(services: [.tasks]),
            GoogleOAuthStartRequest(services: [.calendar], accountID: Self.accountID),
            GoogleOAuthStartRequest(services: [.tasks], accountID: Self.accountID),
            GoogleOAuthStartRequest(services: [.calendar], forceConsent: true),
            GoogleOAuthStartRequest(services: [.tasks], forceConsent: true),
            GoogleOAuthStartRequest(
                services: [.calendar],
                forceConsent: true,
                accountID: Self.accountID,
                connectNew: true
            ),
            GoogleOAuthStartRequest(
                services: [.tasks],
                forceConsent: true,
                accountID: Self.accountID,
                connectNew: true
            ),
        ] {
            do {
                _ = try await client.startGoogleOAuth(
                    request,
                    idempotencyKey: "google.calendar-upgrade_03"
                )
                Issue.record("A write scope request without an explicit existing-account upgrade was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            }
        }
        #expect(URLProtocolStub.storage.requests(for: Self.apiToken).count == 2)
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

    @Test("outbound preview, approval, and enqueue use exact bound contracts")
    func outboundFlowUsesExactContracts() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.outboundPreviewEnvelope()),
            .init(statusCode: 200, body: Self.outboundApprovalEnvelope()),
            .init(statusCode: 202, body: Self.outboundAcceptedEnvelope())
        )
        let client = makeClient()
        let previewRequest = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert
        )

        let preview = try await client.previewGoogleOutbound(
            accountID: Self.accountID,
            request: previewRequest
        )
        let approval = try await client.approveGoogleOutbound(
            accountID: Self.accountID,
            previewID: preview.id,
            expectedPreviewHash: preview.previewHash
        )
        let accepted = try await client.enqueueGoogleOutbound(
            accountID: Self.accountID,
            request: .init(
                collectionID: Self.collectionID,
                itemID: Self.itemID,
                expectedItemRevision: 9,
                operation: .upsert,
                approvalCapability: approval.approvalCapability
            )
        )

        #expect(preview.accountID == Self.accountID)
        #expect(preview.collectionID == Self.collectionID)
        #expect(preview.itemID == Self.itemID)
        #expect(preview.itemRevision == 9)
        #expect(preview.entityKind == .calendarEvent)
        #expect(preview.operation == .upsert)
        #expect(preview.providerResourceID == nil)
        #expect(preview.providerETag == nil)
        #expect(preview.providerPayload["summary"] == .string("Private planning canary"))
        #expect(approval.previewID == Self.previewID)
        #expect(accepted.outboxID == Self.outboxID)
        #expect(accepted.replayed == false)

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        let accountPath = "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())"
        #expect(requests.map(\.method) == ["POST", "POST", "POST"])
        #expect(requests[0].url.path == accountPath + "/outbound/previews")
        #expect(requests[1].url.path == accountPath + "/outbound/previews/\(Self.previewID.uuidString.lowercased())/approve")
        #expect(requests[2].url.path == accountPath + "/outbound")
        let previewBody = try #require(requests[0].jsonBody)
        #expect(Set(previewBody.keys) == Set([
            "collection_id", "item_id", "expected_item_revision", "operation",
        ]))
        #expect(previewBody["collection_id"] as? String == Self.collectionID.uuidString)
        #expect(previewBody["item_id"] as? String == Self.itemID.uuidString)
        #expect((previewBody["expected_item_revision"] as? NSNumber)?.uint64Value == 9)
        #expect(previewBody["operation"] as? String == "upsert")
        let approvalBody = try #require(requests[1].jsonBody)
        #expect(Set(approvalBody.keys) == ["expected_preview_hash"])
        #expect(approvalBody["expected_preview_hash"] as? String == Self.previewHash)
        let enqueueBody = try #require(requests[2].jsonBody)
        #expect(Set(enqueueBody.keys) == Set([
            "collection_id", "item_id", "expected_item_revision", "operation",
            "approval_capability",
        ]))
        #expect(enqueueBody["approval_capability"] as? String == Self.approvalCapability)
    }

    @Test("Task outbound previews require the exact inert server projection")
    func taskOutboundPreviewUsesExactProviderProjection() async throws {
        let validPayload = Self.validTaskProviderPayload()
        var maximumScalarPayload = validPayload
        maximumScalarPayload["notes"] = String(repeating: "😀", count: 100_000)
        var invalidPayloads: [[String: Any]] = []

        var missingTitle = validPayload
        missingTitle.removeValue(forKey: "title")
        invalidPayloads.append(missingTitle)

        var extraField = validPayload
        extraField["kind"] = "tasks#task"
        invalidPayloads.append(extraField)

        let fieldMutations: [(String, Any)] = [
            ("id", "provider-task-id"),
            ("etag", "provider-etag"),
            ("status", "cancelled"),
            ("due", "tomorrow"),
            ("updated", "2026-09-02T09:00:00Z"),
            ("parent", "provider-parent"),
            ("position", "0001"),
            ("links", [["type": "email"]]),
            ("deleted", true),
            ("hidden", true),
            ("notes", "ordinary\n[DayWeave item:visible-marker]"),
            ("title", "   "),
            ("title", String(repeating: "e\u{301}", count: 251)),
            ("notes", String(repeating: "e\u{301}", count: 50_001)),
        ]
        for (key, value) in fieldMutations {
            var payload = validPayload
            payload[key] = value
            invalidPayloads.append(payload)
        }

        var completedWithoutTimestamp = validPayload
        completedWithoutTimestamp["completed"] = NSNull()
        invalidPayloads.append(completedWithoutTimestamp)

        var activeWithCompletion = validPayload
        activeWithCompletion["status"] = "needsAction"
        invalidPayloads.append(activeWithCompletion)

        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 200,
                body: try Self.outboundTaskPreviewEnvelope(payload: validPayload)
            ),
            .init(
                statusCode: 200,
                body: try Self.outboundTaskPreviewEnvelope(payload: maximumScalarPayload)
            )
        )
        for payload in invalidPayloads {
            URLProtocolStub.storage.enqueue(
                key: Self.apiToken,
                .init(
                    statusCode: 200,
                    body: try Self.outboundTaskPreviewEnvelope(payload: payload)
                )
            )
        }
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 200,
                body: try Self.outboundTaskPreviewEnvelope(
                    payload: [:],
                    operation: "delete",
                    existing: true
                )
            ),
            .init(
                statusCode: 200,
                body: try Self.outboundTaskPreviewEnvelope(
                    payload: validPayload,
                    operation: "delete",
                    existing: true
                )
            )
        )

        let client = makeClient()
        let upsertRequest = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert
        )
        let preview = try await client.previewGoogleOutbound(
            accountID: Self.accountID,
            request: upsertRequest
        )
        #expect(preview.entityKind == .task)
        #expect(preview.providerPayload["title"] == .string("Private task"))
        #expect(preview.providerPayload["status"] == .string("completed"))
        #expect(preview.providerPayload["id"] == .string(""))
        #expect(preview.providerPayload["etag"] == .null)

        let maximumScalarPreview = try await client.previewGoogleOutbound(
            accountID: Self.accountID,
            request: upsertRequest
        )
        guard case let .string(maximumNotes)? = maximumScalarPreview.providerPayload["notes"] else {
            Issue.record("The maximum valid Unicode-scalar Task notes were not retained")
            return
        }
        #expect(maximumNotes.unicodeScalars.count == 100_000)

        for _ in invalidPayloads {
            await expectResponseDecodingFailure {
                _ = try await client.previewGoogleOutbound(
                    accountID: Self.accountID,
                    request: upsertRequest
                )
            }
        }

        let deleteRequest = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .delete
        )
        let deletion = try await client.previewGoogleOutbound(
            accountID: Self.accountID,
            request: deleteRequest
        )
        #expect(deletion.entityKind == .task)
        #expect(deletion.providerPayload.isEmpty)
        #expect(deletion.providerResourceID == "provider-task-id")
        #expect(deletion.providerETag == "provider-etag")
        await expectResponseDecodingFailure {
            _ = try await client.previewGoogleOutbound(
                accountID: Self.accountID,
                request: deleteRequest
            )
        }
    }

    @Test("outbound expiry validation tolerates the supported device clock skew")
    func outboundExpiryBoundsTolerateClockSkew() async throws {
        let reference = Date(timeIntervalSince1970: 1_788_076_800)
        let acceptedOffsets: [TimeInterval] = [-5 * 60, 30 * 60, 35 * 60]
        let rejectedOffsets: [TimeInterval] = [-(5 * 60) - 1, (35 * 60) + 1]
        for offset in acceptedOffsets + rejectedOffsets {
            URLProtocolStub.storage.enqueue(
                key: Self.apiToken,
                .init(
                    statusCode: 200,
                    body: Self.outboundPreviewEnvelope(
                        expiry: reference.addingTimeInterval(offset)
                    )
                )
            )
        }
        for offset in acceptedOffsets + rejectedOffsets {
            URLProtocolStub.storage.enqueue(
                key: Self.apiToken,
                .init(
                    statusCode: 200,
                    body: Self.outboundApprovalEnvelope(
                        expiry: reference.addingTimeInterval(offset)
                    )
                )
            )
        }
        let client = makeClient(now: { reference })
        let request = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert
        )

        for _ in acceptedOffsets {
            _ = try await client.previewGoogleOutbound(
                accountID: Self.accountID,
                request: request
            )
        }
        for _ in rejectedOffsets {
            await expectResponseDecodingFailure {
                _ = try await client.previewGoogleOutbound(
                    accountID: Self.accountID,
                    request: request
                )
            }
        }
        for _ in acceptedOffsets {
            _ = try await client.approveGoogleOutbound(
                accountID: Self.accountID,
                previewID: Self.previewID,
                expectedPreviewHash: Self.previewHash
            )
        }
        for _ in rejectedOffsets {
            await expectResponseDecodingFailure {
                _ = try await client.approveGoogleOutbound(
                    accountID: Self.accountID,
                    previewID: Self.previewID,
                    expectedPreviewHash: Self.previewHash
                )
            }
        }
    }

    @Test("outbound methods require exact statuses and strict response wrappers")
    func outboundResponsesFailClosed() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 201, body: Self.outboundPreviewEnvelope()),
            .init(statusCode: 201, body: Self.outboundApprovalEnvelope()),
            .init(statusCode: 200, body: Self.outboundAcceptedEnvelope()),
            .init(statusCode: 200, body: Self.outboundPreviewEnvelope(extraEnvelopeField: true)),
            .init(statusCode: 200, body: Self.outboundPreviewEnvelope(extraPreviewField: true)),
            .init(statusCode: 200, body: Self.outboundApprovalEnvelope(extraApprovalField: true)),
            .init(statusCode: 202, body: Self.outboundAcceptedEnvelope(extraOutboundField: true))
        )
        let client = makeClient()
        let previewRequest = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert
        )
        let enqueueRequest = GoogleOutboundEnqueueRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert,
            approvalCapability: Self.approvalCapability
        )

        do {
            _ = try await client.previewGoogleOutbound(
                accountID: Self.accountID,
                request: previewRequest
            )
            Issue.record("Preview accepted HTTP 201")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(statusCode: 201, code: nil, message: nil, requestID: nil))
        }
        do {
            _ = try await client.approveGoogleOutbound(
                accountID: Self.accountID,
                previewID: Self.previewID,
                expectedPreviewHash: Self.previewHash
            )
            Issue.record("Approval accepted HTTP 201")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(statusCode: 201, code: nil, message: nil, requestID: nil))
        }
        do {
            _ = try await client.enqueueGoogleOutbound(
                accountID: Self.accountID,
                request: enqueueRequest
            )
            Issue.record("Enqueue accepted HTTP 200")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(statusCode: 200, code: nil, message: nil, requestID: nil))
        }

        for operation in 0..<4 {
            do {
                switch operation {
                case 0, 1:
                    _ = try await client.previewGoogleOutbound(
                        accountID: Self.accountID,
                        request: previewRequest
                    )
                case 2:
                    _ = try await client.approveGoogleOutbound(
                        accountID: Self.accountID,
                        previewID: Self.previewID,
                        expectedPreviewHash: Self.previewHash
                    )
                default:
                    _ = try await client.enqueueGoogleOutbound(
                        accountID: Self.accountID,
                        request: enqueueRequest
                    )
                }
                Issue.record("Unknown outbound response field was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            }
        }
    }

    @Test("outbound success rejects nested duplicate keys and oversized provider payloads")
    func outboundSuccessResourceGuards() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.outboundPreviewWithDuplicatePayloadKey()),
            .init(statusCode: 200, body: Self.outboundApprovalWithDuplicateKey()),
            .init(statusCode: 202, body: Self.outboundAcceptedWithDuplicateKey()),
            .init(statusCode: 200, body: try Self.oversizedOutboundPreviewEnvelope())
        )
        let client = makeClient()
        let previewRequest = GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert
        )
        let enqueueRequest = GoogleOutboundEnqueueRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .upsert,
            approvalCapability: Self.approvalCapability
        )

        for operation in 0..<4 {
            do {
                switch operation {
                case 0, 3:
                    _ = try await client.previewGoogleOutbound(
                        accountID: Self.accountID,
                        request: previewRequest
                    )
                case 1:
                    _ = try await client.approveGoogleOutbound(
                        accountID: Self.accountID,
                        previewID: Self.previewID,
                        expectedPreviewHash: Self.previewHash
                    )
                default:
                    _ = try await client.enqueueGoogleOutbound(
                        accountID: Self.accountID,
                        request: enqueueRequest
                    )
                }
                Issue.record("Unsafe outbound success response was accepted")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            }
        }
    }

    @Test("outbound capabilities never enter diagnostics or server errors")
    func outboundCapabilityDiagnosticsAreSecretSafe() async throws {
        let capability = Self.approvalCapability
        let errorBody = Data(
            "{\"error\":{\"code\":\"conflict\",\"message\":\"capability \(capability) rejected\"}}".utf8
        )
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 409, body: errorBody)
        )
        let client = makeClient()
        let request = GoogleOutboundEnqueueRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 9,
            operation: .delete,
            approvalCapability: capability
        )
        let approval = GoogleOutboundApproval(
            previewID: Self.previewID,
            approvalCapability: capability,
            expiresAt: Date().addingTimeInterval(10 * 60)
        )

        for diagnostics in [
            String(describing: request), String(reflecting: request), Self.reflectedChildren(request),
            String(describing: approval), String(reflecting: approval),
            Self.reflectedChildren(approval),
        ] {
            #expect(!diagnostics.contains(capability))
        }

        do {
            _ = try await client.enqueueGoogleOutbound(
                accountID: Self.accountID,
                request: request
            )
            Issue.record("Outbound error response unexpectedly succeeded")
        } catch let error as DayWeaveAPIError {
            let diagnostics = [
                String(describing: error), String(reflecting: error),
                Self.reflectedChildren(error),
            ].joined(separator: " ")
            #expect(!diagnostics.contains(capability))
            guard case let .server(_, _, message, _) = error else {
                Issue.record("Expected a typed server error")
                return
            }
            #expect(message == "capability [redacted] rejected")
        }
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
            GoogleOAuthStartRequest(services: [.tasks]),
            GoogleOAuthStartRequest(services: [.calendar, .tasks]),
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
                role: .readOnly,
                calendarPolicy: .init(publishAllDay: true)
            )
            Issue.record("Outbound publication policy was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.previewGoogleOutbound(
                accountID: Self.accountID,
                request: .init(
                    collectionID: Self.collectionID,
                    itemID: Self.itemID,
                    expectedItemRevision: 0,
                    operation: .upsert
                )
            )
            Issue.record("Zero outbound item revision was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.approveGoogleOutbound(
                accountID: Self.accountID,
                previewID: Self.previewID,
                expectedPreviewHash: String(repeating: "A", count: 64)
            )
            Issue.record("Non-canonical outbound preview hash was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.enqueueGoogleOutbound(
                accountID: Self.accountID,
                request: .init(
                    collectionID: Self.collectionID,
                    itemID: Self.itemID,
                    expectedItemRevision: 9,
                    operation: .upsert,
                    approvalCapability: "true"
                )
            )
            Issue.record("Invalid outbound approval capability was accepted")
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

        do {
            _ = try DayWeaveAPIClient(
                baseURL: baseURL,
                session: URLProtocolStub.makeSession(),
                durableAuthCoordinator: legacyCoordinator
            )
            Issue.record("Outbound client accepted a legacy credential before journal creation")
        } catch let error as DurableAuthError {
            #expect(error == .enrollmentRequired)
        }

        let unreadableCoordinator = DurableAuthCoordinator(
            stateStore: GoogleAPITestDurableAuthStateStore(
                initial: nil,
                loadFailure: true
            ),
            legacyStore: GoogleAPITestBearerTokenStore()
        )
        do {
            _ = try DayWeaveAPIClient(
                baseURL: baseURL,
                session: URLProtocolStub.makeSession(),
                durableAuthCoordinator: unreadableCoordinator
            )
            Issue.record("Outbound client manufactured a binding after a Keychain read failure")
        } catch let error as DurableAuthError {
            #expect(error == .localStateUnavailable)
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

    @Test("writable Task-list responses require a read-only-safe Calendar policy")
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
                body: Self.collectionEnvelope(
                    revision: 4,
                    selected: true,
                    visible: true,
                    role: "writable",
                    publicationEnabled: true
                )
            ),
            .init(
                statusCode: 200,
                body: Self.collectionEnvelope(
                    revision: 4,
                    selected: true,
                    visible: true,
                    role: "writable",
                    kind: "task_list",
                    publicationEnabled: true
                )
            ),
            .init(
                statusCode: 200,
                body: Self.collectionEnvelope(
                    revision: 4,
                    selected: true,
                    visible: true,
                    role: "writable",
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
        let publicationPolicy = GoogleCalendarPolicy(
            publishAllDay: true,
            publishTentative: true,
            publishFree: true
        )
        let writableCalendar = try await client.configureGoogleCollection(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedRevision: 3,
            selected: true,
            visible: true,
            role: .writable,
            calendarPolicy: publicationPolicy
        )
        #expect(writableCalendar.kind == .calendar)
        #expect(writableCalendar.syncRole == .writable)
        #expect(writableCalendar.calendarPolicy == publicationPolicy)
        do {
            _ = try await client.configureGoogleCollection(
                accountID: Self.accountID,
                collectionID: Self.collectionID,
                expectedRevision: 3,
                selected: true,
                visible: true,
                role: .writable,
                calendarPolicy: publicationPolicy
            )
            Issue.record("A writable Task-list response with Calendar publication policy was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
        let writableTaskList = try await client.configureGoogleCollection(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedRevision: 3,
            selected: true,
            visible: true,
            role: .writable,
            calendarPolicy: .init()
        )
        #expect(writableTaskList.kind == .taskList)
        #expect(writableTaskList.syncRole == .writable)
        #expect(writableTaskList.calendarPolicy.isReadOnlySafe)
        let existingWritable = try await client.googleCollections(accountID: Self.accountID)
        #expect(existingWritable.first?.syncRole == .writable)
    }

    @Test("generated schedule publication uses direct-root strict contract and redacts capability")
    func generatedSchedulePublicationContract() async throws {
        let current = Date(timeIntervalSince1970: 1_788_425_200)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let starts = formatter.string(from: current.addingTimeInterval(3_600))
        let ends = formatter.string(from: current.addingTimeInterval(7_200))
        let expires = formatter.string(from: current.addingTimeInterval(600))
        let completed = formatter.string(from: current.addingTimeInterval(10))
        let capability = Self.scheduleApprovalCapability
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Data(
                """
                {
                  "id":"\(Self.schedulePreviewID.uuidString.lowercased())",
                  "account_id":"\(Self.accountID.uuidString.lowercased())",
                  "collection_id":"\(Self.collectionID.uuidString.lowercased())",
                  "collection_revision":4,
                  "collection_display_name":"Primary calendar",
                  "schedule_revision_id":"\(Self.scheduleRevisionID.uuidString.lowercased())",
                  "schedule_revision_number":12,
                  "preview_hash":"\(Self.previewHash)",
                  "create_count":1,
                  "update_count":0,
                  "delete_count":0,
                  "noop_count":0,
                  "changes":[{
                    "ordinal":0,
                    "slot_id":"\(Self.scheduleSlotID.uuidString.lowercased())",
                    "source_block_id":"\(Self.scheduleBlockID.uuidString.lowercased())",
                    "operation":"create",
                    "provider_resource_id":null,
                    "provider_etag":null,
                    "summary":"Busy",
                    "starts_at":"\(starts)",
                    "ends_at":"\(ends)"
                  }],
                  "expires_at":"\(expires)"
                }
                """.utf8
            )),
            .init(statusCode: 200, body: Data(
                """
                {"preview_id":"\(Self.schedulePreviewID.uuidString.lowercased())","approval_capability":"\(capability)","expires_at":"\(expires)"}
                """.utf8
            )),
            .init(statusCode: 202, body: Data(
                """
                {"publication_id":"\(Self.schedulePublicationID.uuidString.lowercased())","replayed":false}
                """.utf8
            )),
            .init(statusCode: 200, body: Data(
                """
                {
                  "publication_id":"\(Self.schedulePublicationID.uuidString.lowercased())",
                  "account_id":"\(Self.accountID.uuidString.lowercased())",
                  "collection_id":"\(Self.collectionID.uuidString.lowercased())",
                  "schedule_revision_id":"\(Self.scheduleRevisionID.uuidString.lowercased())",
                  "state":"published",
                  "total_count":1,
                  "pending_count":0,
                  "delivering_count":0,
                  "published_count":1,
                  "conflicted_count":0,
                  "failed_count":0,
                  "superseded_count":0,
                  "created_at":"\(formatter.string(from: current))",
                  "completed_at":"\(completed)",
                  "last_error_code":null
                }
                """.utf8
            ))
        )
        let client = makeClient(now: { current })
        let preview = try await client.previewGoogleSchedulePublication(
            accountID: Self.accountID,
            request: .init(
                collectionID: Self.collectionID,
                expectedScheduleRevisionID: Self.scheduleRevisionID
            )
        )
        #expect(preview.changes.first?.summary == "Busy")
        let approval = try await client.approveGoogleSchedulePublication(
            accountID: Self.accountID,
            previewID: preview.id,
            expectedPreviewHash: preview.previewHash
        )
        let accepted = try await client.enqueueGoogleSchedulePublication(
            accountID: Self.accountID,
            request: .init(
                previewID: preview.id,
                collectionID: preview.collectionID,
                expectedScheduleRevisionID: preview.scheduleRevisionID,
                approvalCapability: approval.approvalCapability
            )
        )
        let status = try await client.googleSchedulePublicationStatus(
            accountID: Self.accountID,
            publicationID: accepted.publicationID
        )
        #expect(status.state == .published)

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        #expect(requests.suffix(4).map(\.url.path) == [
            "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())/schedule-publications/previews",
            "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())/schedule-publications/previews/\(Self.schedulePreviewID.uuidString.lowercased())/approve",
            "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())/schedule-publications",
            "/gateway/v1/integrations/google/accounts/\(Self.accountID.uuidString.lowercased())/schedule-publications/\(Self.schedulePublicationID.uuidString.lowercased())",
        ])
        let enqueueBody = try #require(requests.last(where: {
            $0.url.path.hasSuffix("/schedule-publications")
        })?.jsonBody)
        #expect(Set(enqueueBody.keys) == [
            "preview_id", "collection_id", "expected_schedule_revision_id",
            "approval_capability",
        ])
        #expect(enqueueBody["approval_capability"] as? String == capability)
        let diagnostic = DayWeaveDiagnosticSanitizer.text(
            "transport failed with \(capability)",
            secrets: [],
            maximumCharacters: 200
        )
        #expect(diagnostic == "transport failed with [redacted]")
        #expect(!String(reflecting: approval).contains(capability))
    }

    @Test("generated schedule responses reject unknown and duplicate direct-root fields")
    func generatedScheduleRejectsUnknownFields() async throws {
        let current = Date(timeIntervalSince1970: 1_788_425_200)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 202, body: Data(
                """
                {"publication_id":"\(Self.schedulePublicationID.uuidString.lowercased())","replayed":false,"unknown":true}
                """.utf8
            ))
        )
        let client = makeClient(now: { current })
        do {
            _ = try await client.enqueueGoogleSchedulePublication(
                accountID: Self.accountID,
                request: .init(
                    previewID: Self.schedulePreviewID,
                    collectionID: Self.collectionID,
                    expectedScheduleRevisionID: Self.scheduleRevisionID,
                    approvalCapability: Self.scheduleApprovalCapability
                )
            )
            Issue.record("An unknown schedule acceptance field was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }

        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 202, body: Data(
                """
                {"publication_id":"\(Self.schedulePublicationID.uuidString.lowercased())","publication_id":"\(Self.schedulePublicationID.uuidString.lowercased())","replayed":false}
                """.utf8
            ))
        )
        do {
            _ = try await client.enqueueGoogleSchedulePublication(
                accountID: Self.accountID,
                request: .init(
                    previewID: Self.schedulePreviewID,
                    collectionID: Self.collectionID,
                    expectedScheduleRevisionID: Self.scheduleRevisionID,
                    approvalCapability: Self.scheduleApprovalCapability
                )
            )
            Issue.record("A duplicate schedule acceptance field was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
        _ = formatter
    }

    private func makeClient(
        now: @escaping @Sendable () -> Date = Date.init
    ) -> DayWeaveAPIClient {
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
            authCoordinator: coordinator,
            now: now
        )
    }

    private func expectResponseDecodingFailure(
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            Issue.record("An out-of-bounds outbound expiry was accepted")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        } catch {
            Issue.record("An unexpected outbound expiry error was returned")
        }
    }

    private static let baseURL = try! DayWeaveAPIBaseURL("https://api.example.com/gateway")
    private static let apiToken = "google-integration-test-token"
    private static let accountID = UUID(uuidString: "11111111-aaaa-4aaa-8aaa-111111111111")!
    private static let refreshRequestID = UUID(
        uuidString: "11111111-bbbb-4bbb-8bbb-111111111111"
    )!
    private static let otherAccountID = UUID(uuidString: "22222222-bbbb-4bbb-8bbb-222222222222")!
    private static let collectionID = UUID(uuidString: "33333333-cccc-4ccc-8ccc-333333333333")!
    private static let itemID = UUID(uuidString: "44444444-dddd-4ddd-8ddd-444444444444")!
    private static let previewID = UUID(uuidString: "77777777-aaaa-4aaa-8aaa-777777777777")!
    private static let outboxID = UUID(uuidString: "88888888-bbbb-4bbb-8bbb-888888888888")!
    private static let previewHash = String(repeating: "a", count: 64)
    private static let schedulePreviewID = UUID(
        uuidString: "99999999-aaaa-4aaa-8aaa-999999999999"
    )!
    private static let scheduleRevisionID = UUID(
        uuidString: "99999999-bbbb-4bbb-8bbb-999999999999"
    )!
    private static let schedulePublicationID = UUID(
        uuidString: "99999999-cccc-4ccc-8ccc-999999999999"
    )!
    private static let scheduleSlotID = UUID(
        uuidString: "99999999-dddd-4ddd-8ddd-999999999999"
    )!
    private static let scheduleBlockID = UUID(
        uuidString: "99999999-eeee-4eee-8eee-999999999999"
    )!
    private static let scheduleApprovalCapability: String = {
        let payload = Data([UInt8](repeating: 11, count: 32)).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return "dw_gsa1_" + payload
    }()
    private static let approvalCapability: String = {
        let payload = Data([UInt8](repeating: 7, count: 32)).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return "dw_" + "ga1_" + payload
    }()

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
        kind: String = "calendar",
        publicationEnabled: Bool = false
    ) -> Data {
        Data(
            "{\"collection\":\(collectionObject(revision: revision, selected: selected, visible: visible, role: role, kind: kind, publicationEnabled: publicationEnabled))}".utf8
        )
    }

    private static func collectionObject(
        revision: UInt64,
        accountID: UUID = accountID,
        selected: Bool = false,
        visible: Bool = true,
        role: String = "read_only",
        kind: String = "calendar",
        publicationEnabled: Bool = false
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
          "calendar_policy":\(calendarPolicyObject(publicationEnabled: publicationEnabled)),
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

    private static func calendarPolicyObject(publicationEnabled: Bool = false) -> String {
        """
        {
          "confirmed_busy":"blocking",
          "tentative":"visible_nonblocking",
          "free":"visible_nonblocking",
          "all_day":"visible_nonblocking",
          "publish_all_day":\(publicationEnabled),
          "publish_tentative":\(publicationEnabled),
          "publish_free":\(publicationEnabled)
        }
        """
    }

    private static func outboundPreviewEnvelope(
        expiry: Date = Date().addingTimeInterval(10 * 60),
        extraEnvelopeField: Bool = false,
        extraPreviewField: Bool = false
    ) -> Data {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let expiry = formatter.string(from: expiry)
        let previewExtra = extraPreviewField ? ",\"unknown\":true" : ""
        let envelopeExtra = extraEnvelopeField ? ",\"unknown\":true" : ""
        return Data(
            """
            {
              "preview":{
                "id":"\(previewID.uuidString.lowercased())",
                "account_id":"\(accountID.uuidString.lowercased())",
                "collection_id":"\(collectionID.uuidString.lowercased())",
                "collection_revision":4,
                "collection_display_name":"Primary calendar",
                "item_id":"\(itemID.uuidString.lowercased())",
                "item_revision":9,
                "entity_kind":"calendar_event",
                "operation":"upsert",
                "provider_resource_id":null,
                "provider_etag":null,
                "preview_hash":"\(previewHash)",
                "provider_payload":{"summary":"Private planning canary"},
                "expires_at":"\(expiry)"\(previewExtra)
              }\(envelopeExtra)
            }
            """.utf8
        )
    }

    private static func validTaskProviderPayload() -> [String: Any] {
        [
            "id": "",
            "etag": NSNull(),
            "title": "Private task",
            "notes": "First line\nSecond line",
            "status": "completed",
            "due": "2026-09-03T18:00:00+00:00",
            "completed": "2026-09-02T09:30:00Z",
            "updated": NSNull(),
            "parent": NSNull(),
            "position": NSNull(),
            "links": NSNull(),
            "deleted": false,
            "hidden": false,
        ]
    }

    private static func outboundTaskPreviewEnvelope(
        payload: [String: Any],
        operation: String = "upsert",
        existing: Bool = false
    ) throws -> Data {
        var envelope = try #require(
            JSONSerialization.jsonObject(with: outboundPreviewEnvelope())
                as? [String: Any]
        )
        var preview = try #require(envelope["preview"] as? [String: Any])
        preview["collection_display_name"] = "Personal tasks"
        preview["entity_kind"] = "task"
        preview["operation"] = operation
        preview["provider_resource_id"] = existing ? "provider-task-id" : NSNull()
        preview["provider_etag"] = existing ? "provider-etag" : NSNull()
        preview["provider_payload"] = payload
        envelope["preview"] = preview
        return try JSONSerialization.data(withJSONObject: envelope)
    }

    private static func outboundApprovalEnvelope(
        expiry: Date = Date().addingTimeInterval(10 * 60),
        extraApprovalField: Bool = false
    ) -> Data {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let expiry = formatter.string(from: expiry)
        let extra = extraApprovalField ? ",\"unknown\":true" : ""
        return Data(
            """
            {"approval":{
              "preview_id":"\(previewID.uuidString.lowercased())",
              "approval_capability":"\(approvalCapability)",
              "expires_at":"\(expiry)"\(extra)
            }}
            """.utf8
        )
    }

    private static func outboundAcceptedEnvelope(extraOutboundField: Bool = false) -> Data {
        let extra = extraOutboundField ? ",\"unknown\":true" : ""
        return Data(
            "{\"outbound\":{\"outbox_id\":\"\(outboxID.uuidString.lowercased())\",\"replayed\":false\(extra)}}".utf8
        )
    }

    private static func outboundPreviewWithDuplicatePayloadKey() -> Data {
        replacing(
            in: outboundPreviewEnvelope(),
            target: #""summary":"Private planning canary""#,
            replacement: #""summary":"Private planning canary","\u0073ummary":"forged""#
        )
    }

    private static func outboundApprovalWithDuplicateKey() -> Data {
        let identity = previewID.uuidString.lowercased()
        return replacing(
            in: outboundApprovalEnvelope(),
            target: "\"preview_id\":\"\(identity)\"",
            replacement: "\"preview_id\":\"\(identity)\",\"\\u0070review_id\":\"\(identity)\""
        )
    }

    private static func outboundAcceptedWithDuplicateKey() -> Data {
        replacing(
            in: outboundAcceptedEnvelope(),
            target: "\"replayed\":false",
            replacement: "\"replayed\":false,\"\\u0072eplayed\":true"
        )
    }

    private static func oversizedOutboundPreviewEnvelope() throws -> Data {
        var envelope = try #require(
            JSONSerialization.jsonObject(with: outboundPreviewEnvelope())
                as? [String: Any]
        )
        var preview = try #require(envelope["preview"] as? [String: Any])
        preview["provider_payload"] = [
            "values": [Any](repeating: NSNull(), count: 20_001),
        ]
        envelope["preview"] = preview
        return try JSONSerialization.data(withJSONObject: envelope)
    }

    private static func replacing(
        in data: Data,
        target: String,
        replacement: String
    ) -> Data {
        Data(
            String(decoding: data, as: UTF8.self)
                .replacingOccurrences(of: target, with: replacement)
                .utf8
        )
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
    private let loadFailure: Bool

    init(initial: DurableAuthEnvelope?, loadFailure: Bool = false) {
        envelope = initial
        self.loadFailure = loadFailure
    }

    func loadEnvelope() throws -> DurableAuthEnvelope? {
        if loadFailure { throw GoogleAPITestStateFailure.unreadable }
        return lock.withLock { envelope }
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

private enum GoogleAPITestStateFailure: Error {
    case unreadable
}

private struct GoogleAPITestBearerTokenStore: BearerTokenStoring {
    func loadCredential() -> OriginBoundBearerCredential? { nil }
    func saveCredential(_ credential: OriginBoundBearerCredential) {}
    func deleteCredential() {}
}
#endif
