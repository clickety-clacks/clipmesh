import XCTest
import UIKit
import UniformTypeIdentifiers
@testable import ClipMesh

final class ClipboardFileImportTests: XCTestCase {
    @MainActor
    func testImageProviderPreservesBytesAndFilename() async throws {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 32, height: 32))
        let png = renderer.pngData { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 32, height: 32))
        }
        let provider = NSItemProvider()
        provider.suggestedName = "sample.png"
        provider.registerDataRepresentation(forTypeIdentifier: UTType.png.identifier, visibility: .all) { completion in
            completion(png, nil)
            return nil
        }
        let result = try await LocalFileSelection.clipboardFiles([provider])
        XCTAssertEqual(result.count, 1)
        XCTAssertEqual(result[0].descriptor.name, "sample.png")
        XCTAssertEqual(result[0].descriptor.media_type, "image/png")
        XCTAssertEqual(result[0].data, png)
        XCTAssertEqual(result[0].descriptor.sha256, FileTransferClient.hash(png))
    }

    @MainActor
    func testImageRepresentationWinsOverCaption() async throws {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 24, height: 24))
        let png = renderer.pngData { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 24, height: 24))
        }
        let provider = NSItemProvider(object: "image caption" as NSString)
        provider.suggestedName = "shared-image.png"
        provider.registerDataRepresentation(forTypeIdentifier: UTType.png.identifier, visibility: .all) { completion in
            completion(png, nil)
            return nil
        }

        let result = try await LocalFileSelection.clipboardFiles([provider])

        XCTAssertEqual(result.count, 1)
        XCTAssertEqual(result[0].descriptor.media_type, "image/png")
        XCTAssertEqual(result[0].data, png)
    }

    @MainActor
    func testImageObjectProviderProducesPasteableImage() async throws {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 18, height: 12))
        let image = renderer.image { context in
            UIColor.systemRed.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 18, height: 12))
        }
        let provider = NSItemProvider(object: image)
        provider.suggestedName = "shared-image"

        let result = try await LocalFileSelection.clipboardFiles([provider])

        XCTAssertEqual(result.count, 1)
        XCTAssertTrue(result[0].descriptor.media_type.hasPrefix("image/"))
        XCTAssertNotNil(UIImage(data: result[0].data))
        XCTAssertTrue(result[0].descriptor.name.hasPrefix("shared-image"))
    }

    @MainActor
    func testFileURLImportsContentsInsteadOfPathText() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("archive.zip")
        let data = Data([0, 255, 128, 42])
        try data.write(to: url)
        let provider = NSItemProvider(item: url as NSURL, typeIdentifier: UTType.fileURL.identifier)
        let result = try await LocalFileSelection.clipboardFiles([provider])
        XCTAssertEqual(result.count, 1)
        XCTAssertEqual(result[0].descriptor.name, "archive.zip")
        XCTAssertEqual(result[0].data, data)
    }

    @MainActor
    func testBinaryClipboardProviderKeepsSuggestedExtension() async throws {
        let provider = NSItemProvider()
        provider.suggestedName = "export.custom"
        let data = Data([0x00, 0x7F, 0xFF])
        provider.registerDataRepresentation(forTypeIdentifier: UTType.data.identifier, visibility: .all) { completion in
            completion(data, nil)
            return nil
        }

        let result = try await LocalFileSelection.clipboardFiles([provider])

        XCTAssertEqual(result.count, 1)
        XCTAssertEqual(result[0].descriptor.name, "export.custom")
        XCTAssertEqual(result[0].data, data)
    }

    @MainActor
    func testExplicitFileURLWinsOverImagePreviewRepresentation() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("source.png")
        let source = UIGraphicsImageRenderer(size: CGSize(width: 14, height: 14)).pngData { context in
            UIColor.systemGreen.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 14, height: 14))
        }
        try source.write(to: url)
        let provider = NSItemProvider(item: url as NSURL, typeIdentifier: UTType.fileURL.identifier)
        let preview = UIGraphicsImageRenderer(size: CGSize(width: 14, height: 14)).pngData { context in
            UIColor.systemPurple.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 14, height: 14))
        }
        provider.registerDataRepresentation(forTypeIdentifier: UTType.png.identifier, visibility: .all) { completion in
            completion(preview, nil)
            return nil
        }

        let result = try await LocalFileSelection.clipboardFiles([provider])

        XCTAssertEqual(result.count, 1)
        XCTAssertEqual(result[0].descriptor.name, "source.png")
        XCTAssertEqual(result[0].data, source)
    }

    @MainActor
    func testTextProviderDoesNotBecomeAFile() async throws {
        let provider = NSItemProvider(object: "clipboard text" as NSString)
        let result = try await LocalFileSelection.clipboardFiles([provider])
        XCTAssertTrue(result.isEmpty)
    }

    @MainActor
    func testNotesTextWithWebArchiveDoesNotBecomeAFile() async throws {
        let provider = NSItemProvider(object: "Notes plain text" as NSString)
        provider.registerDataRepresentation(forTypeIdentifier: "com.apple.webarchive", visibility: .all) { completion in
            completion(Data("archive representation".utf8), nil)
            return nil
        }
        let result = try await LocalFileSelection.clipboardFiles([provider])
        XCTAssertTrue(result.isEmpty)
        XCTAssertTrue(provider.canLoadObject(ofClass: NSString.self))
    }

    @MainActor
    func testFileManifestRejectsUnsafeMetadataAndOversizedSelections() throws {
        func file(_ name: String, size: UInt64 = 0, mime: String = "application/octet-stream", hash: String = String(repeating: "a", count: 64)) -> MeshFileDescriptor {
            MeshFileDescriptor(name: name, media_type: mime, size_bytes: size, sha256: hash)
        }
        XCTAssertNoThrow(try MeshFileManifest(files: [file("report.pdf"), file("empty.bin")]).validate())
        for name in ["", ".", "..", "../report", "a/b", "a\\b", "a:b", "trailing.", "trailing ", "line\nfeed", String(repeating: "é", count: 128)] {
            XCTAssertThrowsError(try MeshFileManifest(files: [file(name)]).validate())
        }
        for mime in ["", "text", "/plain", "text/", "text/plain/extra", "text/plain; charset=utf8"] {
            XCTAssertThrowsError(try MeshFileManifest(files: [file("safe", mime: mime)]).validate())
        }
        XCTAssertThrowsError(try MeshFileManifest(files: [file("safe", hash: String(repeating: "A", count: 64))]).validate())
        XCTAssertThrowsError(try MeshFileManifest(files: [file("same"), file("SAME")]).validate())
        XCTAssertThrowsError(try MeshFileManifest(files: []).validate())
        XCTAssertThrowsError(try MeshFileManifest(files: [file("huge", size: UInt64.max)]).validate())
        XCTAssertThrowsError(try MeshFileManifest(files: (0..<6).map { file("file\($0)", size: 100 * 1024 * 1024) }).validate())
        XCTAssertThrowsError(try MeshFileManifest(files: (0..<33).map { file("file\($0)") }).validate())
    }
}
