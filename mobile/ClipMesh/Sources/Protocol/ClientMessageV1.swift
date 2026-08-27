enum ClientMessageV1: Equatable {
    case acknowledge(AckV1)
    case clearHistory(ClearHistoryRequestV1)
    case resume(ResumeRequestV1)
}
