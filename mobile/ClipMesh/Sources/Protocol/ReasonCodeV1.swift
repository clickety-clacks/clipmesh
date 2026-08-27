enum ReasonCodeV1: String, Equatable {
    case ackInvalid = "ack_invalid"
    case adapterUnavailable = "adapter_unavailable"
    case bindFailed = "bind_failed"
    case clearGenerationAhead = "clear_generation_ahead"
    case clearGenerationExhausted = "clear_generation_exhausted"
    case clearGenerationStale = "clear_generation_stale"
    case clientIdentityClaimForbidden = "client_identity_claim_forbidden"
    case configMissingRequired = "config_missing_required"
    case configParseFailed = "config_parse_failed"
    case configUnknownField = "config_unknown_field"
    case configValueInvalid = "config_value_invalid"
    case connectionLimitReached = "connection_limit_reached"
    case contentTypeUnsupported = "content_type_unsupported"
    case createdAtInFuture = "created_at_in_future"
    case cursorAhead = "cursor_ahead"
    case databaseIntegrityFailed = "database_integrity_failed"
    case databaseSchemaUnsupported = "database_schema_unsupported"
    case eventTooOld = "event_too_old"
    case heartbeatTimeout = "heartbeat_timeout"
    case historyCleared = "history_cleared"
    case httpMethodNotAllowed = "http_method_not_allowed"
    case httpPathNotFound = "http_path_not_found"
    case hubCursorExhausted = "hub_cursor_exhausted"
    case localStateUnavailable = "local_state_unavailable"
    case lockStateUnknown = "lock_state_unknown"
    case messageIDConflict = "message_id_conflict"
    case messageIDReplay = "message_id_replay"
    case messageRateLimited = "message_rate_limited"
    case messageTooLarge = "message_too_large"
    case outboxFull = "outbox_full"
    case payloadEmpty = "payload_empty"
    case payloadEncodingInvalid = "payload_encoding_invalid"
    case payloadHashMismatch = "payload_hash_mismatch"
    case payloadLengthMismatch = "payload_length_mismatch"
    case payloadTooLarge = "payload_too_large"
    case protocolSchemaInvalid = "protocol_schema_invalid"
    case protocolVersionUnsupported = "protocol_version_unsupported"
    case publishRateLimited = "publish_rate_limited"
    case requestHeadersTooLarge = "request_headers_too_large"
    case requestIDConflict = "request_id_conflict"
    case requestRateLimited = "request_rate_limited"
    case resumeContextIncomplete = "resume_context_incomplete"
    case resumeCursorWithoutContext = "resume_cursor_without_context"
    case resumeDeadlineExceeded = "resume_deadline_exceeded"
    case resumeRequired = "resume_required"
    case sessionContextStale = "session_context_stale"
    case slowConsumer = "slow_consumer"
    case statePathInsecure = "state_path_insecure"
    case storageUnavailable = "storage_unavailable"
    case tailnetBindUnverified = "tailnet_bind_unverified"
    case tailnetPeerUnverified = "tailnet_peer_unverified"
    case tailscaleLocalAPIUnavailable = "tailscale_localapi_unavailable"

    var retryable: Bool {
        switch self {
        case .adapterUnavailable, .bindFailed, .clearGenerationAhead,
             .connectionLimitReached, .createdAtInFuture, .databaseIntegrityFailed,
             .heartbeatTimeout, .historyCleared, .localStateUnavailable,
             .lockStateUnknown, .messageRateLimited, .outboxFull,
             .publishRateLimited, .requestRateLimited, .resumeDeadlineExceeded,
             .sessionContextStale, .slowConsumer, .storageUnavailable,
             .tailnetPeerUnverified, .tailscaleLocalAPIUnavailable:
            true
        default:
            false
        }
    }
}
