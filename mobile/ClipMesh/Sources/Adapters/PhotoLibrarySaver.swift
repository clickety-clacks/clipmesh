import Foundation

#if canImport(Photos)
import Photos
#endif

struct PhotoLibrarySaveItem: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case photo
        case video
    }

    let url: URL
    let kind: Kind
}

enum PhotoLibrarySaveError: Error, Equatable {
    case noMedia
    case permissionDenied
    case permissionRestricted
    case saveFailed

    var userMessage: String {
        switch self {
        case .noMedia:
            return "There are no photos or videos to save."
        case .permissionDenied:
            return "Photos access is off. Allow ClipMesh to add photos in Settings."
        case .permissionRestricted:
            return "Photos access is unavailable on this device."
        case .saveFailed:
            return "Could not save the media to Photos."
        }
    }
}

enum PhotoLibrarySaver {
    static func saveItems(urls: [URL], descriptors: [MeshFileDescriptor]) -> [PhotoLibrarySaveItem] {
        guard urls.count == descriptors.count else { return [] }
        return zip(urls, descriptors).compactMap { url, descriptor in
            let mediaType = descriptor.media_type.lowercased()
            if mediaType.hasPrefix("image/") {
                return PhotoLibrarySaveItem(url: url, kind: .photo)
            }
            if mediaType.hasPrefix("video/") {
                return PhotoLibrarySaveItem(url: url, kind: .video)
            }
            return nil
        }
    }

    static func save(_ items: [PhotoLibrarySaveItem]) async throws {
        guard !items.isEmpty else { throw PhotoLibrarySaveError.noMedia }

        #if canImport(Photos)
        let authorization = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        switch authorization {
        case .authorized, .limited:
            break
        case .restricted:
            throw PhotoLibrarySaveError.permissionRestricted
        case .denied, .notDetermined:
            throw PhotoLibrarySaveError.permissionDenied
        @unknown default:
            throw PhotoLibrarySaveError.permissionDenied
        }

        do {
            try await PHPhotoLibrary.shared().performChanges {
                for item in items {
                    let request = PHAssetCreationRequest.forAsset()
                    let resourceType: PHAssetResourceType = item.kind == .photo ? .photo : .video
                    request.addResource(with: resourceType, fileURL: item.url, options: nil)
                }
            }
        } catch {
            throw PhotoLibrarySaveError.saveFailed
        }
        #else
        // The iOS target can run as an iPad app on Vision Pro, but a build for
        // a platform without Photos must not pretend that saving succeeded.
        throw PhotoLibrarySaveError.saveFailed
        #endif
    }
}
