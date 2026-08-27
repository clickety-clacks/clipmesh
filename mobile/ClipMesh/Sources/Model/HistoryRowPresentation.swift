import Foundation

struct HistoryRowPresentation: Identifiable, Equatable {
    let id: UUID
    let cursor: UInt64
    let acceptedAt: Date
    let preview: String
    let isStale: Bool
}
