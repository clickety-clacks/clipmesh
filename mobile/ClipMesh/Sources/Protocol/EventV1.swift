import Foundation

struct EventV1: Equatable {
    let historyEpoch: UUID
    let clearGeneration: UInt64
    let cursor: UInt64
    let delivery: DeliveryV1
    let acceptedAtMilliseconds: Int64
    let expiresAtMilliseconds: Int64
    let sourcePeerID: String
    let messageID: UUID
    let createdAtMilliseconds: Int64
    let content: ClipContentV1
}
