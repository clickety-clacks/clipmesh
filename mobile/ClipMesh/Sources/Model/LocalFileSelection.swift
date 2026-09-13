import Foundation
import ImageIO
import QuickLookThumbnailing
import UIKit
import UniformTypeIdentifiers

struct LocalFileSelection: Identifiable {
    private static let maximumClipboardFileBytes = 100 * 1024 * 1024

    let id = UUID()
    let descriptor: MeshFileDescriptor
    let data: Data
    let thumbnail: UIImage?

    static func clipboardFiles(_ providers: [NSItemProvider]) async throws -> [Self] {
        let candidates = providers.compactMap { provider -> (NSItemProvider, String)? in
            let types = provider.registeredTypeIdentifiers
            // Prefer the actual media representation over an accompanying URL
            // or text caption. Generic binary files remain supported.
            let preferred: String?
            if types.contains(UTType.fileURL.identifier) {
                // A file provider may also expose an image thumbnail. Keep
                // the selected file as the source of truth in that case.
                preferred = UTType.fileURL.identifier
            } else {
                preferred = types.first { identifier in
                    guard let type = UTType(identifier) else { return false }
                    return type.conforms(to: .image) || type.conforms(to: .movie)
                } ?? types.first { identifier in
                    // Notes and browsers offer a web archive alongside their
                    // plain-text representation. That is alternate formatting,
                    // not a user-selected file. Explicit file URLs and media
                    // above still take priority over accompanying captions.
                    guard !provider.canLoadObject(ofClass: NSString.self) else { return false }
                    guard let type = UTType(identifier) else { return false }
                    return type.conforms(to: .data) && !type.conforms(to: .text) && !type.conforms(to: .url)
                }
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
            let representation = try await loadClipboardRepresentation(provider, identifier: identifier)
            let suggested = provider.suggestedName ?? "Clipboard"
            let name = URL(fileURLWithPath: suggested).lastPathComponent
            let suffix = representation.type.preferredFilenameExtension ?? "bin"
            let safeName = name.isEmpty || name == "." || name == ".." ? "Clipboard" : name
            let suggestedURL = URL(fileURLWithPath: safeName)
            let suggestedType = suggestedURL.pathExtension.isEmpty
                ? nil
                : UTType(filenameExtension: suggestedURL.pathExtension)
            let filename: String
            if suggestedURL.pathExtension.isEmpty {
                filename = safeName + "." + suffix
            } else if !representation.type.conforms(to: .image)
                        || suggestedType?.conforms(to: representation.type) == true {
                filename = safeName
            } else {
                filename = suggestedURL.deletingPathExtension().lastPathComponent + "." + suffix
            }
            let destination = folder.appendingPathComponent(filename)
            guard representation.data.count <= Self.maximumClipboardFileBytes else { throw FileTransferFailure.limit }
            try representation.data.write(to: destination, options: [.completeFileProtection])
            let loaded = try await read([destination])
            total += loaded[0].descriptor.size_bytes
            guard total <= 500 * 1024 * 1024 else { throw FileTransferFailure.limit }
            files.append(contentsOf: loaded)
        }
        return files
    }

    private static func loadClipboardRepresentation(
        _ provider: NSItemProvider,
        identifier: String,
    ) async throws -> (data: Data, type: UTType) {
        let declaredType = UTType(identifier) ?? .data

        do {
            let data = try await loadFileRepresentation(provider, identifier: identifier)
            return (data, resolvedImageType(data, declared: declaredType))
        } catch let error {
            if case FileTransferFailure.limit = error { throw error }
        }
        // Screenshots and some share extensions expose an image object or
        // data representation without a file representation. Ask for bytes
        // next so the original image format survives when possible.
        do {
            let data = try await loadDataRepresentation(provider, identifier: identifier)
            return (data, resolvedImageType(data, declared: declaredType))
        } catch let error {
            if case FileTransferFailure.limit = error { throw error }
        }
        if declaredType.conforms(to: .image),
           let image = try? await loadImageObject(provider),
           let data = image.pngData() {
            guard data.count <= Self.maximumClipboardFileBytes else { throw FileTransferFailure.limit }
            return (data, .png)
        }
        throw FileTransferFailure.invalidReply
    }

    private static func resolvedImageType(_ data: Data, declared: UTType) -> UTType {
        guard declared == .image,
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let identifier = CGImageSourceGetType(source) as String?,
              let detected = UTType(identifier)
        else { return declared }
        return detected
    }

    private static func loadDataRepresentation(
        _ provider: NSItemProvider,
        identifier: String,
    ) async throws -> Data {
        let maximumBytes = 100 * 1024 * 1024
        return try await withCheckedThrowingContinuation { continuation in
            provider.loadDataRepresentation(forTypeIdentifier: identifier) { data, error in
                if let data {
                    if data.count <= maximumBytes {
                        continuation.resume(returning: data)
                    } else {
                        continuation.resume(throwing: FileTransferFailure.limit)
                    }
                } else {
                    continuation.resume(throwing: error ?? FileTransferFailure.invalidReply)
                }
            }
        }
    }

    private static func loadFileRepresentation(
        _ provider: NSItemProvider,
        identifier: String,
    ) async throws -> Data {
        let maximumBytes = 100 * 1024 * 1024
        return try await withCheckedThrowingContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: identifier) { source, error in
                do {
                    guard let source else { throw error ?? FileTransferFailure.invalidReply }
                    let values = try source.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
                    guard values.isRegularFile == true, let size = values.fileSize,
                          size <= maximumBytes else {
                        throw FileTransferFailure.limit
                    }
                    let data = try Data(contentsOf: source, options: .mappedIfSafe)
                    guard data.count <= maximumBytes else {
                        throw FileTransferFailure.limit
                    }
                    continuation.resume(returning: data)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    private static func loadImageObject(_ provider: NSItemProvider) async throws -> UIImage {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadObject(ofClass: UIImage.self) { value, error in
                if let image = value as? UIImage {
                    continuation.resume(returning: image)
                } else {
                    continuation.resume(throwing: error ?? FileTransferFailure.invalidReply)
                }
            }
        }
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
