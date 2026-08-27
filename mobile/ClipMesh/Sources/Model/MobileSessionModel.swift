import Foundation
import Observation

@MainActor
@Observable
final class MobileSessionModel {
    private(set) var lifecycleState: MobileLifecycleState = .inactive
    private(set) var visibleHistory: [HistoryRowPresentation] = []
    private(set) var errorCode: String?
    var hubURLText: String

    @ObservationIgnored private let codec = ProtocolV1Codec()
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private let pasteboard: any PasteboardWriting
    @ObservationIgnored private let preferences: any PreferencesStoring
    @ObservationIgnored private let transport: any ClipTransport
    @ObservationIgnored private var acknowledgementTask: Task<Void, Never>?
    @ObservationIgnored private var clearRequestTask: Task<Void, Never>?
    @ObservationIgnored private var connectionTask: Task<Void, Never>?
    @ObservationIgnored private var clearGeneration: UInt64?
    @ObservationIgnored private var historyEpoch: UUID?
    @ObservationIgnored private var lastAcknowledgementAtMilliseconds: Int64?
    @ObservationIgnored private var lastCursor: UInt64?
    @ObservationIgnored private var maximumMessageBytes = ProtocolV1Codec.hardMaximumMessageBytes
    @ObservationIgnored private var maximumPayloadBytes = ProtocolV1Codec.hardMaximumPayloadBytes
    @ObservationIgnored private var pendingAcknowledgement: AckV1?
    @ObservationIgnored private var pendingClearRequest: ClearHistoryRequestV1?
    @ObservationIgnored private var processedMessageIDs: Set<UUID> = []
    @ObservationIgnored private var hasStartedResume = false
    @ObservationIgnored private var resumeBoundary: UInt64?
    @ObservationIgnored private var selfPeerID: String?
    @ObservationIgnored private var sessionHello: ServerHelloV1?
    @ObservationIgnored private var storedHistory: [StoredClip] = []

    init(
        transport: any ClipTransport = URLSessionClipTransport(),
        pasteboard: any PasteboardWriting = SystemPasteboardWriter(),
        preferences: any PreferencesStoring = ProtectedPreferences(),
        now: @escaping () -> Date = { .now },
    ) {
        self.transport = transport
        self.pasteboard = pasteboard
        self.preferences = preferences
        self.now = now
        hubURLText = preferences.loadEndpoint()?.displayValue ?? ""
    }

    var isHistoryObscured: Bool {
        lifecycleState == .inactive
    }

    func activate() {
        startConnection()
    }

    func refresh() {
        guard lifecycleState != .inactive else {
            return
        }
        startConnection()
    }

    func deactivate() {
        connectionTask?.cancel()
        connectionTask = nil
        acknowledgementTask?.cancel()
        acknowledgementTask = nil
        clearRequestTask?.cancel()
        clearRequestTask = nil
        pendingAcknowledgement = nil
        hasStartedResume = false
        resumeBoundary = nil
        transport.close()
        lifecycleState = .inactive
        errorCode = nil
        preferences.saveConnectionState(.disconnected)
    }

    @discardableResult
    func saveHubURL() -> Bool {
        do {
            let endpoint = try HubEndpoint(hubURLText)
            preferences.saveEndpoint(endpoint)
            hubURLText = endpoint.displayValue
            errorCode = nil
            return true
        } catch {
            errorCode = ReasonCodeV1.configValueInvalid.rawValue
            return false
        }
    }

    func clearLocalHistory() {
        storedHistory.removeAll()
        updateVisibleHistory()
    }

    func copyHistoryItem(_ id: UUID) {
        guard lifecycleState != .inactive,
              let clip = storedHistory.first(where: { $0.messageID == id })
        else {
            return
        }
        guard clip.expiresAtMilliseconds > nowMilliseconds else {
            storedHistory.removeAll(where: { $0.messageID == id })
            updateVisibleHistory()
            return
        }
        do {
            try pasteboard.write(clip.content)
        } catch {
            transitionToError(ReasonCodeV1.adapterUnavailable.rawValue)
        }
    }

    func requestSharedClear() {
        guard lifecycleState == .foregroundLive,
              pendingClearRequest == nil,
              let clearGeneration
        else {
            return
        }
        let request = ClearHistoryRequestV1(
            requestID: UUID(),
            expectedClearGeneration: clearGeneration,
        )
        pendingClearRequest = request
        clearRequestTask = Task {
            do {
                try await send(.clearHistory(request))
            } catch is CancellationError {
            } catch {
                transitionToError(contentFreeCode(for: error))
            }
            clearRequestTask = nil
        }
    }

    private func startConnection() {
        connectionTask?.cancel()
        acknowledgementTask?.cancel()
        acknowledgementTask = nil
        clearRequestTask?.cancel()
        clearRequestTask = nil
        pendingAcknowledgement = nil
        lastAcknowledgementAtMilliseconds = nil
        hasStartedResume = false
        resumeBoundary = nil
        transport.close()
        pruneExpiredHistory()
        storedHistory = storedHistory.map { clip in
            var stale = clip
            stale.isStale = true
            return stale
        }
        updateVisibleHistory()
        lifecycleState = .foregroundConnecting
        errorCode = nil
        preferences.saveConnectionState(.connecting)

        guard let endpoint = preferences.loadEndpoint() else {
            transitionToError(ReasonCodeV1.configMissingRequired.rawValue)
            return
        }
        connectionTask = Task {
            await runSession(endpoint)
        }
    }

    private func runSession(_ endpoint: HubEndpoint) async {
        do {
            try await transport.open(endpoint)
            let firstData = try await transport.receive()
            let first = try codec.decodeHubMessage(firstData)
            guard case let .serverHello(hello) = first else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            try applyServerHello(hello)
            try await send(.resume(currentResumeRequest))

            while !Task.isCancelled, lifecycleState != .inactive {
                let data = try await transport.receive()
                let message = try codec.decodeHubMessage(
                    data,
                    maximumMessageBytes: maximumMessageBytes,
                    maximumPayloadBytes: maximumPayloadBytes,
                )
                let acknowledgement = try apply(message)
                if case .resumeComplete = message {
                    if let acknowledgement {
                        try await send(.acknowledge(acknowledgement))
                        pendingAcknowledgement = nil
                        lastAcknowledgementAtMilliseconds = nowMilliseconds
                    }
                    if let pendingClearRequest {
                        try await send(.clearHistory(pendingClearRequest))
                    }
                } else if let acknowledgement {
                    if lifecycleState == .foregroundConnecting {
                        pendingAcknowledgement = acknowledgement
                    } else {
                        await queueLiveAcknowledgement(acknowledgement)
                    }
                }
            }
        } catch {
            guard !Task.isCancelled,
                  lifecycleState != .inactive,
                  lifecycleState != .foregroundError
            else {
                return
            }
            transitionToError(contentFreeCode(for: error))
        }
    }

    private var currentResumeRequest: ResumeRequestV1 {
        ResumeRequestV1(
            knownHistoryEpoch: historyEpoch,
            knownClearGeneration: clearGeneration,
            afterCursor: lastCursor,
        )
    }

    private func apply(_ message: HubMessageV1) throws -> AckV1? {
        switch message {
        case .serverHello:
            throw ProtocolFailure.protocolSchemaInvalid
        case let .resumeStarted(value):
            try applyResumeStarted(value)
            return nil
        case let .event(value):
            return try applyEvent(value)
        case let .resumeComplete(value):
            return try applyResumeComplete(value)
        case let .clearNotice(value):
            try applyClearNotice(value)
            return nil
        case let .clearAccepted(value):
            if pendingClearRequest?.requestID == value.requestID {
                pendingClearRequest = nil
            }
            return nil
        case let .clearRejected(value):
            if pendingClearRequest?.requestID == value.requestID {
                pendingClearRequest = nil
            }
            errorCode = value.code.rawValue
            return nil
        case let .error(value):
            transitionToError(value.code.rawValue)
            return nil
        case .publishAccepted, .publishRejected:
            throw ProtocolFailure.protocolSchemaInvalid
        }
    }

    private func applyServerHello(_ value: ServerHelloV1) throws {
        guard lifecycleState == .foregroundConnecting else {
            throw ProtocolFailure.sessionContextStale
        }
        sessionHello = value
        selfPeerID = value.selfPeerID
        maximumMessageBytes = value.limits.maximumWebSocketMessageBytes
        maximumPayloadBytes = value.limits.maximumPayloadBytes
    }

    private func applyResumeStarted(_ value: ResumeStartedV1) throws {
        guard lifecycleState == .foregroundConnecting,
              !hasStartedResume,
              let hello = sessionHello,
              hello.historyEpoch == value.historyEpoch,
              hello.clearGeneration == value.clearGeneration,
              value.requestedAfterCursor == currentResumeRequest.afterCursor,
              value.status == expectedResumeStatus(for: value)
        else {
            throw ProtocolFailure.sessionContextStale
        }

        switch value.status {
        case .fresh, .epochChanged, .generationChanged:
            storedHistory.removeAll()
            processedMessageIDs.removeAll()
            lastCursor = nil
            pendingClearRequest = nil
        case .gap:
            if let lostThroughCursor = value.lostThroughCursor {
                storedHistory.removeAll(where: { $0.cursor <= lostThroughCursor })
            }
        case .complete:
            break
        }
        historyEpoch = value.historyEpoch
        clearGeneration = value.clearGeneration
        hasStartedResume = true
        resumeBoundary = value.boundaryCursor
        updateVisibleHistory()
    }

    private func applyEvent(_ value: EventV1) throws -> AckV1? {
        guard value.historyEpoch == historyEpoch else {
            throw ProtocolFailure.sessionContextStale
        }
        guard hasValidExpiry(value) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        guard let clearGeneration else {
            throw ProtocolFailure.sessionContextStale
        }
        if value.clearGeneration < clearGeneration {
            throw ProtocolFailure.clearGenerationStale
        }
        if value.clearGeneration > clearGeneration {
            throw ProtocolFailure.clearGenerationAhead
        }
        if processedMessageIDs.contains(value.messageID) {
            return acknowledgement(for: value.cursor)
        }
        if let lastCursor, value.cursor <= lastCursor {
            throw ProtocolFailure.cursorAhead
        }
        let isUnexpired = value.expiresAtMilliseconds > nowMilliseconds
        switch value.delivery {
        case .resume:
            guard lifecycleState == .foregroundConnecting,
                  hasStartedResume,
                  resumeBoundary.map({ value.cursor <= $0 }) == true
            else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
        case .live:
            guard lifecycleState == .foregroundLive else {
                throw ProtocolFailure.protocolSchemaInvalid
            }
            pruneExpiredHistory()
            if isUnexpired, value.sourcePeerID != selfPeerID {
                try pasteboard.write(value.content)
            }
        }

        processedMessageIDs.insert(value.messageID)
        lastCursor = value.cursor
        if isUnexpired {
            storedHistory.removeAll(where: { $0.messageID == value.messageID })
            storedHistory.append(
                StoredClip(
                    messageID: value.messageID,
                    cursor: value.cursor,
                    acceptedAtMilliseconds: value.acceptedAtMilliseconds,
                    expiresAtMilliseconds: value.expiresAtMilliseconds,
                    content: value.content,
                    isStale: lifecycleState != .foregroundLive,
                ),
            )
            if lifecycleState == .foregroundLive {
                sortAndTrimHistory()
                updateVisibleHistory()
            }
        }
        return acknowledgement(for: value.cursor)
    }

    private func applyResumeComplete(_ value: ResumeCompleteV1) throws -> AckV1? {
        guard lifecycleState == .foregroundConnecting,
              hasStartedResume,
              value.historyEpoch == historyEpoch,
              value.clearGeneration == clearGeneration,
              value.boundaryCursor == resumeBoundary
        else {
            throw ProtocolFailure.sessionContextStale
        }
        if let boundary = value.boundaryCursor, let lastCursor, lastCursor > boundary {
            throw ProtocolFailure.cursorAhead
        }
        storedHistory = storedHistory.map { clip in
            var current = clip
            current.isStale = false
            return current
        }
        sortAndTrimHistory()
        updateVisibleHistory()
        lifecycleState = .foregroundLive
        hasStartedResume = false
        resumeBoundary = nil
        errorCode = nil
        preferences.saveConnectionState(.live)

        guard let lastCursor else {
            return nil
        }
        return acknowledgement(for: lastCursor)
    }

    private func applyClearNotice(_ value: ClearNoticeV1) throws {
        guard let currentGeneration = clearGeneration else {
            throw ProtocolFailure.sessionContextStale
        }
        guard value.clearGeneration >= currentGeneration else {
            throw ProtocolFailure.clearGenerationStale
        }
        if value.clearGeneration == currentGeneration {
            return
        }
        clearGeneration = value.clearGeneration
        lastCursor = value.clearedThroughCursor
        storedHistory.removeAll()
        processedMessageIDs.removeAll()
        pendingAcknowledgement = nil
        pendingClearRequest = nil
        updateVisibleHistory()
    }

    private func acknowledgement(for cursor: UInt64) -> AckV1? {
        guard let historyEpoch, let clearGeneration else {
            return nil
        }
        return AckV1(
            historyEpoch: historyEpoch,
            clearGeneration: clearGeneration,
            cursor: cursor,
        )
    }

    private func expectedResumeStatus(for value: ResumeStartedV1) -> ResumeStatusV1 {
        let request = currentResumeRequest
        if let knownGeneration = request.knownClearGeneration,
           knownGeneration != value.clearGeneration
        {
            return .generationChanged
        }
        if let knownEpoch = request.knownHistoryEpoch,
           knownEpoch != value.historyEpoch
        {
            return .epochChanged
        }
        if request.knownHistoryEpoch == nil,
           request.knownClearGeneration == nil,
           request.afterCursor == nil
        {
            return .fresh
        }
        if let cursor = request.afterCursor,
           let lostThroughCursor = value.lostThroughCursor,
           cursor < lostThroughCursor
        {
            return .gap
        }
        return .complete
    }

    private func hasValidExpiry(_ value: EventV1) -> Bool {
        guard let retentionSeconds = sessionHello?.limits.retentionSeconds else {
            return false
        }
        let (retentionMilliseconds, retentionOverflow) = Int64(retentionSeconds)
            .multipliedReportingOverflow(by: 1000)
        let (expectedExpiry, expiryOverflow) = value.acceptedAtMilliseconds
            .addingReportingOverflow(retentionMilliseconds)
        return !retentionOverflow
            && !expiryOverflow
            && value.expiresAtMilliseconds == expectedExpiry
    }

    private func queueLiveAcknowledgement(_ acknowledgement: AckV1) async {
        pendingAcknowledgement = acknowledgement
        let now = nowMilliseconds
        if lastAcknowledgementAtMilliseconds.map({ now - $0 >= 2000 }) ?? true {
            do {
                try await send(.acknowledge(acknowledgement))
                pendingAcknowledgement = nil
                lastAcknowledgementAtMilliseconds = now
            } catch {
                transitionToError(contentFreeCode(for: error))
            }
            return
        }
        guard acknowledgementTask == nil,
              let lastAcknowledgement = lastAcknowledgementAtMilliseconds
        else {
            return
        }
        let delay = max(0, 2000 - (now - lastAcknowledgement))
        acknowledgementTask = Task {
            do {
                try await Task.sleep(for: .milliseconds(delay))
                guard lifecycleState == .foregroundLive,
                      let pendingAcknowledgement
                else {
                    acknowledgementTask = nil
                    return
                }
                try await send(.acknowledge(pendingAcknowledgement))
                self.pendingAcknowledgement = nil
                lastAcknowledgementAtMilliseconds = nowMilliseconds
                acknowledgementTask = nil
            } catch is CancellationError {
                acknowledgementTask = nil
            } catch {
                acknowledgementTask = nil
                transitionToError(contentFreeCode(for: error))
            }
        }
    }

    private func send(_ message: ClientMessageV1) async throws {
        try await transport.send(codec.encodeClientMessage(message))
    }

    private func transitionToError(_ code: String) {
        acknowledgementTask?.cancel()
        acknowledgementTask = nil
        clearRequestTask?.cancel()
        clearRequestTask = nil
        pendingAcknowledgement = nil
        transport.close()
        lifecycleState = .foregroundError
        errorCode = code
        preferences.saveConnectionState(.error)
    }

    private func pruneExpiredHistory() {
        let now = nowMilliseconds
        storedHistory.removeAll(where: { $0.expiresAtMilliseconds <= now })
        updateVisibleHistory()
    }

    private func updateVisibleHistory() {
        visibleHistory = storedHistory.map(\.presentation)
    }

    private func sortAndTrimHistory() {
        storedHistory.sort { $0.cursor > $1.cursor }
        let historyLimit = sessionHello?.limits.historyMaximumEntries ?? 500
        if storedHistory.count > historyLimit {
            storedHistory.removeLast(storedHistory.count - historyLimit)
        }
    }

    private var nowMilliseconds: Int64 {
        Int64((now().timeIntervalSince1970 * 1000).rounded(.towardZero))
    }

    private func contentFreeCode(for error: Error) -> String {
        (error as? ProtocolFailure)?.rawValue ?? ReasonCodeV1.adapterUnavailable.rawValue
    }
}
