@testable import ClipMesh
import XCTest

final class ProtocolV1CodecTests: XCTestCase {
    @MainActor
    private let codec = ProtocolV1Codec()

    @MainActor
    func testCanonicalRustEventPreservesExactContentAndPreviewRules() throws {
        let data = ProtocolFixtures.event(
            delivery: "live",
            cursor: 1,
            messageSuffix: "000000000001",
            sourcePeerID: "peer-reserved-source",
            text: "alpha\u{1}\n  beta",
        )
        guard case let .event(event) = try codec.decodeHubMessage(data) else {
            return XCTFail("expected event")
        }
        XCTAssertTrue(event.content.toPlatform() == "alpha\u{1}\n  beta")
        XCTAssertTrue(event.content.preview(maximumScalars: 11) == "alpha� beta")
        XCTAssertEqual(event.content.toWire().payloadBytes, Data("alpha\u{1}\n  beta".utf8).count)
        XCTAssertTrue(String(describing: event.content) == "[redacted]")
        XCTAssertTrue(String(reflecting: event.content) == "ClipContentV1([redacted])")
    }

    @MainActor
    func testClosedSchemaRejectsUnknownAndDuplicateFields() throws {
        let valid = String(decoding: ProtocolFixtures.resumeComplete(boundary: nil), as: UTF8.self)
        let unknown = valid.replacing("\"boundary_cursor\":null", with: "\"boundary_cursor\":null,\"unexpected\":true")
        XCTAssertThrowsError(try codec.decodeHubMessage(Data(unknown.utf8))) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolSchemaInvalid)
        }
        let duplicate = valid.replacing("\"type\":\"resume_complete\"", with: "\"type\":\"resume_complete\",\"type\":\"resume_complete\"")
        XCTAssertThrowsError(try codec.decodeHubMessage(Data(duplicate.utf8))) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolSchemaInvalid)
        }
    }

    @MainActor
    func testUnsupportedVersionPrecedesMessageSpecificHandling() throws {
        let unsupported = ProtocolFixtures.data("{\"protocol_version\":2,\"type\":\"event\"}")
        XCTAssertThrowsError(try codec.decodeHubMessage(unsupported)) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolVersionUnsupported)
        }
    }

    @MainActor
    func testFailureCodeSetAndRetryabilityAreClosed() throws {
        let unknown = ProtocolFixtures.data(
            "{\"protocol_version\":1,\"type\":\"error\",\"code\":\"new_code\",\"retryable\":false}",
        )
        XCTAssertThrowsError(try codec.decodeHubMessage(unknown)) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolSchemaInvalid)
        }

        let wrongRetryability = ProtocolFixtures.data(
            "{\"protocol_version\":1,\"type\":\"error\",\"code\":\"heartbeat_timeout\",\"retryable\":false}",
        )
        XCTAssertThrowsError(try codec.decodeHubMessage(wrongRetryability)) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolSchemaInvalid)
        }

        let valid = ProtocolFixtures.data(
            "{\"protocol_version\":1,\"type\":\"error\",\"code\":\"heartbeat_timeout\",\"retryable\":true}",
        )
        guard case let .error(reason) = try codec.decodeHubMessage(valid) else {
            return XCTFail("expected error")
        }
        XCTAssertEqual(reason.code, .heartbeatTimeout)
    }

    @MainActor
    func testLegacyRustFixtureIsRejectedAsAnObsoleteClosedShape() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "publish-v1", withExtension: "json"))
        let data = try Data(contentsOf: url)
        XCTAssertThrowsError(try codec.decodeHubMessage(data)) { error in
            XCTAssertEqual(error as? ProtocolFailure, .protocolSchemaInvalid)
        }
    }

    @MainActor
    func testCapturedProductionRustFramesMapToSwiftVersionOne() throws {
        let url = try XCTUnwrap(
            Bundle(for: Self.self).url(forResource: "rust-hub-frames-v1", withExtension: "jsonl"),
        )
        let text = try String(contentsOf: url, encoding: .utf8)
        let messages = try text.split(whereSeparator: \.isNewline).map { line in
            try codec.decodeHubMessage(Data(line.utf8))
        }
        XCTAssertEqual(messages.count, 4)
        guard case let .serverHello(hello) = messages[0],
              case .resumeStarted = messages[1],
              case .resumeComplete = messages[2],
              case let .event(event) = messages[3]
        else {
            return XCTFail("unexpected Rust frame sequence")
        }
        XCTAssertEqual(hello.limits.maximumPayloadBytes, 262_144)
        XCTAssertTrue(event.content.toPlatform() == "fixture text")
        XCTAssertEqual(event.content.toWire().payloadBytes, 12)
    }

    @MainActor
    func testClientMessagesUseClosedVersionOneShapes() throws {
        let resume = try codec.encodeClientMessage(
            .resume(ResumeRequestV1(knownHistoryEpoch: nil, knownClearGeneration: nil, afterCursor: nil)),
        )
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: resume) as? [String: Any])
        XCTAssertEqual(Set(object.keys), [
            "protocol_version", "type", "known_history_epoch", "known_clear_generation", "after_cursor",
        ])
        XCTAssertEqual(object["protocol_version"] as? Int, 1)
        XCTAssertEqual(object["type"] as? String, "resume")
    }
}
