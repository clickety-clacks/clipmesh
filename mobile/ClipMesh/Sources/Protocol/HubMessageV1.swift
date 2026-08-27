enum HubMessageV1: Equatable {
    case clearAccepted(ClearAcceptedV1)
    case clearNotice(ClearNoticeV1)
    case clearRejected(ClearRejectedV1)
    case error(ErrorV1)
    case event(EventV1)
    case publishAccepted(PublishAcceptedV1)
    case publishRejected(PublishRejectedV1)
    case resumeComplete(ResumeCompleteV1)
    case resumeStarted(ResumeStartedV1)
    case serverHello(ServerHelloV1)
}
