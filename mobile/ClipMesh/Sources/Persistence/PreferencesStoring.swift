@MainActor
protocol PreferencesStoring: AnyObject {
    func loadEndpoint() -> HubEndpoint?
    func saveEndpoint(_ endpoint: HubEndpoint)
    func loadConnectionState() -> ConnectionState
    func saveConnectionState(_ state: ConnectionState)
}
