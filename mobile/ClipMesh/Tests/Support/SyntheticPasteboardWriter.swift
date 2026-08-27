@testable import ClipMesh

@MainActor
final class SyntheticPasteboardWriter: PasteboardWriting {
    private(set) var writes: [String] = []

    func write(_ content: ClipContentV1) throws {
        writes.append(content.toPlatform())
    }
}
