import SwiftUI
import QuickLookThumbnailing

struct FileClipRow: View {
    let clip: MeshFileClip
    let files: FileHistoryModel
    let endpoint: String
    let machineName: String

    @State private var sharePresentation: SharePresentation?

    private struct SharePresentation: Identifiable {
        let id = UUID()
        let items: [Any]
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
                Button("Share", systemImage: "square.and.arrow.up") {
                    sharePresentation = SharePresentation(items: urls.map { $0 as Any })
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier("shareFiles-" + clip.id.uuidString)
                .padding(.horizontal, 16)
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
