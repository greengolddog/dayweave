import Foundation

/// A date-only deadline ends at the exact next civil midnight in its own
/// timezone. Resolve strictly: a skipped midnight is not the following valid
/// time, and an ambiguous midnight uses its earlier occurrence.
enum CanonicalDateDeadline {
    static func boundary(_ value: String, timezoneName: String) -> Date? {
        guard let civil = components(value),
              let timezone = DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName),
              !(civil.year == 9999 && civil.month == 12 && civil.day == 31) else { return nil }
        // The RFC3339 parser uses proleptic Gregorian arithmetic, unlike
        // Foundation Calendar (including .iso8601), which skips 1582-10-05…14.
        guard let start = pickerDate(value) else { return nil }
        let nextCivilUTC = start.addingTimeInterval(86_400)
        let lower = nextCivilUTC.addingTimeInterval(-172_800)
        let upper = nextCivilUTC.addingTimeInterval(172_800)
        var offsets = Set([timezone.secondsFromGMT(for: lower), timezone.secondsFromGMT(for: upper),
                           timezone.secondsFromGMT(for: nextCivilUTC)])
        var cursor = lower
        while let transition = timezone.nextDaylightSavingTimeTransition(after: cursor), transition <= upper {
            guard transition > cursor else { return nil }
            offsets.insert(timezone.secondsFromGMT(for: transition.addingTimeInterval(-1)))
            offsets.insert(timezone.secondsFromGMT(for: transition))
            cursor = transition.addingTimeInterval(1)
        }
        // Only exact inverse mappings are accepted. A gap has none; a fold
        // has two and the smaller UTC instant is the server's chosen boundary.
        return offsets.compactMap { offset -> Date? in
            let candidate = nextCivilUTC.addingTimeInterval(-TimeInterval(offset))
            return timezone.secondsFromGMT(for: candidate) == offset ? candidate : nil
        }.min()
    }

    /// A timezone-free proleptic civil midnight, used only as an arithmetic
    /// reference. It is never stored as the date-only deadline's wire value.
    static func pickerDate(_ value: String) -> Date? {
        guard components(value) != nil else { return nil }
        return CanonicalRFC3339Instant("\(value)T00:00:00Z")?.dateAtWholeSecond
    }

    static func string(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = TimeZone(secondsFromGMT: 0)!
        formatter.gregorianStartDate = Date(timeIntervalSince1970: -100_000_000_000)
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter.string(from: date)
    }

    static func isCanonical(_ value: String) -> Bool { components(value) != nil && value != "9999-12-31" }

    private static func components(_ value: String) -> DateComponents? {
        guard value.utf8.count == 10,
              value.utf8.enumerated().allSatisfy({ index, byte in
                  index == 4 || index == 7 ? byte == 45 : (48...57).contains(byte)
              }), let year = Int(value.prefix(4)), (1...9999).contains(year),
              let month = Int(value.dropFirst(5).prefix(2)), (1...12).contains(month),
              let day = Int(value.suffix(2)) else { return nil }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
        let days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        guard (1...days[month - 1]).contains(day) else { return nil }
        return DateComponents(year: year, month: month, day: day)
    }
}
