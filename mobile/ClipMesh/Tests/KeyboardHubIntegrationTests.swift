@testable import ClipMesh
import XCTest

/// Runs the keyboard's hub path against a real, dedicated test hub. Skipped
/// unless `CLIPMESH_TEST_HUB_URL` names one. Never point it at a hub in use:
/// these tests publish, clear the shared history and, with
/// `CLIPMESH_TEST_HUB_FILL=1`, fill it with 1 MiB clips.
///
/// The hub admits each device 10 connection attempts at once, refilled at 30
/// a minute, and drops attempts beyond that. Every tap and session here is an
/// attempt, so the first test waits for the allowance to refill and each test
/// waits again afterwards, leaving it full for the suites that follow.
@MainActor
final class KeyboardHubIntegrationTests: XCTestCase {
    private var usedHub = false

    override func tearDown() async throws {
        if usedHub {
            try await Task.sleep(for: .seconds(20))
        }
        try await super.tearDown()
    }

    func testCopyReachesTheHubWithIdenticalBytesAndPasteReadsThemBack() async throws {
        let endpoint = try await testHub()
        let text = "keyboard \(UUID().uuidString)\n\temoji 🧪 é\r\nend"
        let copy = await KeyboardHubTap(endpoint: endpoint).publish(text)
        XCTAssertEqual(copy, .confirmed)

        let observer = TestHubSession()
        try await observer.open(endpoint, replayHistory: true)
        defer { observer.close() }
        XCTAssertEqual(observer.replayed.last?.content.toPlatform(), text)
        XCTAssertEqual(observer.replayed.last?.content, try ClipContentV1.fromPlatform(text, maximumBytes: 1_048_576))

        let paste = await KeyboardHubTap(endpoint: endpoint).fetchNewest()
        XCTAssertEqual(paste, .clip(text))
    }

    func testPasteAfterASharedClearIsEmpty() async throws {
        let endpoint = try await testHub()
        let before = await KeyboardHubTap(endpoint: endpoint).publish("before clear")
        XCTAssertEqual(before, .confirmed)
        let session = TestHubSession()
        try await session.open(endpoint)
        try await session.clear()
        session.close()

        let paste = await KeyboardHubTap(endpoint: endpoint).fetchNewest()
        XCTAssertEqual(paste, .empty)
    }

    func testATapWhileTheAppHoldsBothSessionsIsBusyAndLeavesThemUp() async throws {
        let endpoint = try await testHub()
        let first = TestHubSession()
        let second = TestHubSession()
        try await first.open(endpoint)
        try await second.open(endpoint)
        defer { first.close(); second.close() }
        let readers = [first.keepReading(), second.keepReading()]
        defer { readers.forEach { $0.cancel() } }

        let copy = await KeyboardHubTap(endpoint: endpoint).publish("over the limit")
        XCTAssertEqual(copy, .busy)
        let paste = await KeyboardHubTap(endpoint: endpoint).fetchNewest()
        XCTAssertEqual(paste, .busy)

        try await first.transport.ping(timeout: .seconds(5))
        try await second.transport.ping(timeout: .seconds(5))
    }

    /// R9: with the history full of clips at the hard maximum size, Paste
    /// receives only the newest clip rather than the history.
    func testPasteFromAFullHubReceivesOnlyTheNewestClip() async throws {
        guard ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_FILL"] == "1" else {
            throw XCTSkip("Set CLIPMESH_TEST_HUB_FILL=1 to fill the test hub")
        }
        let endpoint = try await testHub()
        let filler = TestHubSession()
        try await filler.open(endpoint)
        let limit = filler.hello.limits.historyMaximumEntries
        let size = filler.hello.limits.maximumPayloadBytes
        XCTAssertEqual(size, 1_048_576)
        var newest = ""
        for index in 0..<limit {
            let marker = "clip \(index) \(UUID().uuidString)\n"
            newest = marker + String(repeating: "m", count: size - marker.utf8.count)
            try await filler.publish(newest)
        }
        filler.close()

        let counting = CountingTransport()
        let paste = await KeyboardHubTap(endpoint: endpoint, transport: counting).fetchNewest()
        XCTAssertEqual(paste, .clip(newest))
        XCTAssertEqual(counting.eventFrames, 1)
        XCTAssertLessThan(counting.receivedBytes, 2 * size)
        print("KEYBOARD_R9 history=\(limit) clip_bytes=\(size) event_frames=\(counting.eventFrames) received_bytes=\(counting.receivedBytes)")
    }

    /// Waits out the previous suite's connection attempts on first use.
    private static var allowanceRefilled = false

    private func testHub() async throws -> HubEndpoint {
        guard let value = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"],
              !value.isEmpty, !value.contains("$(")
        else {
            throw XCTSkip("Requires an isolated ClipMesh test hub")
        }
        if !Self.allowanceRefilled {
            try await Task.sleep(for: .seconds(20))
            Self.allowanceRefilled = true
        }
        usedHub = true
        return try HubEndpoint(value)
    }
}

/// A plain session standing in for the app or another device.
@MainActor
private final class TestHubSession {
    let transport = URLSessionClipTransport()
    private let codec = ProtocolV1Codec()
    private(set) var hello: ServerHelloV1!
    private(set) var generation: UInt64 = 0
    private(set) var replayed: [EventV1] = []

    func open(_ endpoint: HubEndpoint, replayHistory: Bool = false) async throws {
        try await transport.open(endpoint)
        guard case let .serverHello(value) = try await receive() else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        hello = value
        generation = value.clearGeneration
        try await send(.resume(ResumeRequestV1(
            knownHistoryEpoch: value.historyEpoch,
            knownClearGeneration: value.clearGeneration,
            afterCursor: replayHistory ? nil : value.newestCursor,
        )))
        while true {
            switch try await receive() {
            case let .resumeStarted(started):
                generation = started.clearGeneration
            case let .event(event):
                replayed.append(event)
            case let .clearNotice(notice):
                generation = max(generation, notice.clearGeneration)
            case .resumeComplete:
                return
            default:
                throw ProtocolFailure.protocolSchemaInvalid
            }
        }
    }

    func publish(_ text: String) async throws {
        let id = UUID()
        try await send(.publish(
            messageID: id,
            generation: generation,
            createdAt: Int64(Date().timeIntervalSince1970 * 1000),
            content: ClipContentV1.fromPlatform(text, maximumBytes: hello.limits.maximumPayloadBytes),
        ))
        while true {
            switch try await receive() {
            case let .publishAccepted(value) where value.messageID == id:
                return
            case let .publishRejected(value) where value.code == .publishRateLimited:
                try await Task.sleep(for: .milliseconds(200))
                return try await publish(text)
            case let .publishRejected(value):
                throw HubTestFailure.rejected(value.code)
            default:
                continue
            }
        }
    }

    func clear() async throws {
        let id = UUID()
        try await send(.clearHistory(ClearHistoryRequestV1(requestID: id, expectedClearGeneration: generation)))
        while true {
            if case let .clearAccepted(value) = try await receive(), value.requestID == id {
                generation = value.clearGeneration
                return
            }
        }
    }

    /// URLSession answers pings only while a receive is outstanding.
    func keepReading() -> Task<Void, Never> {
        Task { [transport] in
            while (try? await transport.receive()) != nil {}
        }
    }

    func close() {
        transport.close()
    }

    private func send(_ message: ClientMessageV1) async throws {
        try await transport.send(codec.encodeClientMessage(message))
    }

    private func receive() async throws -> HubMessageV1 {
        try await codec.decodeHubMessage(transport.receive())
    }
}

private enum HubTestFailure: Error {
    case rejected(ReasonCodeV1)
}

/// Counts what one keyboard tap receives.
@MainActor
private final class CountingTransport: ClipTransport {
    private let inner = URLSessionClipTransport()
    private(set) var receivedBytes = 0
    private(set) var eventFrames = 0

    func open(_ endpoint: HubEndpoint) async throws {
        try await inner.open(endpoint)
    }

    func send(_ data: Data) async throws {
        try await inner.send(data)
    }

    func receive() async throws -> Data {
        let data = try await inner.receive()
        receivedBytes += data.count
        if case .event = try? ProtocolV1Codec().decodeHubMessage(data) {
            eventFrames += 1
        }
        return data
    }

    func ping(timeout: Duration) async throws {
        try await inner.ping(timeout: timeout)
    }

    func close() {
        inner.close()
    }

    var refusedUpgradeStatusCode: Int? {
        inner.refusedUpgradeStatusCode
    }
}
