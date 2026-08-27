import Foundation

struct PublishRejectedV1: Equatable {
    let messageID: UUID?
    let code: ReasonCodeV1
    let retryable: Bool
}
