import Foundation

private enum SuggestionConfigurationError: LocalizedError {
    case tokenRequiredForOriginChange
    case durableCredentialManagedSeparately

    var errorDescription: String? {
        switch self {
        case .tokenRequiredForOriginChange:
            "Enter the bearer token again when changing the API origin. A saved credential is never forwarded to a different origin."
        case .durableCredentialManagedSeparately:
            "Use the rotating-session controls to replace this credential."
        }
    }
}

private enum SuggestionMutationError: LocalizedError {
    case invalidResponse

    var errorDescription: String? {
        "The suggestions API returned a mutation result with the wrong identity, status, or revision. The local inbox was left unchanged."
    }
}

protocol SuggestionAPIConfigurationStoring: Sendable {
    func loadBaseURL() -> String?
    func saveBaseURL(_ value: String)
}

struct UserDefaultsSuggestionAPIConfigurationStore: SuggestionAPIConfigurationStoring, @unchecked Sendable {
    static let baseURLKey = "dayweave.suggestions-api.base-url.v1"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func loadBaseURL() -> String? {
        defaults.string(forKey: Self.baseURLKey)
    }

    func saveBaseURL(_ value: String) {
        defaults.set(value, forKey: Self.baseURLKey)
    }
}

enum SuggestionSyncStatus: Equatable, Sendable {
    case configurationRequired(String)
    case ready
    case refreshing
    case online(updatedAt: Date, message: String)
    case failed(String)

    var message: String {
        switch self {
        case let .configurationRequired(message), let .failed(message):
            message
        case .ready:
            "Configured — the API has not been checked yet."
        case .refreshing:
            "Refreshing external proposals…"
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
final class SuggestionSyncStore: ObservableObject {
    @Published private(set) var proposals: [DayWeaveProposal] = []
    @Published private(set) var status: SuggestionSyncStatus
    @Published private(set) var tokenConfigured: Bool
    @Published private(set) var activeProposalIDs: Set<UUID> = []
    @Published private(set) var baseURLString: String

    private let configurationStore: any SuggestionAPIConfigurationStoring
    private let tokenStore: any BearerTokenStoring
    private let authCoordinator: DurableAuthCoordinator?
    private let session: URLSession
    private let now: @Sendable () -> Date
    private var refreshID: UUID?
    private var configurationGeneration: UInt64 = 0
    private var proposalsConfigurationIdentifier: String?

    init(
        configurationStore: any SuggestionAPIConfigurationStoring = UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        authCoordinator: DurableAuthCoordinator? = nil,
        session: URLSession = makeDayWeaveEphemeralSession(),
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.configurationStore = configurationStore
        self.tokenStore = tokenStore
        self.authCoordinator = authCoordinator
        self.session = session
        self.now = now

        let baseURLString = configurationStore.loadBaseURL() ?? ""
        self.baseURLString = baseURLString
        if let baseURL = try? DayWeaveAPIBaseURL(baseURLString) {
            do {
                let hasToken: Bool
                if let authCoordinator {
                    hasToken = authCoordinator.hasUsableCredential(boundTo: baseURL)
                } else {
                    hasToken = try tokenStore.loadToken(boundTo: baseURL) != nil
                }
                tokenConfigured = hasToken
                status = Self.initialStatus(baseURLString: baseURLString, tokenConfigured: hasToken)
            } catch {
                tokenConfigured = false
                status = .failed(error.localizedDescription)
            }
        } else {
            tokenConfigured = false
            status = Self.initialStatus(baseURLString: baseURLString, tokenConfigured: false)
        }
    }

    var isConfigured: Bool {
        tokenConfigured && (try? DayWeaveAPIBaseURL(baseURLString)) != nil
    }

    var isRefreshing: Bool {
        refreshID != nil
    }

    @discardableResult
    func applyConfiguration(baseURL: String, newToken: String) -> Bool {
        guard refreshID == nil, activeProposalIDs.isEmpty else {
            status = .failed("Wait for the current proposal operation before changing API credentials.")
            return false
        }
        do {
            let validatedURL = try DayWeaveAPIBaseURL(baseURL)
            let trimmedToken = newToken.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedToken.isEmpty {
                guard authCoordinator == nil else {
                    throw SuggestionConfigurationError.durableCredentialManagedSeparately
                }
                try tokenStore.saveCredential(.init(
                    token: trimmedToken,
                    origin: validatedURL.credentialOriginIdentifier
                ))
            } else {
                let hasCredential: Bool
                if let authCoordinator {
                    hasCredential = authCoordinator.hasUsableCredential(boundTo: validatedURL)
                } else {
                    hasCredential = try tokenStore.loadToken(boundTo: validatedURL) != nil
                }
                guard hasCredential else {
                    throw SuggestionConfigurationError.tokenRequiredForOriginChange
                }
            }

            configurationStore.saveBaseURL(validatedURL.url.absoluteString)
            baseURLString = validatedURL.url.absoluteString
            tokenConfigured = true
            configurationGeneration &+= 1
            refreshID = nil
            proposals = []
            proposalsConfigurationIdentifier = nil
            status = .ready
            return true
        } catch {
            status = .failed(error.localizedDescription)
            return false
        }
    }

    func clearBearerToken() {
        guard refreshID == nil, activeProposalIDs.isEmpty else {
            status = .failed("Wait for the current proposal operation before removing API credentials.")
            return
        }
        do {
            guard authCoordinator == nil else {
                throw SuggestionConfigurationError.durableCredentialManagedSeparately
            }
            try tokenStore.deleteCredential()
            tokenConfigured = false
            configurationGeneration &+= 1
            refreshID = nil
            proposals = []
            proposalsConfigurationIdentifier = nil
            status = .configurationRequired("Add a bearer token in Settings to load external proposals.")
        } catch {
            status = .failed(error.localizedDescription)
        }
    }

    func durableAuthenticationDidChange() {
        guard refreshID == nil, activeProposalIDs.isEmpty else {
            status = .failed("Wait for the current proposal operation before refreshing authentication state.")
            return
        }
        configurationGeneration &+= 1
        proposals = []
        proposalsConfigurationIdentifier = nil
        if let baseURL = try? DayWeaveAPIBaseURL(baseURLString) {
            tokenConfigured = authCoordinator?.hasUsableCredential(boundTo: baseURL)
                ?? ((try? tokenStore.loadToken(boundTo: baseURL))??.isEmpty == false)
        } else {
            tokenConfigured = false
        }
        status = Self.initialStatus(
            baseURLString: baseURLString,
            tokenConfigured: tokenConfigured
        )
    }

    func refresh() async {
        guard refreshID == nil else { return }
        guard activeProposalIDs.isEmpty else {
            status = .failed("Wait for the current proposal action to finish before refreshing.")
            return
        }
        guard let client = makeClient() else { return }

        let requestID = UUID()
        let generation = configurationGeneration
        refreshID = requestID
        status = .refreshing
        defer {
            if refreshID == requestID {
                refreshID = nil
            }
        }

        do {
            let fetched = try await client.listSuggestions(status: .pending, limit: 200)
            guard refreshID == requestID, configurationGeneration == generation else { return }
            proposals = fetched
                .filter { $0.status == .pending }
                .sorted { $0.createdAt > $1.createdAt }
            proposalsConfigurationIdentifier = client.configurationIdentifier
            status = .online(
                updatedAt: now(),
                message: "Loaded \(proposals.count) pending external proposal\(proposals.count == 1 ? "" : "s")"
            )
        } catch {
            guard refreshID == requestID, configurationGeneration == generation else { return }
            status = .failed(error.localizedDescription)
        }
    }

    func accept(_ proposal: DayWeaveProposal) async {
        guard let client = makeClient() else { return }
        guard let proposal = beginOperation(
            proposal,
            configurationIdentifier: client.configurationIdentifier
        ) else { return }
        defer { activeProposalIDs.remove(proposal.id) }
        let generation = configurationGeneration

        do {
            let updated = try await client.acceptSuggestion(
                id: proposal.id,
                expectedRevision: proposal.revision,
                note: "Approved in the macOS Suggestions Inbox; schedule unchanged"
            )
            guard configurationGeneration == generation,
                  proposalsConfigurationIdentifier == client.configurationIdentifier else { return }
            guard updated.id == proposal.id,
                  updated.revision > proposal.revision,
                  updated.status == .accepted else {
                throw SuggestionMutationError.invalidResponse
            }
            proposals.removeAll { $0.id == proposal.id }
            status = .online(
                updatedAt: now(),
                message: "Proposal approved remotely; no schedule changes were applied"
            )
        } catch {
            guard configurationGeneration == generation else { return }
            status = .failed(error.localizedDescription)
        }
    }

    func reject(_ proposal: DayWeaveProposal) async {
        guard let client = makeClient() else { return }
        guard let proposal = beginOperation(
            proposal,
            configurationIdentifier: client.configurationIdentifier
        ) else { return }
        defer { activeProposalIDs.remove(proposal.id) }
        let generation = configurationGeneration

        do {
            let updated = try await client.rejectSuggestion(
                id: proposal.id,
                expectedRevision: proposal.revision,
                note: "Rejected in the macOS Suggestions Inbox"
            )
            guard configurationGeneration == generation,
                  proposalsConfigurationIdentifier == client.configurationIdentifier else { return }
            guard updated.id == proposal.id,
                  updated.revision > proposal.revision,
                  updated.status == .rejected else {
                throw SuggestionMutationError.invalidResponse
            }
            proposals.removeAll { $0.id == proposal.id }
            status = .online(updatedAt: now(), message: "Proposal rejected; schedule unchanged")
        } catch {
            guard configurationGeneration == generation else { return }
            status = .failed(error.localizedDescription)
        }
    }

    func edit(_ proposal: DayWeaveProposal, title: String, explanation: String) async -> Bool {
        guard let client = makeClient() else { return false }
        guard let proposal = beginOperation(
            proposal,
            configurationIdentifier: client.configurationIdentifier
        ) else { return false }
        defer { activeProposalIDs.remove(proposal.id) }

        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let explanation = explanation.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else {
            status = .failed("A proposal title is required.")
            return false
        }
        if proposal.explanation != nil, explanation.isEmpty {
            status = .failed("The current API contract does not support clearing an explanation.")
            return false
        }

        let changedTitle = title == proposal.title ? nil : title
        let changedExplanation = explanation == (proposal.explanation ?? "") ? nil : explanation
        guard changedTitle != nil || changedExplanation != nil else {
            status = .failed("Edit the title or explanation before saving.")
            return false
        }
        let generation = configurationGeneration

        do {
            let updated = try await client.editSuggestion(
                id: proposal.id,
                edit: DayWeaveProposalEdit(
                    expectedRevision: proposal.revision,
                    title: changedTitle,
                    explanation: changedExplanation
                )
            )
            guard configurationGeneration == generation,
                  proposalsConfigurationIdentifier == client.configurationIdentifier else {
                return false
            }
            guard updated.id == proposal.id,
                  updated.revision > proposal.revision,
                  updated.status == .pending,
                  changedTitle == nil || updated.title == changedTitle,
                  changedExplanation == nil || updated.explanation == changedExplanation else {
                throw SuggestionMutationError.invalidResponse
            }
            if let index = proposals.firstIndex(where: { $0.id == updated.id }) {
                proposals[index] = updated
            }
            status = .online(updatedAt: now(), message: "Proposal edited; schedule unchanged")
            return true
        } catch {
            guard configurationGeneration == generation else { return false }
            status = .failed(error.localizedDescription)
            return false
        }
    }

    private func beginOperation(
        _ proposal: DayWeaveProposal,
        configurationIdentifier: String
    ) -> DayWeaveProposal? {
        guard refreshID == nil else {
            status = .failed("Wait for the proposal refresh to finish before taking action.")
            return nil
        }
        guard let current = proposals.first(where: { $0.id == proposal.id }),
              current == proposal,
              current.status == .pending,
              proposalsConfigurationIdentifier == configurationIdentifier else {
            status = .failed(
                "This proposal is no longer bound to the current authenticated session. Refresh the inbox before taking action."
            )
            return nil
        }
        guard !activeProposalIDs.contains(current.id) else { return nil }
        activeProposalIDs.insert(current.id)
        return current
    }

    private func makeClient() -> DayWeaveAPIClient? {
        guard tokenConfigured else {
            status = .configurationRequired("Add a bearer token in Settings to load external proposals.")
            return nil
        }
        do {
            let baseURL = try DayWeaveAPIBaseURL(baseURLString)
            if let authCoordinator {
                guard authCoordinator.hasUsableCredential(boundTo: baseURL) else {
                    status = .configurationRequired("Authenticate this Mac in Settings to load external proposals.")
                    return nil
                }
                return DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    authCoordinator: authCoordinator
                )
            }
            guard let token = try tokenStore.loadToken(boundTo: baseURL), !token.isEmpty else {
                status = .configurationRequired("Add a bearer token in Settings to load external proposals.")
                return nil
            }
            return DayWeaveAPIClient(
                baseURL: baseURL,
                session: session,
                bearerToken: token
            )
        } catch {
            status = .configurationRequired(error.localizedDescription)
            return nil
        }
    }

    private static func initialStatus(
        baseURLString: String,
        tokenConfigured: Bool
    ) -> SuggestionSyncStatus {
        guard !baseURLString.isEmpty else {
            return .configurationRequired("Add the DayWeave API URL in Settings.")
        }
        guard (try? DayWeaveAPIBaseURL(baseURLString)) != nil else {
            return .configurationRequired("The saved DayWeave API URL is invalid. Update it in Settings.")
        }
        guard tokenConfigured else {
            return .configurationRequired("Add a bearer token in Settings to load external proposals.")
        }
        return .ready
    }
}
