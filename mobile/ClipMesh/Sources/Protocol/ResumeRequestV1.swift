import Foundation

struct ResumeRequestV1: Equatable {
    let knownHistoryEpoch: UUID?
    let knownClearGeneration: UInt64?
    let afterCursor: UInt64?
}
