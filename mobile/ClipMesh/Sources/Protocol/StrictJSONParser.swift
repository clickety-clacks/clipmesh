import Foundation

struct StrictJSONParser {
    private let bytes: [UInt8]
    private var index = 0

    init(data: Data) throws {
        guard String(data: data, encoding: .utf8) != nil else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        bytes = Array(data)
    }

    mutating func parse() throws -> JSONValue {
        skipWhitespace()
        let value = try parseValue()
        skipWhitespace()
        guard index == bytes.count else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return value
    }

    private mutating func parseValue() throws -> JSONValue {
        guard let byte = current else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        switch byte {
        case 0x22:
            return try .string(parseString())
        case 0x2D, 0x30 ... 0x39:
            return try .number(parseNumber())
        case 0x5B:
            return try .array(parseArray())
        case 0x7B:
            return try .object(parseObject())
        case 0x66:
            try consumeLiteral("false")
            return .bool(false)
        case 0x6E:
            try consumeLiteral("null")
            return .null
        case 0x74:
            try consumeLiteral("true")
            return .bool(true)
        default:
            throw ProtocolFailure.protocolSchemaInvalid
        }
    }

    private mutating func parseArray() throws -> [JSONValue] {
        try consume(0x5B)
        skipWhitespace()
        if current == 0x5D {
            index += 1
            return []
        }

        var values: [JSONValue] = []
        while true {
            skipWhitespace()
            try values.append(parseValue())
            skipWhitespace()
            if current == 0x5D {
                index += 1
                return values
            }
            try consume(0x2C)
        }
    }

    private mutating func parseObject() throws -> [String: JSONValue] {
        try consume(0x7B)
        skipWhitespace()
        if current == 0x7D {
            index += 1
            return [:]
        }

        var object: [String: JSONValue] = [:]
        while true {
            skipWhitespace()
            guard current == 0x22 else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            let key = try parseString()
            guard object[key] == nil else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            skipWhitespace()
            try consume(0x3A)
            skipWhitespace()
            object[key] = try parseValue()
            skipWhitespace()
            if current == 0x7D {
                index += 1
                return object
            }
            try consume(0x2C)
        }
    }

    private mutating func parseString() throws -> String {
        let start = index
        try consume(0x22)
        var escaped = false
        while let byte = current {
            if byte < 0x20 {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            index += 1
            if escaped {
                escaped = false
            } else if byte == 0x5C {
                escaped = true
            } else if byte == 0x22 {
                let token = Data(bytes[start ..< index])
                return try JSONDecoder().decode(String.self, from: token)
            }
        }
        throw ProtocolFailure.protocolSchemaInvalid
    }

    private mutating func parseNumber() throws -> String {
        let start = index
        if current == 0x2D {
            index += 1
        }
        guard let first = current else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        if first == 0x30 {
            index += 1
            if current.map({ (0x30 ... 0x39).contains($0) }) == true {
                throw ProtocolFailure.protocolSchemaInvalid
            }
        } else if (0x31 ... 0x39).contains(first) {
            consumeDigits()
        } else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        if current == 0x2E {
            index += 1
            guard current.map({ (0x30 ... 0x39).contains($0) }) == true else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            consumeDigits()
        }
        if current == 0x45 || current == 0x65 {
            index += 1
            if current == 0x2B || current == 0x2D {
                index += 1
            }
            guard current.map({ (0x30 ... 0x39).contains($0) }) == true else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            consumeDigits()
        }
        return String(decoding: bytes[start ..< index], as: UTF8.self)
    }

    private mutating func consumeDigits() {
        while current.map({ (0x30 ... 0x39).contains($0) }) == true {
            index += 1
        }
    }

    private mutating func consumeLiteral(_ literal: StaticString) throws {
        for byte in literal.withUTF8Buffer({ Array($0) }) {
            try consume(byte)
        }
    }

    private mutating func consume(_ expected: UInt8) throws {
        guard current == expected else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        index += 1
    }

    private mutating func skipWhitespace() {
        while current.map({ [0x09, 0x0A, 0x0D, 0x20].contains($0) }) == true {
            index += 1
        }
    }

    private var current: UInt8? {
        index < bytes.count ? bytes[index] : nil
    }
}
