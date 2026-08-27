import Foundation

struct PublishAcceptedV1: Equatable {
    let messageID: UUID
    let cursor: UInt64
    let expiresAtMilliseconds: Int64
    let duplicate: Bool
}
