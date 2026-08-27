import Foundation

struct ProtocolV1Codec {
    static let hardMaximumMessageBytes = 1_402_200
    static let hardMaximumPayloadBytes = 1_048_576

    func decodeHubMessage(
        _ data: Data,
        maximumMessageBytes: Int = Self.hardMaximumMessageBytes,
        maximumPayloadBytes: Int = Self.hardMaximumPayloadBytes,
    ) throws -> HubMessageV1 {
        guard data.count <= maximumMessageBytes else {
            throw ProtocolFailure.messageTooLarge
        }

        let value: JSONValue
        do {
            var parser = try StrictJSONParser(data: data)
            value = try parser.parse()
        } catch let failure as ProtocolFailure {
            throw failure
        } catch {
            throw ProtocolFailure.protocolSchemaInvalid
        }

        let object = try requireObject(value)
        let version = try requireInteger(object["protocol_version"])
        guard version == 1 else {
            throw ProtocolFailure.protocolVersionUnsupported
        }
        let type = try requireString(object["type"])

        switch type {
        case "server_hello":
            return try .serverHello(decodeServerHello(object))
        case "resume_started":
            return try .resumeStarted(decodeResumeStarted(object))
        case "event":
            return try .event(decodeEvent(object, maximumPayloadBytes: maximumPayloadBytes))
        case "resume_complete":
            return try .resumeComplete(decodeResumeComplete(object))
        case "publish_accepted":
            return try .publishAccepted(decodePublishAccepted(object))
        case "publish_rejected":
            return try .publishRejected(decodePublishRejected(object))
        case "clear_accepted":
            return try .clearAccepted(decodeClearAccepted(object))
        case "clear_rejected":
            return try .clearRejected(decodeClearRejected(object))
        case "clear_notice":
            return try .clearNotice(decodeClearNotice(object))
        case "error":
            return try .error(decodeError(object))
        default:
            throw ProtocolFailure.protocolSchemaInvalid
        }
    }

    func encodeClientMessage(_ message: ClientMessageV1) throws -> Data {
        let object: [String: Any] = switch message {
        case let .resume(value):
            [
                "protocol_version": 1,
                "type": "resume",
                "known_history_epoch": value.knownHistoryEpoch?.uuidString.lowercased() ?? NSNull(),
                "known_clear_generation": value.knownClearGeneration.map(String.init) ?? NSNull(),
                "after_cursor": value.afterCursor.map(String.init) ?? NSNull(),
            ]
        case let .acknowledge(value):
            [
                "protocol_version": 1,
                "type": "ack",
                "history_epoch": value.historyEpoch.uuidString.lowercased(),
                "clear_generation": String(value.clearGeneration),
                "cursor": String(value.cursor),
            ]
        case let .clearHistory(value):
            [
                "protocol_version": 1,
                "type": "clear_history",
                "request_id": value.requestID.uuidString.lowercased(),
                "expected_clear_generation": String(value.expectedClearGeneration),
            ]
        }
        guard JSONSerialization.isValidJSONObject(object) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes])
    }

    private func decodeServerHello(_ object: [String: JSONValue]) throws -> ServerHelloV1 {
        try requireKeys(
            object,
            [
                "protocol_version", "type", "session_id", "self_peer_id", "history_epoch",
                "clear_generation", "newest_cursor", "server_time_ms", "limits",
            ],
        )
        let limitsObject = try requireObject(object["limits"])
        try requireKeys(
            limitsObject,
            [
                "max_payload_bytes", "retention_seconds", "history_max_entries",
                "max_clock_skew_ms", "max_websocket_message_bytes",
            ],
        )
        let maximumPayloadBytes = try requirePositiveInt(limitsObject["max_payload_bytes"])
        let retentionSeconds = try requirePositiveInt(limitsObject["retention_seconds"])
        let historyMaximumEntries = try requirePositiveInt(limitsObject["history_max_entries"])
        let maximumClockSkewMilliseconds = try requirePositiveInt(limitsObject["max_clock_skew_ms"])
        let maximumWebSocketMessageBytes = try requirePositiveInt(limitsObject["max_websocket_message_bytes"])
        guard (1 ... Self.hardMaximumPayloadBytes).contains(maximumPayloadBytes),
              (60 ... 31_536_000).contains(retentionSeconds),
              (1 ... 10000).contains(historyMaximumEntries),
              maximumClockSkewMilliseconds == 120_000,
              maximumWebSocketMessageBytes == 4 * ((maximumPayloadBytes + 2) / 3) + 4096
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return try ServerHelloV1(
            sessionID: requireUUID(object["session_id"]),
            selfPeerID: requirePeerID(object["self_peer_id"]),
            historyEpoch: requireUUID(object["history_epoch"]),
            clearGeneration: requireU64Decimal(object["clear_generation"]),
            newestCursor: requireOptionalU64Decimal(object["newest_cursor"]),
            serverTimeMilliseconds: requireInt64(object["server_time_ms"]),
            limits: LimitsV1(
                maximumPayloadBytes: maximumPayloadBytes,
                retentionSeconds: retentionSeconds,
                historyMaximumEntries: historyMaximumEntries,
                maximumClockSkewMilliseconds: maximumClockSkewMilliseconds,
                maximumWebSocketMessageBytes: maximumWebSocketMessageBytes,
            ),
        )
    }

    private func decodeResumeStarted(_ object: [String: JSONValue]) throws -> ResumeStartedV1 {
        try requireKeys(
            object,
            [
                "protocol_version", "type", "history_epoch", "clear_generation", "status",
                "requested_after_cursor", "boundary_cursor", "lost_through_cursor",
            ],
        )
        guard let status = try ResumeStatusV1(rawValue: requireString(object["status"])) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return try ResumeStartedV1(
            historyEpoch: requireUUID(object["history_epoch"]),
            clearGeneration: requireU64Decimal(object["clear_generation"]),
            status: status,
            requestedAfterCursor: requireOptionalU64Decimal(object["requested_after_cursor"]),
            boundaryCursor: requireOptionalU64Decimal(object["boundary_cursor"]),
            lostThroughCursor: requireOptionalU64Decimal(object["lost_through_cursor"]),
        )
    }

    private func decodeEvent(
        _ object: [String: JSONValue],
        maximumPayloadBytes: Int,
    ) throws -> EventV1 {
        try requireKeys(
            object,
            [
                "protocol_version", "type", "history_epoch", "clear_generation", "cursor",
                "delivery", "accepted_at_ms", "expires_at_ms", "source_peer_id", "event",
            ],
        )
        guard let delivery = try DeliveryV1(rawValue: requireString(object["delivery"])) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        let event = try requireObject(object["event"])
        try requireKeys(
            event,
            [
                "message_id", "clear_generation", "created_at_ms", "content_type",
                "payload_bytes", "content_sha256", "payload_b64",
            ],
        )
        let outerGeneration = try requireU64Decimal(object["clear_generation"])
        let eventGeneration = try requireU64Decimal(event["clear_generation"])
        guard outerGeneration == eventGeneration else {
            throw ProtocolFailure.clearGenerationStale
        }
        return try EventV1(
            historyEpoch: requireUUID(object["history_epoch"]),
            clearGeneration: outerGeneration,
            cursor: requireU64Decimal(object["cursor"]),
            delivery: delivery,
            acceptedAtMilliseconds: requireInt64(object["accepted_at_ms"]),
            expiresAtMilliseconds: requireInt64(object["expires_at_ms"]),
            sourcePeerID: requirePeerID(object["source_peer_id"]),
            messageID: requireUUID(event["message_id"]),
            createdAtMilliseconds: requireInt64(event["created_at_ms"]),
            content: ClipContentV1.fromWire(
                contentType: requireString(event["content_type"]),
                payloadBase64URL: requireString(event["payload_b64"]),
                payloadBytes: requirePositiveInt(event["payload_bytes"]),
                contentSHA256: requireString(event["content_sha256"]),
                maximumBytes: maximumPayloadBytes,
            ),
        )
    }

    private func decodeResumeComplete(_ object: [String: JSONValue]) throws -> ResumeCompleteV1 {
        try requireKeys(
            object,
            ["protocol_version", "type", "history_epoch", "clear_generation", "boundary_cursor"],
        )
        return try ResumeCompleteV1(
            historyEpoch: requireUUID(object["history_epoch"]),
            clearGeneration: requireU64Decimal(object["clear_generation"]),
            boundaryCursor: requireOptionalU64Decimal(object["boundary_cursor"]),
        )
    }

    private func decodePublishAccepted(_ object: [String: JSONValue]) throws -> PublishAcceptedV1 {
        try requireKeys(
            object,
            ["protocol_version", "type", "message_id", "cursor", "expires_at_ms", "duplicate"],
        )
        return try PublishAcceptedV1(
            messageID: requireUUID(object["message_id"]),
            cursor: requireU64Decimal(object["cursor"]),
            expiresAtMilliseconds: requireInt64(object["expires_at_ms"]),
            duplicate: requireBool(object["duplicate"]),
        )
    }

    private func decodePublishRejected(_ object: [String: JSONValue]) throws -> PublishRejectedV1 {
        try requireKeys(object, ["protocol_version", "type", "message_id", "code", "retryable"])
        let reason = try requireReason(object["code"], retryable: object["retryable"])
        return try PublishRejectedV1(
            messageID: requireOptionalUUID(object["message_id"]),
            code: reason,
            retryable: reason.retryable,
        )
    }

    private func decodeClearAccepted(_ object: [String: JSONValue]) throws -> ClearAcceptedV1 {
        try requireKeys(
            object,
            [
                "protocol_version", "type", "request_id", "clear_generation",
                "cleared_through_cursor", "duplicate",
            ],
        )
        return try ClearAcceptedV1(
            requestID: requireUUID(object["request_id"]),
            clearGeneration: requireU64Decimal(object["clear_generation"]),
            clearedThroughCursor: requireOptionalU64Decimal(object["cleared_through_cursor"]),
            duplicate: requireBool(object["duplicate"]),
        )
    }

    private func decodeClearRejected(_ object: [String: JSONValue]) throws -> ClearRejectedV1 {
        try requireKeys(object, ["protocol_version", "type", "request_id", "code", "retryable"])
        let reason = try requireReason(object["code"], retryable: object["retryable"])
        return try ClearRejectedV1(
            requestID: requireOptionalUUID(object["request_id"]),
            code: reason,
            retryable: reason.retryable,
        )
    }

    private func decodeClearNotice(_ object: [String: JSONValue]) throws -> ClearNoticeV1 {
        try requireKeys(
            object,
            [
                "protocol_version", "type", "request_id", "clear_generation",
                "cleared_through_cursor",
            ],
        )
        return try ClearNoticeV1(
            requestID: requireUUID(object["request_id"]),
            clearGeneration: requireU64Decimal(object["clear_generation"]),
            clearedThroughCursor: requireOptionalU64Decimal(object["cleared_through_cursor"]),
        )
    }

    private func decodeError(_ object: [String: JSONValue]) throws -> ErrorV1 {
        try requireKeys(object, ["protocol_version", "type", "code", "retryable"])
        let reason = try requireReason(object["code"], retryable: object["retryable"])
        return ErrorV1(code: reason, retryable: reason.retryable)
    }

    private func requireKeys(_ object: [String: JSONValue], _ keys: Set<String>) throws {
        guard Set(object.keys) == keys else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
    }

    private func requireObject(_ value: JSONValue?) throws -> [String: JSONValue] {
        guard case let .object(object) = value else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return object
    }

    private func requireString(_ value: JSONValue?) throws -> String {
        guard case let .string(string) = value else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return string
    }

    private func requireBool(_ value: JSONValue?) throws -> Bool {
        guard case let .bool(boolean) = value else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return boolean
    }

    private func requireInteger(_ value: JSONValue?) throws -> Int64 {
        guard case let .number(raw) = value,
              !raw.contains("."), !raw.contains("e"), !raw.contains("E"),
              let integer = Int64(raw)
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return integer
    }

    private func requireInt64(_ value: JSONValue?) throws -> Int64 {
        try requireInteger(value)
    }

    private func requirePositiveInt(_ value: JSONValue?) throws -> Int {
        let integer = try requireInteger(value)
        guard integer > 0, let result = Int(exactly: integer) else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return result
    }

    private func requireU64Decimal(_ value: JSONValue?) throws -> UInt64 {
        let string = try requireString(value)
        guard !string.isEmpty,
              string.first != "0",
              string.allSatisfy(\.isASCII),
              string.allSatisfy(\.isNumber),
              let result = UInt64(string),
              result > 0
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return result
    }

    private func requireOptionalU64Decimal(_ value: JSONValue?) throws -> UInt64? {
        if case .null = value {
            return nil
        }
        return try requireU64Decimal(value)
    }

    private func requireUUID(_ value: JSONValue?) throws -> UUID {
        let string = try requireString(value)
        guard string.count == 36,
              string == string.lowercased(),
              string[string.index(string.startIndex, offsetBy: 14)] == "4",
              ["8", "9", "a", "b"].contains(string[string.index(string.startIndex, offsetBy: 19)]),
              let uuid = UUID(uuidString: string),
              uuid.uuidString.lowercased() == string
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return uuid
    }

    private func requireOptionalUUID(_ value: JSONValue?) throws -> UUID? {
        if case .null = value {
            return nil
        }
        return try requireUUID(value)
    }

    private func requirePeerID(_ value: JSONValue?) throws -> String {
        let string = try requireString(value)
        guard !string.isEmpty,
              string.count <= 512,
              !string.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return string
    }

    private func requireReason(
        _ value: JSONValue?,
        retryable retryableValue: JSONValue?,
    ) throws -> ReasonCodeV1 {
        guard let reason = try ReasonCodeV1(rawValue: requireString(value)),
              try requireBool(retryableValue) == reason.retryable
        else {
            throw ProtocolFailure.protocolSchemaInvalid
        }
        return reason
    }
}
