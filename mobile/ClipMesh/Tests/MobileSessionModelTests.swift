@testable import ClipMesh
import XCTest

@MainActor
final class MobileSessionModelTests: XCTestCase {
    func testExplicitReadCanFinishAfterPermissionReactivation() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(), ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(transport: transport, pasteboard: SyntheticPasteboardWriter(),
                                       preferences: preferences, now: { ProtocolFixtures.now })
        let completion = Task { await model.finishExplicitClipboardRead("synthetic permitted") }
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(messageCount("publish", in: transport), 0)
        model.activate()
        await completion.value
        try await waitUntil("explicit publish after permission") {
            self.messageCount("publish", in: transport) == 1
        }
        model.deactivate()
    }

    func testAbandonedExplicitReadIsNotSentOnLaterLaunch() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(), ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(transport: transport, pasteboard: SyntheticPasteboardWriter(),
                                       preferences: preferences, now: { ProtocolFixtures.now })
        await model.finishExplicitClipboardRead("synthetic abandoned")
        model.activate()
        try await waitUntil("connected") { model.canPublish }
        XCTAssertEqual(messageCount("publish", in: transport), 0)
        model.deactivate()
    }

    func testPublishRejectionAndOversizedTextDoNotReportSuccess() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(), ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(transport: transport, pasteboard: SyntheticPasteboardWriter(),
                                       preferences: preferences, now: { ProtocolFixtures.now })
        model.activate()
        try await waitUntil("connected") { model.canPublish }
        model.publishClipboardText(String(repeating: "x", count: 262145))
        XCTAssertNil(model.pendingPublishID)
        XCTAssertEqual(messageCount("publish", in: transport), 0)
        model.publishClipboardText("synthetic rejected")
        let id = try XCTUnwrap(model.pendingPublishID)
        try await waitUntil("publish sent") { self.messageCount("publish", in: transport) == 1 }
        transport.enqueue(ProtocolFixtures.data("""
            {"protocol_version":1,"type":"publish_rejected","message_id":"\(id.uuidString.lowercased())","code":"publish_rate_limited","retryable":true}
            """))
        try await waitUntil("rejected") { model.pendingPublishID == nil }
        XCTAssertEqual(model.actionFeedback, "ClipMesh rejected this copy: publish_rate_limited")
        XCTAssertTrue(model.canPublish)
        model.deactivate()
    }

    func testExplicitPublishWaitsForAcceptanceAndNeverWritesClipboard() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(), ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let pasteboard = SyntheticPasteboardWriter()
        let model = MobileSessionModel(transport: transport, pasteboard: pasteboard,
                                       preferences: preferences, now: { ProtocolFixtures.now })
        model.publishClipboardText("inactive")
        XCTAssertTrue(transport.sentMessages.isEmpty)
        model.activate()
        try await waitUntil("connected") { model.canPublish }
        XCTAssertEqual(messageCount("publish", in: transport), 0)
        model.publishClipboardText(nil)
        model.publishClipboardText("")
        XCTAssertNil(model.pendingPublishID)
        model.publishClipboardText("fixture text")
        let id = try XCTUnwrap(model.pendingPublishID)
        model.publishClipboardText("double tap")
        try await waitUntil("publish sent") { self.messageCount("publish", in: transport) == 1 }
        XCTAssertEqual(model.actionFeedback, "Sending to ClipMesh…")
        let messages = try transport.sentMessages.map {
            try XCTUnwrap(JSONSerialization.jsonObject(with: $0) as? [String: Any])
        }
        let publish = try XCTUnwrap(messages.first { $0["type"] as? String == "publish" })
        let event = try XCTUnwrap(publish["event"] as? [String: Any])
        XCTAssertEqual(event["payload_b64"] as? String, "Zml4dHVyZSB0ZXh0")
        XCTAssertEqual(event["clear_generation"] as? String, "1")
        transport.enqueue(ProtocolFixtures.data("""
            {"protocol_version":1,"type":"publish_accepted","message_id":"\(id.uuidString.lowercased())","cursor":"1","expires_at_ms":1700604800000,"duplicate":false}
            """))
        try await waitUntil("accepted") { model.pendingPublishID == nil }
        XCTAssertEqual(model.actionFeedback, "Copied to ClipMesh")
        XCTAssertTrue(pasteboard.writes.isEmpty)
        XCTAssertEqual(model.lifecycleState, .foregroundLive)
        model.publishClipboardText("interrupted")
        model.deactivate()
        XCTAssertNil(model.pendingPublishID)
        XCTAssertFalse(model.canPublish)
        XCTAssertTrue(pasteboard.writes.isEmpty)
    }

    func testCatchUpDedupeLivePreviewLocalClearAndBackgroundNeverWrite() async throws {
        let resume = ProtocolFixtures.event(
            delivery: "resume",
            cursor: 1,
            messageSuffix: "000000000001",
            sourcePeerID: "peer-reserved-source",
            text: "synthetic resume",
        )
        let live = ProtocolFixtures.event(
            delivery: "live",
            cursor: 2,
            messageSuffix: "000000000002",
            sourcePeerID: "peer-reserved-source",
            text: "synthetic live",
        )
        let selfLive = ProtocolFixtures.event(
            delivery: "live",
            cursor: 3,
            messageSuffix: "000000000003",
            sourcePeerID: "peer-reserved-mobile",
            text: "synthetic self",
        )
        let expiredLive = ProtocolFixtures.event(
            delivery: "live",
            cursor: 4,
            messageSuffix: "000000000004",
            sourcePeerID: "peer-reserved-source",
            text: "synthetic expired",
            acceptedAtMilliseconds: 1_699_395_200_000,
            expiresAtMilliseconds: 1_700_000_000_000,
        )
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeStarted(boundaryCursor: 1),
            resume,
            ProtocolFixtures.resumeComplete(boundary: 1),
            live,
            live,
            selfLive,
            expiredLive,
        ])
        let pasteboard = SyntheticPasteboardWriter()
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: pasteboard,
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("live queue") {
            model.lifecycleState == .foregroundLive
                && model.visibleHistory.count == 3
                && transport.isWaitingForInput
        }

        XCTAssertEqual(pasteboard.writes.count, 0)
        XCTAssertTrue(pasteboard.writes.isEmpty)
        XCTAssertEqual(model.visibleHistory.map(\.cursor), [3, 2, 1])
        XCTAssertTrue(transport.sentMessages.contains { String(decoding: $0, as: UTF8.self).contains("\"type\":\"resume\"") })
        XCTAssertEqual(messageCount("ack", in: transport), 1)

        model.clearLocalHistory()
        XCTAssertTrue(model.visibleHistory.isEmpty)
        XCTAssertEqual(pasteboard.writes.count, 0)

        transport.enqueue(live)
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(model.visibleHistory.isEmpty)
        XCTAssertEqual(pasteboard.writes.count, 0)

        model.deactivate()
        transport.enqueue(
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 5,
                messageSuffix: "000000000005",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic background",
            ),
        )
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(model.lifecycleState, .inactive)
        XCTAssertEqual(pasteboard.writes.count, 0)
    }

    func testFiveHundredResumeRowsStayStaleAndNeverWriteUntilCatchUpCompletes() async throws {
        let resumeRows = (1 ... 500).map { cursor in
            ProtocolFixtures.event(
                delivery: "resume",
                cursor: UInt64(cursor),
                messageSuffix: String(format: "%012d", cursor),
                sourcePeerID: "peer-reserved-source",
                text: String(repeating: "x", count: 200) + " \(cursor)",
            )
        }
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(newestCursor: 500),
            ProtocolFixtures.resumeStarted(status: "fresh", boundaryCursor: 500),
        ] + resumeRows)
        let pasteboard = SyntheticPasteboardWriter()
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: pasteboard,
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("five hundred replay rows processed") {
            model.lifecycleState == .foregroundConnecting && transport.isWaitingForInput
        }
        XCTAssertTrue(pasteboard.writes.isEmpty)
        XCTAssertTrue(model.visibleHistory.isEmpty)

        transport.enqueue(ProtocolFixtures.resumeComplete(boundary: 500))
        try await waitUntil("five hundred current rows") {
            model.lifecycleState == .foregroundLive && model.visibleHistory.count == 500
        }
        XCTAssertTrue(pasteboard.writes.isEmpty)
        XCTAssertEqual(model.visibleHistory.map(\.cursor), Array((1 ... 500).reversed()).map(UInt64.init))
        XCTAssertTrue(model.visibleHistory.allSatisfy { !$0.isStale })
        XCTAssertTrue(model.visibleHistory.allSatisfy { $0.preview.unicodeScalars.count == 160 })
        XCTAssertEqual(messageCount("ack", in: transport), 1)
        model.deactivate()
    }

    func testEmptyResumeDoesNotAcknowledgeAnUnsentBoundary() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(newestCursor: 5),
            ProtocolFixtures.resumeStarted(status: "fresh", boundaryCursor: 5),
            ProtocolFixtures.resumeComplete(boundary: 5),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SyntheticPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitForLive(model)

        XCTAssertEqual(messageCount("ack", in: transport), 0)
        model.deactivate()
    }

    func testReconnectGenerationAndLaterLiveClipNeverWrite() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1, newestCursor: 1),
            ProtocolFixtures.resumeStarted(status: "fresh", generation: 1, boundaryCursor: 1),
            ProtocolFixtures.event(
                delivery: "resume",
                cursor: 1,
                messageSuffix: "000000000008",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic before clear",
            ),
            ProtocolFixtures.resumeComplete(boundary: 1, generation: 1),
        ])
        let pasteboard = SyntheticPasteboardWriter()
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: pasteboard,
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("initial catch-up") {
            model.lifecycleState == .foregroundLive && model.visibleHistory.count == 1
        }
        XCTAssertTrue(pasteboard.writes.isEmpty)

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
        try await waitUntil("generation catch-up") {
            model.lifecycleState == .foregroundLive && model.visibleHistory.isEmpty
        }
        XCTAssertTrue(pasteboard.writes.isEmpty)

        let resumeMessages = transport.sentMessages.compactMap { data -> String? in
            let value = String(decoding: data, as: UTF8.self)
            return value.contains("\"type\":\"resume\"") ? value : nil
        }
        XCTAssertEqual(resumeMessages.count, 2)
        XCTAssertTrue(resumeMessages[1].contains("\"known_clear_generation\":\"1\""))
        XCTAssertTrue(resumeMessages[1].contains("\"after_cursor\":\"1\""))

        transport.enqueue(
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 2,
                messageSuffix: "000000000009",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic after clear",
                generation: 2,
            ),
        )
        try await waitUntil("post-clear live preview") { model.visibleHistory.count == 1 }
        XCTAssertTrue(pasteboard.writes.isEmpty)
        model.deactivate()
    }

    func testLaterLiveEventPrunesExpiredVisibleHistory() async throws {
        var currentTime = ProtocolFixtures.now
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 1,
                messageSuffix: "000000000016",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic expiring first",
            ),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SyntheticPasteboardWriter(),
            preferences: preferences,
            now: { currentTime },
        )

        model.activate()
        try await waitUntil("first live row") { model.visibleHistory.map(\.cursor) == [1] }

        currentTime = Date(timeIntervalSince1970: 1_700_604_801)
        transport.enqueue(
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 2,
                messageSuffix: "000000000017",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic unexpired second",
                acceptedAtMilliseconds: 1_700_604_801_000,
                expiresAtMilliseconds: 1_701_209_601_000,
            ),
        )
        try await waitUntil("second live row") { model.visibleHistory.first?.cursor == 2 }

        XCTAssertEqual(model.visibleHistory.map(\.cursor), [2])
        model.deactivate()
    }

    func testClearNoticeDropsProcessStateWithoutPasteboardWrite() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeStarted(),
            ProtocolFixtures.resumeComplete(boundary: nil),
            ProtocolFixtures.event(
                delivery: "live",
                cursor: 1,
                messageSuffix: "000000000004",
                sourcePeerID: "peer-reserved-source",
                text: "synthetic clear target",
            ),
            ProtocolFixtures.clearNotice(generation: 2, requestSuffix: "000000000005"),
        ])
        let pasteboard = SyntheticPasteboardWriter()
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: pasteboard,
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("clear notice") { model.lifecycleState == .foregroundLive && model.visibleHistory.isEmpty }

        XCTAssertEqual(pasteboard.writes.count, 0)
        model.deactivate()
    }

    func testResumeCompletionBeforeStartIsRejectedWithoutPasteboardWrite() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(),
            ProtocolFixtures.resumeComplete(boundary: nil),
        ])
        let pasteboard = SyntheticPasteboardWriter()
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: pasteboard,
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitUntil("invalid resume order") { model.lifecycleState == .foregroundError }

        XCTAssertEqual(model.errorCode, ProtocolFailure.sessionContextStale.rawValue)
        XCTAssertTrue(pasteboard.writes.isEmpty)
    }

    func testGenerationCatchUpDropsAnOldPendingSharedClear() async throws {
        let transport = SyntheticClipTransport(incoming: [
            ProtocolFixtures.hello(generation: 1),
            ProtocolFixtures.resumeStarted(status: "fresh", generation: 1),
            ProtocolFixtures.resumeComplete(boundary: nil, generation: 1),
        ])
        let preferences = MemoryPreferences()
        preferences.endpoint = try ProtocolFixtures.endpoint()
        let model = MobileSessionModel(
            transport: transport,
            pasteboard: SyntheticPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )

        model.activate()
        try await waitForLive(model)
        model.requestSharedClear()
        try await waitUntil("initial clear request") { self.clearRequestCount(in: transport) == 1 }

        model.refresh()
        transport.enqueue(ProtocolFixtures.hello(generation: 2))
        transport.enqueue(ProtocolFixtures.resumeStarted(status: "generation_changed", generation: 2))
        transport.enqueue(ProtocolFixtures.resumeComplete(boundary: nil, generation: 2))
        try await waitForLive(model)
        try await Task.sleep(for: .milliseconds(20))

        XCTAssertEqual(clearRequestCount(in: transport), 1)
        model.deactivate()
    }

    func testProcessRestartRetainsOnlyAllowedPreferences() async throws {
        let suiteName = "org.example.ClipMeshTests.process-restart"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defaults.removePersistentDomain(forName: suiteName)
        let preferences = ProtectedPreferences(defaults: defaults)
        try preferences.saveEndpoint(ProtocolFixtures.endpoint())
        let running = MobileSessionModel(
            transport: SyntheticClipTransport(incoming: [
                ProtocolFixtures.hello(),
                ProtocolFixtures.resumeStarted(),
                ProtocolFixtures.resumeComplete(boundary: nil),
                ProtocolFixtures.event(
                    delivery: "live",
                    cursor: 1,
                    messageSuffix: "000000000012",
                    sourcePeerID: "peer-reserved-source",
                    text: "synthetic process-only content",
                ),
            ]),
            pasteboard: SyntheticPasteboardWriter(),
            preferences: preferences,
            now: { ProtocolFixtures.now },
        )
        running.activate()
        try await waitUntil("process history") { running.visibleHistory.count == 1 }
        running.deactivate()

        let restarted = MobileSessionModel(
            transport: SyntheticClipTransport(),
            pasteboard: SyntheticPasteboardWriter(),
            preferences: ProtectedPreferences(defaults: defaults),
            now: { ProtocolFixtures.now },
        )

        XCTAssertTrue(restarted.visibleHistory.isEmpty)
        XCTAssertEqual(restarted.lifecycleState, .inactive)
        let persisted = try XCTUnwrap(defaults.persistentDomain(forName: suiteName))
        XCTAssertEqual(Set(persisted.keys), ProtectedPreferences.allowedKeys)
        XCTAssertEqual(defaults.string(forKey: "connection_state"), ConnectionState.disconnected.rawValue)
        defaults.removePersistentDomain(forName: suiteName)
    }

    private func waitForLive(_ model: MobileSessionModel) async throws {
        try await waitUntil("foreground live") { model.lifecycleState == .foregroundLive }
    }

    private func clearRequestCount(in transport: SyntheticClipTransport) -> Int {
        messageCount("clear_history", in: transport)
    }

    private func messageCount(_ type: String, in transport: SyntheticClipTransport) -> Int {
        transport.sentMessages.count { message in
            String(decoding: message, as: UTF8.self).contains("\"type\":\"\(type)\"")
        }
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
