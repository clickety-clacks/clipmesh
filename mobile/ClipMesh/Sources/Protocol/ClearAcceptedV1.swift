import Foundation

struct ClearAcceptedV1: Equatable {
    let requestID: UUID
    let clearGeneration: UInt64
    let clearedThroughCursor: UInt64?
    let duplicate: Bool
}
