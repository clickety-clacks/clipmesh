import SwiftUI

struct ConnectionStatusView: View {
    let state: MobileLifecycleState
    let errorCode: String?

    var body: some View {
        Label(title, systemImage: symbol)
            .labelStyle(.titleAndIcon)
            .fixedSize()
            .font(.footnote)
            .foregroundStyle(state == .foregroundError ? .red : .secondary)
    }

    private var title: String {
        switch state {
        case .foregroundConnecting:
            "Connecting"
        case .foregroundError:
            "Offline"
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
