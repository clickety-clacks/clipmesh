import Foundation

@MainActor
protocol ClipTransport: AnyObject {
    func open(_ endpoint: HubEndpoint) async throws
    func send(_ data: Data) async throws
    func receive() async throws -> Data
    func close()
}
