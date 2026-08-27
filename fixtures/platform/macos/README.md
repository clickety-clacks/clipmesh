# macOS native adapter capture

`r5-native-pasteboard-v1.json` came from
`capture_macos_adapter` on 2026-08-27 at 03:50 PT. The capture ran on arm64
macOS 26.5.2 (25F84) with Xcode 26.6 (17F113), Swift 6.3.3, and Rust 1.93.1.

The capture used a native isolated `NSPasteboard` created with
`pasteboardWithUniqueName`. It wrote fixed synthetic local and remote UTF-8
text, read back the native declared types, and measured native `changeCount`
revisions. It queried the current console lock state through
`CGSessionCopyCurrentDictionary`. It did not read or write the user's general
pasteboard, activate a service, open a network listener, or use private
topology.

The raw command output and its SHA-256 remain in the producer's owner-only
evidence manifest. This sanitized fixture contains only fixed synthetic text
and generic platform metadata. The capture found no eligible explicit
confidential or transient signal, so the checked-in hint registry remains
empty.
