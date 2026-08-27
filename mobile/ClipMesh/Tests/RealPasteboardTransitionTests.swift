@testable import ClipMesh
import UIKit
import XCTest

@MainActor
final class RealPasteboardTransitionTests: XCTestCase {
    func testRealGeneralPasteboardChangesOnlyForEligibleLiveRemoteClip() async throws {
        let pasteboard = UIPasteboard.general
        pasteboard.string = "synthetic direct overwrite"
        let baselineChangeCount = pasteboard.changeCount
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeStarted(boundaryCursor: 1),
            ProtocolFixtures.event(
                delivery: "resume",
                cursor: 1,
                messageSuffix: "000000000006",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic retained",
            ),
            ProtocolFixtures.resumeComplete(boundary: 1),
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 2,
                messageSuffix: "000000000007",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic direct overwrite",
            ),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SystemPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        let expectation = expectation(description: "one live pasteboard write")
        let task = Task { @MainActor in
            while model.lifecycleState != .foregroundLive || model.visibleHistory.count != 2 {
                try await Task.sleep(for: .milliseconds(5))
            }
            expectation.fulfill()
        }
        await fulfillment(of: [expectation], timeout: 2)
        task.cancel()

        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 1)
        XCTAssertTrue(pasteboard.string == "synthetic direct overwrite")

        let retainedRow = try XCTUnwrap(model.visibleHistory.first(where: { $0.cursor == 1 }))
        model.copyHistoryItem(retainedRow.id)
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 2)
        XCTAssertTrue(pasteboard.string == "synthetic retained")

        model.clearLocalHistory()
        XCTAssertTrue(model.visibleHistory.isEmpty)
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 2)

        model.deactivate()
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 2)
    }

    func testRealGeneralPasteboardGenerationCatchUpWritesOnlyTheLaterLiveRemoteClip() async throws {
        let pasteboard = UIPasteboard.general
        pasteboard.string = "synthetic offline baseline"
        let baselineChangeCount = pasteboard.changeCount
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: 1),
            ProtocolFixtures.resumeStarted(status: "fresh", generation: 1, boundaryCursor: 1),
            ProtocolFixtures.event(
                delivery: "resume",
                cursor: 1,
                messageSuffix: "000000000015",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic prior generation",
            ),
            ProtocolFixtures.resumeComplete(boundary: 1, generation: 1),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SystemPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("initial generation") {
            model.lifecycleState == .foregroundLive && model.visibleHistory.count == 1
        }
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 0)

        model.refresh()
        transport.enqueue(ProtocolFixtures.hello(generation: 2, newestCursor: 1))
        transport.enqueue(
            ProtocolFixtures.resumeStarted(
                status: "generation_changed",
                generation: 2,
                requestedAfterCursor: 1,
                boundaryCursor: 1,
                lostThroughCursor: 1,
            ),
        )
        transport.enqueue(ProtocolFixtures.resumeComplete(boundary: 1, generation: 2))
        try await waitUntil("new generation catch-up") {
            model.lifecycleState == .foregroundLive && model.visibleHistory.isEmpty
        }
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 0)

        transport.enqueue(
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 2,
                messageSuffix: "000000000016",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic first later live",
                generation: 2,
            ),
        )
        try await waitUntil("first later live") { pasteboard.changeCount - baselineChangeCount == 1 }
        XCTAssertTrue(pasteboard.string == "synthetic first later live")
        model.deactivate()
    }

    func testRealGeneralPasteboardDoesNotChangeForSharedClearOrBackground() async throws {
        let pasteboard = UIPasteboard.general
        pasteboard.string = "synthetic clear baseline"
        let baselineChangeCount = pasteboard.changeCount
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 1,
                messageSuffix: "000000000013",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic before shared clear",
            ),
            ProtocolFixtures.clearNotice(generation: 2, requestSuffix: "000000000014"),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SystemPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        let expectation = expectation(description: "shared clear processed")
        let task = Task { @MainActor in
            while model.lifecycleState != .foregroundLive || !model.visibleHistory.isEmpty {
                try await Task.sleep(for: .milliseconds(5))
            }
            expectation.fulfill()
        }
        await fulfillment(of: [expectation], timeout: 2)
        task.cancel()

        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 1)
        XCTAssertTrue(pasteboard.string == "synthetic before shared clear")
        model.deactivate()
        XCTAssertEqual(pasteboard.changeCount - baselineChangeCount, 1)
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
