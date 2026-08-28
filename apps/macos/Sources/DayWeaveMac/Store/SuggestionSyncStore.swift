import Foundation

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
    private let session: URLSession
    private let now: @Sendable () -> Date
    private var refreshID: UUID?
    private var configurationGeneration: UInt64 = 0

    init(
        configurationStore: any SuggestionAPIConfigurationStoring = UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        session: URLSession = .shared,
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.configurationStore = configurationStore
        self.tokenStore = tokenStore
        self.session = session
        self.now = now

        let baseURLString = configurationStore.loadBaseURL() ?? ""
        self.baseURLString = baseURLString
        do {
            let hasToken = try tokenStore.loadToken() != nil
            tokenConfigured = hasToken
            status = Self.initialStatus(baseURLString: baseURLString, tokenConfigured: hasToken)
        } catch {
            tokenConfigured = false
            status = .failed(error.localizedDescription)
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
        do {
            let validatedURL = try DayWeaveAPIBaseURL(baseURL)
            let trimmedToken = newToken.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedToken.isEmpty {
                try tokenStore.saveToken(trimmedToken)
            } else if !tokenConfigured {
                throw BearerTokenStoreError.emptyToken
            }

            configurationStore.saveBaseURL(validatedURL.url.absoluteString)
            baseURLString = validatedURL.url.absoluteString
            tokenConfigured = true
            configurationGeneration &+= 1
            refreshID = nil
            proposals = []
            status = .ready
            return true
        } catch {
            status = .failed(error.localizedDescription)
            return false
        }
    }

    func clearBearerToken() {
        do {
            try tokenStore.deleteToken()
            tokenConfigured = false
            configurationGeneration &+= 1
            refreshID = nil
            proposals = []
            status = .configurationRequired("Add a bearer token in Settings to load external proposals.")
        } catch {
            status = .failed(error.localizedDescription)
        }
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
        guard beginOperation(proposal) else { return }
        defer { activeProposalIDs.remove(proposal.id) }
        guard let client = makeClient() else { return }
        let generation = configurationGeneration

        do {
            _ = try await client.acceptSuggestion(
                id: proposal.id,
                expectedRevision: proposal.revision,
                note: "Approved in the macOS Suggestions Inbox; schedule unchanged"
            )
            guard configurationGeneration == generation else { return }
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
        guard beginOperation(proposal) else { return }
        defer { activeProposalIDs.remove(proposal.id) }
        guard let client = makeClient() else { return }
        let generation = configurationGeneration

        do {
            _ = try await client.rejectSuggestion(
                id: proposal.id,
                expectedRevision: proposal.revision,
                note: "Rejected in the macOS Suggestions Inbox"
            )
            guard configurationGeneration == generation else { return }
            proposals.removeAll { $0.id == proposal.id }
            status = .online(updatedAt: now(), message: "Proposal rejected; schedule unchanged")
        } catch {
            guard configurationGeneration == generation else { return }
            status = .failed(error.localizedDescription)
        }
    }

    func edit(_ proposal: DayWeaveProposal, title: String, explanation: String) async -> Bool {
        guard beginOperation(proposal) else { return false }
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
        guard let client = makeClient() else { return false }
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
            guard configurationGeneration == generation else { return false }
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

    private func beginOperation(_ proposal: DayWeaveProposal) -> Bool {
        guard refreshID == nil else {
            status = .failed("Wait for the proposal refresh to finish before taking action.")
            return false
        }
        guard proposal.status == .pending else {
            status = .failed("Only pending proposals can be changed.")
            return false
        }
        guard !activeProposalIDs.contains(proposal.id) else { return false }
        activeProposalIDs.insert(proposal.id)
        return true
    }

    private func makeClient() -> DayWeaveAPIClient? {
        guard tokenConfigured else {
            status = .configurationRequired("Add a bearer token in Settings to load external proposals.")
            return nil
        }
        do {
            return DayWeaveAPIClient(
                baseURL: try DayWeaveAPIBaseURL(baseURLString),
                session: session,
                tokenStore: tokenStore
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
