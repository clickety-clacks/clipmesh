import Foundation

struct AckV1: Equatable {
    let historyEpoch: UUID
    let clearGeneration: UInt64
    let cursor: UInt64
}
