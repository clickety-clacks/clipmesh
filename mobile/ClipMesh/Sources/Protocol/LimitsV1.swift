struct LimitsV1: Equatable {
    let maximumPayloadBytes: Int
    let retentionSeconds: Int
    let historyMaximumEntries: Int
    let maximumClockSkewMilliseconds: Int
    let maximumWebSocketMessageBytes: Int
}
