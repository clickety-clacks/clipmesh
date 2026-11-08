import Foundation

/// The keyboard's status line: one outcome for the last tap.
enum KeyboardStatus: Equatable {
    enum CopyKind: Equatable {
        case copy
        case cut
    }

    case idle
    case noSelection
    case fullAccessNeeded
    case iPhoneWriteFailed(CopyKind)
    case copy(CopyKind, KeyboardCopyOutcome)
    case pasteFetching
    case paste(KeyboardPasteOutcome)

    var text: String {
        switch self {
        case .idle:
            ""
        case .noSelection:
            "No text selected"
        case .fullAccessNeeded:
            "Allow Full Access to use ClipMesh: Settings > General > Keyboard > Keyboards > ClipMesh"
        case let .iPhoneWriteFailed(kind):
            kind == .cut ? "Couldn't cut to iPhone" : "Couldn't copy to iPhone"
        case let .copy(kind, outcome):
            "\(kind == .cut ? "Cut" : "Copied") to iPhone\(outcome.suffix)"
        case .pasteFetching:
            "Getting the newest ClipMesh clip…"
        case let .paste(outcome):
            outcome.text
        }
    }
}

/// The ClipMesh half of a Copy or Cut whose iPhone write succeeded.
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
        case .confirmed: " and ClipMesh"
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
