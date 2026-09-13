import CryptoKit
import Foundation

struct MeshFileDescriptor: Codable, Equatable {
    let name: String
    let media_type: String
    let size_bytes: UInt64
    let sha256: String
}

struct MeshFileManifest: Codable, Equatable {
    let files: [MeshFileDescriptor]

    func validate() throws {
        guard !files.isEmpty, files.count <= 32 else { throw FileTransferFailure.limit }
        var names: Set<String> = []
        var total: UInt64 = 0
        let mimeCharacters = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$&^_.+-".utf8)
        for file in files {
            guard !file.name.isEmpty, file.name.utf8.count <= 255,
                  file.name != ".", file.name != "..",
                  !file.name.hasSuffix("."), !file.name.hasSuffix(" "),
                  !file.name.contains(where: { "/\\:".contains($0) }),
                  !file.name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
                  names.insert(file.name.lowercased()).inserted,
                  file.sha256.utf8.count == 64,
                  file.sha256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw FileTransferFailure.invalidReply
            }
            let parts = file.media_type.split(separator: "/", omittingEmptySubsequences: false)
            guard parts.count == 2, parts.allSatisfy({
                !$0.isEmpty && $0.utf8.count <= 127 && $0.utf8.allSatisfy { mimeCharacters.contains($0) }
            }) else { throw FileTransferFailure.invalidReply }
            guard file.size_bytes <= 100 * 1024 * 1024 else { throw FileTransferFailure.limit }
            total += file.size_bytes
        }
        guard total <= 500 * 1024 * 1024 else { throw FileTransferFailure.limit }
    }
}

struct MeshFileClip: Codable, Identifiable, Equatable {
    let clip_id: UUID
    let accepted_at: Int64
    let manifest: MeshFileManifest
    var id: UUID { clip_id }
}

struct MeshMachine: Equatable {
    let peerID: String
    let name: String
}

struct MeshMachineDirectory: Equatable {
    let namesByPeerID: [String: String]
    let fileSourcePeerIDs: [UUID: String]
}

private let maximumSourceMetadataPeers = 1_024
private let maximumSourceMetadataFileSources = 500

enum MetadataHTTPFailure: Error {
    case status(Int)
    case invalidReply
    case responseTooLarge
}

private final class MetadataRedirectBlocker: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void,
    ) {
        completionHandler(nil)
    }
}

enum FileTransferFailure: Error {
    case disconnected, invalidReply, rejected, busy, integrity, limit
}

/// One foreground operation owns a connection. Closing it cancels pending work.
/// This client never reads or writes the system clipboard.
@MainActor
final class FileTransferClient {
    static let chunkBytes = 256 * 1024
    static let maximumFileBytes = 100 * 1024 * 1024
    private var session: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var busy = false

    func open(_ endpoint: HubEndpoint) {
        close()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.timeoutIntervalForResource = 120
        let session = URLSession(configuration: configuration)
        let socket = session.webSocketTask(with: endpoint.url, protocols: ["clipmesh.files.v1"])
        // History includes metadata only and is currently capped at 500 clips.
        socket.maximumMessageSize = 16 * 1024 * 1024
        self.session = session
        self.socket = socket
        socket.resume()
    }

    func close() {
        socket?.cancel(with: .goingAway, reason: nil)
        session?.invalidateAndCancel()
        socket = nil
        session = nil
    }

    func history() async throws -> [MeshFileClip] {
        guard !busy else { throw FileTransferFailure.busy }
        busy = true
        defer { busy = false }
        let reply = try await exchange(["type": "history"])
        guard reply["type"] as? String == "history", let clips = reply["clips"] else {
            throw FileTransferFailure.invalidReply
        }
        let decoded = try JSONDecoder().decode([MeshFileClip].self, from: JSONSerialization.data(withJSONObject: clips))
        guard decoded.count <= 500, Set(decoded.map(\.id)).count == decoded.count else {
            throw FileTransferFailure.invalidReply
        }
        for clip in decoded { try clip.manifest.validate() }
        return decoded
    }

    /// Ask the additive HTTP metadata endpoint for display names and file
    /// provenance. The deployed file WebSocket contract remains unchanged.
    static func sourceMetadata(endpoint: HubEndpoint) async throws -> MeshMachineDirectory {
        let object = try await metadataObject(endpoint: endpoint, path: "/v1/source-metadata")
        return try decodeSourceMetadata(object)
    }

    static func decodeSourceMetadata(_ object: [String: Any]) throws -> MeshMachineDirectory {
        guard object["protocol_version"] as? Int == 1,
              object["type"] as? String == "source_metadata",
              let rawMachines = object["peers"] as? [[String: Any]]
        else { throw MetadataHTTPFailure.invalidReply }
        guard rawMachines.count <= maximumSourceMetadataPeers else {
            throw MetadataHTTPFailure.invalidReply
        }
        var result: [MeshMachine] = []
        var seen: Set<String> = []
        for raw in rawMachines {
            let peerID = raw["id"] as? String
            let name = raw["display_name"] as? String
            guard raw.keys.count == 2,
                  raw.keys.contains("id"), raw.keys.contains("display_name"),
                  let peerID, !peerID.isEmpty, peerID.utf8.count <= 512,
                  !peerID.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
                  let name, !name.isEmpty, name.utf8.count <= 512,
                  !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
                  seen.insert(peerID).inserted else {
                throw MetadataHTTPFailure.invalidReply
            }
            result.append(MeshMachine(peerID: peerID, name: name))
        }
        var fileSources: [UUID: String] = [:]
        guard let rawFileSources = object["file_sources"] as? [[String: Any]] else {
            throw MetadataHTTPFailure.invalidReply
        }
        guard rawFileSources.count <= maximumSourceMetadataFileSources else {
            throw MetadataHTTPFailure.invalidReply
        }
        for raw in rawFileSources {
            guard let clipText = raw["clip_id"] as? String,
                  let clipID = UUID(uuidString: clipText),
                  clipID.uuidString.lowercased() == clipText,
                  let peerID = raw["source_peer_id"] as? String,
                  !peerID.isEmpty, peerID.utf8.count <= 512,
                  !peerID.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
                  raw.keys.count == 2,
                  raw.keys.contains("clip_id"), raw.keys.contains("source_peer_id"),
                  fileSources[clipID] == nil else { throw MetadataHTTPFailure.invalidReply }
            fileSources[clipID] = peerID
        }
        return MeshMachineDirectory(
            namesByPeerID: Dictionary(uniqueKeysWithValues: result.map { ($0.peerID, $0.name) }),
            fileSourcePeerIDs: fileSources,
        )
    }

    private static func metadataObject(endpoint: HubEndpoint, path: String) async throws -> [String: Any] {
        var request = URLRequest(url: endpoint.httpURL(path: path), cachePolicy: .reloadIgnoringLocalCacheData)
        request.httpMethod = "GET"
        request.timeoutInterval = 10
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForResource = 10
        let session = URLSession(configuration: configuration, delegate: MetadataRedirectBlocker(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw MetadataHTTPFailure.invalidReply }
        guard http.statusCode == 200 else { throw MetadataHTTPFailure.status(http.statusCode) }
        var data = Data()
        data.reserveCapacity(4096)
        for try await byte in bytes {
            guard data.count < 1024 * 1024 else { throw MetadataHTTPFailure.responseTooLarge }
            data.append(byte)
        }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw MetadataHTTPFailure.invalidReply
        }
        guard object["protocol_version"] as? Int == 1 else { throw MetadataHTTPFailure.invalidReply }
        return object
    }

    func send(_ files: [(MeshFileDescriptor, Data)], clipID: UUID = UUID()) async throws {
        guard !busy else { throw FileTransferFailure.busy }
        try MeshFileManifest(files: files.map(\.0)).validate()
        guard !files.isEmpty, files.count <= 32,
              files.reduce(0, { $0 + $1.1.count }) <= 500 * 1024 * 1024 else {
            throw FileTransferFailure.limit
        }
        busy = true
        defer { busy = false }
        var uploads: [String] = []
        for (descriptor, data) in files {
            guard data.count <= Self.maximumFileBytes,
                  descriptor.size_bytes == data.count,
                  Self.hash(data) == descriptor.sha256 else { throw FileTransferFailure.integrity }
            let metadata = try JSONSerialization.jsonObject(with: JSONEncoder().encode(descriptor))
            let begin = try await exchange(["type": "begin", "file": metadata])
            guard begin["type"] as? String == "ready", let upload = begin["upload_id"] as? String,
                  UUID(uuidString: upload) != nil else { throw FileTransferFailure.invalidReply }
            var offset = 0
            while offset < data.count {
                try Task.checkCancellation()
                let end = min(offset + Self.chunkBytes, data.count)
                let chunk = data.subdata(in: offset..<end)
                let reply = try await exchange(["type": "chunk", "upload_id": upload,
                    "offset": offset, "payload_b64": Self.encode(chunk)])
                guard reply["type"] as? String == "ready",
                      reply["upload_id"] as? String == upload,
                      reply["offset"] as? Int == end else { throw FileTransferFailure.invalidReply }
                offset = end
            }
            let finish = try await exchange(["type": "finish", "upload_id": upload])
            guard finish["type"] as? String == "complete",
                  finish["upload_id"] as? String == upload else { throw FileTransferFailure.invalidReply }
            uploads.append(upload)
        }
        let manifest = MeshFileManifest(files: files.map(\.0))
        let metadata = try JSONSerialization.jsonObject(with: JSONEncoder().encode(manifest))
        let clip = clipID.uuidString.lowercased()
        let reply = try await exchange(["type": "publish", "clip_id": clip,
            "manifest": metadata, "uploads": uploads])
        guard reply["type"] as? String == "published", reply["clip_id"] as? String == clip else {
            throw FileTransferFailure.invalidReply
        }
    }

    func download(_ clip: MeshFileClip, index: Int) async throws -> Data {
        guard !busy else { throw FileTransferFailure.busy }
        try clip.manifest.validate()
        guard clip.manifest.files.indices.contains(index) else { throw FileTransferFailure.invalidReply }
        let descriptor = clip.manifest.files[index]
        guard descriptor.size_bytes <= Self.maximumFileBytes else { throw FileTransferFailure.limit }
        busy = true
        defer { busy = false }
        var data = Data()
        while true {
            try Task.checkCancellation()
            let reply = try await exchange(["type": "download", "clip_id": clip.id.uuidString.lowercased(),
                "file_index": index, "offset": data.count])
            guard reply["type"] as? String == "data", reply["offset"] as? Int == data.count,
                  let encoded = reply["payload_b64"] as? String, let chunk = Self.decode(encoded),
                  chunk.count <= Self.chunkBytes, data.count + chunk.count <= descriptor.size_bytes,
                  let complete = reply["complete"] as? Bool else { throw FileTransferFailure.invalidReply }
            data.append(chunk)
            if complete { break }
            guard !chunk.isEmpty else { throw FileTransferFailure.invalidReply }
        }
        guard data.count == descriptor.size_bytes, Self.hash(data) == descriptor.sha256 else {
            throw FileTransferFailure.integrity
        }
        return data
    }

    private func exchange(_ fields: [String: Any]) async throws -> [String: Any] {
        guard let socket else { throw FileTransferFailure.disconnected }
        let id = UUID().uuidString.lowercased()
        var request = fields
        request["request_id"] = id
        let bytes = try JSONSerialization.data(withJSONObject: request)
        while true {
        try Task.checkCancellation()
        try await socket.send(.string(String(decoding: bytes, as: UTF8.self)))
        let message = try await socket.receive()
        guard self.socket === socket,
              let response = socket.response as? HTTPURLResponse,
              response.value(forHTTPHeaderField: "Sec-WebSocket-Protocol") == "clipmesh.files.v1",
              response.value(forHTTPHeaderField: "Sec-WebSocket-Extensions") == nil,
              case let .string(text) = message,
              let reply = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
              reply["request_id"] as? String == id else { throw FileTransferFailure.invalidReply }
        if reply["type"] as? String == "rejected" {
            if reply["code"] as? String == "rate_limited" {
                try await Task.sleep(for: .milliseconds(550))
                continue
            }
            throw FileTransferFailure.rejected
        }
        return reply
        }
    }

    static func hash(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func encode(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }

    private static func decode(_ text: String) -> Data? {
        var padded = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        padded += String(repeating: "=", count: (4 - padded.count % 4) % 4)
        guard let data = Data(base64Encoded: padded), encode(data) == text else { return nil }
        return data
    }
}
