import UIKit

@MainActor
final class SystemPasteboardWriter: PasteboardWriting {
    func write(_ content: ClipContentV1) throws {
        UIPasteboard.general.string = content.toPlatform()
    }
}
