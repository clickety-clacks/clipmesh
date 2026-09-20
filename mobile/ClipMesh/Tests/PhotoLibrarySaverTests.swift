import XCTest
@testable import ClipMesh

final class PhotoLibrarySaverTests: XCTestCase {
    @MainActor
    func testSaveItemsSelectsMediaAndPreservesOriginalURLs() {
        let imageURL = URL(fileURLWithPath: "/tmp/original-photo.png")
        let videoURL = URL(fileURLWithPath: "/tmp/original-video.mov")
        let otherURL = URL(fileURLWithPath: "/tmp/original-archive.zip")
        let descriptors = [
            MeshFileDescriptor(name: "photo.png", media_type: "image/png", size_bytes: 1, sha256: String(repeating: "a", count: 64)),
            MeshFileDescriptor(name: "movie.mov", media_type: "video/quicktime", size_bytes: 1, sha256: String(repeating: "b", count: 64)),
            MeshFileDescriptor(name: "archive.zip", media_type: "application/zip", size_bytes: 1, sha256: String(repeating: "c", count: 64)),
        ]

        let result = PhotoLibrarySaver.saveItems(
            urls: [imageURL, videoURL, otherURL],
            descriptors: descriptors,
        )

        XCTAssertEqual(result.map(\.url), [imageURL, videoURL])
        XCTAssertEqual(result.map(\.kind), [.photo, .video])
    }

    @MainActor
    func testSaveItemsRejectsAIncompleteURLMapping() {
        let descriptor = MeshFileDescriptor(
            name: "photo.png",
            media_type: "image/png",
            size_bytes: 1,
            sha256: String(repeating: "a", count: 64),
        )

        XCTAssertTrue(PhotoLibrarySaver.saveItems(urls: [], descriptors: [descriptor]).isEmpty)
    }

    @MainActor
    func testSaveErrorMessagesExplainPermissionAndFailure() {
        XCTAssertTrue(PhotoLibrarySaveError.permissionDenied.userMessage.contains("Settings"))
        XCTAssertTrue(PhotoLibrarySaveError.saveFailed.userMessage.contains("Photos"))
    }
}
