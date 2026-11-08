@testable import ClipMesh
import XCTest

@MainActor
final class KeyboardActionsTests: XCTestCase {
    func testCutRefusesAndChangesNothing() async throws {
        let long = String(repeating: "Words keep the selection long enough to test. ", count: 40)
        // Short, an under-report (the first 79 characters of a longer
        // selection) and repeated text all look the same to the keyboard.
        for selection in ["hello mesh", String(long.prefix(79)), long] {
            let harness = try Harness(selection: selection)
            harness.actions.cut()
            XCTAssertTrue(harness.pasteboard.writes.isEmpty)
            XCTAssertTrue(harness.target.log.isEmpty)
            XCTAssertTrue(harness.transport.openedEndpoints.isEmpty)
            XCTAssertEqual(harness.statuses, [.cutRefused])
            XCTAssertEqual(harness.statuses.last?.text,
                           "Didn't cut: couldn't confirm the whole selection. Copy still works.")
        }
    }

    func testCopyLeavesTheFieldAloneAndStatesHowMuchItCopied() async throws {
        let harness = try Harness(selection: "hello mesh")
        let copy = Task { await harness.actions.copy() }
        try await waitUntil("publish sent") { harness.transport.sentMessages.count { self.isPublish($0) } == 1 }
        XCTAssertEqual(harness.statuses.last?.text,
                       "Copied 10 characters to iPhone. Sending to ClipMesh… This may be only part of your selection, or differ in spacing.")
        harness.transport.enqueue(try Self.accepted(for: harness.transport))
        await copy.value
        XCTAssertEqual(harness.pasteboard.writes, ["hello mesh"])
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertEqual(harness.statuses.last, .copy(characters: 10, .confirmed))
        XCTAssertEqual(harness.statuses.last?.text,
                       "Copied 10 characters to iPhone and ClipMesh. This may be only part of your selection, or differ in spacing.")
    }

    func testCopyCountsCharactersAsTheUserSeesThem() async throws {
        let text = "a👩‍👩‍👧\té\n"
        let harness = try Harness(selection: text, hasEndpoint: false)
        await harness.actions.copy()
        XCTAssertEqual(harness.pasteboard.writes, [text])
        XCTAssertEqual(harness.statuses.last, .copy(characters: 5, .notSetUp))
        XCTAssertTrue(KeyboardStatus.copy(characters: 1, .confirmed).text.hasPrefix("Copied 1 character to"))
    }

    func testNoSelectionChangesNothing() async throws {
        for selection in [nil, ""] as [String?] {
            let harness = try Harness(selection: selection)
            harness.actions.cut()
            await harness.actions.copy()
            XCTAssertTrue(harness.pasteboard.writes.isEmpty)
            XCTAssertTrue(harness.target.log.isEmpty)
            XCTAssertTrue(harness.transport.openedEndpoints.isEmpty)
            XCTAssertEqual(harness.statuses, [.noSelection, .noSelection])
            XCTAssertEqual(harness.statuses.last?.text, "No selected text shared with the keyboard")
        }
    }

    func testWithoutFullAccessNoKeyChangesAnything() async throws {
        let harness = try Harness(selection: "secret", fullAccess: false)
        harness.actions.cut()
        await harness.actions.copy()
        await harness.actions.paste()
        XCTAssertTrue(harness.pasteboard.writes.isEmpty)
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertTrue(harness.transport.openedEndpoints.isEmpty)
        XCTAssertEqual(harness.statuses, [.fullAccessNeeded, .fullAccessNeeded, .fullAccessNeeded])
        XCTAssertTrue(harness.statuses[0].text.contains("Settings > General > Keyboard > Keyboards > ClipMesh"))
    }

    func testFailedClipboardWriteKeepsTheSelectionAndSendsNothing() async throws {
        let harness = try Harness(selection: "keep me")
        harness.pasteboard.succeeds = false
        await harness.actions.copy()
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertTrue(harness.transport.openedEndpoints.isEmpty)
        XCTAssertEqual(harness.statuses.last, .iPhoneWriteFailed)
    }

    func testWithoutAHubURLCopyStillWritesTheClipboard() async throws {
        let harness = try Harness(selection: "offline", hasEndpoint: false)
        await harness.actions.copy()
        XCTAssertEqual(harness.pasteboard.writes, ["offline"])
        XCTAssertTrue(harness.transport.openedEndpoints.isEmpty)
        XCTAssertEqual(harness.statuses.last?.text, "Copied 7 characters to iPhone. Set up ClipMesh in the app first. This may be only part of your selection, or differ in spacing.")

        await harness.actions.paste()
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertEqual(harness.statuses.last?.text, "Set up ClipMesh in the app first")
    }

    func testPasteInsertsTheNewestClipAndNeverTouchesTheClipboard() async throws {
        let harness = try Harness(selection: nil, incoming: Self.newestClip("from the mac"))
        await harness.actions.paste()
        XCTAssertEqual(harness.target.log, ["insert:from the mac"])
        XCTAssertTrue(harness.pasteboard.writes.isEmpty)
        XCTAssertEqual(harness.statuses, [.pasteFetching, .paste(.pasted)])
    }

    func testDismissalDiscardsAPasteInFlight() async throws {
        let harness = try Harness(selection: nil, incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: 3)])
        let paste = Task { await harness.actions.paste() }
        try await waitUntil("resume sent") { !harness.transport.sentMessages.isEmpty }
        harness.actions.cancel()
        Self.newestClip("too late").dropFirst().forEach(harness.transport.enqueue)
        await paste.value
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertEqual(harness.statuses, [.pasteFetching])
    }

    func testALaterTapDiscardsTheEarlierPaste() async throws {
        let first = SyntheticClipTransport(incoming: [ProtocolFixtures.hello(generation: 1, newestCursor: 3)])
        let second = SyntheticClipTransport(incoming: Self.newestClip("second"))
        let harness = try Harness(selection: nil, transports: [first, second])
        let paste = Task { await harness.actions.paste() }
        try await waitUntil("first resume sent") { !first.sentMessages.isEmpty }
        await harness.actions.paste()
        await paste.value
        XCTAssertEqual(harness.target.log, ["insert:second"])
    }

    func testFailedPasteInsertsNothing() async throws {
        let harness = try Harness(selection: nil, incoming: [], window: .milliseconds(50))
        await harness.actions.paste()
        XCTAssertTrue(harness.target.log.isEmpty)
        XCTAssertEqual(harness.statuses.last?.text, "Couldn't reach ClipMesh")
    }

    func testEveryCopyOutcomeReadsDifferently() {
        let outcomes: [KeyboardCopyOutcome] = [
            .sending, .confirmed, .unconfirmed, .unreachable, .refused(.payloadTooLarge), .busy, .notSetUp,
        ]
        let texts = outcomes.map { KeyboardStatus.copy(characters: 79, $0).text }
        XCTAssertEqual(Set(texts).count, outcomes.count)
        XCTAssertTrue(texts.allSatisfy { $0.hasPrefix("Copied 79 characters to iPhone") })
        XCTAssertTrue(texts.allSatisfy { $0.hasSuffix(" This may be only part of your selection, or differ in spacing.") })
        XCTAssertEqual(KeyboardStatus.copy(characters: 79, .refused(.payloadTooLarge)).text,
                       "Copied 79 characters to iPhone. ClipMesh refused it (too large). This may be only part of your selection, or differ in spacing.")
    }

    // MARK: - Support

    private static func newestClip(_ text: String) -> [Data] {
        [
            ProtocolFixtures.hello(generation: 1, newestCursor: 3),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 1, requestedAfterCursor: 2, boundaryCursor: 3),
            ProtocolFixtures.event(
                delivery: "resume", cursor: 3, messageSuffix: "000000000003",
                sourcePeerID: "peer-reserved-source", text: text,
            ),
            ProtocolFixtures.resumeComplete(boundary: 3, generation: 1),
        ]
    }

    private static func accepted(for transport: SyntheticClipTransport) throws -> Data {
        let publish = try XCTUnwrap(transport.sentMessages.compactMap {
            (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any]
        }.first { $0["type"] as? String == "publish" })
        let event = try XCTUnwrap(publish["event"] as? [String: Any])
        let id = try XCTUnwrap(event["message_id"] as? String)
        return ProtocolFixtures.data("""
            {"protocol_version":1,"type":"publish_accepted","message_id":"\(id)","cursor":"1","expires_at_ms":1700604800000,"duplicate":false}
            """)
    }

    private func isPublish(_ data: Data) -> Bool {
        String(decoding: data, as: UTF8.self).contains("\"type\":\"publish\"")
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

@MainActor
private final class Harness {
    let target: FakeTextTarget
    let pasteboard = FakeKeyboardPasteboard()
    var statuses: [KeyboardStatus] = []
    private(set) var actions: KeyboardActions!
    private var transports: [SyntheticClipTransport]

    var transport: SyntheticClipTransport {
        transports[0]
    }

    convenience init(
        selection: String?,
        fullAccess: Bool = true,
        hasEndpoint: Bool = true,
        incoming: [Data] = [
            ProtocolFixtures.hello(generation: 1, newestCursor: nil),
            ProtocolFixtures.resumeStarted(status: "complete", generation: 1, boundaryCursor: nil),
            ProtocolFixtures.resumeComplete(boundary: nil, generation: 1),
        ],
        window: Duration = .seconds(2),
    ) throws {
        try self.init(
            selection: selection,
            fullAccess: fullAccess,
            hasEndpoint: hasEndpoint,
            transports: [SyntheticClipTransport(incoming: incoming)],
            window: window,
        )
    }

    init(
        selection: String?,
        fullAccess: Bool = true,
        hasEndpoint: Bool = true,
        transports: [SyntheticClipTransport],
        window: Duration = .seconds(2),
    ) throws {
        target = FakeTextTarget(selection: selection)
        self.transports = transports
        let resolved = hasEndpoint ? try ProtocolFixtures.endpoint() : nil
        var remaining = transports
        actions = KeyboardActions(
            target: target,
            pasteboard: pasteboard,
            hasFullAccess: { fullAccess },
            loadEndpoint: { resolved },
            makeTap: { endpoint in
                KeyboardHubTap(
                    endpoint: endpoint,
                    transport: remaining.removeFirst(),
                    now: { ProtocolFixtures.now },
                    window: window,
                )
            },
            onStatus: { [unowned self] in statuses.append($0) },
        )
    }
}

@MainActor
private final class FakeTextTarget: KeyboardTextTarget {
    let selectedText: String?
    private(set) var log: [String] = []

    init(selection: String?) {
        selectedText = selection
    }

    func insertText(_ text: String) {
        log.append("insert:\(text)")
    }
}

@MainActor
private final class FakeKeyboardPasteboard: KeyboardPasteboard {
    var succeeds = true
    private(set) var writes: [String] = []

    func write(_ text: String) -> Bool {
        guard succeeds else {
            return false
        }
        writes.append(text)
        return true
    }
}
