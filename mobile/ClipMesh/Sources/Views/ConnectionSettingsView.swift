import SwiftUI

struct ConnectionSettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: MobileSessionModel

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(
                        "ws://<Tailnet IP>/v1/stream",
                        text: $model.hubURLText,
                        axis: .vertical,
                    )
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    if model.errorCode == ReasonCodeV1.configValueInvalid.rawValue {
                        Text("Enter a numeric Tailnet ws URL with the /v1/stream path.")
                            .foregroundStyle(.red)
                    }
                } header: {
                    Text("Hub")
                } footer: {
                    Text("ClipMesh stores only this generic hub URL and content-free connection state in protected preferences.")
                }
            }
            .navigationTitle("Connection")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: dismiss.callAsFunction)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", action: save)
                }
            }
        }
    }

    private func save() {
        guard model.saveHubURL() else {
            return
        }
        dismiss()
        model.refresh()
    }
}
