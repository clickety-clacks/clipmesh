import Foundation

struct ClearRejectedV1: Equatable {
    let requestID: UUID?
    let code: ReasonCodeV1
    let retryable: Bool
}
