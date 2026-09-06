# ClipMesh for iPhone and iPad

Opening the app refreshes shared history without reading or writing the device
clipboard. Incoming clips update the latest preview and history only.

Tap **Copy to ClipMesh** to send clipboard text. iOS may ask for paste
permission. The app reads asynchronously and allows a brief reconnection after
the permission dialog. It does not save a pending read for a later launch.
Success appears only after the hub accepts the clip. An uncertain result is
reported as uncertain, without an automatic retry.

Tap the latest preview or a history row to copy retained text to the device.
This version supports text, not images or files.

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

`project.yml` is the XcodeGen source. After changing targets or source files,
run `xcodegen generate` here and retain the generated Xcode project.

## Network boundary

The app accepts only numeric Tailnet endpoints. App Transport Security
exceptions cover `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, matching that
validation. There is no global arbitrary-load exception. The hub protocol uses
WebSockets inside the encrypted Tailscale network, without separate application
TLS. Apple documents IP-range exceptions in
[NSExceptionDomains](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSAppTransportSecurity/NSExceptionDomains).
