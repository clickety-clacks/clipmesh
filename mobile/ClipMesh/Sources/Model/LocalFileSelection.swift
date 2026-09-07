import Foundation
import QuickLookThumbnailing
import UIKit
import UniformTypeIdentifiers

struct LocalFileSelection: Identifiable {
    let id = UUID()
    let descriptor: MeshFileDescriptor
    let data: Data
    let thumbnail: UIImage?

    static func clipboardFiles(_ providers: [NSItemProvider]) async throws -> [Self] {
        let candidates = providers.compactMap { provider -> (NSItemProvider, String)? in
            let types = provider.registeredTypeIdentifiers
            // Prefer the actual media representation over an accompanying URL
            // or text caption. Generic binary files remain supported.
            let preferred = types.contains(UTType.fileURL.identifier) ? UTType.fileURL.identifier : types.first { identifier in
                guard let type = UTType(identifier) else { return false }
                return type.conforms(to: .image) || type.conforms(to: .movie)
            } ?? types.first { identifier in
                guard let type = UTType(identifier) else { return false }
                return type.conforms(to: .data) && !type.conforms(to: .text) && !type.conforms(to: .url)
            }
            return preferred.map { (provider, $0) }
        }
        guard candidates.count <= 32 else { throw FileTransferFailure.limit }
        var files: [Self] = []
        var total: UInt64 = 0
        for (provider, identifier) in candidates {
            if identifier == UTType.fileURL.identifier {
                let url: URL = try await withCheckedThrowingContinuation { continuation in
                    provider.loadItem(forTypeIdentifier: identifier, options: nil) { value, error in
                        let url = (value as? URL) ?? (value as? Data).flatMap { URL(dataRepresentation: $0, relativeTo: nil) }
                        if let url, url.isFileURL { continuation.resume(returning: url) }
                        else { continuation.resume(throwing: error ?? FileTransferFailure.invalidReply) }
                    }
                }
                let loaded = try await read([url])
                total += loaded[0].descriptor.size_bytes
                guard total <= 500 * 1024 * 1024 else { throw FileTransferFailure.limit }
                files.append(contentsOf: loaded)
                continue
            }
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent("clipmesh-import-" + UUID().uuidString)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false,
                attributes: [.protectionKey: FileProtectionType.complete])
            defer { try? FileManager.default.removeItem(at: folder) }
            let suggested = provider.suggestedName ?? "Clipboard"
            let name = URL(fileURLWithPath: suggested).lastPathComponent
            let suffix = UTType(identifier)?.preferredFilenameExtension ?? "bin"
            let safeName = name.isEmpty || name == "." || name == ".." ? "Clipboard" : name
            let filename = URL(fileURLWithPath: safeName).pathExtension.isEmpty ? safeName + "." + suffix : safeName
            let destination = folder.appendingPathComponent(filename)
            let url: URL = try await withCheckedThrowingContinuation { continuation in
                provider.loadFileRepresentation(forTypeIdentifier: identifier) { source, error in
                    do {
                        guard let source else { throw error ?? FileTransferFailure.invalidReply }
                        let values = try source.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
                        guard values.isRegularFile == true, let size = values.fileSize, size <= 100 * 1024 * 1024 else {
                            throw FileTransferFailure.limit
                        }
                        // The provider deletes its temporary file when this
                        // callback returns, so copy before resuming the task.
                        try FileManager.default.copyItem(at: source, to: destination)
                        continuation.resume(returning: destination)
                    } catch { continuation.resume(throwing: error) }
                }
            }
            let loaded = try await read([url])
            total += loaded[0].descriptor.size_bytes
            guard total <= 500 * 1024 * 1024 else { throw FileTransferFailure.limit }
            files.append(contentsOf: loaded)
        }
        return files
    }

    static func read(_ urls: [URL]) async throws -> [Self] {
        guard !urls.isEmpty, urls.count <= 32 else { throw FileTransferFailure.limit }
        var selection: [Self] = []
        var total = 0
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey, .contentTypeKey])
            guard values.isRegularFile == true, let size = values.fileSize,
                  size <= FileTransferClient.maximumFileBytes else { throw FileTransferFailure.limit }
            total += size
            guard total <= 500 * 1024 * 1024 else { throw FileTransferFailure.limit }
            let data = try Data(contentsOf: url, options: .mappedIfSafe)
            guard data.count == size else { throw FileTransferFailure.integrity }
            let type = values.contentType ?? .data
            var thumbnail: UIImage?
            if type.conforms(to: .image) || type.conforms(to: .movie) {
                let request = QLThumbnailGenerator.Request(fileAt: url,
                    size: CGSize(width: 320, height: 220), scale: 2, representationTypes: .thumbnail)
                thumbnail = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request).uiImage
            }
            selection.append(Self(descriptor: MeshFileDescriptor(name: url.lastPathComponent,
                media_type: type.preferredMIMEType ?? "application/octet-stream",
                size_bytes: UInt64(size), sha256: FileTransferClient.hash(data)), data: data, thumbnail: thumbnail))
        }
        return selection
    }
}
