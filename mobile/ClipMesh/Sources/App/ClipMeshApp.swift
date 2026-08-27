import SwiftUI

@main
struct ClipMeshApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var model = MobileSessionModel()

    var body: some Scene {
        WindowGroup {
            ContentView(model: model)
                .onChange(of: scenePhase, initial: true, scenePhaseChanged)
        }
    }

    private func scenePhaseChanged(_: ScenePhase, _ newPhase: ScenePhase) {
        switch newPhase {
        case .active:
            model.activate()
        case .background, .inactive:
            model.deactivate()
        @unknown default:
            model.deactivate()
        }
    }
}
