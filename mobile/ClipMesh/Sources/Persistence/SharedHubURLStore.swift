import Foundation

/// The one value the app shares with its keyboard: the saved hub URL.
///
/// The keyboard runs in its own process and cannot read the app's standard
/// defaults, so the app mirrors the URL into the App Group's defaults. Nothing
/// else (no clip text, cursor or peer identity) is written here.
struct SharedHubURLStore {
    static let appGroupIdentifier = "group.co.clicketyclacks.ClipMesh"
    static let allowedKeys: Set<String> = ["hub_url"]

    private let defaults: UserDefaults?

    init(defaults: UserDefaults? = UserDefaults(suiteName: Self.appGroupIdentifier)) {
        self.defaults = defaults
    }

    func load() -> HubEndpoint? {
        guard let value = defaults?.string(forKey: "hub_url") else {
            return nil
        }
        return try? HubEndpoint(value)
    }

    func save(_ endpoint: HubEndpoint) {
        defaults?.set(endpoint.displayValue, forKey: "hub_url")
    }
}
