import Foundation

struct ResumeStartedV1: Equatable {
    let historyEpoch: UUID
    let clearGeneration: UInt64
    let status: ResumeStatusV1
    let requestedAfterCursor: UInt64?
    let boundaryCursor: UInt64?
    let lostThroughCursor: UInt64?
}
