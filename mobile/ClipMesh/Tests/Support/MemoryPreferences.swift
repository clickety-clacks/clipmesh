@testable import ClipMesh

@MainActor
final class MemoryPreferences: PreferencesStoring {
    var endpoint: HubEndpoint?
    var connectionState: ConnectionState = .disconnected

    func loadEndpoint() -> HubEndpoint? {
        endpoint
    }

    func saveEndpoint(_ endpoint: HubEndpoint) {
        self.endpoint = endpoint
    }

    func loadConnectionState() -> ConnectionState {
        connectionState
    }

    func saveConnectionState(_ state: ConnectionState) {
        connectionState = state
    }
}
