import Foundation
import Network

struct HubEndpoint: Equatable {
    let url: URL

    init(_ value: String) throws {
        guard let components = URLComponents(string: value),
              components.scheme == "ws",
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.path == "/v1/stream",
              components.percentEncodedPath == "/v1/stream",
              let host = components.host,
              components.port.map({ (1 ... 65535).contains($0) }) ?? true,
              Self.isTailnetNodeAddress(host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))),
              let url = components.url
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        self.url = url
    }

    var displayValue: String {
        url.absoluteString
    }

    private static func isTailnetNodeAddress(_ host: String) -> Bool {
        if let address = IPv4Address(host) {
            let bytes = [UInt8](address.rawValue)
            return bytes.count == 4 && bytes[0] == 100 && (64 ... 127).contains(bytes[1])
        }
        if let address = IPv6Address(host) {
            let bytes = [UInt8](address.rawValue)
            return bytes.count == 16
                && bytes[0] == 0xFD
                && bytes[1] == 0x7A
                && bytes[2] == 0x11
                && bytes[3] == 0x5C
                && bytes[4] == 0xA1
                && bytes[5] == 0xE0
        }
        return false
    }
}
