@MainActor
protocol PasteboardWriting: AnyObject {
    func write(_ content: ClipContentV1) throws
}
