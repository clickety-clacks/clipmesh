import Foundation

@MainActor
final class ProtectedPreferences: PreferencesStoring {
    static let allowedKeys: Set<String> = ["connection_state", "hub_url"]

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func loadEndpoint() -> HubEndpoint? {
        guard let value = defaults.string(forKey: "hub_url") else {
            return nil
        }
        return try? HubEndpoint(value)
    }

    func saveEndpoint(_ endpoint: HubEndpoint) {
        defaults.set(endpoint.displayValue, forKey: "hub_url")
    }

    func loadConnectionState() -> ConnectionState {
        defaults.string(forKey: "connection_state")
            .flatMap(ConnectionState.init(rawValue:)) ?? .disconnected
    }

    func saveConnectionState(_ state: ConnectionState) {
        defaults.set(state.rawValue, forKey: "connection_state")
    }
}
