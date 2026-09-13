import Foundation
import Observation
import UIKit
import UniformTypeIdentifiers

@MainActor
@Observable
final class FileHistoryModel {
    typealias SourceMetadataLoader = @MainActor (HubEndpoint) async throws -> MeshMachineDirectory

    private(set) var clips: [MeshFileClip] = []
    private(set) var error: String?
    private(set) var feedback: String?
    private(set) var downloading: Set<UUID> = []
    private(set) var localFiles: [UUID: [URL]] = [:]
    private(set) var machineNames: [String: String] = [:]
    private(set) var fileSourcePeerIDs: [UUID: String] = [:]
    @ObservationIgnored private var clients: [UUID: FileTransferClient] = [:]
    @ObservationIgnored private var revision = UUID()
    @ObservationIgnored private var cachedBytes = 0
    @ObservationIgnored private var cacheDirectories: [UUID: URL] = [:]
    @ObservationIgnored private var cacheSizes: [UUID: Int] = [:]
    @ObservationIgnored private var locallyHidden: Set<UUID> = []
    @ObservationIgnored private var sourceMetadataUpdatedAt: Date?
    @ObservationIgnored private var sourceMetadataRetryUntil: Date?
    @ObservationIgnored private var sourceMetadataRefreshTask: Task<Void, Never>?
    @ObservationIgnored private var sourceMetadataRefreshTaskID: UUID?
    @ObservationIgnored private var sourceMetadataScheduledEndpoint: String?
    @ObservationIgnored private var sourceMetadataScheduledTextPeerIDs: Set<String> = []
    @ObservationIgnored private var sourceMetadataScheduledDeadline: Date?
    @ObservationIgnored private var sourceMetadataFileIDs: Set<UUID> = []
    @ObservationIgnored private var sourceMetadataTextPeerIDs: Set<String> = []
    @ObservationIgnored private var sourceMetadataEndpoint: String?
    @ObservationIgnored private var sourceMetadataTask: Task<MeshMachineDirectory, Error>?
    @ObservationIgnored private var sourceMetadataTaskID: UUID?
    @ObservationIgnored private var sourceMetadataTaskFileIDs: Set<UUID> = []
    @ObservationIgnored private var sourceMetadataTaskTextPeerIDs: Set<String> = []
    @ObservationIgnored private let sourceMetadataLoader: SourceMetadataLoader
    @ObservationIgnored private let sourceMetadataFreshnessInterval: TimeInterval
    @ObservationIgnored private let sourceMetadataMinimumInterval: TimeInterval

    init(
        sourceMetadataLoader: @escaping SourceMetadataLoader = { endpoint in
            try await FileTransferClient.sourceMetadata(endpoint: endpoint)
        },
        sourceMetadataFreshnessInterval: TimeInterval = 30,
        sourceMetadataMinimumInterval: TimeInterval = 10,
    ) {
        self.sourceMetadataLoader = sourceMetadataLoader
        self.sourceMetadataFreshnessInterval = max(0.05, sourceMetadataFreshnessInterval)
        self.sourceMetadataMinimumInterval = max(0.05, min(sourceMetadataMinimumInterval, sourceMetadataFreshnessInterval))
    }

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
                // A data-only provider is not consistently offered as an
                // image by every iOS consumer. Register the decoded object as
                // well, while retaining the exact bytes for file-aware apps.
                if type.conforms(to: .image), let image = UIImage(data: data) {
                    provider.registerObject(image, visibility: .all)
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
        sourceMetadataTask?.cancel()
        sourceMetadataTask = nil
        sourceMetadataTaskID = nil
        sourceMetadataTaskFileIDs = []
        sourceMetadataTaskTextPeerIDs = []
        cancelSourceMetadataRefresh()
        sourceMetadataUpdatedAt = nil
        sourceMetadataRetryUntil = nil
        sourceMetadataFileIDs = []
        sourceMetadataTextPeerIDs = []
        sourceMetadataEndpoint = nil
        machineNames = [:]
        fileSourcePeerIDs = [:]
    }

    func refresh(endpoint: String) async {
        let current = revision
        let id = UUID()
        let client = FileTransferClient()
        clients[id] = client
        defer { client.close(); clients[id] = nil }
        do {
            let hubEndpoint = try HubEndpoint(endpoint)
            client.open(hubEndpoint)
            let received = try await client.history()
            guard current == revision, !Task.isCancelled else { return }
            apply(received)
            await loadSourceMetadata(endpoint: hubEndpoint, textPeerIDs: sourceMetadataTextPeerIDs)
        } catch {
            guard current == revision, !Task.isCancelled else { return }
            self.error = "File history unavailable"
        }
    }

    private func apply(_ received: [MeshFileClip]) {
        let retained = Set(received.map(\.id))
        locallyHidden.formIntersection(retained)
        fileSourcePeerIDs = fileSourcePeerIDs.filter { retained.contains($0.key) }
        clips = received.filter { !locallyHidden.contains($0.id) }
        error = nil
        for id in Array(cacheDirectories.keys) where !retained.contains(id) { removeCachedFiles(id) }
    }

    func observe(endpoint: String) async {
        let current = revision
        guard let hubEndpoint = try? HubEndpoint(endpoint) else { return }
        while current == revision, !Task.isCancelled {
            let id = UUID()
            let client = FileTransferClient()
            clients[id] = client
            do {
                client.open(hubEndpoint)
                while current == revision, !Task.isCancelled {
                    let received = try await client.history()
                    guard current == revision, !Task.isCancelled else { break }
                    apply(received)
                    await loadSourceMetadata(endpoint: hubEndpoint, textPeerIDs: sourceMetadataTextPeerIDs)
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

    func refreshSourceMetadata(endpoint: String, textPeerIDs: Set<String> = []) async {
        guard let endpoint = try? HubEndpoint(endpoint) else { return }
        await loadSourceMetadata(endpoint: endpoint, textPeerIDs: textPeerIDs)
    }

    private func cancelSourceMetadataRefresh() {
        sourceMetadataRefreshTask?.cancel()
        sourceMetadataRefreshTask = nil
        sourceMetadataRefreshTaskID = nil
        sourceMetadataScheduledEndpoint = nil
        sourceMetadataScheduledTextPeerIDs = []
        sourceMetadataScheduledDeadline = nil
    }

    private func scheduleSourceMetadataRefresh(
        endpoint: HubEndpoint,
        textPeerIDs: Set<String>,
        after delay: TimeInterval,
    ) {
        let now = Date()
        let deadline = now.addingTimeInterval(max(0.05, delay))
        var scheduledTextPeerIDs = textPeerIDs
        if sourceMetadataRefreshTaskID != nil,
           sourceMetadataScheduledEndpoint == endpoint.displayValue,
           let existingDeadline = sourceMetadataScheduledDeadline {
            sourceMetadataScheduledTextPeerIDs.formUnion(textPeerIDs)
            scheduledTextPeerIDs.formUnion(sourceMetadataScheduledTextPeerIDs)
            guard existingDeadline > deadline else { return }
            sourceMetadataRefreshTask?.cancel()
            sourceMetadataRefreshTask = nil
            sourceMetadataRefreshTaskID = nil
        } else if sourceMetadataRefreshTask != nil {
            cancelSourceMetadataRefresh()
        }

        let scheduleID = UUID()
        sourceMetadataRefreshTaskID = scheduleID
        sourceMetadataScheduledEndpoint = endpoint.displayValue
        sourceMetadataScheduledTextPeerIDs = scheduledTextPeerIDs
        sourceMetadataScheduledDeadline = deadline
        sourceMetadataRefreshTask = Task { [weak self] in
            do {
                try await Task.sleep(for: .seconds(max(0.05, delay) + 0.05))
            } catch {
                return
            }
            guard !Task.isCancelled, let self else { return }
            await self.runScheduledSourceMetadataRefresh(scheduleID: scheduleID)
        }
    }

    private func runScheduledSourceMetadataRefresh(scheduleID: UUID) async {
        guard sourceMetadataRefreshTaskID == scheduleID,
              let endpointValue = sourceMetadataScheduledEndpoint,
              let endpoint = try? HubEndpoint(endpointValue) else { return }
        let textPeerIDs = sourceMetadataScheduledTextPeerIDs
        sourceMetadataRefreshTask = nil
        sourceMetadataRefreshTaskID = nil
        sourceMetadataScheduledEndpoint = nil
        sourceMetadataScheduledTextPeerIDs = []
        sourceMetadataScheduledDeadline = nil
        await loadSourceMetadata(endpoint: endpoint, textPeerIDs: textPeerIDs)
    }

    private func loadSourceMetadata(endpoint: HubEndpoint, textPeerIDs: Set<String>) async {
        let currentRevision = revision
        let endpointValue = endpoint.displayValue
        if sourceMetadataEndpoint != endpoint.displayValue {
            sourceMetadataTask?.cancel()
            sourceMetadataTask = nil
            sourceMetadataTaskID = nil
            sourceMetadataTaskFileIDs = []
            sourceMetadataTaskTextPeerIDs = []
            cancelSourceMetadataRefresh()
            sourceMetadataEndpoint = endpoint.displayValue
            sourceMetadataUpdatedAt = nil
            sourceMetadataRetryUntil = nil
            sourceMetadataFileIDs = []
            sourceMetadataTextPeerIDs = []
            machineNames = [:]
            fileSourcePeerIDs = [:]
        }
        let ids = Set(clips.map(\.id))
        if sourceMetadataTask == nil,
           let retryUntil = sourceMetadataRetryUntil,
           retryUntil > Date() {
            scheduleSourceMetadataRefresh(
                endpoint: endpoint,
                textPeerIDs: textPeerIDs,
                after: retryUntil.timeIntervalSinceNow,
            )
            return
        }
        // A history poll can produce a new clip every few seconds. Join an
        // in-flight request so its snapshot can queue one follow-up, but do
        // not start a fresh request for every changed row. A single delayed
        // refresh picks up the current clip/source set after the minimum gap.
        if sourceMetadataTask == nil,
           let updatedAt = sourceMetadataUpdatedAt {
            let needsPromptRefresh = ids != sourceMetadataFileIDs
                || !textPeerIDs.isSubset(of: sourceMetadataTextPeerIDs)
            let interval = needsPromptRefresh
                ? sourceMetadataMinimumInterval
                : sourceMetadataFreshnessInterval
            let elapsed = Date().timeIntervalSince(updatedAt)
            if elapsed < interval {
                scheduleSourceMetadataRefresh(
                    endpoint: endpoint,
                    textPeerIDs: textPeerIDs,
                    after: interval - elapsed,
                )
                return
            }
        }
        let requestID: UUID
        let request: Task<MeshMachineDirectory, Error>
        let requestFileIDs: Set<UUID>
        let requestTextPeerIDs: Set<String>
        var followUpNeeded = false
        if let existing = sourceMetadataTask,
           let existingID = sourceMetadataTaskID,
           sourceMetadataEndpoint == endpointValue {
            request = existing
            requestID = existingID
            requestFileIDs = sourceMetadataTaskFileIDs
            requestTextPeerIDs = sourceMetadataTaskTextPeerIDs
            followUpNeeded = ids != requestFileIDs || !textPeerIDs.isSubset(of: requestTextPeerIDs)
        } else {
            requestID = UUID()
            requestFileIDs = ids
            requestTextPeerIDs = sourceMetadataTextPeerIDs.union(textPeerIDs)
            cancelSourceMetadataRefresh()
            sourceMetadataUpdatedAt = Date()
            sourceMetadataFileIDs = ids
            request = Task { try await sourceMetadataLoader(endpoint) }
            sourceMetadataTask = request
            sourceMetadataTaskID = requestID
            sourceMetadataTaskFileIDs = requestFileIDs
            sourceMetadataTaskTextPeerIDs = requestTextPeerIDs
        }
        let directory = try? await request.value
        if sourceMetadataTaskID == requestID {
            sourceMetadataTask = nil
            sourceMetadataTaskID = nil
            sourceMetadataTaskFileIDs = []
            sourceMetadataTaskTextPeerIDs = []
        }
        guard let directory else {
            guard currentRevision == revision,
                  sourceMetadataEndpoint == endpointValue,
                  !Task.isCancelled else { return }
            // A missing or unsupported endpoint is nonfatal to history. Keep
            // it from being retried for every replayed history response,
            // including requests that ask for newly seen text peers.
            sourceMetadataRetryUntil = Date().addingTimeInterval(30)
            scheduleSourceMetadataRefresh(
                endpoint: endpoint,
                textPeerIDs: textPeerIDs,
                after: 30,
            )
            return
        }
        guard currentRevision == revision,
              sourceMetadataEndpoint == endpointValue,
              !Task.isCancelled else { return }
        machineNames = directory.namesByPeerID
        fileSourcePeerIDs = directory.fileSourcePeerIDs
        sourceMetadataRetryUntil = nil
        sourceMetadataTextPeerIDs.formUnion(requestTextPeerIDs)
        let clipsChanged = Set(clips.map(\.id)) != ids
        if followUpNeeded {
            cancelSourceMetadataRefresh()
            sourceMetadataUpdatedAt = nil
            await loadSourceMetadata(endpoint: endpoint, textPeerIDs: textPeerIDs)
        } else if clipsChanged {
            let elapsed = sourceMetadataUpdatedAt.map { Date().timeIntervalSince($0) } ?? 0
            scheduleSourceMetadataRefresh(
                endpoint: endpoint,
                textPeerIDs: textPeerIDs,
                after: max(0, sourceMetadataMinimumInterval - elapsed),
            )
        } else {
            scheduleSourceMetadataRefresh(
                endpoint: endpoint,
                textPeerIDs: sourceMetadataTextPeerIDs,
                after: sourceMetadataFreshnessInterval,
            )
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
