import CryptoKit
import Foundation

enum ScheduleProfileValidationError: LocalizedError, Equatable, Sendable {
    case invalidTimezone
    case invalidLocalTime
    case invalidWindow
    case invalidSleepInterval
    case invalidProtectedInterval
    case invalidWeekdayAvailability
    case invalidContexts
    case invalidLocation

    var errorDescription: String? {
        switch self {
        case .invalidTimezone:
            "Choose a recognized IANA timezone."
        case .invalidLocalTime:
            "Local times must be between 00:00 and 23:59."
        case .invalidWindow:
            "Availability windows must be non-empty, ordered, and non-overlapping."
        case .invalidSleepInterval:
            "Sleep must be one overnight interval whose start is later than its end."
        case .invalidProtectedInterval:
            "Protected time must be at most eight hours per day and remain inside waking time without overlapping availability."
        case .invalidWeekdayAvailability:
            "A schedule profile must define Monday through Sunday exactly once; enabled days need at least one availability window."
        case .invalidContexts:
            "Contexts must be normalized, unique, and within the profile limits."
        case .invalidLocation:
            "The optional location is empty or exceeds the profile limit."
        }
    }
}

enum ScheduleWeekday: Int, Codable, CaseIterable, Comparable, Sendable {
    case monday = 1
    case tuesday
    case wednesday
    case thursday
    case friday
    case saturday
    case sunday

    static func < (left: Self, right: Self) -> Bool {
        left.rawValue < right.rawValue
    }

    fileprivate static func weekday(containing date: Date, calendar: Calendar) -> Self? {
        // Foundation uses Sunday=1. The profile uses the ISO order Monday=1.
        let foundationWeekday = calendar.component(.weekday, from: date)
        return Self(rawValue: ((foundationWeekday + 5) % 7) + 1)
    }
}

struct ScheduleLocalTime: Codable, Comparable, Hashable, Sendable {
    static let minutesPerDay = 24 * 60

    let minutesSinceMidnight: UInt16

    init(minutesSinceMidnight: Int) throws(ScheduleProfileValidationError) {
        guard (0..<Self.minutesPerDay).contains(minutesSinceMidnight),
              let value = UInt16(exactly: minutesSinceMidnight) else {
            throw .invalidLocalTime
        }
        self.minutesSinceMidnight = value
    }

    init(hour: Int, minute: Int) throws(ScheduleProfileValidationError) {
        guard (0..<24).contains(hour), (0..<60).contains(minute) else {
            throw .invalidLocalTime
        }
        try self.init(minutesSinceMidnight: hour * 60 + minute)
    }

    var hour: Int { Int(minutesSinceMidnight) / 60 }
    var minute: Int { Int(minutesSinceMidnight) % 60 }

    static func < (left: Self, right: Self) -> Bool {
        left.minutesSinceMidnight < right.minutesSinceMidnight
    }

    private enum CodingKeys: String, CodingKey {
        case minutesSinceMidnight
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let minutes = try container.decode(Int.self, forKey: .minutesSinceMidnight)
        do {
            try self.init(minutesSinceMidnight: minutes)
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .minutesSinceMidnight,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ScheduleLocalTimeWindow: Codable, Equatable, Sendable {
    let start: ScheduleLocalTime
    let end: ScheduleLocalTime

    init(
        start: ScheduleLocalTime,
        end: ScheduleLocalTime
    ) throws(ScheduleProfileValidationError) {
        guard start < end else { throw .invalidWindow }
        self.start = start
        self.end = end
    }

    var durationMinutes: Int {
        Int(end.minutesSinceMidnight - start.minutesSinceMidnight)
    }

    private enum CodingKeys: String, CodingKey { case start, end }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let start = try container.decode(ScheduleLocalTime.self, forKey: .start)
        let end = try container.decode(ScheduleLocalTime.self, forKey: .end)
        do {
            try self.init(start: start, end: end)
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .end,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ScheduleSleepInterval: Codable, Equatable, Sendable {
    let start: ScheduleLocalTime
    let end: ScheduleLocalTime

    init(
        start: ScheduleLocalTime,
        end: ScheduleLocalTime
    ) throws(ScheduleProfileValidationError) {
        guard start > end else { throw .invalidSleepInterval }
        self.start = start
        self.end = end
    }

    var durationMinutes: Int {
        ScheduleLocalTime.minutesPerDay
            - Int(start.minutesSinceMidnight)
            + Int(end.minutesSinceMidnight)
    }

    private enum CodingKeys: String, CodingKey { case start, end }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let start = try container.decode(ScheduleLocalTime.self, forKey: .start)
        let end = try container.decode(ScheduleLocalTime.self, forKey: .end)
        do {
            try self.init(start: start, end: end)
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .end,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ScheduleAvailabilityDay: Codable, Equatable, Sendable {
    static let maximumWindows = 8

    let weekday: ScheduleWeekday
    let isEnabled: Bool
    let windows: [ScheduleLocalTimeWindow]

    init(
        weekday: ScheduleWeekday,
        isEnabled: Bool,
        windows: [ScheduleLocalTimeWindow]
    ) throws(ScheduleProfileValidationError) {
        let ordered = windows.sorted {
            if $0.start != $1.start { return $0.start < $1.start }
            return $0.end < $1.end
        }
        guard ordered.count <= Self.maximumWindows,
              isEnabled ? !ordered.isEmpty : ordered.isEmpty,
              zip(ordered, ordered.dropFirst()).allSatisfy({ $0.end <= $1.start }) else {
            throw .invalidWeekdayAvailability
        }
        self.weekday = weekday
        self.isEnabled = isEnabled
        self.windows = ordered
    }

    private enum CodingKeys: String, CodingKey { case weekday, isEnabled, windows }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let weekday = try container.decode(ScheduleWeekday.self, forKey: .weekday)
        let isEnabled = try container.decode(Bool.self, forKey: .isEnabled)
        let windows = try container.decode([ScheduleLocalTimeWindow].self, forKey: .windows)
        do {
            try self.init(weekday: weekday, isEnabled: isEnabled, windows: windows)
            guard self.windows == windows else { throw ScheduleProfileValidationError.invalidWindow }
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .windows,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ScheduleProtectedDay: Codable, Equatable, Sendable {
    static let maximumWindows = 8

    let weekday: ScheduleWeekday
    let isEnabled: Bool
    let windows: [ScheduleLocalTimeWindow]

    init(
        weekday: ScheduleWeekday,
        isEnabled: Bool,
        windows: [ScheduleLocalTimeWindow]
    ) throws(ScheduleProfileValidationError) {
        let ordered = windows.sorted {
            if $0.start != $1.start { return $0.start < $1.start }
            return $0.end < $1.end
        }
        let totalMinutes = ordered.reduce(0) { $0 + $1.durationMinutes }
        guard ordered.count <= Self.maximumWindows,
              totalMinutes <= ScheduleProfile.maximumProtectedFreeMinutes,
              isEnabled ? !ordered.isEmpty : ordered.isEmpty,
              zip(ordered, ordered.dropFirst()).allSatisfy({ $0.end <= $1.start }) else {
            throw .invalidProtectedInterval
        }
        self.weekday = weekday
        self.isEnabled = isEnabled
        self.windows = ordered
    }

    private enum CodingKeys: String, CodingKey { case weekday, isEnabled, windows }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let weekday = try container.decode(ScheduleWeekday.self, forKey: .weekday)
        let isEnabled = try container.decode(Bool.self, forKey: .isEnabled)
        let windows = try container.decode([ScheduleLocalTimeWindow].self, forKey: .windows)
        do {
            try self.init(weekday: weekday, isEnabled: isEnabled, windows: windows)
            guard self.windows == windows else {
                throw ScheduleProfileValidationError.invalidProtectedInterval
            }
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .windows,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ScheduleProfile: Codable, Equatable, Sendable {
    static let maximumContexts = 16
    static let maximumContextBytes = 64
    static let maximumLocationBytes = 256
    static let maximumProtectedFreeMinutes = 8 * 60

    let timezoneName: String
    let availability: [ScheduleAvailabilityDay]
    let sleep: ScheduleSleepInterval
    let protectedTime: [ScheduleProtectedDay]
    let defaultEnergy: EnergyLevel
    let contexts: [String]
    let location: String?

    init(
        timezoneName: String,
        availability: [ScheduleAvailabilityDay],
        sleep: ScheduleSleepInterval,
        protectedTime: [ScheduleProtectedDay],
        defaultEnergy: EnergyLevel,
        contexts: [String],
        location: String?
    ) throws(ScheduleProfileValidationError) {
        guard Self.isKnownIANATimezone(timezoneName) else { throw .invalidTimezone }

        let orderedAvailability = availability.sorted { $0.weekday < $1.weekday }
        guard orderedAvailability.map(\.weekday) == ScheduleWeekday.allCases else {
            throw .invalidWeekdayAvailability
        }
        let orderedProtectedTime = protectedTime.sorted { $0.weekday < $1.weekday }
        guard orderedProtectedTime.map(\.weekday) == ScheduleWeekday.allCases else {
            throw .invalidProtectedInterval
        }
        let protectedByWeekday = Dictionary(uniqueKeysWithValues: orderedProtectedTime.map {
            ($0.weekday, $0.windows)
        })
        guard orderedProtectedTime.allSatisfy({ day in
            day.windows.allSatisfy { $0.start >= sleep.end && $0.end <= sleep.start }
        }), orderedAvailability.allSatisfy({ day in
            day.windows.allSatisfy { window in
                window.start >= sleep.end
                    && window.end <= sleep.start
                    && (protectedByWeekday[day.weekday] ?? []).allSatisfy { protectedWindow in
                        window.end <= protectedWindow.start
                            || protectedWindow.end <= window.start
                    }
            }
        }) else {
            throw .invalidWindow
        }

        let normalizedContexts = contexts.map(Self.normalizeContext).sorted()
        guard normalizedContexts.count <= Self.maximumContexts,
              normalizedContexts.allSatisfy({ context in
                  !context.isEmpty
                      && context.utf8.count <= Self.maximumContextBytes
                      && !context.unicodeScalars.contains(
                          where: CharacterSet.controlCharacters.contains
                      )
              }),
              Set(normalizedContexts).count == normalizedContexts.count else {
            throw .invalidContexts
        }
        let normalizedLocation = location.map(Self.normalizeWhitespace)
        if let normalizedLocation {
            guard !normalizedLocation.isEmpty,
                  normalizedLocation.utf8.count <= Self.maximumLocationBytes,
                  !normalizedLocation.unicodeScalars.contains(
                      where: CharacterSet.controlCharacters.contains
                  ) else {
                throw .invalidLocation
            }
        }

        self.timezoneName = timezoneName
        self.availability = orderedAvailability
        self.sleep = sleep
        self.protectedTime = orderedProtectedTime
        self.defaultEnergy = defaultEnergy
        self.contexts = normalizedContexts
        self.location = normalizedLocation
    }

    var protectedFreeMinutes: Int {
        let terminalStarts = protectedTime.compactMap { day -> ScheduleLocalTime? in
            guard let terminal = day.windows.last, terminal.end == sleep.start else {
                return nil
            }
            return terminal.start
        }
        guard terminalStarts.count == ScheduleWeekday.allCases.count,
              Set(terminalStarts).count == 1,
              let start = terminalStarts.first else { return 0 }
        return Int(sleep.start.minutesSinceMidnight - start.minutesSinceMidnight)
    }

    var hasValidShape: Bool {
        guard let rebuilt = try? Self(
            timezoneName: timezoneName,
            availability: availability,
            sleep: sleep,
            protectedTime: protectedTime,
            defaultEnergy: defaultEnergy,
            contexts: contexts,
            location: location
        ) else { return false }
        return rebuilt == self
    }

    static func legacyDefault(
        timezoneName: String,
        protectedFreeMinutes: Int
    ) throws(ScheduleProfileValidationError) -> Self {
        guard (0...maximumProtectedFreeMinutes).contains(protectedFreeMinutes) else {
            throw .invalidProtectedInterval
        }
        let wake = try ScheduleLocalTime(hour: 6, minute: 0)
        let sleepStart = try ScheduleLocalTime(hour: 23, minute: 0)
        let availabilityEnd = try ScheduleLocalTime(
            minutesSinceMidnight: Int(sleepStart.minutesSinceMidnight) - protectedFreeMinutes
        )
        let window = try ScheduleLocalTimeWindow(start: wake, end: availabilityEnd)
        var days: [ScheduleAvailabilityDay] = []
        for weekday in ScheduleWeekday.allCases {
            days.append(try ScheduleAvailabilityDay(
                weekday: weekday,
                isEnabled: true,
                windows: [window]
            ))
        }
        let protectedWindow = protectedFreeMinutes == 0
            ? nil
            : try ScheduleLocalTimeWindow(start: availabilityEnd, end: sleepStart)
        var protectedDays: [ScheduleProtectedDay] = []
        for weekday in ScheduleWeekday.allCases {
            protectedDays.append(try ScheduleProtectedDay(
                weekday: weekday,
                isEnabled: protectedWindow != nil,
                windows: [protectedWindow].compactMap { $0 }
            ))
        }
        return try Self(
            timezoneName: Self.normalizedTimezoneName(timezoneName),
            availability: days,
            sleep: ScheduleSleepInterval(start: sleepStart, end: wake),
            protectedTime: protectedDays,
            defaultEnergy: .medium,
            contexts: [],
            location: nil
        )
    }

    static func normalizeContext(_ value: String) -> String {
        normalizeWhitespace(value.precomposedStringWithCanonicalMapping).lowercased()
    }

    static func normalizeLocation(_ value: String?) -> String? {
        value.map(normalizeWhitespace).flatMap { $0.isEmpty ? nil : $0 }
    }

    static func normalizedTimezoneName(_ value: String) -> String {
        value == "GMT" ? "UTC" : value
    }

    static func isKnownIANATimezone(_ value: String) -> Bool {
        DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: value) != nil
    }

    private static func normalizeWhitespace(_ value: String) -> String {
        value
            .split(whereSeparator: \Character.isWhitespace)
            .joined(separator: " ")
    }

    private enum CodingKeys: String, CodingKey {
        case timezoneName, availability, sleep, protectedTime
        case defaultEnergy, contexts, location
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let timezoneName = try container.decode(String.self, forKey: .timezoneName)
        let availability = try container.decode(
            [ScheduleAvailabilityDay].self,
            forKey: .availability
        )
        let sleep = try container.decode(ScheduleSleepInterval.self, forKey: .sleep)
        let protectedTime = try container.decode(
            [ScheduleProtectedDay].self,
            forKey: .protectedTime
        )
        let defaultEnergy = try container.decode(EnergyLevel.self, forKey: .defaultEnergy)
        let contexts = try container.decode([String].self, forKey: .contexts)
        let location = try container.decodeIfPresent(String.self, forKey: .location)
        do {
            try self.init(
                timezoneName: timezoneName,
                availability: availability,
                sleep: sleep,
                protectedTime: protectedTime,
                defaultEnergy: defaultEnergy,
                contexts: contexts,
                location: location
            )
            guard self.availability == availability,
                  self.protectedTime == protectedTime,
                  self.contexts == contexts,
                  self.location == location else {
                throw ScheduleProfileValidationError.invalidContexts
            }
        } catch {
            throw DecodingError.dataCorruptedError(
                forKey: .timezoneName,
                in: container,
                debugDescription: error.localizedDescription
            )
        }
    }
}

struct ExpandedScheduleProfile: Equatable, Sendable {
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let availability: [DayWeaveSchedulePreviewRequest.Availability]
    let fixedBlocks: [DayWeaveSchedulePreviewRequest.FixedBlock]
}

enum ScheduleProfileExpansionError: LocalizedError, Equatable, Sendable {
    case invalidProfile
    case invalidClock
    case invalidFixedBlockSet

    var errorDescription: String? {
        switch self {
        case .invalidProfile: "The encrypted schedule profile is invalid."
        case .invalidClock: "The schedule horizon could not be represented in the profile timezone."
        case .invalidFixedBlockSet: "The profile produced duplicate fixed-block identities."
        }
    }
}

extension ScheduleProfile {
    func expanded(
        asOf requestedAsOf: Date,
        horizonDayCount: Int = 7
    ) throws(ScheduleProfileExpansionError) -> ExpandedScheduleProfile {
        guard hasValidShape,
              requestedAsOf.timeIntervalSinceReferenceDate.isFinite,
              horizonDayCount == 7,
              let timezone = TimeZone(identifier: timezoneName) else {
            throw .invalidProfile
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = timezone
        let horizonStart = calendar.startOfDay(for: requestedAsOf)
        guard let horizonEnd = calendar.date(
            byAdding: .day,
            value: horizonDayCount,
            to: horizonStart
        ), horizonStart <= requestedAsOf, requestedAsOf < horizonEnd else {
            throw .invalidClock
        }

        let daysByWeekday = Dictionary(uniqueKeysWithValues: availability.map {
            ($0.weekday, $0)
        })
        var expandedAvailability: [DayWeaveSchedulePreviewRequest.Availability] = []
        for offset in 0..<horizonDayCount {
            guard let day = calendar.date(byAdding: .day, value: offset, to: horizonStart),
                  let weekday = ScheduleWeekday.weekday(containing: day, calendar: calendar),
                  let availabilityDay = daysByWeekday[weekday] else {
                throw .invalidClock
            }
            guard availabilityDay.isEnabled else { continue }
            for window in availabilityDay.windows {
                guard let wallStart = Self.wallDate(
                    window.start,
                    on: day,
                    calendar: calendar,
                    repeatedTimePolicy: .last
                ), let wallEnd = Self.wallDate(
                    window.end,
                    on: day,
                    calendar: calendar,
                    repeatedTimePolicy: .first
                ) else {
                    throw .invalidClock
                }
                let start = max(wallStart, requestedAsOf, horizonStart)
                let end = min(wallEnd, horizonEnd)
                if start < end {
                    expandedAvailability.append(.init(
                        start: start,
                        end: end,
                        contexts: contexts,
                        location: location,
                        energy: defaultEnergy.rawValue
                    ))
                }
            }
        }

        var fixedBlocks: [DayWeaveSchedulePreviewRequest.FixedBlock] = []
        for offset in -1..<horizonDayCount {
            guard let startDay = calendar.date(byAdding: .day, value: offset, to: horizonStart),
                  let endDay = calendar.date(byAdding: .day, value: offset + 1, to: horizonStart),
                  let start = Self.wallDate(
                    sleep.start,
                    on: startDay,
                    calendar: calendar,
                    repeatedTimePolicy: .first
                  ), let end = Self.wallDate(
                    sleep.end,
                    on: endDay,
                    calendar: calendar,
                    repeatedTimePolicy: .last
                  ), start < end else {
                throw .invalidClock
            }
            if end > horizonStart, start < horizonEnd {
                fixedBlocks.append(.init(
                    id: try Self.fixedBlockID(
                        kind: "sleep",
                        timezoneName: timezoneName,
                        localAnchor: startDay,
                        start: sleep.start,
                        end: sleep.end,
                        calendar: calendar
                    ),
                    isSensitive: true,
                    title: "Sleep",
                    start: start,
                    end: end,
                    source: "sleep"
                ))
            }
        }
        let protectedByWeekday = Dictionary(uniqueKeysWithValues: protectedTime.map {
            ($0.weekday, $0)
        })
        for offset in 0..<horizonDayCount {
            guard let day = calendar.date(byAdding: .day, value: offset, to: horizonStart),
                  let weekday = ScheduleWeekday.weekday(containing: day, calendar: calendar),
                  let protectedDay = protectedByWeekday[weekday] else {
                throw .invalidClock
            }
            guard protectedDay.isEnabled else { continue }
            for protectedWindow in protectedDay.windows {
                guard
                      let start = Self.wallDate(
                        protectedWindow.start,
                        on: day,
                        calendar: calendar,
                        repeatedTimePolicy: .first
                      ), let end = Self.wallDate(
                        protectedWindow.end,
                        on: day,
                        calendar: calendar,
                        repeatedTimePolicy: .last
                      ), start < end else {
                    throw .invalidClock
                }
                if end > horizonStart, start < horizonEnd {
                    fixedBlocks.append(.init(
                        id: try Self.fixedBlockID(
                            kind: "protected_time",
                            timezoneName: timezoneName,
                            localAnchor: day,
                            start: protectedWindow.start,
                            end: protectedWindow.end,
                            calendar: calendar
                        ),
                        isSensitive: true,
                        title: "Protected time",
                        start: start,
                        end: end,
                        source: "protected_time"
                    ))
                }
            }
        }
        fixedBlocks.sort {
            if $0.start != $1.start { return $0.start < $1.start }
            if $0.end != $1.end { return $0.end < $1.end }
            return $0.id.uuidString < $1.id.uuidString
        }
        // Wall-clock intervals are disjoint in the validated profile. During
        // a fall-back, however, `.first` starts and `.last` ends may make two
        // adjacent protective intervals overlap in absolute time. Retaining
        // both is intentional: the scheduler unions external busy time, and
        // dropping or shortening either block could invent usable capacity.
        guard Set(fixedBlocks.map(\.id)).count == fixedBlocks.count else {
            throw .invalidFixedBlockSet
        }

        return ExpandedScheduleProfile(
            asOf: requestedAsOf,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: timezoneName,
            availability: expandedAvailability,
            fixedBlocks: fixedBlocks
        )
    }

    private static func wallDate(
        _ time: ScheduleLocalTime,
        on day: Date,
        calendar: Calendar,
        repeatedTimePolicy: Calendar.RepeatedTimePolicy
    ) -> Date? {
        calendar.date(
            bySettingHour: time.hour,
            minute: time.minute,
            second: 0,
            of: day,
            matchingPolicy: .strict,
            repeatedTimePolicy: repeatedTimePolicy,
            direction: .forward
        )
    }

    private static func fixedBlockID(
        kind: String,
        timezoneName: String,
        localAnchor: Date,
        start: ScheduleLocalTime,
        end: ScheduleLocalTime,
        calendar: Calendar
    ) throws(ScheduleProfileExpansionError) -> UUID {
        let components = calendar.dateComponents([.year, .month, .day], from: localAnchor)
        guard let year = components.year,
              let month = components.month,
              let day = components.day else {
            throw .invalidClock
        }
        let identity = [
            "dayweave-schedule-profile-v1",
            kind,
            timezoneName,
            String(format: "%04d-%02d-%02d", year, month, day),
            String(start.minutesSinceMidnight),
            String(end.minutesSinceMidnight),
        ].joined(separator: "|")
        var bytes = Array(SHA256.hash(data: Data(identity.utf8)).prefix(16))
        // RFC 9562 UUIDv8 leaves the payload application-defined while keeping
        // the standard variant/version bits recognizable to every decoder.
        bytes[6] = (bytes[6] & 0x0f) | 0x80
        bytes[8] = (bytes[8] & 0x3f) | 0x80
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }
}
