import Foundation

struct HistoryRowPresentation: Identifiable, Equatable {
    let id: UUID
    let cursor: UInt64
    let acceptedAt: Date
    let preview: String
    let searchableContent: String
    let sourcePeerID: String
    let sourceMachineName: String
    let isStale: Bool

    func matches(_ query: String) -> Bool {
        searchableContent.localizedStandardContains(query)
            || sourceMachineName.localizedStandardContains(query)
    }
}
