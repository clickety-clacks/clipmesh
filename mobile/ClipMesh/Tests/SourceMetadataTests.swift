@testable import ClipMesh
import XCTest

@MainActor
final class SourceMetadataTests: XCTestCase {
    private let clipID = "00000000-0000-4000-8000-00000000000a"

    private func metadata(
        peers: [[String: Any]] = [["id": "peer-gibson", "display_name": "gibson"]],
        fileSources: [[String: Any]]? = nil,
    ) -> [String: Any] {
        [
            "protocol_version": 1,
            "type": "source_metadata",
            "peers": peers,
            "file_sources": fileSources ?? [["clip_id": clipID, "source_peer_id": "peer-gibson"]],
        ]
    }

    func testDecodesPeerNamesAndFileSources() throws {
        let result = try FileTransferClient.decodeSourceMetadata(metadata())

        XCTAssertEqual(result.namesByPeerID, ["peer-gibson": "gibson"])
        XCTAssertEqual(result.fileSourcePeerIDs[UUID(uuidString: clipID)!], "peer-gibson")
    }

    func testRejectsDuplicatePeerIDsAndFileClipIDs() {
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(peers: [
            ["id": "peer-gibson", "display_name": "gibson"],
            ["id": "peer-gibson", "display_name": "other"],
        ])))
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(fileSources: [
            ["clip_id": clipID, "source_peer_id": "peer-gibson"],
            ["clip_id": clipID, "source_peer_id": "peer-other"],
        ])))
    }

    func testRejectsUnknownFieldsMalformedValuesAndNonCanonicalUUIDs() {
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(peers: [
            ["id": "peer-gibson", "display_name": "gibson", "extra": true],
        ])))
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(fileSources: [
            ["clip_id": clipID.uppercased(), "source_peer_id": "peer-gibson"],
        ])))
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(fileSources: [
            ["clip_id": clipID, "source_peer_id": "peer\nname"],
        ])))
    }

    func testRejectsOversizedBoundedArrays() {
        let peers = (0 ..< 1_025).map { index in
            ["id": "peer-\(index)", "display_name": "machine-\(index)"] as [String: Any]
        }
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(peers: peers)))

        let files = (0 ..< 501).map { index in
            [
                "clip_id": String(format: "00000000-0000-4000-8000-%012d", index),
                "source_peer_id": "peer-gibson",
            ] as [String: Any]
        }
        XCTAssertThrowsError(try FileTransferClient.decodeSourceMetadata(metadata(fileSources: files)))
    }

    func testConcurrentMetadataRefreshesShareOneRequest() async {
        var calls = 0
        let model = FileHistoryModel { _ in
            calls += 1
            try await Task.sleep(for: .milliseconds(100))
            return MeshMachineDirectory(namesByPeerID: ["peer": "gibson"], fileSourcePeerIDs: [:])
        }
        let endpoint = "ws://100.64.0.7:18494/v1/stream"
        let first = Task { await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer"]) }
        await Task.yield()
        let second = Task { await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer"]) }
        await first.value
        await second.value

        XCTAssertEqual(calls, 1)
        XCTAssertEqual(model.machineNames, ["peer": "gibson"])
        model.stop()
    }

    func testNewIDsJoinedToAnOlderRequestForceOneFollowUp() async {
        var calls = 0
        let model = FileHistoryModel { _ in
            calls += 1
            try await Task.sleep(for: .milliseconds(100))
            return MeshMachineDirectory(
                namesByPeerID: ["peer-new": "new-machine"],
                fileSourcePeerIDs: [:],
            )
        }
        let endpoint = "ws://100.64.0.7:18494/v1/stream"
        let first = Task { await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-old"]) }
        await Task.yield()
        let second = Task { await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-new"]) }
        await first.value
        await second.value

        XCTAssertEqual(calls, 2)
        XCTAssertEqual(model.machineNames, ["peer-new": "new-machine"])
        model.stop()
    }

    func testFailedMetadataRefreshIsThrottledAcrossNewPeerIDs() async {
        var calls = 0
        let model = FileHistoryModel { _ in
            calls += 1
            throw MetadataHTTPFailure.status(404)
        }
        let endpoint = "ws://100.64.0.7:18494/v1/stream"

        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-old"])
        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-new"])

        XCTAssertEqual(calls, 1)
        XCTAssertTrue(model.machineNames.isEmpty)
        model.stop()
    }

    func testFreshMetadataIsThrottledAcrossChangedPeerIDs() async {
        var calls = 0
        let model = FileHistoryModel { _ in
            calls += 1
            return MeshMachineDirectory(namesByPeerID: ["peer": "gibson"], fileSourcePeerIDs: [:])
        }
        let endpoint = "ws://100.64.0.7:18494/v1/stream"

        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-old"])
        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-new"])

        XCTAssertEqual(calls, 1)
        model.stop()
    }

    func testNewPeerSchedulesAnEventualRefreshWithoutAnotherEvent() async {
        var calls = 0
        let model = FileHistoryModel(
            sourceMetadataLoader: { _ in
                calls += 1
                return MeshMachineDirectory(namesByPeerID: ["peer": "gibson"], fileSourcePeerIDs: [:])
            },
            sourceMetadataFreshnessInterval: 1,
            sourceMetadataMinimumInterval: 0.05,
        )
        let endpoint = "ws://100.64.0.7:18494/v1/stream"

        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-old"])
        await model.refreshSourceMetadata(endpoint: endpoint, textPeerIDs: ["peer-new"])
        try? await Task.sleep(for: .milliseconds(400))

        XCTAssertEqual(calls, 2)
        model.stop()
    }

    func testEndpointSwitchRejectsDelayedOldMetadataReply() async {
        let oldEndpoint = "ws://100.64.0.7:18494/v1/stream"
        let newEndpoint = "ws://100.64.0.8:18494/v1/stream"
        var calls: [String] = []
        let oldStarted = expectation(description: "old metadata request started")
        let model = FileHistoryModel { endpoint in
            calls.append(endpoint.displayValue)
            if endpoint.displayValue == oldEndpoint {
                oldStarted.fulfill()
                try? await Task.sleep(for: .milliseconds(100))
                return MeshMachineDirectory(namesByPeerID: ["peer-old": "old"], fileSourcePeerIDs: [:])
            }
            return MeshMachineDirectory(namesByPeerID: ["peer-new": "new"], fileSourcePeerIDs: [:])
        }

        let oldTask = Task {
            await model.refreshSourceMetadata(endpoint: oldEndpoint, textPeerIDs: ["peer-old"])
        }
        await fulfillment(of: [oldStarted], timeout: 1)
        await model.refreshSourceMetadata(endpoint: newEndpoint, textPeerIDs: ["peer-new"])
        await oldTask.value

        XCTAssertEqual(calls, [oldEndpoint, newEndpoint])
        XCTAssertEqual(model.machineNames, ["peer-new": "new"])
        model.stop()
    }

    func testStopPreventsAStaleMetadataReplyFromApplying() async {
        let model = FileHistoryModel { _ in
            try? await Task.sleep(for: .milliseconds(100))
            return MeshMachineDirectory(namesByPeerID: ["peer": "old"], fileSourcePeerIDs: [:])
        }
        let task = Task {
            await model.refreshSourceMetadata(
                endpoint: "ws://100.64.0.7:18494/v1/stream",
                textPeerIDs: ["peer"],
            )
        }
        await Task.yield()
        model.stop()
        await task.value

        XCTAssertTrue(model.machineNames.isEmpty)
    }
}
