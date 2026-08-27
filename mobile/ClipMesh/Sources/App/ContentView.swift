import SwiftUI

struct ContentView: View {
    let model: MobileSessionModel

    @State private var isConfirmingSharedClear = false
    @State private var isShowingSettings = false

    var body: some View {
        NavigationStack {
            HistoryListView(model: model)
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
}
