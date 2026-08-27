import SwiftUI

struct HistoryListView: View {
    let model: MobileSessionModel

    var body: some View {
        if model.visibleHistory.isEmpty {
            ContentUnavailableView(
                "No ClipMesh History",
                systemImage: "doc.on.clipboard",
                description: Text("Foreground catch-up and live text clips appear here."),
            )
        } else {
            List(model.visibleHistory) { row in
                HistoryRowView(row: row, select: model.copyHistoryItem)
            }
        }
    }
}
