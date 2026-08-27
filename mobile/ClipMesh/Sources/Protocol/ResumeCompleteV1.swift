import Foundation

struct ResumeCompleteV1: Equatable {
    let historyEpoch: UUID
    let clearGeneration: UInt64
    let boundaryCursor: UInt64?
}
