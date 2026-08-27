@testable import ClipMesh
import CryptoKit
import Foundation

enum ProtocolFixtures {
    static let epoch = "00000000-0000-4000-8000-000000000010"
    static let session = "00000000-0000-4000-8000-000000000011"
    static let now = Date(timeIntervalSince1970: 1_700_000_001)

    @MainActor
    static func endpoint() throws -> HubEndpoint {
        try HubEndpoint("ws://100.64.0.7:8123/v1/stream")
    }

    static func hello(generation: UInt64 = 1, newestCursor: UInt64? = nil) -> Data {
        let newestCursorValue = newestCursor.map { "\"\($0)\"" } ?? "null"
        return data(
            """
            {"protocol_version":1,"type":"server_hello","session_id":"\(session)","self_peer_id":"peer-reserved-mobile","history_epoch":"\(epoch)","clear_generation":"\(generation)","newest_cursor":\(newestCursorValue),"server_time_ms":1700000000000,"limits":{"max_payload_bytes":262144,"retention_seconds":604800,"history_max_entries":500,"max_clock_skew_ms":120000,"max_websocket_message_bytes":353624}}
            """,
        )
    }

    static func resumeStarted(
        status: String = "fresh",
        generation: UInt64 = 1,
        requestedAfterCursor: UInt64? = nil,
        boundaryCursor: UInt64? = nil,
        lostThroughCursor: UInt64? = nil,
    ) -> Data {
        let requested = requestedAfterCursor.map { "\"\($0)\"" } ?? "null"
        let boundary = boundaryCursor.map { "\"\($0)\"" } ?? "null"
        let lostThrough = lostThroughCursor.map { "\"\($0)\"" } ?? "null"
        return data(
            """
            {"protocol_version":1,"type":"resume_started","history_epoch":"\(epoch)","clear_generation":"\(generation)","status":"\(status)","requested_after_cursor":\(requested),"boundary_cursor":\(boundary),"lost_through_cursor":\(lostThrough)}
            """,
        )
    }

    static func resumeComplete(boundary: UInt64?, generation: UInt64 = 1) -> Data {
        let boundaryValue = boundary.map { "\"\($0)\"" } ?? "null"
        return data(
            """
            {"protocol_version":1,"type":"resume_complete","history_epoch":"\(epoch)","clear_generation":"\(generation)","boundary_cursor":\(boundaryValue)}
            """,
        )
    }

    static func event(
        delivery: String,
        cursor: UInt64,
        messageSuffix: String,
        sourcePeerID: String,
        text: String,
        generation: UInt64 = 1,
        acceptedAtMilliseconds: Int64 = 1_700_000_000_000,
        expiresAtMilliseconds: Int64 = 1_700_604_800_000,
    ) -> Data {
        let payload = Data(text.utf8)
        let base64URL = payload.base64EncodedString()
            .replacing("+", with: "-")
            .replacing("/", with: "_")
            .replacing("=", with: "")
        let hash = SHA256.hash(data: payload).map { byte in
            let value = String(byte, radix: 16)
            return value.count == 1 ? "0" + value : value
        }.joined()
        return data(
            """
            {"protocol_version":1,"type":"event","history_epoch":"\(epoch)","clear_generation":"\(generation)","cursor":"\(cursor)","delivery":"\(delivery)","accepted_at_ms":\(acceptedAtMilliseconds),"expires_at_ms":\(expiresAtMilliseconds),"source_peer_id":"\(sourcePeerID)","event":{"message_id":"00000000-0000-4000-8000-\(messageSuffix)","clear_generation":"\(generation)","created_at_ms":\(acceptedAtMilliseconds),"content_type":"text/plain","payload_bytes":\(payload.count),"content_sha256":"\(hash)","payload_b64":"\(base64URL)"}}
            """,
        )
    }

    static func clearNotice(generation: UInt64, requestSuffix: String) -> Data {
        data(
            """
            {"protocol_version":1,"type":"clear_notice","request_id":"00000000-0000-4000-8000-\(requestSuffix)","clear_generation":"\(generation)","cleared_through_cursor":"2"}
            """,
        )
    }

    static func data(_ value: String) -> Data {
        Data(value.utf8)
    }
}
