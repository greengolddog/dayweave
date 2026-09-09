import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Independent progress foreground polling", .serialized)
@MainActor
struct ItemProgressPollingLoopTests {
    @Test("failure backoff is bounded and reconnect resets it without cancelling an active operation")
    func boundedRetryAndReconnect() async throws {
        let sleeper = ProgressLifecycleSleeper()
        var calls = 0
        let loop = ItemProgressPollingLoop(sleep: { try await sleeper.wait($0) }) {
            calls += 1; return .unavailable
        }
        defer { loop.stop() }
        for (index, seconds) in [5, 10, 20, 40, 60, 60].enumerated() {
            try #require(await eventuallyProgress { calls == index + 1 && sleeper.hasDelay(.seconds(seconds)) })
            if index < 5 { sleeper.advance(.seconds(seconds)) }
        }
        loop.wake()
        try #require(await eventuallyProgress { calls == 7 && sleeper.hasDelay(.seconds(5)) })
        loop.stop()
        try #require(await eventuallyProgress { sleeper.count == 0 })
        loop.wake()
        for _ in 0..<30 { await Task.yield() }
        #expect(calls == 7)
    }

    @Test("wake coalesces while work is held; local contention retries quickly without network backoff")
    func wakeDoesNotCancelInFlightWork() async throws {
        let sleeper = ProgressLifecycleSleeper(), operationGate = ProgressLifecycleSleeper()
        var calls = 0, wasCancelled = false
        let loop = ItemProgressPollingLoop(sleep: { try await sleeper.wait($0) }) {
            calls += 1
            if calls == 1 {
                do { try await operationGate.wait(.seconds(99)) } catch { wasCancelled = true }
                return .unavailable
            }
            return calls == 2 ? .busy : .success
        }
        defer { loop.stop() }
        try #require(await eventuallyProgress { operationGate.count == 1 })
        loop.wake(); loop.wake(); loop.wake()
        #expect(calls == 1 && !wasCancelled && operationGate.count == 1)
        operationGate.advance(.seconds(99))
        try #require(await eventuallyProgress { calls == 2 && sleeper.hasDelay(.seconds(1)) })
        sleeper.advance(.seconds(1))
        try #require(await eventuallyProgress { calls == 3 && sleeper.hasDelay(.seconds(5)) })
        #expect(!wasCancelled)
    }
}

@MainActor
func eventuallyProgress(_ predicate: @MainActor () async -> Bool) async -> Bool {
    for _ in 0..<10_000 {
        if await predicate() { return true }
        await Task.yield()
    }
    return false
}

@MainActor
final class ProgressLifecycleSleeper {
    private struct Entry {
        let id: UUID
        let duration: Duration
        let continuation: CheckedContinuation<Void, Error>
    }
    private var entries: [Entry] = []
    var count: Int { entries.count }
    func hasDelay(_ duration: Duration) -> Bool { entries.contains { $0.duration == duration } }
    func wait(_ duration: Duration) async throws {
        let id = UUID()
        try await withTaskCancellationHandler {
            try Task.checkCancellation()
            try await withCheckedThrowingContinuation { continuation in
                entries.append(.init(id: id, duration: duration, continuation: continuation))
            }
        } onCancel: {
            Task { @MainActor in self.cancel(id) }
        }
    }
    func advance(_ duration: Duration) {
        guard let index = entries.firstIndex(where: { $0.duration == duration }) else { return }
        entries.remove(at: index).continuation.resume()
    }
    private func cancel(_ id: UUID) {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return }
        entries.remove(at: index).continuation.resume(throwing: CancellationError())
    }
}
#endif
