import Foundation
import Observation
import UIKit
import UniformTypeIdentifiers

@MainActor
@Observable
final class FileHistoryModel {
    private(set) var clips: [MeshFileClip] = []
    private(set) var error: String?
    private(set) var feedback: String?
    private(set) var downloading: Set<UUID> = []
    private(set) var localFiles: [UUID: [URL]] = [:]
    @ObservationIgnored private var clients: [UUID: FileTransferClient] = [:]
    @ObservationIgnored private var revision = UUID()
    @ObservationIgnored private var cachedBytes = 0
    @ObservationIgnored private var cacheDirectories: [UUID: URL] = [:]
    @ObservationIgnored private var cacheSizes: [UUID: Int] = [:]
    @ObservationIgnored private var locallyHidden: Set<UUID> = []

    func copy(_ clip: MeshFileClip, to pasteboard: UIPasteboard = .general) {
        guard let urls = localFiles[clip.id], urls.count == clip.manifest.files.count else { return }
        do {
            let providers = try zip(urls, clip.manifest.files).map { url, descriptor in
                let data = try Data(contentsOf: url)
                guard data.count == descriptor.size_bytes, FileTransferClient.hash(data) == descriptor.sha256 else {
                    throw FileTransferFailure.integrity
                }
                let type = UTType(mimeType: descriptor.media_type) ?? UTType(filenameExtension: url.pathExtension) ?? .data
                let provider = NSItemProvider()
                provider.suggestedName = descriptor.name
                // Own the bytes, not a cache URL. Clearing ClipMesh history
                // must not invalidate something the user explicitly copied.
                provider.registerDataRepresentation(forTypeIdentifier: type.identifier, visibility: .all) { completion in
                    completion(data, nil)
                    return nil
                }
                return provider
            }
            pasteboard.setItemProviders(providers, localOnly: true, expirationDate: nil)
            feedback = "Copied to device clipboard"
        } catch { self.error = "Could not copy files" }
    }

    func clear() {
        stop()
        locallyHidden.formUnion(clips.map(\.id))
        clips = []
        feedback = nil
        error = nil
        for id in Array(cacheDirectories.keys) { removeCachedFiles(id) }
    }

    private func removeCachedFiles(_ id: UUID) {
        guard let directory = cacheDirectories[id] else { return }
        do {
            // Only directories created and tracked by this cache are targets.
            if FileManager.default.fileExists(atPath: directory.path) {
                try FileManager.default.removeItem(at: directory)
            }
            cachedBytes -= cacheSizes.removeValue(forKey: id) ?? 0
            cacheDirectories[id] = nil
            localFiles[id] = nil
        } catch { self.error = "Could not remove cached files" }
    }

    func stop() {
        revision = UUID()
        clients.values.forEach { $0.close() }
        clients.removeAll()
        downloading.removeAll()
    }

    func refresh(endpoint: String) async {
        let current = revision
        let id = UUID()
        let client = FileTransferClient()
        clients[id] = client
        defer { client.close(); clients[id] = nil }
        do {
            client.open(try HubEndpoint(endpoint))
            let received = try await client.history()
            guard current == revision, !Task.isCancelled else { return }
            apply(received)
        } catch {
            guard current == revision, !Task.isCancelled else { return }
            self.error = "File history unavailable"
        }
    }

    private func apply(_ received: [MeshFileClip]) {
        let retained = Set(received.map(\.id))
        locallyHidden.formIntersection(retained)
        clips = received.filter { !locallyHidden.contains($0.id) }
        error = nil
        for id in Array(cacheDirectories.keys) where !retained.contains(id) { removeCachedFiles(id) }
    }

    func observe(endpoint: String) async {
        let current = revision
        while current == revision, !Task.isCancelled {
            let id = UUID()
            let client = FileTransferClient()
            clients[id] = client
            do {
                client.open(try HubEndpoint(endpoint))
                while current == revision, !Task.isCancelled {
                    let received = try await client.history()
                    guard current == revision, !Task.isCancelled else { break }
                    apply(received)
                    // Media previews do not touch the system clipboard.
                    // Cache capacity also bounds automatic downloads.
                    for clip in clips where clip.manifest.files.contains(where: {
                        $0.media_type.hasPrefix("image/") || $0.media_type.hasPrefix("video/")
                    }) {
                        guard current == revision, !Task.isCancelled else { break }
                        let bytes = clip.manifest.files.reduce(UInt64(0)) { $0 + min($1.size_bytes, 500 * 1024 * 1024) }
                        if bytes + UInt64(cachedBytes) <= 500 * 1024 * 1024 {
                            await download(clip, endpoint: endpoint, using: client)
                        }
                    }
                    try await Task.sleep(for: .seconds(3))
                }
            } catch {
                if current == revision, !Task.isCancelled { self.error = "File history unavailable" }
            }
            client.close()
            clients[id] = nil
            guard current == revision, !Task.isCancelled else { return }
            do { try await Task.sleep(for: .seconds(5)) } catch { return }
        }
    }

    func download(_ clip: MeshFileClip, endpoint: String, using sharedClient: FileTransferClient? = nil) async {
        guard localFiles[clip.id] == nil, downloading.isEmpty else { return }
        let current = revision
        downloading.insert(clip.id)
        defer { if current == revision { downloading.remove(clip.id) } }
        // Preview downloads run serially between history requests, so they
        // can share that connection instead of consuming another peer slot.
        let client = sharedClient ?? FileTransferClient()
        let operationID = UUID()
        if sharedClient == nil { clients[operationID] = client }
        defer {
            if sharedClient == nil { client.close(); clients[operationID] = nil }
        }
        var folder: URL?
        do {
            guard !clip.manifest.files.isEmpty, clip.manifest.files.count <= 32 else { throw FileTransferFailure.limit }
            let total = clip.manifest.files.reduce(UInt64(0)) { $0 + min($1.size_bytes, UInt64(FileTransferClient.maximumFileBytes + 1)) }
            guard total <= 500 * 1024 * 1024, UInt64(cachedBytes) + total <= 500 * 1024 * 1024 else {
                throw FileTransferFailure.limit
            }
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("clipmesh-" + UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                attributes: [.protectionKey: FileProtectionType.complete])
            folder = directory
            if sharedClient == nil { client.open(try HubEndpoint(endpoint)) }
            var urls: [URL] = []
            var names: Set<String> = []
            for (index, file) in clip.manifest.files.enumerated() {
                guard !file.name.isEmpty, file.name != ".", file.name != "..",
                      !file.name.contains("/"), !file.name.contains("\\"), !file.name.contains(":"),
                      !file.name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
                      names.insert(file.name.lowercased()).inserted else { throw FileTransferFailure.invalidReply }
                let data = try await client.download(clip, index: index)
                guard current == revision, !Task.isCancelled else { throw CancellationError() }
                let url = directory.appendingPathComponent(file.name, isDirectory: false)
                try data.write(to: url, options: [.withoutOverwriting, .completeFileProtection])
                urls.append(url)
            }
            localFiles[clip.id] = urls
            cacheDirectories[clip.id] = directory
            cacheSizes[clip.id] = Int(total)
            cachedBytes += Int(total)
            folder = nil
            error = nil
        } catch {
            if current == revision, !Task.isCancelled { self.error = "Could not download files" }
        }
        // Only remove this operation's newly created private staging directory.
        if let folder { try? FileManager.default.removeItem(at: folder) }
    }
}
