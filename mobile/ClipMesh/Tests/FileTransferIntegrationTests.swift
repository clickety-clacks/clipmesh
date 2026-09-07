import XCTest
import UIKit
import UniformTypeIdentifiers
@testable import ClipMesh

final class FileTransferIntegrationTests: XCTestCase {
    @MainActor
    func testBinarySelectionThroughRealHub() async throws {
        guard let value = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"],
              !value.isEmpty, !value.contains("$(") else {
            throw XCTSkip("Requires an isolated ClipMesh test hub")
        }
        let endpoint = try HubEndpoint(value)
        let sender = FileTransferClient()
        let receiver = FileTransferClient()
        sender.open(endpoint)
        receiver.open(endpoint)
        defer { sender.close(); receiver.close() }
        // Cross a transfer-chunk boundary and include bytes invalid as UTF-8.
        // Exceeds the server's initial message burst to exercise throttling.
        let binary = Data((0..<(6 * 1024 * 1024 + 19)).map { UInt8($0 % 256) })
        let empty = Data()
        let descriptors = [
            MeshFileDescriptor(name: "fixture.zip", media_type: "application/zip", size_bytes: UInt64(binary.count), sha256: FileTransferClient.hash(binary)),
            MeshFileDescriptor(name: "empty.bin", media_type: "application/octet-stream", size_bytes: 0, sha256: FileTransferClient.hash(empty))
        ]
        let id = UUID()
        try await sender.send(Array(zip(descriptors, [binary, empty])), clipID: id)
        let clips = try await receiver.history()
        let clip = try XCTUnwrap(clips.first { $0.id == id })
        XCTAssertEqual(clip.manifest.files, descriptors)
        let received = try await receiver.download(clip, index: 0)
        XCTAssertEqual(received, binary)
        let receivedEmpty = try await receiver.download(clip, index: 1)
        XCTAssertTrue(receivedEmpty.isEmpty)
        let historyModel = FileHistoryModel()
        await historyModel.refresh(endpoint: value)
        await historyModel.download(clip, endpoint: value)
        let cached = try XCTUnwrap(historyModel.localFiles[clip.id])
        XCTAssertEqual(cached.count, 2)
        XCTAssertEqual(try Data(contentsOf: cached[0]), binary)
        let pasteboard = UIPasteboard.withUniqueName()
        defer { UIPasteboard.remove(withName: pasteboard.name) }
        pasteboard.string = "untouched until explicit copy"
        historyModel.copy(clip, to: pasteboard)
        let providers = pasteboard.itemProviders
        XCTAssertEqual(providers.count, 2)
        historyModel.clear()
        XCTAssertTrue(historyModel.clips.isEmpty)
        XCTAssertTrue(historyModel.localFiles.isEmpty)
        for url in cached { XCTAssertFalse(FileManager.default.fileExists(atPath: url.path)) }
        // Copied bytes remain available after the app deletes its own cache.
        let copied: Data = try await withCheckedThrowingContinuation { continuation in
            providers[0].loadDataRepresentation(forTypeIdentifier: UTType.zip.identifier) { data, error in
                if let data { continuation.resume(returning: data) }
                else { continuation.resume(throwing: error ?? FileTransferFailure.invalidReply) }
            }
        }
        XCTAssertEqual(copied, binary)

        let observer = FileHistoryModel()
        let watching = Task { await observer.observe(endpoint: value) }
        defer { observer.stop(); watching.cancel(); observer.clear() }
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 20, height: 20))
        let image = renderer.pngData { context in
            UIColor.blue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 20, height: 20))
        }
        let imageID = UUID()
        let imageFile = MeshFileDescriptor(name: "preview.png", media_type: "image/png", size_bytes: UInt64(image.count), sha256: FileTransferClient.hash(image))
        try await sender.send([(imageFile, image)], clipID: imageID)
        let deadline = ContinuousClock.now.advanced(by: .seconds(20))
        while observer.localFiles[imageID] == nil, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(100))
        }
        let automatic = try XCTUnwrap(observer.localFiles[imageID]?.first)
        XCTAssertEqual(try Data(contentsOf: automatic), image)
        XCTAssertTrue(observer.clips.contains { $0.id == imageID })

        // A history reset must remove cached previews without disabling
        // future arrivals. Restart only after clear has stopped the old loop.
        observer.clear()
        watching.cancel()
        await watching.value
        XCTAssertFalse(FileManager.default.fileExists(atPath: automatic.path))
        let restarted = Task { await observer.observe(endpoint: value) }
        defer { observer.stop(); restarted.cancel() }
        let nextID = UUID()
        try await sender.send([(imageFile, image)], clipID: nextID)
        let restartDeadline = ContinuousClock.now.advanced(by: .seconds(20))
        while observer.localFiles[nextID] == nil, ContinuousClock.now < restartDeadline {
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertNotNil(observer.localFiles[nextID])
        XCTAssertFalse(observer.clips.contains { $0.id == imageID })
    }
}
