import Foundation
import Network

/// A reconnect wakes only the delay, never an in-flight write. Cancellation
/// leaves the operation's own authority/lease checks responsible for late replies.
@MainActor
final class ItemProgressPollingLoop {
    enum Outcome { case success, busy, unavailable }
    private var task: Task<Void, Never>?
    private var delayTask: Task<Void, Error>?
    private var wakeVersion: UInt64 = 0

    init(initialDelay: Duration? = nil,
         sleep: @escaping @Sendable (Duration) async throws -> Void,
         operation: @escaping @MainActor () async -> Outcome) {
        task = Task { [weak self] in
            guard let self else { return }
            var retrySeconds: Int64 = 5
            if let initialDelay, !(await self.wait(initialDelay, sleep: sleep)) { return }
            while !Task.isCancelled {
                let startedWake = self.wakeVersion
                let outcome = await operation()
                guard !Task.isCancelled else { return }
                if self.wakeVersion != startedWake { retrySeconds = 5; continue }
                let delay: Duration
                switch outcome {
                case .success: retrySeconds = 5; delay = .seconds(5)
                case .busy: delay = .seconds(1)
                case .unavailable:
                    delay = .seconds(retrySeconds)
                    retrySeconds = min(retrySeconds * 2, 60)
                }
                let beforeDelay = self.wakeVersion
                guard await self.wait(delay, sleep: sleep) else { return }
                if self.wakeVersion != beforeDelay { retrySeconds = 5 }
            }
        }
    }

    func wake() { wakeVersion &+= 1; delayTask?.cancel() }
    func stop() { task?.cancel(); delayTask?.cancel(); task = nil; delayTask = nil }

    private func wait(_ delay: Duration, sleep: @escaping @Sendable (Duration) async throws -> Void) async -> Bool {
        let version = wakeVersion
        let sleeper = Task { try await sleep(delay) }
        delayTask = sleeper
        defer { delayTask = nil }
        do { try await sleeper.value }
        catch { return !Task.isCancelled && wakeVersion != version }
        return !Task.isCancelled
    }
}

enum ItemProgressConnectivity {
    /// A satisfied path is a retry hint, never authentication or server-read proof.
    static func updates() -> AsyncStream<Bool> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let monitor = NWPathMonitor()
            monitor.pathUpdateHandler = { continuation.yield($0.status == .satisfied) }
            continuation.onTermination = { _ in monitor.cancel() }
            monitor.start(queue: DispatchQueue(label: "dayweave.progress.connectivity"))
        }
    }
}
