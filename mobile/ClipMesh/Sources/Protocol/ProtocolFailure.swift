import Foundation

enum ProtocolFailure: String, Error, Equatable, LocalizedError {
    case ackInvalid = "ack_invalid"
    case clearGenerationAhead = "clear_generation_ahead"
    case clearGenerationStale = "clear_generation_stale"
    case contentTypeUnsupported = "content_type_unsupported"
    case cursorAhead = "cursor_ahead"
    case messageTooLarge = "message_too_large"
    case payloadEmpty = "payload_empty"
    case payloadEncodingInvalid = "payload_encoding_invalid"
    case payloadHashMismatch = "payload_hash_mismatch"
    case payloadLengthMismatch = "payload_length_mismatch"
    case payloadTooLarge = "payload_too_large"
    case protocolSchemaInvalid = "protocol_schema_invalid"
    case protocolVersionUnsupported = "protocol_version_unsupported"
    case sessionContextStale = "session_context_stale"

    var errorDescription: String? {
        rawValue
    }
}
