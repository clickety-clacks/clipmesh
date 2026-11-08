import UIKit

/// The ClipMesh keyboard: Cut, Copy, Paste, the system's globe key when iOS
/// asks for one, and a status line for the last tap.
final class KeyboardViewController: UIInputViewController {
    private let statusLabel = UILabel()
    private let globeButton = UIButton(type: .system)
    private var actions: KeyboardActions?

    override func viewDidLoad() {
        super.viewDidLoad()
        actions = KeyboardActions(
            target: ProxyTextTarget(self),
            pasteboard: SystemKeyboardPasteboard(),
            hasFullAccess: { [weak self] in self?.hasFullAccess ?? false },
            loadEndpoint: { SharedHubURLStore().load() },
            onStatus: { [weak self] status in self?.show(status) },
        )

        let cut = keyButton("Cut", symbol: "scissors", identifier: "keyboard.cut") { $0.cut() }
        let copy = keyButton("Copy", symbol: "doc.on.doc", identifier: "keyboard.copy") { await $0.copy() }
        let paste = keyButton("Paste", symbol: "doc.on.clipboard", identifier: "keyboard.paste") { await $0.paste() }

        globeButton.setImage(UIImage(systemName: "globe"), for: .normal)
        globeButton.accessibilityLabel = "Next keyboard"
        globeButton.accessibilityIdentifier = "keyboard.globe"
        globeButton.addTarget(self, action: #selector(handleInputModeList(from:with:)), for: .allTouchEvents)
        globeButton.widthAnchor.constraint(equalToConstant: 44).isActive = true

        let keys = UIStackView(arrangedSubviews: [globeButton, cut, copy, paste])
        keys.axis = .horizontal
        keys.spacing = 8
        keys.distribution = .fill
        [cut, copy, paste].forEach { $0.widthAnchor.constraint(equalTo: cut.widthAnchor).isActive = true }

        statusLabel.font = .preferredFont(forTextStyle: .footnote)
        statusLabel.adjustsFontForContentSizeCategory = true
        statusLabel.textColor = .secondaryLabel
        statusLabel.textAlignment = .center
        statusLabel.numberOfLines = 3
        statusLabel.accessibilityIdentifier = "keyboard.status"

        let stack = UIStackView(arrangedSubviews: [statusLabel, keys])
        stack.axis = .vertical
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor),
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 8),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -8),
            keys.heightAnchor.constraint(equalToConstant: 48),
            statusLabel.heightAnchor.constraint(greaterThanOrEqualToConstant: 36),
        ])
    }

    override func viewWillLayoutSubviews() {
        globeButton.isHidden = !needsInputModeSwitchKey
        super.viewWillLayoutSubviews()
    }

    override func viewWillDisappear(_ animated: Bool) {
        actions?.cancel()
        super.viewWillDisappear(animated)
    }

    private func keyButton(
        _ title: String,
        symbol: String,
        identifier: String,
        action: @escaping (KeyboardActions) async -> Void,
    ) -> UIButton {
        var configuration = UIButton.Configuration.gray()
        configuration.title = title
        configuration.image = UIImage(systemName: symbol)
        configuration.imagePadding = 6
        let button = UIButton(configuration: configuration, primaryAction: UIAction { [weak self] _ in
            guard let actions = self?.actions else {
                return
            }
            Task { await action(actions) }
        })
        button.accessibilityIdentifier = identifier
        return button
    }

    private func show(_ status: KeyboardStatus) {
        statusLabel.text = status.text
    }
}

/// Reads the selection and inserts pasted text through the keyboard's text document proxy.
private final class ProxyTextTarget: KeyboardTextTarget {
    private weak var controller: UIInputViewController?

    init(_ controller: UIInputViewController) {
        self.controller = controller
    }

    var selectedText: String? {
        controller?.textDocumentProxy.selectedText
    }

    func insertText(_ text: String) {
        controller?.textDocumentProxy.insertText(text)
    }
}

private final class SystemKeyboardPasteboard: KeyboardPasteboard {
    func write(_ text: String) -> Bool {
        let pasteboard = UIPasteboard.general
        let before = pasteboard.changeCount
        pasteboard.string = text
        // changeCount moves only when the write took; reading it does not
        // read the clipboard's contents or prompt for paste permission.
        return pasteboard.changeCount != before
    }
}
