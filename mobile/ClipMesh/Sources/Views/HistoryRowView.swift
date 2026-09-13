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
                Text("From \(row.sourceMachineName)")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
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
        .padding(.horizontal, 16)
        .buttonStyle(.plain)
        .accessibilityLabel("Text from \(row.sourceMachineName), \(row.preview)")
        .accessibilityHint("Copies this retained text to the pasteboard")
    }
}
