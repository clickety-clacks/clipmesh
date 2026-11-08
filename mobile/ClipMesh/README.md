# ClipMesh for iPhone and iPad

Opening the app refreshes shared history without reading or writing the device
clipboard. Incoming clips update the latest preview and history only.

Tap **Copy to ClipMesh** to send clipboard text. iOS may ask for paste
permission. The app reads asynchronously and allows a brief reconnection after
the permission dialog. It does not save a pending read for a later launch.
Success appears only after the hub accepts the clip. An uncertain result is
reported as uncertain, without an automatic retry.

Tap the latest preview or a history row to copy retained text to the device.
Choose files from the attachment menu to send images and other files. File
history offers previews, download, copy, and share actions. History rows keep
their source machine name and can be searched by text, filename, or machine.

## ClipMesh keyboard

The app includes a keyboard with **Cut**, **Copy** and **Paste** keys, a globe
key when iOS asks for one, and a status line for the last tap. Cut and Copy
write the selected text to the device clipboard and send it to ClipMesh in one
tap. Paste inserts the newest ClipMesh clip and leaves the device clipboard
alone. The keyboard has no letter keys; switch back with the globe key.

To set it up:

1. Open the ClipMesh app once and save the hub URL. The keyboard reads only
   that URL, through the app's App Group.
2. Open **Settings > General > Keyboard > Keyboards > Add New Keyboard** and
   choose **ClipMesh**.
3. Tap **ClipMesh** in that list and turn on **Allow Full Access**. Without it
   the keyboard can neither reach the hub nor use the clipboard, and every key
   only says how to turn it on.

The keyboard connects to the hub only for the length of a tap. While the app is
open and holding its two connections, the hub's per-device limit refuses the
keyboard and its status reads busy.

## Build and test

Open `ClipMesh.xcodeproj` in Xcode, or run:

```sh
xcodebuild -project mobile/ClipMesh/ClipMesh.xcodeproj \
  -scheme ClipMesh -destination 'platform=iOS Simulator,id=<simulator-uuid>' \
  CODE_SIGNING_ALLOWED=NO test
```

Unit tests use synthetic transport and both synthetic and native pasteboards.
The UI test skips unless `CLIPMESH_TEST_HUB_URL` is supplied as an xcodebuild
setting. Set it to a numeric Tailnet WebSocket URL ending in `/v1/stream` on a
dedicated test hub. The test publishes synthetic text, exercises actual button
taps and paste permission, relaunches the app with a different local clipboard,
and verifies explicit copying back. It changes the simulator clipboard.

The same setting runs `KeyboardHubIntegrationTests` against the hub. They
publish, clear the shared history and hold two connections at once, so the hub
must be one nobody uses. Add `TEST_RUNNER_CLIPMESH_TEST_HUB_FILL=1` to also
fill its whole history with clips at the maximum payload size and check that
Paste receives only the newest one.

`project.yml` is the XcodeGen source. After changing targets or source files,
run `xcodegen generate` here and retain the generated Xcode project.

## Network boundary

The app accepts only numeric Tailnet endpoints. App Transport Security
exceptions cover `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, matching that
validation. There is no global arbitrary-load exception. The hub protocol uses
WebSockets inside the encrypted Tailscale network, without separate application
TLS. Apple documents IP-range exceptions under the `NSExceptionDomains` key of
`NSAppTransportSecurity`.
