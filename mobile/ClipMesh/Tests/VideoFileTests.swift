import AVFoundation
import QuickLookThumbnailing
import XCTest
@testable import ClipMesh

final class VideoFileTests: XCTestCase {
    @MainActor
    func testVideoThumbnailAndNetworkRoundTrip() async throws {
        guard let endpoint = ProcessInfo.processInfo.environment["CLIPMESH_TEST_HUB_URL"], endpoint.hasPrefix("ws://") else {
            throw XCTSkip("Requires an isolated test hub")
        }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appendingPathComponent("fixture.mov")
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 64, AVVideoHeightKey: 64
        ])
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32ARGB,
            kCVPixelBufferWidthKey as String: 64, kCVPixelBufferHeightKey as String: 64
        ])
        writer.add(input)
        XCTAssertTrue(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        var buffer: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 64, 64, kCVPixelFormatType_32ARGB, nil, &buffer), kCVReturnSuccess)
        let pixels = try XCTUnwrap(buffer)
        CVPixelBufferLockBaseAddress(pixels, [])
        memset(CVPixelBufferGetBaseAddress(pixels), 180, CVPixelBufferGetDataSize(pixels))
        CVPixelBufferUnlockBaseAddress(pixels, [])
        for frame in 0..<30 {
            let deadline = ContinuousClock.now.advanced(by: .seconds(5))
            while !input.isReadyForMoreMediaData, ContinuousClock.now < deadline {
                try await Task.sleep(for: .milliseconds(10))
            }
            XCTAssertTrue(input.isReadyForMoreMediaData)
            XCTAssertTrue(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        XCTAssertEqual(writer.status, .completed)
        let selection = try await LocalFileSelection.read([url])
        XCTAssertNotNil(selection[0].thumbnail, "Video must produce a real thumbnail")
        XCTAssertTrue(selection[0].descriptor.media_type.hasPrefix("video/"))
        let sender = FileTransferClient()
        sender.open(try HubEndpoint(endpoint))
        defer { sender.close() }
        let id = UUID()
        try await sender.send(selection.map { ($0.descriptor, $0.data) }, clipID: id)
        let history = FileHistoryModel()
        defer { history.clear() }
        await history.refresh(endpoint: endpoint)
        let clip = try XCTUnwrap(history.clips.first { $0.id == id })
        await history.download(clip, endpoint: endpoint)
        let downloaded = try XCTUnwrap(history.localFiles[id]?.first)
        XCTAssertEqual(try Data(contentsOf: downloaded), selection[0].data)
        let received = try await LocalFileSelection.read([downloaded])
        XCTAssertNotNil(received[0].thumbnail, "Downloaded video must also produce a thumbnail")
    }
}
