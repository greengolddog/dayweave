import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("macOS appearance preferences")
@MainActor
struct AppearanceControllerTests {
    @Test("a new profile follows the system with the blue accent")
    func defaults() {
        let controller = AppearanceController(store: TestAppearanceStore())

        #expect(controller.preferences == .defaults)
        #expect(controller.preferences.mode == .system)
        #expect(controller.preferences.accent == .blue)
        #expect(controller.preferredColorScheme == nil)
    }

    @Test("stored appearance is restored before the first view")
    func restoresStoredPreference() {
        let stored = DayWeaveAppearancePreferences(mode: .dark, accent: .purple)
        let controller = AppearanceController(store: TestAppearanceStore(stored: stored))

        #expect(controller.preferences == stored)
        #expect(controller.preferredColorScheme == .dark)
    }

    @Test("theme and accent changes persist independently")
    func changesAreDurable() {
        let store = TestAppearanceStore()
        let controller = AppearanceController(store: store)

        #expect(controller.setMode(.light))
        #expect(controller.setAccent(.teal))
        #expect(controller.preferences == DayWeaveAppearancePreferences(
            mode: .light,
            accent: .teal
        ))
        #expect(store.saved == [
            DayWeaveAppearancePreferences(mode: .light, accent: .blue),
            DayWeaveAppearancePreferences(mode: .light, accent: .teal),
        ])
    }

    @Test("a failed save does not create a false live selection")
    func failedSaveKeepsPreviousSelection() {
        let store = TestAppearanceStore(saveError: .writeFailed)
        let controller = AppearanceController(store: store)

        #expect(!controller.setAccent(.orange))
        #expect(controller.preferences == .defaults)
        #expect(controller.statusMessage != nil)
    }

    @Test("invalid stored appearance resets without affecting planner data")
    func invalidRecordResetsLocally() {
        let controller = AppearanceController(store: TestAppearanceStore(
            loadError: .invalidStoredPreferences
        ))

        #expect(controller.preferences == .defaults)
        #expect(controller.statusMessage != nil)
    }

    @Test("the UserDefaults appearance record is versioned and bounded")
    func userDefaultsRecordRoundTrips() throws {
        let suiteName = "com.greengolddog.dayweave.tests.appearance.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let key = "test-appearance"
        let store = UserDefaultsAppearancePreferencesStore(defaults: defaults, key: key)
        let preferences = DayWeaveAppearancePreferences(mode: .dark, accent: .green)

        try store.save(preferences)
        #expect(try store.load() == preferences)

        defaults.set(Data("malformed".utf8), forKey: key)
        #expect(throws: AppearancePreferencesStoreError.invalidStoredPreferences) {
            try store.load()
        }
    }
}
#endif

@MainActor
private final class TestAppearanceStore: AppearancePreferencesStoring {
    let stored: DayWeaveAppearancePreferences?
    let loadError: AppearancePreferencesStoreError?
    let saveError: AppearancePreferencesStoreError?
    private(set) var saved: [DayWeaveAppearancePreferences] = []

    init(
        stored: DayWeaveAppearancePreferences? = nil,
        loadError: AppearancePreferencesStoreError? = nil,
        saveError: AppearancePreferencesStoreError? = nil
    ) {
        self.stored = stored
        self.loadError = loadError
        self.saveError = saveError
    }

    func load() throws -> DayWeaveAppearancePreferences? {
        if let loadError { throw loadError }
        return stored
    }

    func save(_ preferences: DayWeaveAppearancePreferences) throws {
        if let saveError { throw saveError }
        saved.append(preferences)
    }
}
