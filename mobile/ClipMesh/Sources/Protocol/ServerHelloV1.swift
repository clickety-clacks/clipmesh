import Foundation

struct ServerHelloV1: Equatable {
    let sessionID: UUID
    let selfPeerID: String
    let historyEpoch: UUID
    let clearGeneration: UInt64
    let newestCursor: UInt64?
    let serverTimeMilliseconds: Int64
    let limits: LimitsV1
}
