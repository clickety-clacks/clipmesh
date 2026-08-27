struct WireContentV1: Equatable {
    let contentType: String
    let payloadBase64URL: String
    let payloadBytes: Int
    let contentSHA256: String
}
