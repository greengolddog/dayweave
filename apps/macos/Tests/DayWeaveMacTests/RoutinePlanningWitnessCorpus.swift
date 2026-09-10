import Foundation

/// Test corpus extraction preserves the producer's numeric tokens. Foundation
/// JSON object reserialization would silently turn the negative `1.0` into `1`.
struct RoutinePlanningCorpusValue {
    let data: Data
    func string() throws -> String { try JSONDecoder().decode(String.self, from: data) }
    func object() throws -> [String: Self] {
        var reader = Reader(data), result: [String: Self] = [:]
        try reader.take(123)
        if reader.next == 125 { return result }
        repeat {
            let name = try reader.value().string()
            try reader.take(58)
            guard result.updateValue(try reader.value(), forKey: name) == nil else { throw Failure.invalid }
            if reader.next == 125 { try reader.take(125); return result }
            try reader.take(44)
        } while true
    }
    func array() throws -> [Self] {
        var reader = Reader(data), result: [Self] = []
        try reader.take(91)
        if reader.next == 93 { return result }
        repeat {
            result.append(try reader.value())
            if reader.next == 93 { try reader.take(93); return result }
            try reader.take(44)
        } while true
    }
    private enum Failure: Error { case invalid }
    private struct Reader {
        let bytes: [UInt8]
        var index = 0
        init(_ data: Data) { bytes = Array(data) }
        var next: UInt8? {
            mutating get { skip(); return index < bytes.count ? bytes[index] : nil }
        }
        mutating func skip() { while index < bytes.count, [9, 10, 13, 32].contains(bytes[index]) { index += 1 } }
        mutating func take(_ expected: UInt8) throws {
            guard next == expected else { throw Failure.invalid }; index += 1
        }
        mutating func value() throws -> RoutinePlanningCorpusValue {
            skip(); let start = index
            var depth = 0, quoted = false, escaped = false
            while index < bytes.count {
                let byte = bytes[index]
                if quoted {
                    if escaped { escaped = false }
                    else if byte == 92 { escaped = true }
                    else if byte == 34 { quoted = false; if depth == 0 { index += 1; break } }
                } else if byte == 34 { quoted = true }
                else if byte == 123 || byte == 91 { depth += 1 }
                else if byte == 125 || byte == 93 {
                    if depth == 0 { break }
                    depth -= 1
                    if depth == 0 { index += 1; break }
                } else if depth == 0 && [9, 10, 13, 32, 44].contains(byte) { break }
                index += 1
            }
            guard index > start, depth == 0, !quoted else { throw Failure.invalid }
            return .init(data: Data(bytes[start..<index]))
        }
    }
}
