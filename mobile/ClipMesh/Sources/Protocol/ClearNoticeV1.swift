import Foundation

struct ClearNoticeV1: Equatable {
    let requestID: UUID
    let clearGeneration: UInt64
    let clearedThroughCursor: UInt64?
}
