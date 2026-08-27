import Foundation

struct StoredClip: Equatable {
    let messageID: UUID
    let cursor: UInt64
    let acceptedAtMilliseconds: Int64
    let expiresAtMilliseconds: Int64
    let content: ClipContentV1
    var isStale: Bool

    var presentation: HistoryRowPresentation {
        HistoryRowPresentation(
            id: messageID,
            cursor: cursor,
            acceptedAt: Date(timeIntervalSince1970: Double(acceptedAtMilliseconds) / 1000),
            preview: content.preview(maximumScalars: 160),
            isStale: isStale,
        )
    }
}
