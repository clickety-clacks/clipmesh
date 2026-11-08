@testable import ClipMesh
import XCTest

@MainActor
final class KeyboardHubTapTests: XCTestCase {
    func testCopyResumesAfterNewestThenPublishesAndConfirms() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 2, newestCursor: 3),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 2, requestedAfterCursor: 3, boundaryCursor: 3),
            ProtocolFixtures.resumeComplete(boundary: 3, generation: 2),
        ])
        let tap = try makeTap(transport)
        let text = "emoji 🧪\ttab\nnewline"
        let result = Task { await tap.publish(text) }
        try await waitUntil("publish sent") { self.messages("publish", in: transport).count == 1 }

        let resume = try XCTUnwrap(messages("resume", in: transport).first)
        XCTAssertEqual(resume["after_cursor"] as? String, "3")
        let publish = try XCTUnwrap(messages("publish", in: transport).first)
        let event = try XCTUnwrap(publish["event"] as? [String: Any])
        XCTAssertEqual(event["clear_generation"] as? String, "2")
        let wire = try ClipContentV1.fromPlatform(text, maximumBytes: 262_144).toWire()
        XCTAssertEqual(event["payload_b64"] as? String, wire.payloadBase64URL)
        XCTAssertEqual(event["content_sha256"] as? String, wire.contentSHA256)

        let messageID = try XCTUnwrap(event["message_id"] as? String)
        transport.enqueue(Self.accepted(messageID))
        let outcome = await result.value
        XCTAssertEqual(outcome, .confirmed)
        XCTAssertGreaterThan(transport.closeCount, 0)
        XCTAssertEqual(transport.openedEndpoints.count, 1)
    }

    func testCopyIgnoresAnotherPublishsAcceptance() async throws {
        let transport = Self.liveTransport()
        let tap = try makeTap(transport, window: .milliseconds(200))
        let result = Task { await tap.publish("mine") }
        try await waitUntil("publish sent") { self.messages("publish", in: transport).count == 1 }
        transport.enqueue(Self.accepted("00000000-0000-4000-8000-0000000000ff"))
        let outcome = await result.value
        XCTAssertEqual(outcome, .unconfirmed)
    }

    func testCopyWithoutConfirmationInWindowIsUnconfirmed() async throws {
        let transport = Self.liveTransport()
        let tap = try makeTap(transport, window: .milliseconds(50))
        let outcome = await tap.publish("silent hub")
        XCTAssertEqual(outcome, .unconfirmed)
        XCTAssertEqual(messages("publish", in: transport).count, 1)
    }

    func testRejectedPublishReportsTheHubsReason() async throws {
        let transport = Self.liveTransport()
        let tap = try makeTap(transport)
        let result = Task { await tap.publish("too fast") }
        try await waitUntil("publish sent") { self.messages("publish", in: transport).count == 1 }
        transport.enqueue(ProtocolFixtures.data("""
            {"protocol_version":1,"type":"publish_rejected","message_id":null,"code":"publish_rate_limited","retryable":true}
            """))
        let outcome = await result.value
        XCTAssertEqual(outcome, .refused(.publishRateLimited))
    }

    func testTextOverTheHubLimitIsRefusedWithoutPublishing() async throws {
        let transport = Self.liveTransport()
        let tap = try makeTap(transport)
        let outcome = await tap.publish(String(repeating: "x", count: 262_145))
        XCTAssertEqual(outcome, .refused(.payloadTooLarge))
        XCTAssertTrue(messages("publish", in: transport).isEmpty)
    }

    func testNoAnswerBeforeTheSendIsUnreachable() async throws {
        let transport = SyntheticClipTransport()
        let tap = try makeTap(transport, window: .milliseconds(50))
        let outcome = await tap.publish("nobody home")
        XCTAssertEqual(outcome, .unreachable)
        XCTAssertTrue(messages("publish", in: transport).isEmpty)
    }

    func testRefusedUpgradeAtTheConnectionLimitIsBusy() async throws {
        let transport = SyntheticClipTransport()
        transport.refusedUpgradeStatusCode = 429
        let tap = try makeTap(transport, window: .milliseconds(50))
        let copy = await tap.publish("busy")
        XCTAssertEqual(copy, .busy)

        let pasteTransport = SyntheticClipTransport()
        pasteTransport.refusedUpgradeStatusCode = 429
        let paste = try await makeTap(pasteTransport, window: .milliseconds(50)).fetchNewest()
        XCTAssertEqual(paste, .busy)
    }

    func testConnectionLimitErrorFrameIsBusy() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: nil),
            ProtocolFixtures.data("""
                {"protocol_version":1,"type":"error","code":"connection_limit_reached","retryable":true}
                """),
        ])
        let outcome = try await makeTap(transport).publish("busy")
        XCTAssertEqual(outcome, .busy)
    }

    func testCancelledCopyReportsCancelled() async throws {
        let transport = SyntheticClipTransport(incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: nil)])
        let tap = try makeTap(transport)
        let result = Task { await tap.publish("dismissed") }
        try await waitUntil("resume sent") { !self.messages("resume", in: transport).isEmpty }
        tap.cancel()
        let outcome = await result.value
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertTrue(messages("publish", in: transport).isEmpty)
    }

    func testPasteAsksOnlyForTheNewestClip() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: 5),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 1, requestedAfterCursor: 4, boundaryCursor: 5),
            ProtocolFixtures.event(
                delivery: "resume", cursor: 5, messageSuffix: "000000000005",
                sourcePeerID: "peer-reserved-source", text: "two",
            ),
            ProtocolFixtures.resumeComplete(boundary: 5, generation: 1),
        ])
        let outcome = try await makeTap(transport).fetchNewest()
        XCTAssertEqual(outcome, .clip("two"))
        let resume = try XCTUnwrap(messages("resume", in: transport).first)
        XCTAssertEqual(resume["after_cursor"] as? String, "4")
        XCTAssertGreaterThan(transport.closeCount, 0)
    }

    func testPasteWithNewestCursorOneSendsNoAfterCursor() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: 1),
            ProtocolFixtures.resumeStarted(status: "generation_changed", generation: 1, boundaryCursor: 1),
            ProtocolFixtures.event(
                delivery: "resume", cursor: 1, messageSuffix: "000000000001",
                sourcePeerID: "peer-reserved-source", text: "only",
            ),
            ProtocolFixtures.resumeComplete(boundary: 1, generation: 1),
        ])
        let outcome = try await makeTap(transport).fetchNewest()
        XCTAssertEqual(outcome, .clip("only"))
        let resume = try XCTUnwrap(messages("resume", in: transport).first)
        XCTAssertTrue(resume["after_cursor"] is NSNull)
    }

    func testPasteFromAnEmptyHubSendsNoResume() async throws {
        let transport = SyntheticClipTransport(incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: nil)])
        let outcome = try await makeTap(transport).fetchNewest()
        XCTAssertEqual(outcome, .empty)
        XCTAssertTrue(messages("resume", in: transport).isEmpty)
    }

    func testPasteAfterASharedClearIsEmpty() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 2, newestCursor: 2),
            ProtocolFixtures.resumeStarted(
                status: "gap", generation: 2, requestedAfterCursor: 1, boundaryCursor: 2, lostThroughCursor: 2,
            ),
            ProtocolFixtures.resumeComplete(boundary: 2, generation: 2),
        ])
        let outcome = try await makeTap(transport).fetchNewest()
        XCTAssertEqual(outcome, .empty)
    }

    func testPasteSkipsAnExpiredClip() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: 2),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 1, requestedAfterCursor: 1, boundaryCursor: 2),
            ProtocolFixtures.event(
                delivery: "resume", cursor: 2, messageSuffix: "000000000002",
                sourcePeerID: "peer-reserved-source", text: "expired",
                expiresAtMilliseconds: 1_700_000_000_500,
            ),
            ProtocolFixtures.resumeComplete(boundary: 2, generation: 1),
        ])
        let outcome = try await makeTap(transport).fetchNewest()
        XCTAssertEqual(outcome, .empty)
    }

    func testPasteWithNoAnswerInWindowIsUnreachable() async throws {
        let transport = SyntheticClipTransport(incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: 3)])
        let outcome = try await makeTap(transport, window: .milliseconds(50)).fetchNewest()
        XCTAssertEqual(outcome, .unreachable)
    }

    func testCancelledPasteReportsCancelled() async throws {
        let transport = SyntheticClipTransport(incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: 3)])
        let tap = try makeTap(transport)
        let result = Task { await tap.fetchNewest() }
        try await waitUntil("resume sent") { !self.messages("resume", in: transport).isEmpty }
        tap.cancel()
        let outcome = await result.value
        XCTAssertEqual(outcome, .cancelled)
    }

    // MARK: - Support

    private static func liveTransport() -> SyntheticClipTransport {
        SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: nil),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 1, boundaryCursor: nil),
            ProtocolFixtures.resumeComplete(boundary: nil, generation: 1),
        ])
    }

    private static func accepted(_ messageID: String) -> Data {
        ProtocolFixtures.data("""
            {"protocol_version":1,"type":"publish_accepted","message_id":"\(messageID)","cursor":"4","expires_at_ms":1700604800000,"duplicate":false}
            """)
    }

    private func makeTap(_ transport: SyntheticClipTransport, window: Duration = .seconds(2)) throws -> KeyboardHubTap {
        try KeyboardHubTap(
            endpoint: ProtocolFixtures.endpoint(),
            transport: transport,
            now: { ProtocolFixtures.now },
            window: window,
        )
    }

    private func messages(_ type: String, in transport: SyntheticClipTransport) -> [[String: Any]] {
        transport.sentMessages.compactMap { data in
            (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        }.filter { $0["type"] as? String == type }
    }

    private func waitUntil(_ label: String, condition: @escaping @MainActor () -> Bool) async throws {
        let expectation = expectation(description: label)
        let task = Task { @MainActor in
            while !condition() {
                try await Task.sleep(for: .milliseconds(5))
            }
            expectation.fulfill()
        }
        await fulfillment(of: [expectation], timeout: 2)
        task.cancel()
    }
}
