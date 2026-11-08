import Foundation

/// The keyboard's status line: one outcome for the last tap.
enum KeyboardStatus: Equatable {
    case idle
    case noSelection
    case fullAccessNeeded
    case cutRefused
    case iPhoneWriteFailed
    /// A Copy that wrote the iPhone clipboard, with the reported text's
    /// length in characters as the user sees them.
    case copy(characters: Int, KeyboardCopyOutcome)
    case pasteFetching
    case paste(KeyboardPasteOutcome)

    var text: String {
        switch self {
        case .idle:
            ""
        case .noSelection:
            // The keyboard cannot tell an empty selection from one the host
            // field did not share.
            "No selected text shared with the keyboard"
        case .fullAccessNeeded:
            "Allow Full Access to use ClipMesh: Settings > General > Keyboard > Keyboards > ClipMesh"
        case .cutRefused:
            "Didn't cut: couldn't confirm the whole selection. Copy still works."
        case .iPhoneWriteFailed:
            "Couldn't copy to iPhone"
        case let .copy(characters, outcome):
            // iOS can report only part of a selection, with its spacing
            // changed, and gives no sign of it, so every copy says so.
            "Copied \(characters) \(characters == 1 ? "character" : "characters") to iPhone\(outcome.suffix)"
                + " This may be only part of your selection, or differ in spacing."
        case .pasteFetching:
            "Getting the newest ClipMesh clip…"
        case let .paste(outcome):
            outcome.text
        }
    }
}

/// The ClipMesh half of a Copy whose iPhone write succeeded.
enum KeyboardCopyOutcome: Equatable {
    case sending
    case confirmed
    case unconfirmed
    case unreachable
    case refused(ReasonCodeV1)
    case busy
    case notSetUp

    init?(_ result: KeyboardPublishResult) {
        switch result {
        case .confirmed: self = .confirmed
        case .unconfirmed: self = .unconfirmed
        case .unreachable: self = .unreachable
        case let .refused(code): self = .refused(code)
        case .busy: self = .busy
        case .cancelled: return nil
        }
    }

    var suffix: String {
        switch self {
        case .sending: ". Sending to ClipMesh…"
        case .confirmed: " and ClipMesh."
        case .unconfirmed: ". ClipMesh didn't confirm."
        case .unreachable: ". Couldn't reach ClipMesh."
        case let .refused(code): ". ClipMesh refused it (\(Self.describe(code)))."
        case .busy: ". ClipMesh is busy on this device."
        case .notSetUp: ". Set up ClipMesh in the app first."
        }
    }

    private static func describe(_ code: ReasonCodeV1) -> String {
        code == .payloadTooLarge ? "too large" : code.rawValue.replacing("_", with: " ")
    }
}

enum KeyboardPasteOutcome: Equatable {
    case pasted
    case empty
    case busy
    case unreachable
    case notSetUp

    init?(_ result: KeyboardFetchResult) {
        switch result {
        case .clip: self = .pasted
        case .empty: self = .empty
        case .busy: self = .busy
        case .unreachable: self = .unreachable
        case .cancelled: return nil
        }
    }

    var text: String {
        switch self {
        case .pasted: "Pasted from ClipMesh"
        case .empty: "Nothing in ClipMesh"
        case .busy: "ClipMesh is busy on this device"
        case .unreachable: "Couldn't reach ClipMesh"
        case .notSetUp: "Set up ClipMesh in the app first"
        }
    }
}
