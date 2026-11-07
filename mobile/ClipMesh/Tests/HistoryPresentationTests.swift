@testable import ClipMesh
import XCTest

@MainActor
final class HistoryPresentationTests: XCTestCase {
    func testSearchIncludesFullTextBeyondPreviewAndMachineName() {
        let row = HistoryRowPresentation(
            id: UUID(),
            cursor: 1,
            acceptedAt: Date(timeIntervalSince1970: 1_700_000_000),
            preview: "short preview",
            searchableContent: "short preview with the complete searchable sentence",
            sourcePeerID: "peer-source",
            sourceMachineName: "example-host",
            isStale: false,
        )

        XCTAssertTrue(row.matches("complete searchable sentence"))
        XCTAssertTrue(row.matches("example-host"))
    }

    func testSourceMetadataMapsPeerNamesAndFileSources() throws {
        let clipID = "00000000-0000-4000-8000-000000000001"
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data("""
        {"protocol_version":1,"type":"source_metadata","peers":[{"id":"peer-example-host","display_name":"example-host"}],"file_sources":[{"clip_id":"\(clipID)","source_peer_id":"peer-example-host"}]}
        """.utf8)) as? [String: Any])

        let metadata = try FileTransferClient.decodeSourceMetadata(object)
        XCTAssertEqual(metadata.namesByPeerID["peer-example-host"], "example-host")
        XCTAssertEqual(metadata.fileSourcePeerIDs[UUID(uuidString: clipID)!], "peer-example-host")
    }

    func testFileHistorySchemaRemainsCompatibleWithOldReplies() throws {
        let json = """
        {"clip_id":"00000000-0000-4000-8000-000000000001","accepted_at":1700000000000,"manifest":{"files":[{"name":"photo.png","media_type":"image/png","size_bytes":1,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}}
        """

        let clip = try JSONDecoder().decode(MeshFileClip.self, from: Data(json.utf8))
        XCTAssertEqual(clip.manifest.files.first?.name, "photo.png")
    }
}
