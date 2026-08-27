import Foundation

struct ClearHistoryRequestV1: Equatable {
    let requestID: UUID
    let expectedClearGeneration: UInt64
}
