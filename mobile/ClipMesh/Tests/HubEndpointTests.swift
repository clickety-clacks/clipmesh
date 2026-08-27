@testable import ClipMesh
import XCTest

final class HubEndpointTests: XCTestCase {
    @MainActor
    func testAcceptsNumericTailnetIPv4AndIPv6URLs() throws {
        XCTAssertNoThrow(try HubEndpoint("ws://100.64.0.7:8123/v1/stream"))
        XCTAssertNoThrow(try HubEndpoint("ws://[fd7a:115c:a1e0::7]:8123/v1/stream"))
    }

    @MainActor
    func testRejectsHostnameSchemeIdentityAndTargetDefects() {
        for value in [
            "ws://hub.example.invalid:8123/v1/stream",
            "wss://100.64.0.7:8123/v1/stream",
            "ws://member@100.64.0.7:8123/v1/stream",
            "ws://100.64.0.7:8123/v1/stream?peer=member",
            "ws://100.64.0.7:8123/v1/%73tream",
            "ws://100.64.0.7:8123/other",
            "ws://192.0.2.7:8123/v1/stream",
        ] {
            XCTAssertThrowsError(try HubEndpoint(value))
        }
    }
}
