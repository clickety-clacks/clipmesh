import SwiftUI
import UIKit

struct ContentView: View {
    let model: MobileSessionModel

    @State private var isConfirmingSharedClear = false
    @State private var isShowingSettings = false
    @State private var isReadingClipboard = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Button {
                    copyToClipMesh()
                } label: {
                    Label("Copy to ClipMesh", systemImage: "arrow.up.doc")
                        .font(.title2.bold())
                        .frame(maxWidth: .infinity, minHeight: 64)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("copyToClipMesh")
                .disabled(!model.canPublish || isReadingClipboard)
                .accessibilityHint("Sends the text on this device's clipboard to ClipMesh")

                if let latest = model.visibleHistory.first {
                    Button { model.copyHistoryItem(latest.id) } label: {
                        VStack(alignment: .leading, spacing: 12) {
                            Text(latest.isStale ? "Last seen on ClipMesh" : "On ClipMesh")
                                .font(.headline)
                            Text(latest.preview).lineLimit(5)
                            Label("Tap to copy to this device", systemImage: "doc.on.doc")
                                .font(.caption)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("latestClip")
                }
                if let feedback = model.actionFeedback {
                    Text(feedback).font(.callout).accessibilityIdentifier("clipboardFeedback")
                }
                HistoryListView(model: model)
            }
                .padding(.horizontal)
                .navigationTitle("ClipMesh")
                .safeAreaInset(edge: .top) {
                    ConnectionStatusView(state: model.lifecycleState, errorCode: model.errorCode)
                }
                .toolbar {
                    ToolbarItemGroup(placement: .topBarTrailing) {
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

    private func copyToClipMesh() {
        guard model.canPublish, !isReadingClipboard else { return }
        isReadingClipboard = true
        guard let provider = UIPasteboard.general.itemProviders.first(where: {
            $0.canLoadObject(ofClass: NSString.self)
        }) else {
            model.publishClipboardText(nil)
            isReadingClipboard = false
            return
        }
        provider.loadObject(ofClass: NSString.self) { value, _ in
            let text = value as? String
            Task { @MainActor in
                await model.finishExplicitClipboardRead(text)
                isReadingClipboard = false
            }
        }
    }
}
