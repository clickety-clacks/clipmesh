import Foundation

/// The host field, as the keyboard sees it through its text document proxy.
@MainActor
protocol KeyboardTextTarget: AnyObject {
    var selectedText: String? { get }
    func deleteBackward()
    func insertText(_ text: String)
}

/// The iPhone clipboard. `write` returns whether the clipboard now holds
/// `text`; it never reads the clipboard's contents.
@MainActor
protocol KeyboardPasteboard: AnyObject {
    func write(_ text: String) -> Bool
}

/// Runs the keyboard's three keys. One tap at a time: a new tap, or the
/// keyboard going away, ends the previous tap's hub session and discards its
/// result.
@MainActor
final class KeyboardActions {
    private let target: KeyboardTextTarget
    private let pasteboard: KeyboardPasteboard
    private let hasFullAccess: () -> Bool
    private let loadEndpoint: () -> HubEndpoint?
    private let makeTap: (HubEndpoint) -> KeyboardHubTap
    private let onStatus: (KeyboardStatus) -> Void
    private var currentTap: KeyboardHubTap?
    private var tapNumber = 0

    init(
        target: KeyboardTextTarget,
        pasteboard: KeyboardPasteboard,
        hasFullAccess: @escaping () -> Bool,
        loadEndpoint: @escaping () -> HubEndpoint?,
        makeTap: @escaping (HubEndpoint) -> KeyboardHubTap = { KeyboardHubTap(endpoint: $0) },
        onStatus: @escaping (KeyboardStatus) -> Void,
    ) {
        self.target = target
        self.pasteboard = pasteboard
        self.hasFullAccess = hasFullAccess
        self.loadEndpoint = loadEndpoint
        self.makeTap = makeTap
        self.onStatus = onStatus
    }

    /// Ends any tap in flight. Called when the keyboard is dismissed.
    func cancel() {
        tapNumber += 1
        currentTap?.cancel()
        currentTap = nil
    }

    func copy() async {
        await copyOrCut(.copy)
    }

    func cut() async {
        await copyOrCut(.cut)
    }

    func paste() async {
        let tap = beginTap()
        guard hasFullAccess() else {
            onStatus(.fullAccessNeeded)
            return
        }
        guard let endpoint = loadEndpoint() else {
            onStatus(.paste(.notSetUp))
            return
        }
        onStatus(.pasteFetching)
        let hub = makeTap(endpoint)
        currentTap = hub
        let result = await hub.fetchNewest()
        guard tap == tapNumber, let outcome = KeyboardPasteOutcome(result) else {
            return
        }
        currentTap = nil
        if case let .clip(text) = result {
            target.insertText(text)
        }
        onStatus(.paste(outcome))
    }

    private func copyOrCut(_ kind: KeyboardStatus.CopyKind) async {
        let tap = beginTap()
        guard hasFullAccess() else {
            onStatus(.fullAccessNeeded)
            return
        }
        guard let text = target.selectedText, !text.isEmpty else {
            onStatus(.noSelection)
            return
        }
        guard pasteboard.write(text) else {
            onStatus(.iPhoneWriteFailed(kind))
            return
        }
        if kind == .cut {
            // With a selection, deleteBackward removes exactly the selection.
            target.deleteBackward()
        }
        guard let endpoint = loadEndpoint() else {
            onStatus(.copy(kind, .notSetUp))
            return
        }
        onStatus(.copy(kind, .sending))
        let hub = makeTap(endpoint)
        currentTap = hub
        let result = await hub.publish(text)
        guard tap == tapNumber, let outcome = KeyboardCopyOutcome(result) else {
            return
        }
        currentTap = nil
        onStatus(.copy(kind, outcome))
    }

    private func beginTap() -> Int {
        cancel()
        return tapNumber
    }
}
