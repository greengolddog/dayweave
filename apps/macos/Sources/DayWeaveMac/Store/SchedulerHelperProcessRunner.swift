import Darwin
import Foundation

/// One-shot POSIX runner for the helper. A single monitor owns both `waitpid`
/// and all signals, so a reaped PID can never race a late TERM/KILL.
struct SchedulerHelperProcessRunner: SchedulerHelperProcessRunning, Sendable {
    private let terminationGraceNanoseconds: UInt64

    init(terminationGrace: Duration = .milliseconds(250)) {
        terminationGraceNanoseconds = Self.nanoseconds(terminationGrace) ?? 0
    }

    func run(
        executable: ValidatedSchedulerHelperExecutable,
        standardInput: Data,
        timeout: Duration
    ) async throws -> SchedulerHelperProcessResult {
        try Task.checkCancellation()
        guard standardInput.count <= SchedulerHelperClient.maximumStandardInputBytes else {
            throw SchedulerHelperClientError.inputTooLarge
        }
        guard let timeoutNanoseconds = Self.nanoseconds(timeout), timeoutNanoseconds > 0 else {
            throw SchedulerHelperClientError.timedOut
        }
        let child = try Self.spawn(executable)
        let lifecycle = SchedulerHelperChildLifecycle(pid: child.pid)
        return try await withTaskCancellationHandler {
            let monitor = Task.detached {
                Self.monitor(
                    lifecycle,
                    timeoutNanoseconds: timeoutNanoseconds,
                    graceNanoseconds: terminationGraceNanoseconds
                )
            }
            let input = Task.detached {
                Self.write(
                    standardInput,
                    descriptor: child.standardInput,
                    lifecycle: lifecycle
                )
            }
            let output = Task.detached {
                Self.read(
                    descriptor: child.standardOutput,
                    limit: SchedulerHelperClient.maximumStandardOutputBytes,
                    overflowReason: .standardOutputTooLarge,
                    lifecycle: lifecycle
                )
            }
            let error = Task.detached {
                Self.read(
                    descriptor: child.standardError,
                    limit: SchedulerHelperClient.maximumStandardErrorBytes,
                    overflowReason: .standardErrorTooLarge,
                    lifecycle: lifecycle
                )
            }

            do {
                try SchedulerHelperExecutableValidator.revalidate(executable)
            } catch {
                lifecycle.requestStop(.executableChanged)
            }

            let monitored = await monitor.value
            let inputResult = await input.value
            let outputResult = await output.value
            let errorResult = await error.value

            if Task.isCancelled { throw CancellationError() }
            if monitored.stopReason == .cancelled { throw CancellationError() }
            switch monitored.stopReason {
            case .timedOut:
                throw SchedulerHelperClientError.timedOut
            case .standardOutputTooLarge, .standardErrorTooLarge:
                throw SchedulerHelperClientError.outputTooLarge
            case .executableChanged:
                throw SchedulerHelperClientError.unsafeExecutable
            case .inputOutputFailure:
                throw SchedulerHelperClientError.inputOutputFailure
            case .cancelled, .none:
                break
            }
            if case let .failure(error) = inputResult { throw error }
            if case let .failure(error) = outputResult { throw error }
            if case let .failure(error) = errorResult { throw error }
            guard case let .success(standardOutput) = outputResult,
                  case let .success(standardError) = errorResult else {
                throw SchedulerHelperClientError.inputOutputFailure
            }
            guard let termination = monitored.termination else {
                throw SchedulerHelperClientError.unexpectedTermination
            }
            return SchedulerHelperProcessResult(
                standardOutput: standardOutput,
                standardError: standardError,
                termination: termination
            )
        } onCancel: {
            lifecycle.requestStop(.cancelled)
        }
    }

    private static func spawn(
        _ executable: ValidatedSchedulerHelperExecutable
    ) throws -> SchedulerHelperSpawnedChild {
        var owned = Set<Int32>()
        defer {
            for descriptor in owned { Darwin.close(descriptor) }
        }

        func makePipe() throws -> (read: Int32, write: Int32) {
            var descriptors = [Int32](repeating: -1, count: 2)
            guard Darwin.pipe(&descriptors) == 0 else {
                throw SchedulerHelperClientError.launchFailed
            }
            owned.formUnion(descriptors)
            var normalized: [Int32] = []
            for descriptor in descriptors {
                let duplicate = Darwin.fcntl(descriptor, F_DUPFD_CLOEXEC, STDERR_FILENO + 1)
                guard duplicate >= 0 else {
                    throw SchedulerHelperClientError.launchFailed
                }
                owned.insert(duplicate)
                normalized.append(duplicate)
            }
            for descriptor in descriptors {
                Darwin.close(descriptor)
                owned.remove(descriptor)
            }
            return (normalized[0], normalized[1])
        }

        let standardInput = try makePipe()
        let standardOutput = try makePipe()
        let standardError = try makePipe()
        try setNonBlocking(standardInput.write)
        guard Darwin.fcntl(standardInput.write, F_SETNOSIGPIPE, 1) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
        try setNonBlocking(standardOutput.read)
        try setNonBlocking(standardError.read)

        var actions: posix_spawn_file_actions_t?
        guard posix_spawn_file_actions_init(&actions) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
        defer { posix_spawn_file_actions_destroy(&actions) }
        guard posix_spawn_file_actions_adddup2(&actions, standardInput.read, STDIN_FILENO) == 0,
              posix_spawn_file_actions_adddup2(&actions, standardOutput.write, STDOUT_FILENO) == 0,
              posix_spawn_file_actions_adddup2(&actions, standardError.write, STDERR_FILENO) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
        for descriptor in owned {
            guard posix_spawn_file_actions_addclose(&actions, descriptor) == 0 else {
                throw SchedulerHelperClientError.launchFailed
            }
        }

        var attributes: posix_spawnattr_t?
        guard posix_spawnattr_init(&attributes) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
        defer { posix_spawnattr_destroy(&attributes) }
        var signalMask = sigset_t()
        var defaultSignals = sigset_t()
        guard Darwin.sigemptyset(&signalMask) == 0,
              Darwin.sigemptyset(&defaultSignals) == 0,
              Darwin.sigaddset(&defaultSignals, SIGHUP) == 0,
              Darwin.sigaddset(&defaultSignals, SIGINT) == 0,
              Darwin.sigaddset(&defaultSignals, SIGQUIT) == 0,
              Darwin.sigaddset(&defaultSignals, SIGPIPE) == 0,
              Darwin.sigaddset(&defaultSignals, SIGTERM) == 0,
              posix_spawnattr_setsigmask(&attributes, &signalMask) == 0,
              posix_spawnattr_setsigdefault(&attributes, &defaultSignals) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
        let flags = Int16(
            POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF
        )
        guard posix_spawnattr_setflags(&attributes, flags) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }

        var pid: pid_t = 0
        // Keep this identity check adjacent to posix_spawn, after every pipe
        // and spawn attribute allocation that could otherwise widen the race.
        try SchedulerHelperExecutableValidator.revalidate(executable)
        let spawnResult: Int32 = executable.url.withUnsafeFileSystemRepresentation { path in
            guard let path else { return EINVAL }
            var arguments: [UnsafeMutablePointer<CChar>?] = [
                UnsafeMutablePointer(mutating: path), nil,
            ]
            var environment: [UnsafeMutablePointer<CChar>?] = [nil]
            return arguments.withUnsafeMutableBufferPointer { arguments in
                environment.withUnsafeMutableBufferPointer { environment in
                    posix_spawn(
                        &pid,
                        path,
                        &actions,
                        &attributes,
                        arguments.baseAddress,
                        environment.baseAddress
                    )
                }
            }
        }
        guard spawnResult == 0 else { throw SchedulerHelperClientError.launchFailed }

        Darwin.close(standardInput.read)
        owned.remove(standardInput.read)
        Darwin.close(standardOutput.write)
        owned.remove(standardOutput.write)
        Darwin.close(standardError.write)
        owned.remove(standardError.write)
        owned.remove(standardInput.write)
        owned.remove(standardOutput.read)
        owned.remove(standardError.read)
        return SchedulerHelperSpawnedChild(
            pid: pid,
            standardInput: standardInput.write,
            standardOutput: standardOutput.read,
            standardError: standardError.read
        )
    }

    private static func setNonBlocking(_ descriptor: Int32) throws {
        let flags = Darwin.fcntl(descriptor, F_GETFL)
        guard flags >= 0,
              Darwin.fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0 else {
            throw SchedulerHelperClientError.launchFailed
        }
    }

    private static func write(
        _ data: Data,
        descriptor: Int32,
        lifecycle: SchedulerHelperChildLifecycle
    ) -> Result<Void, SchedulerHelperClientError> {
        defer { Darwin.close(descriptor) }
        var offset = 0
        while offset < data.count {
            if lifecycle.shouldStopIO {
                // A child may emit its complete bounded response and exit
                // before this writer observes EPIPE. Do not let that clean
                // early close mask the authoritative stdout/exit pair.
                return .success(())
            }
            let written: Int = data.withUnsafeBytes { bytes in
                guard let base = bytes.baseAddress else { return 0 }
                return Darwin.write(
                    descriptor,
                    base.advanced(by: offset),
                    data.count - offset
                )
            }
            if written > 0 {
                offset += written
            } else if written < 0, errno == EINTR {
                continue
            } else if written < 0, errno == EAGAIN || errno == EWOULDBLOCK {
                poll(descriptor, events: Int16(POLLOUT))
            } else if written < 0, errno == EPIPE {
                // The helper may reject an envelope before consuming every
                // byte. Its bounded stdout and exit status remain authoritative.
                return .success(())
            } else {
                lifecycle.requestStop(.inputOutputFailure)
                return .failure(.inputOutputFailure)
            }
        }
        return .success(())
    }

    private static func read(
        descriptor: Int32,
        limit: Int,
        overflowReason: SchedulerHelperStopReason,
        lifecycle: SchedulerHelperChildLifecycle
    ) -> Result<Data, SchedulerHelperClientError> {
        defer { Darwin.close(descriptor) }
        var accumulated = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1_024)
        while true {
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, bytes.count)
            }
            if count > 0 {
                guard count <= limit - accumulated.count else {
                    lifecycle.requestStop(overflowReason)
                    return .failure(.outputTooLarge)
                }
                accumulated.append(buffer, count: count)
            } else if count == 0 {
                return .success(accumulated)
            } else if errno == EINTR {
                continue
            } else if errno == EAGAIN || errno == EWOULDBLOCK {
                if lifecycle.isReaped { return .success(accumulated) }
                poll(descriptor, events: Int16(POLLIN))
            } else {
                lifecycle.requestStop(.inputOutputFailure)
                return .failure(.inputOutputFailure)
            }
        }
    }

    private static func poll(_ descriptor: Int32, events: Int16) {
        var descriptor = pollfd(fd: descriptor, events: events, revents: 0)
        while Darwin.poll(&descriptor, 1, 10) < 0, errno == EINTR {}
    }

    private static func monitor(
        _ lifecycle: SchedulerHelperChildLifecycle,
        timeoutNanoseconds: UInt64,
        graceNanoseconds: UInt64
    ) -> SchedulerHelperMonitorResult {
        let startedAt = DispatchTime.now().uptimeNanoseconds
        var termSentAt: UInt64?
        var killSent = false
        while true {
            var status: Int32 = 0
            let waited = Darwin.waitpid(lifecycle.pid, &status, WNOHANG)
            if waited == lifecycle.pid {
                let reason = lifecycle.markReaped()
                return SchedulerHelperMonitorResult(
                    termination: decodeTermination(status),
                    stopReason: reason
                )
            }
            if waited < 0 {
                if errno == EINTR { continue }
                let reason = lifecycle.markReaped()
                return SchedulerHelperMonitorResult(termination: nil, stopReason: reason)
            }

            let now = DispatchTime.now().uptimeNanoseconds
            if now &- startedAt >= timeoutNanoseconds {
                lifecycle.requestStop(.timedOut)
            }
            if lifecycle.stopReason != nil {
                if termSentAt == nil {
                    _ = Darwin.kill(lifecycle.pid, SIGTERM)
                    termSentAt = now
                } else if !killSent, now &- (termSentAt ?? now) >= graceNanoseconds {
                    _ = Darwin.kill(lifecycle.pid, SIGKILL)
                    killSent = true
                }
            }
            Darwin.usleep(2_000)
        }
    }

    private static func decodeTermination(_ status: Int32) -> SchedulerHelperTermination? {
        let waitStatus = status & 0x7f
        if waitStatus == 0 {
            return .exited((status >> 8) & 0xff)
        }
        if waitStatus != 0x7f {
            return .signaled(waitStatus)
        }
        return nil
    }

    private static func nanoseconds(_ duration: Duration) -> UInt64? {
        let components = duration.components
        guard components.seconds >= 0, components.attoseconds >= 0 else { return nil }
        let seconds = UInt64(components.seconds)
        let (whole, overflow) = seconds.multipliedReportingOverflow(by: 1_000_000_000)
        guard !overflow else { return UInt64.max }
        let fractional = UInt64(components.attoseconds / 1_000_000_000)
        let (value, additionOverflow) = whole.addingReportingOverflow(fractional)
        return additionOverflow ? UInt64.max : value
    }
}

private struct SchedulerHelperSpawnedChild: Sendable {
    let pid: pid_t
    let standardInput: Int32
    let standardOutput: Int32
    let standardError: Int32
}

private enum SchedulerHelperStopReason: Equatable, Sendable {
    case cancelled
    case timedOut
    case standardOutputTooLarge
    case standardErrorTooLarge
    case executableChanged
    case inputOutputFailure
}

private struct SchedulerHelperMonitorResult: Sendable {
    let termination: SchedulerHelperTermination?
    let stopReason: SchedulerHelperStopReason?
}

private final class SchedulerHelperChildLifecycle: @unchecked Sendable {
    let pid: pid_t

    private let lock = NSLock()
    private var requestedStop: SchedulerHelperStopReason?
    private var reaped = false

    init(pid: pid_t) {
        self.pid = pid
    }

    var stopReason: SchedulerHelperStopReason? {
        lock.withLock { requestedStop }
    }

    var isReaped: Bool {
        lock.withLock { reaped }
    }

    var shouldStopIO: Bool {
        lock.withLock { reaped || requestedStop != nil }
    }

    func requestStop(_ reason: SchedulerHelperStopReason) {
        lock.withLock {
            if requestedStop == nil, !reaped { requestedStop = reason }
        }
    }

    func markReaped() -> SchedulerHelperStopReason? {
        lock.withLock {
            reaped = true
            return requestedStop
        }
    }
}
