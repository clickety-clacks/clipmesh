import SwiftUI

struct ConnectionStatusView: View {
    let state: MobileLifecycleState
    let errorCode: String?

    var body: some View {
        Label(title, systemImage: symbol)
            .font(.footnote)
            .foregroundStyle(state == .foregroundError ? .red : .secondary)
            .padding(.horizontal)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.bar)
    }

    private var title: String {
        switch state {
        case .foregroundConnecting:
            "Connecting and catching up"
        case .foregroundError:
            errorCode.map { "Connection error: \($0)" } ?? "Connection error"
        case .foregroundLive:
            "Live"
        case .inactive:
            "Inactive"
        }
    }

    private var symbol: String {
        switch state {
        case .foregroundConnecting:
            "arrow.trianglehead.2.clockwise.rotate.90"
        case .foregroundError:
            "exclamationmark.triangle"
        case .foregroundLive:
            "checkmark.circle"
        case .inactive:
            "pause.circle"
        }
    }
}
