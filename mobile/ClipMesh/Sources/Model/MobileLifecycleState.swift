enum MobileLifecycleState: String, Equatable {
    case foregroundConnecting = "foreground_connecting"
    case foregroundError = "foreground_error"
    case foregroundLive = "foreground_live"
    case inactive
}
