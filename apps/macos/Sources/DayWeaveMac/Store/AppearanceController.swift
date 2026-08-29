import Combine
import Foundation
import SwiftUI

enum DayWeaveAppearanceMode: String, CaseIterable, Codable, Identifiable, Sendable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

enum DayWeaveAccent: String, CaseIterable, Codable, Identifiable, Sendable {
    case blue
    case indigo
    case purple
    case pink
    case orange
    case green
    case teal

    var id: String { rawValue }

    var title: String {
        rawValue.capitalized
    }

    var color: Color {
        switch self {
        case .blue: .blue
        case .indigo: .indigo
        case .purple: .purple
        case .pink: .pink
        case .orange: .orange
        case .green: .green
        case .teal: .teal
        }
    }
}

struct DayWeaveAppearancePreferences: Codable, Equatable, Sendable {
    static let currentVersion = 1

    let version: Int
    var mode: DayWeaveAppearanceMode
    var accent: DayWeaveAccent

    init(mode: DayWeaveAppearanceMode, accent: DayWeaveAccent) {
        version = Self.currentVersion
        self.mode = mode
        self.accent = accent
    }

    static let defaults = DayWeaveAppearancePreferences(mode: .system, accent: .blue)
}

enum AppearancePreferencesStoreError: Error, Equatable, Sendable {
    case invalidStoredPreferences
    case unsupportedVersion(Int)
    case writeFailed
}

@MainActor
protocol AppearancePreferencesStoring {
    func load() throws -> DayWeaveAppearancePreferences?
    func save(_ preferences: DayWeaveAppearancePreferences) throws
}

@MainActor
final class UserDefaultsAppearancePreferencesStore: AppearancePreferencesStoring {
    static let defaultKey = "dayweave.appearance.preferences-v1"

    private let defaults: UserDefaults
    private let key: String

    init(
        defaults: UserDefaults = .standard,
        key: String = UserDefaultsAppearancePreferencesStore.defaultKey
    ) {
        self.defaults = defaults
        self.key = key
    }

    func load() throws -> DayWeaveAppearancePreferences? {
        guard let data = defaults.data(forKey: key) else { return nil }
        let preferences: DayWeaveAppearancePreferences
        do {
            preferences = try JSONDecoder().decode(DayWeaveAppearancePreferences.self, from: data)
        } catch {
            throw AppearancePreferencesStoreError.invalidStoredPreferences
        }
        guard preferences.version == DayWeaveAppearancePreferences.currentVersion else {
            throw AppearancePreferencesStoreError.unsupportedVersion(preferences.version)
        }
        return preferences
    }

    func save(_ preferences: DayWeaveAppearancePreferences) throws {
        guard preferences.version == DayWeaveAppearancePreferences.currentVersion else {
            throw AppearancePreferencesStoreError.unsupportedVersion(preferences.version)
        }
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            defaults.set(try encoder.encode(preferences), forKey: key)
        } catch {
            throw AppearancePreferencesStoreError.writeFailed
        }
    }
}

@MainActor
final class AppearanceController: ObservableObject {
    @Published private(set) var preferences: DayWeaveAppearancePreferences
    @Published private(set) var statusMessage: String?

    var preferredColorScheme: ColorScheme? { preferences.mode.colorScheme }
    var accentColor: Color { preferences.accent.color }

    private let store: any AppearancePreferencesStoring

    static func live() -> AppearanceController {
        AppearanceController(store: UserDefaultsAppearancePreferencesStore())
    }

    init(store: any AppearancePreferencesStoring) {
        self.store = store
        do {
            preferences = try store.load() ?? .defaults
        } catch {
            preferences = .defaults
            statusMessage = "Saved appearance settings were invalid and have been reset locally."
        }
    }

    @discardableResult
    func setMode(_ mode: DayWeaveAppearanceMode) -> Bool {
        update { $0.mode = mode }
    }

    @discardableResult
    func setAccent(_ accent: DayWeaveAccent) -> Bool {
        update { $0.accent = accent }
    }

    private func update(
        _ mutation: (inout DayWeaveAppearancePreferences) -> Void
    ) -> Bool {
        var replacement = preferences
        mutation(&replacement)
        guard replacement != preferences else { return true }
        do {
            try store.save(replacement)
        } catch {
            statusMessage = "Appearance settings could not be saved."
            return false
        }
        preferences = replacement
        statusMessage = nil
        return true
    }
}
