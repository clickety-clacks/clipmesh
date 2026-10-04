@testable import ClipMesh
import Foundation

@MainActor
final class SyntheticClipTransport: ClipTransport {
    private(set) var openedEndpoints: [HubEndpoint] = []
    private(set) var sentMessages: [Data] = []
    private(set) var isWaitingForInput = false
    private(set) var pingCount = 0
    /// When false, pings fail as they would on a connection the hub dropped.
    var answersPings = true
    private var incoming: [Data]
    private var waiter: CheckedContinuation<Data, any Error>?

    init(incoming: [Data] = []) {
        self.incoming = incoming
    }

    func open(_ endpoint: HubEndpoint) async throws {
        openedEndpoints.append(endpoint)
    }

    func send(_ data: Data) async throws {
        sentMessages.append(data)
    }

    func receive() async throws -> Data {
        if !incoming.isEmpty {
            return incoming.removeFirst()
        }
        return try await withCheckedThrowingContinuation { continuation in
            isWaitingForInput = true
            waiter = continuation
        }
    }

    func ping(timeout _: Duration) async throws {
        pingCount += 1
        if !answersPings {
            throw ProtocolFailure.heartbeatTimeout
        }
    }

    func close() {
        isWaitingForInput = false
        waiter?.resume(throwing: CancellationError())
        waiter = nil
    }

    func enqueue(_ data: Data) {
        if let waiter {
            self.waiter = nil
            isWaitingForInput = false
            waiter.resume(returning: data)
        } else {
            incoming.append(data)
        }
    }
}
