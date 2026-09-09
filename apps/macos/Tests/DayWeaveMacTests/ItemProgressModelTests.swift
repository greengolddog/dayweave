import AppKit
import Foundation
import SwiftUI
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Independent progress exact model")
struct ItemProgressModelTests {
    @Test("shared decimal and Unicode scalar fixtures match")
    func scalarParity() throws {
        let root = try JSONDecoder().decode(ScalarFixture.self, from: fixtureData("values-v1.json"))
        #expect(root.decimals.count == 37 && root.labels.count == 21)
        for value in root.decimals {
            #expect(ItemProgressValidation.decimal(value.value) == value.valid)
        }
        for value in root.labels {
            #expect(ItemProgressValidation.text(value.value, limit: value.max_scalars) == value.valid,
                "Scalar case: \(value.value.unicodeScalars.map(\.value))")
        }
    }

    @Test("all shared component collections accept or reject as a whole")
    func componentParity() throws {
        let root = try fixture("components-v1.json")
        for (key, expected) in [("valid", true), ("invalid", false)] {
            for value in try #require(root[key] as? [[String: Any]]) {
                let data = try JSONSerialization.data(withJSONObject: #require(value["components"]))
                let decoded = try? JSONDecoder().decode([ItemProgressComponent].self, from: data)
                #expect(decoded.map(ItemProgressValidation.components) == true ? expected : !expected)
            }
        }
    }

    @Test("editor normalization is exact and bounded without floating-point conversion")
    func normalizedInput() {
        for (input, expected) in [("0003.5000", "3.5"), ("-0.0", "0"), ("1.0000000", "1"),
            ("-999999999999.999999", "-999999999999.999999")] {
            #expect(ItemProgressValidation.normalizedInputDecimal(input) == expected)
        }
        for input in ["1\n", "1\r", "1e2", "+1", "1.0000001", "1000000000000", String(repeating: "0", count: 65)] {
            #expect(ItemProgressValidation.normalizedInputDecimal(input) == nil)
        }
    }

    @Test("raw duplicate keys and noncanonical integers cannot become progress proof")
    func rawJSONAuthority() {
        for json in [#"{"revision":1,"revision":2}"#, #"{"revision":1,"revi\u0073ion":2}"#,
            #"{"revision":1.0}"#, #"{"revision":1e0}"#, #"{"revision":-0}"#] {
            #expect(!StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: Data(json.utf8)))
        }
        #expect(StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: Data(#"{"revision":1,"target":null}"#.utf8)))
    }

    @Test("an old receipt never overwrites a newer observation")
    func lateReceiptPreservesNewerObservation() throws {
        let id = UUID()
        let newer = ItemProgressSnapshot(itemID: id, itemRevision: 7, revision: 3,
            components: [], updatedAt: "2026-09-08T10:03:00Z")
        let older = ItemProgressSnapshot(itemID: id, itemRevision: 6, revision: 2,
            components: [], updatedAt: "2026-09-08T10:02:00Z")
        var state = ItemProgressState(configurationIdentifier: "synthetic-binding",
            observations: [.init(snapshot: newer, observedAt: Date())], journals: [])
        try state.observe(older, at: Date(), isReadProof: false)
        #expect(state.observations.first?.snapshot == newer)
        #expect(state.observations.first?.isReadProof == true)
    }

    @Test("synthetic three-mode editor renders without production services")
    @MainActor
    func syntheticEditorRender() throws {
        let components = [
            ItemProgressComponent(name: "Draft the guide", value: .percentage(basisPoints: 4_250)),
            ItemProgressComponent(name: "Recorded research", value: .time(elapsedSeconds: 1_200, remainingSeconds: 1_800)),
            ItemProgressComponent(name: "Read the textbook", value: .quantity(current: "3.5", unit: "chapters",
                target: .init(value: "12", direction: .atLeast))),
        ]
        let baseline = ItemProgressSnapshot(itemID: UUID(), itemRevision: 7, revision: 4,
            components: components, updatedAt: "2026-09-08T10:00:00Z")
        let surface = ItemProgressReviewView(context: .init(baseline: baseline, components: components, sensitive: false),
            save: { _ in Issue.record("Synthetic rendering must not save") })
            .background(Color(nsColor: .windowBackgroundColor))
            .environment(\.colorScheme, .light)
        let host = NSHostingView(rootView: surface)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 740, height: 680),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = host
        defer { window.close() }
        host.frame = NSRect(x: 0, y: 0, width: 740, height: 680)
        host.layoutSubtreeIfNeeded()
        // Export is opt-in and displays only this inert synthetic test window.
        // SwiftUI's composited labels otherwise do not enter AppKit's offscreen
        // cache even though embedded native text fields do.
        if ProcessInfo.processInfo.environment["DAYWEAVE_ITEM_PROGRESS_RENDER_DIRECTORY"] != nil {
            window.orderFrontRegardless()
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            host.displayIfNeeded()
        }
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        #expect(bitmap.pixelsWide >= 740 && bitmap.pixelsHigh >= 680)
        if let path = ProcessInfo.processInfo.environment["DAYWEAVE_ITEM_PROGRESS_RENDER_DIRECTORY"] {
            let png = try #require(bitmap.representation(using: .png, properties: [:]))
            try png.write(to: URL(fileURLWithPath: path).appendingPathComponent("macos-item-progress-synthetic.png"))
        }
    }

    private func fixture(_ name: String) throws -> [String: Any] {
        return try #require(JSONSerialization.jsonObject(with: fixtureData(name)) as? [String: Any])
    }

    private func fixtureData(_ name: String) throws -> Data {
        var repo = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repo.deleteLastPathComponent() }
        return try Data(contentsOf: repo.appendingPathComponent("fixtures/item-progress/\(name)"))
    }
    private struct ScalarFixture: Decodable {
        struct Decimal: Decodable { let value: String; let valid: Bool }
        struct Label: Decodable { let value: String; let max_scalars: Int; let valid: Bool }
        let decimals: [Decimal]
        let labels: [Label]
    }
}
#endif
