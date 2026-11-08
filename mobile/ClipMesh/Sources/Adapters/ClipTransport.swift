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
    /// The HTTP status of a WebSocket upgrade the hub refused, such as 429,
    /// once a receive has failed. Nil while the upgrade is pending or accepted.
    var refusedUpgradeStatusCode: Int? { get }
}

extension ClipTransport {
    var refusedUpgradeStatusCode: Int? {
        nil
    }
}
