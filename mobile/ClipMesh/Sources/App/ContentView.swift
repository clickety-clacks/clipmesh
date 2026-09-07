import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ContentView: View {
    let model: MobileSessionModel
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase

    @State private var isConfirmingSharedClear = false
    @State private var isShowingSettings = false
    @State private var isReadingClipboard = false
    @State private var clipboardText: String?
    @State private var clipboardRevision: Int?
    @State private var isChoosingFiles = false
    @State private var selectedFiles: [LocalFileSelection] = []
    @State private var fileImportError: String?
    @State private var fileHistory = FileHistoryModel()
    @State private var appliedHistoryResetID: UUID?

    private enum Clipping: Identifiable {
        case text(HistoryRowPresentation), files(MeshFileClip)
        var id: String {
            switch self {
            case let .text(row): "text-" + row.id.uuidString
            case let .files(row): "files-" + row.id.uuidString
            }
        }
        var date: Date {
            switch self {
            case let .text(row): row.acceptedAt
            case let .files(row): Date(timeIntervalSince1970: Double(row.accepted_at) / 1000)
            }
        }
    }

    private var clippings: [Clipping] {
        (model.visibleHistory.map(Clipping.text) + fileHistory.clips.map(Clipping.files))
            .sorted { $0.date == $1.date ? $0.id < $1.id : $0.date > $1.date }
    }

    var body: some View {
        NavigationStack {
            List {
                Button {
                    Task {
                        if selectedFiles.isEmpty { await model.finishExplicitClipboardRead(clipboardText) }
                        else { await model.sendFiles(selectedFiles) }
                    }
                } label: {
                    VStack(alignment: .leading, spacing: 16) {
                        if isReadingClipboard || model.isSendingFiles {
                            ProgressView()
                        } else if !selectedFiles.isEmpty {
                            ForEach(selectedFiles) { file in
                                VStack(alignment: .leading, spacing: 8) {
                                    if let thumbnail = file.thumbnail {
                                        Image(uiImage: thumbnail).resizable().scaledToFit().frame(maxHeight: 220)
                                    } else {
                                        Image(systemName: "doc").font(.largeTitle)
                                    }
                                    Text(file.descriptor.name).lineLimit(2)
                                    Text(ByteCountFormatter.string(fromByteCount: Int64(file.descriptor.size_bytes), countStyle: .file))
                                        .font(.caption)
                                }
                            }
                        } else if let clipboardText {
                            Text(clipboardText).lineLimit(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        } else {
                            Image(systemName: "clipboard").font(.largeTitle)
                        }
                        Label("Send", systemImage: "arrow.up")
                            .font(.headline)
                    }
                    .padding(.vertical, 12)
                    .frame(maxWidth: .infinity, minHeight: 100, alignment: .leading)
                    .foregroundStyle(colorScheme == .dark ? Color.black : Color.white)
                }
                .buttonStyle(.plain)
                .listRowBackground(colorScheme == .dark ? Color.white : Color.black)
                .accessibilityIdentifier("copyToClipMesh")
                .disabled(!model.canPublish || model.isSendingFiles || isReadingClipboard || (clipboardText == nil && selectedFiles.isEmpty))
                .accessibilityHint("Sends the displayed clipboard to ClipMesh")

                if let fileImportError { Text(fileImportError).font(.callout) }
                if let feedback = model.actionFeedback {
                    Text(feedback).font(.callout).accessibilityIdentifier("clipboardFeedback")
                }
                if let feedback = fileHistory.feedback { Text(feedback).font(.callout) }

                ForEach(clippings) { clipping in
                    switch clipping {
                    case let .files(clip):
                        FileClipRow(clip: clip, files: fileHistory, endpoint: model.hubURLText)
                    case let .text(row):
                    Button { model.copyHistoryItem(row.id) } label: {
                        VStack(alignment: .leading, spacing: 12) {
                            Text(row.preview).lineLimit(8)
                            Text(row.acceptedAt, style: .relative)
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 12)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier(row.id == model.visibleHistory.first?.id ? "latestClip" : "historyClip")
                    }
                }
                if let error = fileHistory.error { Text(error).font(.callout) }
            }
                .listStyle(.insetGrouped)
                .navigationTitle("ClipMesh")
                .navigationBarTitleDisplayMode(.inline)
                .task(id: "\(model.lifecycleState)-\(model.historyResetID)") {
                    // Clear before starting the replacement observer. Separate
                    // onChange/task handlers could stop the new observer.
                    if appliedHistoryResetID != model.historyResetID {
                        fileHistory.clear()
                        appliedHistoryResetID = model.historyResetID
                    }
                    guard model.lifecycleState == .foregroundLive else { fileHistory.stop(); return }
                    await fileHistory.observe(endpoint: model.hubURLText)
                }
                .onChange(of: model.isSendingFiles) { _, sending in
                    if !sending, model.lifecycleState == .foregroundLive {
                        Task { await fileHistory.refresh(endpoint: model.hubURLText) }
                    }
                }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    if phase == .active { refreshClipboardPreview() }
                }
                .onReceive(NotificationCenter.default.publisher(for: UIPasteboard.changedNotification)) { _ in
                    if scenePhase == .active { refreshClipboardPreview() }
                }
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        ConnectionStatusView(state: model.lifecycleState, errorCode: model.errorCode)
                    }
                    ToolbarItemGroup(placement: .topBarTrailing) {
                        Menu("Add files", systemImage: "paperclip") {
                            Button("Choose files", systemImage: "folder") { isChoosingFiles = true }
                            if !selectedFiles.isEmpty {
                                Button("Use clipboard", systemImage: "clipboard") { selectedFiles = [] }
                            }
                        }
                        Button("Refresh", systemImage: "arrow.clockwise", action: model.refresh)
                            .disabled(model.lifecycleState == .inactive)
                        Button("Settings", systemImage: "gear", action: showSettings)
                        Menu("History actions", systemImage: "ellipsis.circle") {
                            Button("Clear local history", action: model.clearLocalHistory)
                                .disabled(model.visibleHistory.isEmpty)
                            Button("Clear shared history", systemImage: "trash", role: .destructive) {
                                isConfirmingSharedClear = true
                            }
                            .disabled(model.lifecycleState != .foregroundLive)
                        }
                    }
                }
                .confirmationDialog(
                    "Clear shared history?",
                    isPresented: $isConfirmingSharedClear,
                    titleVisibility: .visible,
                ) {
                    Button("Clear shared history", role: .destructive, action: model.requestSharedClear)
                } message: {
                    Text("This removes retained ClipMesh history for connected members. It does not change a system clipboard.")
                }
                .sheet(isPresented: $isShowingSettings) {
                    ConnectionSettingsView(model: model)
                }
                .fileImporter(isPresented: $isChoosingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                    Task {
                        do {
                            selectedFiles = try await LocalFileSelection.read(result.get())
                            fileImportError = nil
                        } catch { fileImportError = "Could not read the selected files" }
                    }
                }
                .overlay {
                    if model.isHistoryObscured {
                        InactiveCoverView()
                    }
                }
        }
    }

    private func showSettings() {
        isShowingSettings = true
    }

    private func refreshClipboardPreview() {
        guard !isReadingClipboard else { return }
        let revision = UIPasteboard.general.changeCount
        guard clipboardRevision != revision else { return }
        clipboardRevision = revision
        clipboardText = nil
        selectedFiles = []
        isReadingClipboard = true
        let providers = UIPasteboard.general.itemProviders
        Task {
            defer {
                isReadingClipboard = false
                if scenePhase == .active, UIPasteboard.general.changeCount != revision {
                    refreshClipboardPreview()
                }
            }
            do {
                let files = try await LocalFileSelection.clipboardFiles(providers)
                guard UIPasteboard.general.changeCount == revision else { return }
                if !files.isEmpty { selectedFiles = files; return }
                guard let provider = providers.first(where: { $0.canLoadObject(ofClass: NSString.self) }) else { return }
                let text: String? = await withCheckedContinuation { continuation in
                    provider.loadObject(ofClass: NSString.self) { value, _ in
                        continuation.resume(returning: value as? String)
                    }
                }
                guard UIPasteboard.general.changeCount == revision else { return }
                clipboardText = text
            } catch {
                fileImportError = "Could not read clipboard files"
            }
        }
    }
}
