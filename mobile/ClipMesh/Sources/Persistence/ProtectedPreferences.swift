import Foundation

@MainActor
final class ProtectedPreferences: PreferencesStoring {
    static let allowedKeys: Set<String> = ["connection_state", "hub_url"]

    private let defaults: UserDefaults
    private let sharedHubURL: SharedHubURLStore?

    /// `sharedHubURL` receives a copy of the saved hub URL for the keyboard.
    /// Loading mirrors an existing URL too, so a URL saved before the
    /// keyboard existed reaches it the first time the app opens.
    init(defaults: UserDefaults = .standard, sharedHubURL: SharedHubURLStore? = nil) {
        self.defaults = defaults
        self.sharedHubURL = sharedHubURL
    }

    func loadEndpoint() -> HubEndpoint? {
        guard let value = defaults.string(forKey: "hub_url"),
              let endpoint = try? HubEndpoint(value)
        else {
            return nil
        }
        sharedHubURL?.save(endpoint)
        return endpoint
    }

    func saveEndpoint(_ endpoint: HubEndpoint) {
        defaults.set(endpoint.displayValue, forKey: "hub_url")
        sharedHubURL?.save(endpoint)
    }

    func loadConnectionState() -> ConnectionState {
        defaults.string(forKey: "connection_state")
            .flatMap(ConnectionState.init(rawValue:)) ?? .disconnected
    }

    func saveConnectionState(_ state: ConnectionState) {
        defaults.set(state.rawValue, forKey: "connection_state")
    }
}
