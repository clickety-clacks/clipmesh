import Foundation

@MainActor
final class URLSessionClipTransport: ClipTransport {
    private var session: URLSession?
    private var task: URLSessionWebSocketTask?
    private var validatedUpgrade = false

    func open(_ endpoint: HubEndpoint) async throws {
        close()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        let session = URLSession(configuration: configuration)
        let task = session.webSocketTask(with: endpoint.url, protocols: ["clipmesh.v1"])
        task.maximumMessageSize = ProtocolV1Codec.hardMaximumMessageBytes
        self.session = session
        self.task = task
        validatedUpgrade = false
        task.resume()
    }

    func send(_ data: Data) async throws {
        guard let task, let string = String(data: data, encoding: .utf8) else {
            throw ProtocolFailure.sessionContextStale
        }
        try await task.send(.string(string))
    }

    func receive() async throws -> Data {
        guard let task else {
            throw ProtocolFailure.sessionContextStale
        }
        let message = try await task.receive()
        if !validatedUpgrade {
            guard let response = task.response as? HTTPURLResponse,
                  response.value(forHTTPHeaderField: "Sec-WebSocket-Protocol") == "clipmesh.v1",
                  response.value(forHTTPHeaderField: "Sec-WebSocket-Extensions") == nil
            else {
                close()
                throw ProtocolFailure.protocolVersionUnsupported
            }
            validatedUpgrade = true
        }
        switch message {
        case let .string(string):
            return Data(string.utf8)
        case .data:
            throw ProtocolFailure.protocolSchemaInvalid
        @unknown default:
            throw ProtocolFailure.protocolSchemaInvalid
        }
    }

    func close() {
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        validatedUpgrade = false
        session?.invalidateAndCancel()
        session = nil
    }
}
