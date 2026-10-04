import Foundation

@MainActor
protocol ClipTransport: AnyObject {
    func open(_ endpoint: HubEndpoint) async throws
    func send(_ data: Data) async throws
    func receive() async throws -> Data
    /// Sends a WebSocket ping and returns once the hub answers. Throws if the
    /// answer does not arrive within `timeout` or the connection fails.
    func ping(timeout: Duration) async throws
    func close()
}
