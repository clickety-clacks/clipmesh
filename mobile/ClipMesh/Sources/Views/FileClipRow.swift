import SwiftUI
import QuickLookThumbnailing

struct FileClipRow: View {
    let clip: MeshFileClip
    let files: FileHistoryModel
    let endpoint: String
    let machineName: String

    @State private var sharePresentation: SharePresentation?
    @State private var isSavingToPhotos = false
    @State private var saveStatus: SaveStatus?

    private enum SaveStatus {
        case success(String)
        case failure(String)

        var message: String {
            switch self {
            case let .success(message), let .failure(message): message
            }
        }

        var systemImage: String {
            switch self {
            case .success: "checkmark.circle"
            case .failure: "exclamationmark.triangle"
            }
        }

        var isFailure: Bool {
            if case .failure = self { return true }
            return false
        }
    }

    private struct SharePresentation: Identifiable {
        let id = UUID()
        let items: [Any]
    }

    private var saveableMedia: [PhotoLibrarySaveItem] {
        guard let urls = files.localFiles[clip.id] else { return [] }
        return PhotoLibrarySaver.saveItems(urls: urls, descriptors: clip.manifest.files)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(clip.manifest.files.enumerated()), id: \.offset) { index, file in
                let mediaType = file.media_type.lowercased()
                if mediaType.hasPrefix("image/") || mediaType.hasPrefix("video/"),
                   let urls = files.localFiles[clip.id], urls.indices.contains(index) {
                    FileThumbnail(url: urls[index], name: file.name) { files.copy(clip) }
                }
                Label(file.name, systemImage: mediaType.hasPrefix("video/") ? "film" : mediaType.hasPrefix("image/") ? "photo" : "doc")
                    .lineLimit(2)
                    .padding(.horizontal, 16)
            }
            Text("From \(machineName)")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 16)
            if let urls = files.localFiles[clip.id] {
                Button("Copy", systemImage: "doc.on.doc") { files.copy(clip) }
                    .buttonStyle(.borderless)
                    .padding(.horizontal, 16)
                HStack(spacing: 16) {
                    Button("Share", systemImage: "square.and.arrow.up") {
                        sharePresentation = SharePresentation(items: urls.map { $0 as Any })
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("shareFiles-" + clip.id.uuidString)

                    if !saveableMedia.isEmpty {
                        Button {
                            saveToPhotos(saveableMedia)
                        } label: {
                            if isSavingToPhotos {
                                ProgressView()
                            } else {
                                Label("Save to Photos", systemImage: "photo.badge.arrow.down")
                            }
                        }
                        .buttonStyle(.borderless)
                        .disabled(isSavingToPhotos)
                        .accessibilityIdentifier("saveToPhotos-" + clip.id.uuidString)
                    }
                }
                .padding(.horizontal, 16)

                if let saveStatus {
                    Label(saveStatus.message, systemImage: saveStatus.systemImage)
                        .font(.callout)
                        .foregroundStyle(saveStatus.isFailure ? .red : .secondary)
                        .padding(.horizontal, 16)
                        .accessibilityIdentifier("photosSaveFeedback-" + clip.id.uuidString)
                }
            } else {
                Button {
                    Task { await files.download(clip, endpoint: endpoint) }
                } label: {
                    if files.downloading.contains(clip.id) { ProgressView() }
                    else { Label("Download", systemImage: "arrow.down.circle") }
                }
                .padding(.horizontal, 16)
                .disabled(!files.downloading.isEmpty)
            }
            Text(Date(timeIntervalSince1970: Double(clip.accepted_at) / 1000), style: .relative)
                .font(.caption).foregroundStyle(.secondary)
                .padding(.horizontal, 16)
        }
        .padding(.vertical, 12)
        .sheet(item: $sharePresentation) { presentation in
            ClipMeshShareSheet(activityItems: presentation.items)
                .presentationDetents([.large])
                .presentationSizing(.page)
                .presentationDragIndicator(.visible)
        }
    }

    private func saveToPhotos(_ items: [PhotoLibrarySaveItem]) {
        isSavingToPhotos = true
        saveStatus = nil
        Task { @MainActor in
            defer { isSavingToPhotos = false }
            do {
                try await PhotoLibrarySaver.save(items)
                let count = items.count
                let noun = count == 1 ? "item" : "items"
                saveStatus = .success("Saved \(count) \(noun) to Photos")
            } catch let error as PhotoLibrarySaveError {
                saveStatus = .failure(error.userMessage)
            } catch {
                saveStatus = .failure(PhotoLibrarySaveError.saveFailed.userMessage)
            }
        }
    }
}

private struct FileThumbnail: View {
    let url: URL
    let name: String
    let copy: () -> Void
    @State private var thumbnail: UIImage?

    var body: some View {
        VStack(alignment: .leading) {
            if let thumbnail {
                Button(action: copy) {
                    Image(uiImage: thumbnail).resizable().scaledToFit().frame(maxHeight: 220)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Copy files to clipboard")
                .accessibilityIdentifier("copyFileThumbnail-" + name)
            }
        }
        .task(id: url) {
            thumbnail = nil
            let request = QLThumbnailGenerator.Request(fileAt: url, size: CGSize(width: 320, height: 220), scale: 2, representationTypes: .thumbnail)
            let generated = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request).uiImage
            guard !Task.isCancelled else { return }
            thumbnail = generated
        }
    }
}
