import SwiftUI

struct InactiveCoverView: View {
    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground)
                .ignoresSafeArea()
            Label("Clip history is hidden while inactive", systemImage: "lock.fill")
                .font(.headline)
                .multilineTextAlignment(.center)
                .padding()
        }
        .accessibilityAddTraits(.isModal)
    }
}
