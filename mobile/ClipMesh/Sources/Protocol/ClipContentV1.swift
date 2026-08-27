import CryptoKit
import Foundation

struct ClipContentV1: Equatable, CustomDebugStringConvertible, CustomStringConvertible {
    private let bytes: Data

    static func fromWire(
        contentType: String,
        payloadBase64URL: String,
        payloadBytes: Int,
        contentSHA256: String,
        maximumBytes: Int,
    ) throws -> Self {
        guard contentType == "text/plain" else {
            throw ProtocolFailure.contentTypeUnsupported
        }
        guard !payloadBase64URL.contains("="),
              payloadBase64URL.unicodeScalars.allSatisfy({ scalar in
                  scalar.value == 0x2D || scalar.value == 0x5F
                      || (0x30 ... 0x39).contains(scalar.value)
                      || (0x41 ... 0x5A).contains(scalar.value)
                      || (0x61 ... 0x7A).contains(scalar.value)
              })
        else {
            throw ProtocolFailure.payloadEncodingInvalid
        }
        let translated = payloadBase64URL.replacing("-", with: "+").replacing("_", with: "/")
        let padding = String(repeating: "=", count: (4 - translated.count % 4) % 4)
        guard let data = Data(base64Encoded: translated + padding),
              base64URL(data) == payloadBase64URL
        else {
            throw ProtocolFailure.payloadEncodingInvalid
        }
        return try validate(
            data,
            maximumBytes: maximumBytes,
            declaredBytes: payloadBytes,
            declaredHash: contentSHA256,
        )
    }

    static func fromPlatform(_ text: String, maximumBytes: Int) throws -> Self {
        try validate(Data(text.utf8), maximumBytes: maximumBytes)
    }

    func toWire() -> WireContentV1 {
        WireContentV1(
            contentType: "text/plain",
            payloadBase64URL: Self.base64URL(bytes),
            payloadBytes: bytes.count,
            contentSHA256: Self.sha256(bytes),
        )
    }

    func toPlatform() -> String {
        String(decoding: bytes, as: UTF8.self)
    }

    func sameContent(as other: Self) -> Bool {
        bytes == other.bytes
    }

    func preview(maximumScalars: Int) -> String {
        guard maximumScalars > 0 else {
            return ""
        }
        let text = String(decoding: bytes, as: UTF8.self)
        var output = String.UnicodeScalarView()
        var previousWasWhitespace = false
        let replacement: Unicode.Scalar = "\u{FFFD}"

        for source in text.unicodeScalars {
            let scalar: Unicode.Scalar = if source.value < 0x20, ![0x09, 0x0A, 0x0D].contains(source.value) {
                replacement
            } else {
                source
            }
            if scalar.properties.isWhitespace {
                guard !previousWasWhitespace else {
                    continue
                }
                previousWasWhitespace = true
                guard output.count < maximumScalars else {
                    break
                }
                output.append(" ")
            } else {
                previousWasWhitespace = false
                guard output.count < maximumScalars else {
                    break
                }
                output.append(scalar)
            }
        }
        return String(output)
    }

    var description: String {
        "[redacted]"
    }

    var debugDescription: String {
        "ClipContentV1([redacted])"
    }

    private static func validate(
        _ data: Data,
        maximumBytes: Int,
        declaredBytes: Int? = nil,
        declaredHash: String? = nil,
    ) throws -> Self {
        guard !data.isEmpty else {
            throw ProtocolFailure.payloadEmpty
        }
        guard String(data: data, encoding: .utf8) != nil else {
            throw ProtocolFailure.payloadEncodingInvalid
        }
        guard data.count <= maximumBytes else {
            throw ProtocolFailure.payloadTooLarge
        }
        if let declaredBytes, declaredBytes != data.count {
            throw ProtocolFailure.payloadLengthMismatch
        }
        if let declaredHash, declaredHash != sha256(data) {
            throw ProtocolFailure.payloadHashMismatch
        }
        return Self(bytes: data)
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacing("+", with: "-")
            .replacing("/", with: "_")
            .replacing("=", with: "")
    }

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { byte in
            String(byte, radix: 16).leftPadded(to: 2, with: "0")
        }.joined()
    }
}

private extension String {
    func leftPadded(to length: Int, with character: Character) -> String {
        String(repeating: String(character), count: Swift.max(0, length - count)) + self
    }
}
