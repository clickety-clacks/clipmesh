import Foundation

/// What became of the ClipMesh half of one Copy or Cut tap.
enum KeyboardPublishResult: Equatable {
    /// The hub returned `publish_accepted` for this tap's publish.
    case confirmed
    /// The publish left the keyboard, but no confirmation arrived in time or
    /// the connection failed after the send.
    case unconfirmed
    /// The connection or session failed before the publish was sent.
    case unreachable
    case refused(ReasonCodeV1)
    /// The hub refused the connection at its per-device limit.
    case busy
    /// The tap was superseded or the keyboard was dismissed.
    case cancelled
}

/// What one Paste tap got from the hub.
enum KeyboardFetchResult: Equatable {
    case clip(String)
    case empty
    case busy
    case unreachable
    case cancelled
}

/// One hub session for exactly one keyboard tap.
///
/// The keyboard holds no connection between taps: each tap opens its own
/// session, finishes the resume the protocol requires, does its one thing and
/// closes. Nothing here logs, and nothing outlives the tap.
@MainActor
final class KeyboardHubTap {
    static let window: Duration = .seconds(10)

    private let endpoint: HubEndpoint
    private let transport: any ClipTransport
    private let now: () -> Date
    private let window: Duration
    private let codec = ProtocolV1Codec()
    private var deadlineTask: Task<Void, Never>?
    private var isCancelled = false
    private var hasSent = false

    init(
        endpoint: HubEndpoint,
        transport: any ClipTransport = URLSessionClipTransport(),
        now: @escaping () -> Date = { .now },
        window: Duration = KeyboardHubTap.window,
    ) {
        self.endpoint = endpoint
        self.transport = transport
        self.now = now
        self.window = window
    }

    /// Ends the tap's session. A pending publish or fetch returns `.cancelled`.
    func cancel() {
        isCancelled = true
        finish()
    }

    /// Publishes `text` and waits for the hub's answer.
    func publish(_ text: String) async -> KeyboardPublishResult {
        startDeadline()
        defer { finish() }
        do {
            let hello = try await openSession()
            let content: ClipContentV1
            do {
                content = try ClipContentV1.fromPlatform(text, maximumBytes: hello.limits.maximumPayloadBytes)
            } catch {
                return .refused(Self.reason(for: error))
            }
            let generation = try await resume(hello, afterCursor: hello.newestCursor)
            let messageID = UUID()
            try await send(.publish(
                messageID: messageID,
                generation: generation,
                createdAt: nowMilliseconds,
                content: content,
            ))
            hasSent = true
            startDeadline()
            while true {
                switch try await receive(hello) {
                case let .publishAccepted(value) where value.messageID == messageID:
                    return .confirmed
                case let .publishRejected(value) where value.messageID == nil || value.messageID == messageID:
                    return .refused(value.code)
                case .error:
                    return .unconfirmed
                default:
                    continue
                }
            }
        } catch {
            if isCancelled {
                return .cancelled
            }
            if hasSent {
                return .unconfirmed
            }
            return isBusy(error) ? .busy : .unreachable
        }
    }

    /// Fetches the newest retained text clip without downloading history.
    ///
    /// The resume asks only for clips after one below `newest_cursor`, so the
    /// hub replays at most the newest clip (plus any accepted in between).
    func fetchNewest() async -> KeyboardFetchResult {
        startDeadline()
        defer { finish() }
        do {
            let hello = try await openSession()
            guard let newest = hello.newestCursor else {
                return .empty
            }
            var best: EventV1?
            try await resume(
                hello,
                afterCursor: newest > 1 ? newest - 1 : nil,
                onEvent: { event in
                    guard event.expiresAtMilliseconds > self.nowMilliseconds else {
                        return
                    }
                    if best.map({ event.cursor > $0.cursor }) ?? true {
                        best = event
                    }
                },
                onClear: { notice in
                    if let cleared = notice.clearedThroughCursor, let current = best, current.cursor <= cleared {
                        best = nil
                    }
                },
            )
            guard !isCancelled else {
                return .cancelled
            }
            guard let best else {
                return .empty
            }
            return .clip(best.content.toPlatform())
        } catch {
            if isCancelled {
                return .cancelled
            }
            return isBusy(error) ? .busy : .unreachable
        }
    }

    private func openSession() async throws -> ServerHelloV1 {
        try checkCancelled()
        try await transport.open(endpoint)
        try checkCancelled()
        guard case let .serverHello(hello) = try await codec.decodeHubMessage(transport.receive()) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return hello
    }

    /// Runs the resume the protocol requires before a session is live and
    /// returns the clear generation it settled on.
    @discardableResult
    private func resume(
        _ hello: ServerHelloV1,
        afterCursor: UInt64?,
        onEvent: (EventV1) -> Void = { _ in },
        onClear: (ClearNoticeV1) -> Void = { _ in },
    ) async throws -> UInt64 {
        try await send(.resume(ResumeRequestV1(
            knownHistoryEpoch: hello.historyEpoch,
            knownClearGeneration: hello.clearGeneration,
            afterCursor: afterCursor,
        )))
        var started: ResumeStartedV1?
        var generation = hello.clearGeneration
        while true {
            switch try await receive(hello) {
            case let .resumeStarted(value):
                guard started == nil, value.requestedAfterCursor == afterCursor else {
                    throw ProtocolFailure.sessionContextStale
                }
                started = value
                generation = value.clearGeneration
            case let .event(value):
                guard let started,
                      value.delivery == .resume,
                      value.historyEpoch == started.historyEpoch,
                      value.clearGeneration == generation,
                      started.boundaryCursor.map({ value.cursor <= $0 }) == true
                else {
                    throw ProtocolFailure.sessionContextStale
                }
                onEvent(value)
            case let .clearNotice(value):
                generation = max(generation, value.clearGeneration)
                onClear(value)
            case let .resumeComplete(value):
                guard let started,
                      value.historyEpoch == started.historyEpoch,
                      value.boundaryCursor == started.boundaryCursor
                else {
                    throw ProtocolFailure.sessionContextStale
                }
                return generation
            case let .error(value):
                throw HubRefusal(code: value.code)
            default:
                throw ProtocolFailure.protocolSchemaInvalid
            }
        }
    }

    private func send(_ message: ClientMessageV1) async throws {
        try checkCancelled()
        try await transport.send(codec.encodeClientMessage(message))
    }

    private func receive(_ hello: ServerHelloV1) async throws -> HubMessageV1 {
        try checkCancelled()
        return try await codec.decodeHubMessage(
            transport.receive(),
            maximumMessageBytes: hello.limits.maximumWebSocketMessageBytes,
            maximumPayloadBytes: hello.limits.maximumPayloadBytes,
        )
    }

    private func checkCancelled() throws {
        if isCancelled {
            throw CancellationError()
        }
    }

    private func isBusy(_ error: any Error) -> Bool {
        if let refusal = error as? HubRefusal {
            return refusal.code == .connectionLimitReached
        }
        // The hub refuses an upgrade over its per-device connection limit with
        // HTTP 429. The WebSocket task exposes the status but not the body.
        return transport.refusedUpgradeStatusCode == 429
    }

    private func startDeadline() {
        deadlineTask?.cancel()
        deadlineTask = Task { [weak self, window] in
            try? await Task.sleep(for: window)
            guard !Task.isCancelled else {
                return
            }
            self?.transport.close()
        }
    }

    private func finish() {
        deadlineTask?.cancel()
        deadlineTask = nil
        transport.close()
    }

    private var nowMilliseconds: Int64 {
        Int64((now().timeIntervalSince1970 * 1000).rounded(.down))
    }

    private static func reason(for error: any Error) -> ReasonCodeV1 {
        (error as? ProtocolFailure).flatMap { ReasonCodeV1(rawValue: $0.rawValue) } ?? .payloadTooLarge
    }
}

private struct HubRefusal: Error {
    let code: ReasonCodeV1
}
