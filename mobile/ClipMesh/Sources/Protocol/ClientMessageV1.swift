import Foundation

enum ClientMessageV1: Equatable {
    case publish(messageID: UUID, generation: UInt64, createdAt: Int64, content: ClipContentV1)
    case acknowledge(AckV1)
    case clearHistory(ClearHistoryRequestV1)
    case resume(ResumeRequestV1)
}
