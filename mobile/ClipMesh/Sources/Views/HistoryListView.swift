import SwiftUI

struct HistoryListView: View {
    let model: MobileSessionModel

    var body: some View {
        if model.visibleHistory.isEmpty {
            ContentUnavailableView(
                "Nothing on ClipMesh yet",
                systemImage: "doc.on.clipboard",
                description: Text("Copy text on this device, then tap Copy to ClipMesh. Shared text will appear here."),
            )
        } else {
            List(model.visibleHistory) { row in
                HistoryRowView(row: row, select: model.copyHistoryItem)
            }
        }
    }
}
