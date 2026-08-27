import SwiftUI

struct HistoryRowView: View {
    let row: HistoryRowPresentation
    let select: (UUID) -> Void

    var body: some View {
        Button {
            select(row.id)
        } label: {
            VStack(alignment: .leading) {
                Text(row.preview)
                    .font(.body)
                    .foregroundStyle(.primary)
                    .lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Label {
                    Text(row.acceptedAt, style: .relative)
                } icon: {
                    Image(systemName: row.isStale ? "clock.badge.exclamationmark" : "clock")
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityHint("Copies this retained text to the pasteboard")
    }
}
