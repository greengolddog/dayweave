import Foundation

/// An app-owned canonical item body proposed by Codex. The summary is display
/// context only; it is never promoted into canonical item fields implicitly.
struct CodexSuggestionDraft: Equatable, Sendable {
    let summary: String
    let canonicalDraft: DayWeaveCanonicalItemDraft
}

/// The Codex review sheet is an authority boundary: every model-controlled
/// semantic value must be visible and editable there before approval. This is
/// intentionally narrower than the complete canonical authoring schema,
/// because existing items may retain advanced fields that this editor does not
/// yet render.
struct CodexCanonicalItemDraftReviewValidator {
    static func accepts(
        _ draft: DayWeaveCanonicalItemDraft,
        itemID: UUID,
        now: Date
    ) -> Bool {
        let draft = draft.normalized
        guard draft.isSensitive,
              draft.status == .inbox,
              draft.parentID == nil,
              draft.siblingOrder == 0 else {
            return false
        }
        return acceptsReviewedDraft(draft, itemID: itemID, now: now)
    }

    /// Revalidates the exact user-edited body at approval time. Unlike model
    /// ingress, sensitivity, Inbox/Planned state, and hierarchy are visible
    /// user choices, but no hidden semantic field may appear in the result.
    static func acceptsReviewedDraft(
        _ draft: DayWeaveCanonicalItemDraft,
        itemID: UUID,
        now: Date
    ) -> Bool {
        let draft = draft.normalized
        guard draft.validationIssue(itemID: itemID) == nil,
              dateIsVisibleToMinute(draft.deadlineAt, in: draft.timezoneName),
              dateIsVisibleToMinute(draft.earliestStartAt, in: draft.timezoneName),
              constraintsAreFullyReviewable(draft) else {
            return false
        }

        let editorState = CanonicalItemEditorState(
            itemID: itemID,
            draft: draft,
            now: now,
            timezoneName: draft.timezoneName
        )
        guard editorState.readOnlyDiagnostic == nil,
              editorState.validationIssue == nil,
              editorState.draft == draft else {
            return false
        }

        if draft.kind == .event {
            guard draft.splitPolicy == .indivisible,
                  instantIsVisibleToMinute(
                      editorState.eventStart,
                      in: draft.timezoneName
                  ),
                  instantIsVisibleToMinute(
                      editorState.eventEnd,
                      in: draft.timezoneName
                  ),
                  let start = dayWeavePostgresEpochMicroseconds(editorState.eventStart),
                  let end = dayWeavePostgresEpochMicroseconds(editorState.eventEnd),
                  end > start else {
                return false
            }
            if let duration = draft.durationSeconds {
                let durationMicros = Int64(duration) * 1_000_000
                guard end - start == durationMicros else { return false }
            }
        }
        return true
    }

    private static func constraintsAreFullyReviewable(
        _ draft: DayWeaveCanonicalItemDraft
    ) -> Bool {
        guard case let .object(constraints) = draft.flexibleConstraints else {
            return false
        }
        if draft.kind == .event {
            guard Set(constraints.keys) == ["dayweave_firm_block"],
                  case let .object(firm)? = constraints["dayweave_firm_block"],
                  Set(firm.keys) == [
                      "owned", "starts_at", "ends_at", "all_day", "tentative", "busy",
                  ],
                  firm["owned"] == .bool(true),
                  case let .string(startsAt)? = firm["starts_at"],
                  case let .string(endsAt)? = firm["ends_at"],
                  CodexRFC3339Instant(startsAt)?.isWholeMinute == true,
                  CodexRFC3339Instant(endsAt)?.isWholeMinute == true,
                  case .bool? = firm["all_day"],
                  case .bool? = firm["tentative"],
                  case .bool? = firm["busy"] else {
                return false
            }
            return true
        }

        var allowed: Set<String> = ["energy"]
        if draft.kind == .goal || draft.kind == .routine {
            allowed.insert("has_own_effort")
            guard case .bool? = constraints["has_own_effort"] else { return false }
        }
        return Set(constraints.keys).isSubset(of: allowed)
    }

    private static func dateIsVisibleToMinute(
        _ date: Date?,
        in timezoneName: String
    ) -> Bool {
        date.map { instantIsVisibleToMinute($0, in: timezoneName) } ?? true
    }

    private static func instantIsVisibleToMinute(
        _ date: Date,
        in timezoneName: String
    ) -> Bool {
        guard let microseconds = dayWeavePostgresEpochMicroseconds(date),
              microseconds % 60_000_000 == 0,
              let timeZone = DayWeaveCanonicalItemDraft.supportedTimeZone(
                  identifier: timezoneName
              ) else {
            return false
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let precision = calendar.dateComponents([.second, .nanosecond], from: date)
        guard precision.second == 0, (precision.nanosecond ?? 0) == 0 else {
            return false
        }

        // A DatePicker shows only the local date, hour, and minute. During a
        // fall-back fold two different instants can have that exact same
        // presentation, leaving the model-controlled UTC offset invisible.
        // Reject both occurrences rather than silently choosing one for the
        // user; an ordinary manual edit can still author the desired instant.
        let localComponents = calendar.dateComponents(
            [.era, .year, .month, .day, .hour, .minute, .second],
            from: date
        )
        let searchStart = date.addingTimeInterval(-36 * 60 * 60)
        guard let first = calendar.nextDate(
                  after: searchStart,
                  matching: localComponents,
                  matchingPolicy: .strict,
                  repeatedTimePolicy: .first,
                  direction: .forward
              ),
              let last = calendar.nextDate(
                  after: searchStart,
                  matching: localComponents,
                  matchingPolicy: .strict,
                  repeatedTimePolicy: .last,
                  direction: .forward
              ) else {
            return false
        }
        return first == last && first == date
    }
}

/// Parses the only machine-writable surface in a Codex final answer. This is a
/// deliberately separate trust boundary from canonical API decoding: app-owned
/// fields are constructed locally, all model-supplied keys are exact, and one
/// invalid member rejects the complete envelope.
struct CodexProposalEnvelopeParser {
    static let startMarker = "<dayweave-item-drafts-v1>"
    static let endMarker = "</dayweave-item-drafts-v1>"
    static let maximumEnvelopeBytes = 64 * 1_024
    static let maximumSuggestions = 5

    private static let schema = "dayweave.item-drafts/1"
    private static let maximumSummaryScalars = 1_000
    private static let envelopeKeys: Set<String> = ["schema", "drafts"]
    private static let suggestionKeys: Set<String> = ["summary", "item"]
    private static let itemKeys: Set<String> = [
        "kind", "title", "notes", "timezone_name", "duration_seconds",
        "deadline_at", "earliest_start_at", "recurrence",
        "flexible_constraints", "split_policy", "importance", "urgency",
    ]

    struct Result: Equatable, Sendable {
        let visibleText: String
        let drafts: [CodexSuggestionDraft]
        let containedInvalidEnvelope: Bool
    }

    static func parse(_ rawText: String) -> Result {
        guard let start = rawText.range(of: startMarker) else {
            return Result(
                visibleText: hidingPartialMarker(in: rawText),
                drafts: [],
                containedInvalidEnvelope: false
            )
        }
        let visible = String(rawText[..<start.lowerBound])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard let end = rawText.range(
            of: endMarker,
            range: start.upperBound..<rawText.endIndex
        ), rawText[end.upperBound...].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return invalidResult(visibleText: visible)
        }

        let encoded = String(rawText[start.upperBound..<end.lowerBound])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard encoded.utf8.count <= maximumEnvelopeBytes,
              let data = encoded.data(using: .utf8),
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: data),
              let rawRoot = try? JSONSerialization.jsonObject(with: data),
              let root = rawRoot as? [String: Any],
              allTextAndNumbersAreSafe(in: root),
              Set(root.keys) == envelopeKeys,
              root["schema"] as? String == schema,
              let rawDrafts = root["drafts"] as? [Any],
              rawDrafts.count <= maximumSuggestions else {
            return invalidResult(visibleText: visible)
        }

        var drafts: [CodexSuggestionDraft] = []
        for rawDraft in rawDrafts {
            guard let draft = parseDraft(rawDraft) else {
                return invalidResult(visibleText: visible)
            }
            drafts.append(draft)
        }
        return Result(
            visibleText: visible,
            drafts: drafts,
            containedInvalidEnvelope: false
        )
    }

    static func visibleStreamingText(_ rawText: String) -> String {
        guard let start = rawText.range(of: startMarker) else {
            return hidingPartialMarker(in: rawText)
        }
        return String(rawText[..<start.lowerBound])
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func parseDraft(_ rawValue: Any) -> CodexSuggestionDraft? {
        guard let object = rawValue as? [String: Any],
              Set(object.keys) == suggestionKeys,
              let rawSummary = object["summary"] as? String,
              let item = object["item"] as? [String: Any],
              Set(item.keys) == itemKeys else { return nil }

        let summary = rawSummary.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !summary.isEmpty,
              summary.unicodeScalars.count <= maximumSummaryScalars,
              let canonicalDraft = parseCanonicalDraft(item) else { return nil }
        return CodexSuggestionDraft(summary: summary, canonicalDraft: canonicalDraft)
    }

    private static func parseCanonicalDraft(
        _ item: [String: Any]
    ) -> DayWeaveCanonicalItemDraft? {
        guard let rawKind = item["kind"] as? String,
              let kind = supportedKind(rawKind),
              let rawTitle = item["title"] as? String,
              let notes = nullableString(item["notes"]),
              let timezoneName = item["timezone_name"] as? String,
              DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName) != nil,
              let durationSeconds = nullableUInt32(item["duration_seconds"]),
              let deadlineAt = nullableDate(item["deadline_at"]),
              let earliestStartAt = nullableDate(item["earliest_start_at"]),
              let recurrence = nullableJSONObject(item["recurrence"]),
              let flexibleConstraints = jsonObject(item["flexible_constraints"]),
              let splitPolicy = splitPolicy(item["split_policy"]),
              let importance = score(item["importance"]),
              let urgency = score(item["urgency"]) else { return nil }

        let title = rawTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let candidate = DayWeaveCanonicalItemDraft(
            isSensitive: true,
            kind: kind,
            status: .inbox,
            title: title,
            notes: notes,
            timezoneName: timezoneName,
            durationSeconds: durationSeconds,
            deadlineAt: deadlineAt,
            earliestStartAt: earliestStartAt,
            recurrence: recurrence,
            flexibleConstraints: flexibleConstraints,
            splitPolicy: splitPolicy,
            importance: importance,
            urgency: urgency,
            parentID: nil,
            siblingOrder: 0
        ).normalized
        let validationID = UUID()
        guard CodexCanonicalItemDraftReviewValidator.accepts(
            candidate,
            itemID: validationID,
            now: Date()
        ) else { return nil }
        return candidate
    }

    private static func supportedKind(_ value: String) -> DayWeaveCanonicalItemKind? {
        switch value {
        case "event": .event
        case "task": .task
        case "habit": .habit
        case "routine": .routine
        case "goal": .goal
        case "break": .breakTime
        default: nil
        }
    }

    /// A JSON null is a successful optional parse, so this helper returns a
    /// doubly optional value: outer nil means invalid, inner nil means null.
    private static func nullableString(_ rawValue: Any?) -> String?? {
        guard let rawValue else { return nil }
        if rawValue is NSNull { return .some(nil) }
        guard let value = rawValue as? String else { return nil }
        return .some(value)
    }

    private static func nullableUInt32(_ rawValue: Any?) -> UInt32?? {
        guard let rawValue else { return nil }
        if rawValue is NSNull { return .some(nil) }
        guard let value = unsignedInteger(rawValue), value <= UInt64(UInt32.max) else {
            return nil
        }
        return .some(UInt32(value))
    }

    private static func nullableDate(_ rawValue: Any?) -> Date?? {
        guard let rawValue else { return nil }
        if rawValue is NSNull { return .some(nil) }
        guard let value = rawValue as? String,
              let instant = CodexRFC3339Instant(value),
              instant.isWholeMinute,
              dayWeavePostgresEpochMicroseconds(instant.date) != nil else { return nil }
        return .some(instant.date)
    }

    private static func nullableJSONObject(_ rawValue: Any?) -> JSONValue?? {
        guard let rawValue else { return nil }
        if rawValue is NSNull { return .some(nil) }
        guard let value = jsonObject(rawValue) else { return nil }
        return .some(value)
    }

    private static func jsonObject(_ rawValue: Any?) -> JSONValue? {
        guard let object = rawValue as? [String: Any],
              let converted = jsonValue(object),
              case .object = converted else { return nil }
        return converted
    }

    private static func splitPolicy(_ rawValue: Any?) -> DayWeaveSplitPolicy? {
        guard let object = rawValue as? [String: Any],
              let type = object["type"] as? String else { return nil }
        switch type {
        case "indivisible":
            guard Set(object.keys) == ["type"] else { return nil }
            return .indivisible
        case "splittable":
            guard Set(object.keys) == [
                "type", "minimum_chunk_seconds", "maximum_chunk_seconds",
            ], let minimum = unsignedInteger(object["minimum_chunk_seconds"]),
               let maximum = unsignedInteger(object["maximum_chunk_seconds"]),
               minimum <= UInt64(UInt32.max), maximum <= UInt64(UInt32.max) else {
                return nil
            }
            return .splittable(
                minimumChunkSeconds: UInt32(minimum),
                maximumChunkSeconds: UInt32(maximum)
            )
        default:
            return nil
        }
    }

    private static func score(_ rawValue: Any?) -> UInt8? {
        guard let value = unsignedInteger(rawValue), value <= 100 else { return nil }
        return UInt8(value)
    }

    private static func unsignedInteger(_ rawValue: Any?) -> UInt64? {
        guard let number = rawValue as? NSNumber else { return nil }
        let encoding = String(cString: number.objCType)
        guard !["c", "B", "f", "d"].contains(encoding) else { return nil }
        return UInt64(number.stringValue)
    }

    private static func jsonValue(_ rawValue: Any) -> JSONValue? {
        if rawValue is NSNull { return .null }
        if let string = rawValue as? String { return .string(string) }
        if let number = rawValue as? NSNumber {
            let encoding = String(cString: number.objCType)
            if ["c", "B"].contains(encoding) { return .bool(number.boolValue) }
            guard !["f", "d"].contains(encoding),
                  let unsigned = UInt64(number.stringValue) else { return nil }
            return .number(JSONNumber(unsigned))
        }
        if let array = rawValue as? [Any] {
            var converted: [JSONValue] = []
            converted.reserveCapacity(array.count)
            for element in array {
                guard let value = jsonValue(element) else { return nil }
                converted.append(value)
            }
            return .array(converted)
        }
        if let object = rawValue as? [String: Any] {
            var converted: [String: JSONValue] = [:]
            converted.reserveCapacity(object.count)
            for (key, rawValue) in object {
                guard let value = jsonValue(rawValue) else { return nil }
                converted[key] = value
            }
            return .object(converted)
        }
        return nil
    }

    private static func allTextAndNumbersAreSafe(in rawValue: Any) -> Bool {
        if rawValue is NSNull { return true }
        if let string = rawValue as? String { return isSafeText(string) }
        if let number = rawValue as? NSNumber {
            let encoding = String(cString: number.objCType)
            if ["c", "B"].contains(encoding) { return true }
            return !["f", "d"].contains(encoding) && UInt64(number.stringValue) != nil
        }
        if let array = rawValue as? [Any] {
            return array.allSatisfy(allTextAndNumbersAreSafe)
        }
        if let object = rawValue as? [String: Any] {
            return object.allSatisfy { key, value in
                isSafeText(key) && allTextAndNumbersAreSafe(in: value)
            }
        }
        return false
    }

    private static func isSafeText(_ value: String) -> Bool {
        !value.unicodeScalars.contains { scalar in
            CharacterSet.controlCharacters.contains(scalar)
                || scalar.value == 0x061C
                || scalar.value == 0x200E
                || scalar.value == 0x200F
                || (0x202A...0x202E).contains(scalar.value)
                || (0x2066...0x2069).contains(scalar.value)
        }
    }

    private static func invalidResult(visibleText: String) -> Result {
        Result(visibleText: visibleText, drafts: [], containedInvalidEnvelope: true)
    }

    private static func hidingPartialMarker(in text: String) -> String {
        let maximumPrefixLength = min(startMarker.count - 1, text.count)
        if maximumPrefixLength > 0 {
            for length in stride(from: maximumPrefixLength, through: 1, by: -1) {
                let prefix = startMarker.prefix(length)
                if text.hasSuffix(prefix) { return String(text.dropLast(length)) }
            }
        }
        return text
    }
}

/// A strict RFC 3339 instant parser for model-supplied canonical fields.
/// Foundation's formatter accepts impossible calendar dates and offset forms.
private struct CodexRFC3339Instant {
    private static let secondsFromYearOneToUnixEpoch: Int64 = 62_135_596_800

    let date: Date
    let isWholeMinute: Bool

    init?(_ value: String) {
        let bytes = Array(value.utf8)
        guard bytes.count >= 20,
              bytes[4] == 0x2D, bytes[7] == 0x2D,
              bytes[10] == 0x54,
              bytes[13] == 0x3A, bytes[16] == 0x3A,
              let year = Self.decimal(bytes, 0..<4),
              let month = Self.decimal(bytes, 5..<7),
              let day = Self.decimal(bytes, 8..<10),
              let hour = Self.decimal(bytes, 11..<13),
              let minute = Self.decimal(bytes, 14..<16),
              let second = Self.decimal(bytes, 17..<19),
              (1...9_999).contains(year),
              (1...12).contains(month),
              (1...Self.daysInMonth(month, year: year)).contains(day),
              (0...23).contains(hour),
              (0...59).contains(minute),
              (0...59).contains(second) else { return nil }

        var cursor = 19
        var fraction = 0.0
        if cursor < bytes.count, bytes[cursor] == 0x2E {
            cursor += 1
            let fractionStart = cursor
            var place = 0.1
            while cursor < bytes.count, Self.isDigit(bytes[cursor]) {
                guard cursor - fractionStart < 9 else { return nil }
                fraction += Double(bytes[cursor] - 0x30) * place
                place /= 10
                cursor += 1
            }
            guard cursor > fractionStart else { return nil }
        }

        let offsetSeconds: Int
        if cursor + 1 == bytes.count, bytes[cursor] == 0x5A {
            offsetSeconds = 0
        } else {
            guard cursor + 6 == bytes.count,
                  bytes[cursor] == 0x2B || bytes[cursor] == 0x2D,
                  bytes[cursor + 3] == 0x3A,
                  let offsetHour = Self.decimal(bytes, (cursor + 1)..<(cursor + 3)),
                  let offsetMinute = Self.decimal(bytes, (cursor + 4)..<(cursor + 6)),
                  (0...23).contains(offsetHour),
                  (0...59).contains(offsetMinute) else { return nil }
            let magnitude = offsetHour * 3_600 + offsetMinute * 60
            offsetSeconds = bytes[cursor] == 0x2D ? -magnitude : magnitude
        }

        let priorYear = Int64(year - 1)
        var elapsedDays = priorYear * 365
            + priorYear / 4
            - priorYear / 100
            + priorYear / 400
        let daysBeforeMonth = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
        elapsedDays += Int64(daysBeforeMonth[month - 1])
        if month > 2, Self.isLeapYear(year) { elapsedDays += 1 }
        elapsedDays += Int64(day - 1)
        let wholeSeconds = elapsedDays * 86_400
            + Int64(hour * 3_600 + minute * 60 + second - offsetSeconds)
        let timestamp = TimeInterval(wholeSeconds - Self.secondsFromYearOneToUnixEpoch)
            + fraction
        guard timestamp.isFinite else { return nil }
        date = Date(timeIntervalSince1970: timestamp)
        isWholeMinute = second == 0 && fraction == 0
    }

    private static func decimal(_ bytes: [UInt8], _ range: Range<Int>) -> Int? {
        var result = 0
        for index in range {
            guard index < bytes.count, isDigit(bytes[index]) else { return nil }
            result = result * 10 + Int(bytes[index] - 0x30)
        }
        return result
    }

    private static func daysInMonth(_ month: Int, year: Int) -> Int {
        switch month {
        case 2: isLeapYear(year) ? 29 : 28
        case 4, 6, 9, 11: 30
        default: 31
        }
    }

    private static func isLeapYear(_ year: Int) -> Bool {
        year.isMultiple(of: 400) || (year.isMultiple(of: 4) && !year.isMultiple(of: 100))
    }

    private static func isDigit(_ byte: UInt8) -> Bool {
        (0x30...0x39).contains(byte)
    }
}
