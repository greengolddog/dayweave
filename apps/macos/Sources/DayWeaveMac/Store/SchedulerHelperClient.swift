import Darwin
import CoreFoundation
import Foundation
import Security

protocol LocalScheduleComposing: Sendable {
    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition
}

/// An explicit v2 boundary. A witness is exact input, not permission to publish
/// or execute the result; callers must separately fence its durable installation.
protocol RoutineOccurrenceScheduleComposing: Sendable {
    func composeOccurrences(
        canonicalItems: [DayWeaveCanonicalItem],
        witness: RoutinePlanningWitness
    ) async throws -> RoutineOccurrenceLocalComposition
}

struct RoutineOccurrenceLocalComposition: Equatable, Sendable {
    let composition: LocalScheduleComposition
    let occurrenceSnapshotRevision: UInt64
    /// Exact validated helper-v2 stdout. Display timestamps must never be
    /// re-encoded from the lossy Foundation Date projection for custody.
    let rawOutput: Data
}

enum SchedulerHelperClientError: Error, Equatable, LocalizedError, Sendable {
    case unsupportedCanonicalItem
    case helperUnavailable
    case unsafeExecutable
    case invalidCodeSignature
    case inputTooLarge
    case outputTooLarge
    case launchFailed
    case inputOutputFailure
    case timedOut
    case invalidResponse
    case requestRejected
    case unexpectedTermination

    var errorDescription: String? {
        switch self {
        case .unsupportedCanonicalItem:
            "A canonical item cannot be represented safely by the local scheduler."
        case .helperUnavailable:
            "The local scheduler is unavailable."
        case .unsafeExecutable, .invalidCodeSignature:
            "The local scheduler could not be verified."
        case .inputTooLarge:
            "The local scheduling request is too large."
        case .outputTooLarge:
            "The local scheduler response is too large."
        case .launchFailed:
            "The local scheduler could not be started."
        case .inputOutputFailure:
            "The local scheduler communication channel failed."
        case .timedOut:
            "The local scheduler timed out."
        case .invalidResponse:
            "The local scheduler returned an invalid response."
        case .requestRejected:
            "The local scheduler rejected the request."
        case .unexpectedTermination:
            "The local scheduler stopped unexpectedly."
        }
    }
}

struct SchedulerHelperLocation: Equatable, Sendable {
    let bundleURL: URL
    let executableURL: URL
}

protocol SchedulerHelperLocating: Sendable {
    func locate() throws -> SchedulerHelperLocation
}

struct BundledSchedulerHelperLocator: SchedulerHelperLocating {
    private let bundleURL: URL

    init(bundleURL: URL = Bundle.main.bundleURL) {
        self.bundleURL = bundleURL
    }

    func locate() throws -> SchedulerHelperLocation {
        SchedulerHelperLocation(
            bundleURL: bundleURL,
            executableURL: bundleURL
                .appendingPathComponent("Contents", isDirectory: true)
                .appendingPathComponent("Helpers", isDirectory: true)
                .appendingPathComponent("dayweave-scheduler-helper", isDirectory: false)
        )
    }
}

struct SchedulerHelperExecutableIdentity: Equatable, Sendable {
    let device: UInt64
    let inode: UInt64
    let size: UInt64
    let mode: mode_t
    let linkCount: UInt64
    let owner: uid_t
    let group: gid_t
    let changeTimeSeconds: Int64
    let changeTimeNanoseconds: Int64
}

struct ValidatedSchedulerHelperExecutable: Equatable, Sendable {
    let url: URL
    let identity: SchedulerHelperExecutableIdentity
}

enum SchedulerHelperExecutableValidator {
    private static let relativeComponents = [
        "Contents", "Helpers", "dayweave-scheduler-helper",
    ]

    static func validate(
        _ location: SchedulerHelperLocation
    ) throws -> ValidatedSchedulerHelperExecutable {
        let bundle = location.bundleURL
        let executable = location.executableURL
        guard bundle.isFileURL,
              executable.isFileURL,
              bundle.path.hasPrefix("/"),
              executable.path.hasPrefix("/"),
              bundle.standardizedFileURL.path == bundle.path,
              executable.standardizedFileURL.path == executable.path else {
            throw SchedulerHelperClientError.unsafeExecutable
        }

        let expected = relativeComponents.reduce(bundle) { partial, component in
            partial.appendingPathComponent(component)
        }
        guard executable.path == expected.path else {
            throw SchedulerHelperClientError.unsafeExecutable
        }

        var current = bundle
        for (index, component) in ([String?](arrayLiteral: nil) + relativeComponents.map(Optional.some)).enumerated() {
            if let component { current.appendPathComponent(component) }
            let information = try lstat(current)
            let isLast = index == relativeComponents.count
            let kind = information.st_mode & S_IFMT
            guard isLast ? kind == S_IFREG : kind == S_IFDIR,
                  information.st_mode & 0o022 == 0 else {
                throw SchedulerHelperClientError.unsafeExecutable
            }
            if isLast {
                guard information.st_nlink == 1,
                      information.st_mode & (S_IXUSR | S_IXGRP | S_IXOTH) != 0 else {
                    throw SchedulerHelperClientError.unsafeExecutable
                }
            }
        }

        guard let realBundle = realPath(bundle),
              let realExecutable = realPath(executable) else {
            throw SchedulerHelperClientError.unsafeExecutable
        }
        let expectedRealPath = relativeComponents.reduce(realBundle) { partial, component in
            partial.appendingPathComponent(component)
        }
        guard realExecutable.path == expectedRealPath.path else {
            throw SchedulerHelperClientError.unsafeExecutable
        }

        let information = try lstat(executable)
        return ValidatedSchedulerHelperExecutable(
            url: executable,
            identity: identity(information)
        )
    }

    static func revalidate(_ executable: ValidatedSchedulerHelperExecutable) throws {
        let information = try lstat(executable.url)
        guard information.st_mode & S_IFMT == S_IFREG,
              information.st_nlink == 1,
              information.st_mode & 0o022 == 0,
              information.st_mode & (S_IXUSR | S_IXGRP | S_IXOTH) != 0,
              identity(information) == executable.identity else {
            throw SchedulerHelperClientError.unsafeExecutable
        }
    }

    private static func lstat(_ url: URL) throws -> stat {
        var information = stat()
        let result = url.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.lstat(path, &information)
        }
        guard result == 0 else { throw SchedulerHelperClientError.unsafeExecutable }
        return information
    }

    private static func identity(_ information: stat) -> SchedulerHelperExecutableIdentity {
        SchedulerHelperExecutableIdentity(
            device: UInt64(information.st_dev),
            inode: UInt64(information.st_ino),
            size: UInt64(information.st_size),
            mode: information.st_mode,
            linkCount: UInt64(information.st_nlink),
            owner: information.st_uid,
            group: information.st_gid,
            changeTimeSeconds: Int64(information.st_ctimespec.tv_sec),
            changeTimeNanoseconds: Int64(information.st_ctimespec.tv_nsec)
        )
    }

    private static func realPath(_ url: URL) -> URL? {
        url.withUnsafeFileSystemRepresentation { path in
            guard let path, let resolved = Darwin.realpath(path, nil) else { return nil }
            defer { Darwin.free(resolved) }
            return URL(fileURLWithFileSystemRepresentation: resolved, isDirectory: false, relativeTo: nil)
        }
    }
}

protocol SchedulerHelperCodeSignatureValidating: Sendable {
    func validate(executableURL: URL, hostBundleURL: URL) throws
}

struct ProductionSchedulerHelperCodeSignatureValidator: SchedulerHelperCodeSignatureValidating {
    private static let helperIdentifier = "com.greengolddog.dayweave.scheduler-helper"

    func validate(executableURL: URL, hostBundleURL: URL) throws {
        var helperCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(executableURL as CFURL, [], &helperCode) == errSecSuccess,
              let helperCode else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }
        var identifierRequirement: SecRequirement?
        guard SecRequirementCreateWithString(
            "identifier \"\(Self.helperIdentifier)\"" as CFString,
            [],
            &identifierRequirement
        ) == errSecSuccess,
            let identifierRequirement else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }
        let strictFlags = SecCSFlags(rawValue: kSecCSStrictValidate | kSecCSCheckAllArchitectures)
        guard SecStaticCodeCheckValidity(
            helperCode,
            strictFlags,
            identifierRequirement
        ) == errSecSuccess else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }

        var hostCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(hostBundleURL as CFURL, [], &hostCode) == errSecSuccess,
              let hostCode else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }
        let hostFlags = SecCSFlags(
            rawValue: kSecCSStrictValidate | kSecCSCheckAllArchitectures | kSecCSCheckNestedCode
        )
        guard SecStaticCodeCheckValidity(hostCode, hostFlags, nil) == errSecSuccess else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }

        var runningCode: SecCode?
        var runningStaticCode: SecStaticCode?
        guard SecCodeCopySelf([], &runningCode) == errSecSuccess,
              let runningCode,
              SecCodeCheckValidity(
                runningCode,
                SecCSFlags(rawValue: kSecCSStrictValidate),
                nil
              ) == errSecSuccess,
              SecCodeCopyStaticCode(runningCode, [], &runningStaticCode) == errSecSuccess,
              let runningStaticCode,
              try uniqueCodeHash(hostCode) == uniqueCodeHash(runningStaticCode) else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }
    }

    private func uniqueCodeHash(_ code: SecStaticCode) throws -> Data {
        var information: CFDictionary?
        guard SecCodeCopySigningInformation(
            code,
            SecCSFlags(rawValue: kSecCSSigningInformation),
            &information
        ) == errSecSuccess,
            let values = information as? [String: Any],
            let hash = values[kSecCodeInfoUnique as String] as? Data else {
            throw SchedulerHelperClientError.invalidCodeSignature
        }
        return hash
    }
}

enum SchedulerHelperTermination: Equatable, Sendable {
    case exited(Int32)
    case signaled(Int32)
}

struct SchedulerHelperProcessResult: Equatable, Sendable {
    let standardOutput: Data
    let standardError: Data
    let termination: SchedulerHelperTermination
}

protocol SchedulerHelperProcessRunning: Sendable {
    func run(
        executable: ValidatedSchedulerHelperExecutable,
        standardInput: Data,
        timeout: Duration
    ) async throws -> SchedulerHelperProcessResult
}

struct SchedulerHelperClient: LocalScheduleComposing, RoutineOccurrenceScheduleComposing, Sendable {
    static let maximumStandardInputBytes = 16 * 1_024 * 1_024
    static let maximumStandardOutputBytes = 16 * 1_024 * 1_024
    static let maximumStandardErrorBytes = 16 * 1_024 * 1_024

    private let locator: any SchedulerHelperLocating
    private let processRunner: any SchedulerHelperProcessRunning
    private let signatureValidator: any SchedulerHelperCodeSignatureValidating
    private let timeout: Duration

    init(timeout: Duration = .seconds(30)) {
        locator = BundledSchedulerHelperLocator()
        processRunner = SchedulerHelperProcessRunner()
        signatureValidator = ProductionSchedulerHelperCodeSignatureValidator()
        self.timeout = timeout
    }

#if DEBUG
    /// Test-only injection point. Production construction always uses the
    /// bundle locator, POSIX runner, and host-bound code-signature validator.
    init(
        testingLocator locator: any SchedulerHelperLocating,
        processRunner: any SchedulerHelperProcessRunning,
        signatureValidator: any SchedulerHelperCodeSignatureValidating,
        timeout: Duration = .seconds(30)
    ) {
        self.locator = locator
        self.processRunner = processRunner
        self.signatureValidator = signatureValidator
        self.timeout = timeout
    }
#endif

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        try Task.checkCancellation()
        let location: SchedulerHelperLocation
        do {
            location = try locator.locate()
        } catch {
            throw SchedulerHelperClientError.helperUnavailable
        }
        let executable = try SchedulerHelperExecutableValidator.validate(location)

        guard canonicalItems.count <= 10_000 else {
            throw SchedulerHelperClientError.inputTooLarge
        }
        let projectedItems = try canonicalItems.map(SchedulerHelperCanonicalItemWire.init)
        let request = SchedulerHelperRequestEnvelope(
            request: .init(canonicalItems: projectedItems, schedule: schedule)
        )
        let input: Data
        do {
            input = try Self.encoder.encode(request)
        } catch let error as SchedulerHelperClientError {
            throw error
        } catch {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        guard input.count <= Self.maximumStandardInputBytes else {
            throw SchedulerHelperClientError.inputTooLarge
        }

        // Signature validation stays adjacent to the runner's immediate
        // inode/ctime revalidation; request projection cannot widen this
        // writable-bundle verification window.
        try signatureValidator.validate(
            executableURL: executable.url,
            hostBundleURL: location.bundleURL
        )

        let output = try await processRunner.run(
            executable: executable,
            standardInput: input,
            timeout: timeout
        )
        guard output.standardError.isEmpty else {
            throw SchedulerHelperClientError.invalidResponse
        }
        let response: SchedulerHelperResponseEnvelope
        do {
            response = try Self.decoder.decode(
                SchedulerHelperResponseEnvelope.self,
                from: output.standardOutput
            )
        } catch {
            throw SchedulerHelperClientError.invalidResponse
        }

        switch (output.termination, response.result) {
        case let (.exited(0), .composition(composition)):
            return composition
        case (.exited(2), .error), (.exited(70), .error):
            throw SchedulerHelperClientError.requestRejected
        case (.exited, _):
            throw SchedulerHelperClientError.invalidResponse
        case (.signaled, _):
            throw SchedulerHelperClientError.unexpectedTermination
        }
    }

    func composeOccurrences(
        canonicalItems: [DayWeaveCanonicalItem],
        witness: RoutinePlanningWitness
    ) async throws -> RoutineOccurrenceLocalComposition {
        try Task.checkCancellation()
        guard canonicalItems.count <= 10_000 else {
            throw SchedulerHelperClientError.inputTooLarge
        }
        do {
            try witness.validate(canonicalItems: canonicalItems)
        } catch {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        let location: SchedulerHelperLocation
        do {
            location = try locator.locate()
        } catch {
            throw SchedulerHelperClientError.helperUnavailable
        }
        let executable = try SchedulerHelperExecutableValidator.validate(location)
        let projectedItems = try canonicalItems.map(SchedulerHelperCanonicalItemWire.init)
        let input: Data
        do {
            input = try Self.encoder.encode(SchedulerHelperOccurrenceRequestEnvelope(
                request: .init(canonicalItems: projectedItems, schedule: witness.schedule,
                    occurrenceLifecycle: witness.occurrenceLifecycle)
            ))
        } catch {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        guard input.count <= Self.maximumStandardInputBytes else {
            throw SchedulerHelperClientError.inputTooLarge
        }
        try Task.checkCancellation()
        // Preserve the same host signature and immediate runner inode/ctime
        // checks as v1; neither witness decoding nor encoding widens that gap.
        try signatureValidator.validate(
            executableURL: executable.url,
            hostBundleURL: location.bundleURL
        )
        let output = try await processRunner.run(
            executable: executable, standardInput: input, timeout: timeout
        )
        try Task.checkCancellation()
        return try Self.decodeOccurrenceOutput(output, witness: witness)
    }

    /// Kept separate from the v1 decoder, including its exact key set.
    static func decodeOccurrenceOutput(
        _ output: SchedulerHelperProcessResult,
        witness: RoutinePlanningWitness
    ) throws -> RoutineOccurrenceLocalComposition {
        guard output.standardOutput.count <= maximumStandardOutputBytes,
              output.standardError.count <= maximumStandardErrorBytes else {
            throw SchedulerHelperClientError.outputTooLarge
        }
        guard output.standardError.isEmpty else {
            throw SchedulerHelperClientError.invalidResponse
        }
        do {
            guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: output.standardOutput) else {
                throw SchedulerHelperClientError.invalidResponse
            }
            let root = try SchedulerHelperOccurrenceShape.object(
                JSONSerialization.jsonObject(with: output.standardOutput),
                keys: ["protocol", "version", "result"]
            )
            guard root["protocol"] as? String == "dayweave.scheduler.helper",
                  try SchedulerHelperOccurrenceShape.integer(root["version"]) == 2 else {
                throw SchedulerHelperClientError.invalidResponse
            }
            let result = try SchedulerHelperOccurrenceShape.object(root["result"])
            if result["type"] as? String == "error" {
                _ = try SchedulerHelperOccurrenceShape.object(result, keys: ["type", "error"])
                let error = try SchedulerHelperOccurrenceShape.object(result["error"], keys: ["code", "message"])
                guard error["code"] is String, error["message"] is String else {
                    throw SchedulerHelperClientError.invalidResponse
                }
                switch output.termination {
                case .exited(2), .exited(70): throw SchedulerHelperClientError.requestRejected
                case .signaled: throw SchedulerHelperClientError.unexpectedTermination
                default: throw SchedulerHelperClientError.invalidResponse
                }
            }
            _ = try SchedulerHelperOccurrenceShape.object(result, keys: ["type", "composition"])
            guard result["type"] as? String == "composition" else {
                throw SchedulerHelperClientError.invalidResponse
            }
            var composition = try SchedulerHelperOccurrenceShape.object(result["composition"], keys: [
                "local_input_fingerprint", "source_item_count", "source_item_revisions",
                "accepted_item_count", "rejected_items", "ignored_previous_assignments",
                "plan", "occurrence_snapshot_revision",
            ])
            let head = try SchedulerHelperOccurrenceShape.integer(composition.removeValue(forKey: "occurrence_snapshot_revision"))
            try SchedulerHelperOccurrenceShape.validate(composition)
            // Bind the original clock before the display model rounds it to
            // Foundation Date. Distant instants can collapse distinct wire
            // microseconds to the same floating-point value.
            let plan = try SchedulerHelperOccurrenceShape.object(composition["plan"])
            for key in ["as_of", "horizon_start", "horizon_end"] {
                guard let raw = plan[key] as? String,
                      try RoutinePlanningShape.instant(.string(raw))
                        == RoutinePlanningShape.instant(witness.schedule.fields[key]) else {
                    throw SchedulerHelperClientError.invalidResponse
                }
            }
            let decoded = try decoder.decode(LocalScheduleComposition.self,
                from: JSONSerialization.data(withJSONObject: composition, options: [.sortedKeys]))
            guard head == witness.occurrenceLifecycle.snapshotRevision,
                  decoded.localInputFingerprint == witness.localInputFingerprint,
                  decoded.sourceItemRevisions == witness.sourceItemRevisions,
                  decoded.sourceItemCount == witness.sourceItemRevisions.count,
                  decoded.acceptedItemCount == decoded.sourceItemCount,
                  decoded.rejectedItems.isEmpty else {
                throw SchedulerHelperClientError.invalidResponse
            }
            switch output.termination {
            case .exited(0):
                return .init(composition: decoded, occurrenceSnapshotRevision: head, rawOutput: output.standardOutput)
            case .signaled: throw SchedulerHelperClientError.unexpectedTermination
            default: throw SchedulerHelperClientError.invalidResponse
            }
        } catch let error as SchedulerHelperClientError {
            throw error
        } catch {
            // No private helper response snippets or cause chains escape.
            throw SchedulerHelperClientError.invalidResponse
        }
    }

    static var encoder: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(try SchedulerHelperRFC3339.string(from: date))
        }
        return encoder
    }

    static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = SchedulerHelperRFC3339.date(from: value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Expected an RFC 3339 timestamp"
                )
            }
            return date
        }
        return decoder
    }
}

enum SchedulerHelperRFC3339 {
    static func string(from date: Date) throws -> String {
        let timestamp = date.timeIntervalSince1970
        guard timestamp.isFinite else {
            throw EncodingError.invalidValue(
                date,
                .init(codingPath: [], debugDescription: "Date is outside the helper contract")
            )
        }
        var wholeSeconds = Int64(floor(timestamp))
        var microseconds = Int64(((timestamp - Double(wholeSeconds)) * 1_000_000).rounded())
        if microseconds == 1_000_000 {
            let (incremented, overflow) = wholeSeconds.addingReportingOverflow(1)
            guard !overflow else {
                throw EncodingError.invalidValue(
                    date,
                    .init(codingPath: [], debugDescription: "Date is outside the helper contract")
                )
            }
            wholeSeconds = incremented
            microseconds = 0
        }

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let components = calendar.dateComponents(
            [.year, .month, .day, .hour, .minute, .second],
            from: Date(timeIntervalSince1970: TimeInterval(wholeSeconds))
        )
        guard let year = components.year,
              let month = components.month,
              let day = components.day,
              let hour = components.hour,
              let minute = components.minute,
              let second = components.second,
              (1...9_999).contains(year) else {
            throw EncodingError.invalidValue(
                date,
                .init(codingPath: [], debugDescription: "Date is outside the helper contract")
            )
        }
        return String(
            format: "%04d-%02d-%02dT%02d:%02d:%02d.%06lldZ",
            locale: Locale(identifier: "en_US_POSIX"),
            year, month, day, hour, minute, second, microseconds
        )
    }

    static func date(from value: String) -> Date? {
        let bytes = Array(value.utf8)
        guard bytes.count >= 20,
              bytes[4] == 45,
              bytes[7] == 45,
              bytes[10] == 84,
              bytes[13] == 58,
              bytes[16] == 58,
              let year = decimal(bytes, 0..<4),
              let month = decimal(bytes, 5..<7),
              let day = decimal(bytes, 8..<10),
              let hour = decimal(bytes, 11..<13),
              let minute = decimal(bytes, 14..<16),
              let second = decimal(bytes, 17..<19),
              (1...9_999).contains(year),
              (1...12).contains(month),
              (1...31).contains(day),
              (0...23).contains(hour),
              (0...59).contains(minute),
              (0...59).contains(second) else { return nil }

        var cursor = 19
        var fraction = 0.0
        if cursor < bytes.count, bytes[cursor] == 46 {
            cursor += 1
            let start = cursor
            var scale = 0.1
            while cursor < bytes.count, isDigit(bytes[cursor]) {
                guard cursor - start < 9 else { return nil }
                fraction += Double(bytes[cursor] - 48) * scale
                scale /= 10
                cursor += 1
            }
            guard cursor > start else { return nil }
        }

        let offsetSeconds: Int
        if cursor + 1 == bytes.count, bytes[cursor] == 90 {
            offsetSeconds = 0
        } else {
            guard cursor + 6 == bytes.count,
                  bytes[cursor] == 43 || bytes[cursor] == 45,
                  bytes[cursor + 3] == 58,
                  let offsetHour = decimal(bytes, (cursor + 1)..<(cursor + 3)),
                  let offsetMinute = decimal(bytes, (cursor + 4)..<(cursor + 6)),
                  (0...23).contains(offsetHour),
                  (0...59).contains(offsetMinute) else { return nil }
            let magnitude = offsetHour * 3_600 + offsetMinute * 60
            offsetSeconds = bytes[cursor] == 45 ? -magnitude : magnitude
        }

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        var components = DateComponents()
        components.calendar = calendar
        components.timeZone = calendar.timeZone
        components.year = year
        components.month = month
        components.day = day
        components.hour = hour
        components.minute = minute
        components.second = second
        guard let local = calendar.date(from: components) else { return nil }
        let checked = calendar.dateComponents(
            [.year, .month, .day, .hour, .minute, .second],
            from: local
        )
        guard checked.year == year,
              checked.month == month,
              checked.day == day,
              checked.hour == hour,
              checked.minute == minute,
              checked.second == second else { return nil }
        return local.addingTimeInterval(fraction - Double(offsetSeconds))
    }

    private static func decimal(_ bytes: [UInt8], _ range: Range<Int>) -> Int? {
        var value = 0
        for index in range {
            guard index < bytes.count, isDigit(bytes[index]) else { return nil }
            value = value * 10 + Int(bytes[index] - 48)
        }
        return value
    }

    private static func isDigit(_ byte: UInt8) -> Bool {
        (48...57).contains(byte)
    }
}

private struct SchedulerHelperRequestEnvelope: Encodable {
    let protocolName = "dayweave.scheduler.helper"
    let version = 1
    let operation = "compose"
    let request: SchedulerHelperComposeRequest

    private enum CodingKeys: String, CodingKey {
        case version, operation, request
        case protocolName = "protocol"
    }
}

private struct SchedulerHelperComposeRequest: Encodable {
    let canonicalItems: [SchedulerHelperCanonicalItemWire]
    let schedule: DayWeaveSchedulePreviewRequest

    private enum CodingKeys: String, CodingKey {
        case schedule
        case canonicalItems = "canonical_items"
    }
}

private struct SchedulerHelperOccurrenceRequestEnvelope: Encodable {
    let protocolName = "dayweave.scheduler.helper"
    let version = 2
    let operation = "compose"
    let request: Request

    struct Request: Encodable {
        let canonicalItems: [SchedulerHelperCanonicalItemWire]
        let schedule: RoutinePlanningScheduleInput
        let occurrenceLifecycle: RoutinePlanningLifecycleContext
        private enum CodingKeys: String, CodingKey {
            case schedule
            case canonicalItems = "canonical_items"
            case occurrenceLifecycle = "occurrence_lifecycle"
        }
    }
    private enum CodingKeys: String, CodingKey {
        case version, operation, request
        case protocolName = "protocol"
    }
}

/// The existing display plan has intentionally forward-compatible JSON fields.
/// V2 cannot inherit that permissiveness at a qualified helper boundary: check
/// every nested object's closed wire shape before using the shared typed model.
private enum SchedulerHelperOccurrenceShape {
    static func object(_ value: Any?, keys: Set<String>? = nil) throws -> [String: Any] {
        guard let object = value as? [String: Any],
              keys == nil || Set(object.keys) == keys else {
            throw SchedulerHelperClientError.invalidResponse
        }
        return object
    }

    static func array(_ value: Any?) throws -> [Any] {
        guard let array = value as? [Any], array.count <= 100_000 else {
            throw SchedulerHelperClientError.invalidResponse
        }
        return array
    }

    static func integer(_ value: Any?) throws -> UInt64 {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              let integer = UInt64(number.stringValue) else {
            throw SchedulerHelperClientError.invalidResponse
        }
        return integer
    }

    static func date(_ value: Any?) throws -> Date {
        guard let string = value as? String,
              let date = SchedulerHelperRFC3339.date(from: string) else {
            throw SchedulerHelperClientError.invalidResponse
        }
        return date
    }

    private static func uuid(_ value: Any?, optional: Bool = false) throws {
        if optional, value is NSNull { return }
        guard let string = value as? String, let uuid = UUID(uuidString: string),
              uuid.uuidString.lowercased() == string,
              uuid != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)) else {
            throw SchedulerHelperClientError.invalidResponse
        }
    }

    private static func text(_ value: Any?, among allowed: Set<String>? = nil) throws {
        guard let string = value as? String,
              allowed == nil || allowed?.contains(string) == true else {
            throw SchedulerHelperClientError.invalidResponse
        }
    }

    static func validate(_ composition: [String: Any]) throws {
        // Match the strict parser's aggregate collection budget without a
        // recursive traversal or allocating a second unbounded flattened tree.
        var pending: [Any] = [composition]
        var remaining = 500_000
        while let value = pending.popLast() {
            let children: [Any]
            if let object = value as? [String: Any] { children = Array(object.values) }
            else if let array = value as? [Any] { children = array }
            else { continue }
            guard children.count <= remaining else { throw SchedulerHelperClientError.invalidResponse }
            remaining -= children.count
            pending.append(contentsOf: children)
        }
        for value in try array(composition["rejected_items"]) {
            let rejected = try object(value, keys: ["item_id", "is_sensitive", "title", "reason"])
            try uuid(rejected["item_id"])
        }
        for value in try array(composition["ignored_previous_assignments"]) {
            let ignored = try object(value, keys: ["item_id", "requested_revision", "current_revision", "reason"])
            try uuid(ignored["item_id"])
        }
        let plan = try object(composition["plan"], keys: [
            "as_of", "horizon_start", "horizon_end", "blocks", "unscheduled",
            "decisions", "violations", "score", "occurrences",
        ])
        _ = try object(plan["score"], keys: ["scheduled_minutes", "unscheduled_minutes", "soft_penalty", "moved_minutes"])
        for value in try array(plan["blocks"]) {
            let block = try object(value, keys: ["id", "is_sensitive", "item_id", "occurrence_id", "external_block_id",
                "title", "start", "end", "session_index", "kind", "explanations"])
            try uuid(block["id"])
            for key in ["item_id", "occurrence_id", "external_block_id"] { try uuid(block[key], optional: true) }
            try text(block["kind"], among: ["planned", "pinned", "calendar_event", "external_fixed"])
            for value in try array(block["explanations"]) {
                let explanation = try object(value, keys: ["code", "message"])
                try text(explanation["code"], among: ["fixed_event", "pinned", "hard_deadline", "goal_progress",
                    "habit_or_routine", "priority", "preferred_window", "context_match", "energy_match", "dependency",
                    "stable_time", "earliest_available", "split_session"])
            }
        }
        for value in try array(plan["unscheduled"]) {
            let item = try object(value, keys: ["item_id", "occurrence_id", "remaining", "reason", "message"])
            try uuid(item["item_id"])
            try uuid(item["occurrence_id"], optional: true)
            try text(item["reason"], among: ["missing_duration", "no_capacity", "hard_constraint", "blocked",
                "dependency_unavailable", "dependency_cycle", "session_limit"])
        }
        for value in try array(plan["decisions"]) {
            let decision = try object(value, keys: ["item_id", "occurrence_id", "kind", "message"])
            try uuid(decision["item_id"])
            try uuid(decision["occurrence_id"], optional: true)
            try text(decision["kind"], among: ["container_rolled_up", "terminal_item_ignored", "fixed_event_retained",
                "scheduled", "partially_scheduled", "kept_pinned"])
            try text(decision["message"])
        }
        for value in try array(plan["violations"]) {
            let violation = try object(value, keys: ["kind", "severity", "item_ids", "occurrence_ids",
                "start", "end", "penalty", "message"])
            try text(violation["kind"], among: ["soft_constraint", "fixed_overlap", "pinned_conflict", "deadline_risk",
                "dependency", "buffer_compressed", "capacity"])
            try text(violation["severity"], among: ["warning", "error"])
            for id in try array(violation["item_ids"]) { try uuid(id) }
            for id in try array(violation["occurrence_ids"]) { try uuid(id) }
            for key in ["start", "end"] {
                if !(violation[key] is NSNull) { _ = try date(violation[key]) }
            }
            _ = try integer(violation["penalty"])
            try text(violation["message"])
        }
        for value in try array(plan["occurrences"]) {
            let occurrence = try object(value, keys: ["id", "series_item_id", "identity", "nominal_start", "nominal_end",
                "window_start", "window_end", "local_date", "ordinal", "state"])
            try uuid(occurrence["id"])
            try uuid(occurrence["series_item_id"])
            // RecurrenceOccurrenceIdentity's decoder already enforces every
            // tagged case's exact key set and value bounds.
            try text(occurrence["state"], among: ["generated", "completed", "paused", "skipped"])
            for key in ["nominal_start", "nominal_end", "window_start", "window_end"] { _ = try date(occurrence[key]) }
        }
    }
}

private struct SchedulerHelperResponseEnvelope: Decodable {
    let result: SchedulerHelperResponseResult

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case protocolName = "protocol"
        case version, result
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: SchedulerHelperCodingKey.self)
        guard Set(dynamic.allKeys.map(\.stringValue)) == Set(CodingKeys.allCases.map(\.rawValue)) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Unexpected response envelope")
            )
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard try container.decode(String.self, forKey: .protocolName)
                == "dayweave.scheduler.helper",
              try container.decode(Int.self, forKey: .version) == 1 else {
            throw DecodingError.dataCorruptedError(
                forKey: .version,
                in: container,
                debugDescription: "Unsupported helper protocol"
            )
        }
        result = try container.decode(SchedulerHelperResponseResult.self, forKey: .result)
    }
}

private enum SchedulerHelperResponseResult: Decodable {
    case composition(LocalScheduleComposition)
    case error

    private enum CodingKeys: String, CodingKey {
        case type, composition, error
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: SchedulerHelperCodingKey.self)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "composition":
            guard Set(dynamic.allKeys.map(\.stringValue)) == ["type", "composition"] else {
                throw DecodingError.dataCorruptedError(
                    forKey: .type,
                    in: container,
                    debugDescription: "Unexpected composition result"
                )
            }
            self = .composition(
                try container.decode(LocalScheduleComposition.self, forKey: .composition)
            )
        case "error":
            guard Set(dynamic.allKeys.map(\.stringValue)) == ["type", "error"] else {
                throw DecodingError.dataCorruptedError(
                    forKey: .type,
                    in: container,
                    debugDescription: "Unexpected error result"
                )
            }
            _ = try container.decode(SchedulerHelperErrorPayload.self, forKey: .error)
            self = .error
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "Unsupported helper result"
            )
        }
    }
}

private struct SchedulerHelperErrorPayload: Decodable {
    private enum CodingKeys: String, CodingKey, CaseIterable { case code, message }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: SchedulerHelperCodingKey.self)
        guard Set(dynamic.allKeys.map(\.stringValue)) == Set(CodingKeys.allCases.map(\.rawValue)) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Unexpected error shape")
            )
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        _ = try container.decode(String.self, forKey: .code)
        _ = try container.decode(String.self, forKey: .message)
    }
}
