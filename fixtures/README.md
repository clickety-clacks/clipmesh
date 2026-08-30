# External fixture provenance

ClipMesh keeps raw external captures outside the public repository in
owner-only evidence manifests. Checked-in fixtures contain fixed synthetic
content, reserved identifiers, and generic platform metadata only.

| Seam | Checked-in fixture | Capture and sanitizer evidence |
| --- | --- | --- |
| LocalAPI shape compatibility | `crates/clipmesh-hub-edge/tests/fixtures/r3-localapi-compatibility-v1.json` | The fixture pins upstream Tailscale v1.100.0 revision `c811bb19bf3b0c89061ac7b7a073f6cd23b504d0`. `scripts/test-r3-capture-sanitizer.sh` replays the closed status and WhoIs shapes and proves unknown identity-shaped, malformed, and content-shaped fields fail sanitization. R7 identity and admission tests use only a process-local simulator. |
| Linux Wayland and lock state | `platform/linux/r4-wayland-adapter-v1.json`, `platform/linux/r4-logind-transition-v1.json` | `platform/linux/README.md` records the real isolated capture, platform versions, owner-only raw-manifest custody, structural sanitizers, seeded unknown-MIME refusal, and real private logind transition. |
| macOS pasteboard and lock state | `platform/macos/r5-native-pasteboard-v1.json` | `platform/macos/README.md` records the real isolated native pasteboard capture, platform versions, owner-only raw-manifest custody, and sanitized public fields. |
| Rust and Swift protocol | `protocol/publish-v1.json`, `../mobile/ClipMesh/Tests/Fixtures/rust-hub-frames-v1.jsonl` | `protocol/README.md` and the mobile fixture README bind the synthetic namespace and cross-language production parsers. |

No capture process reads a live user clipboard, starts a ClipMesh listener,
connects to a live hub, or places private topology or raw identity material in
public bytes. Adding an external fixture requires its named capture command,
owner-only manifest, structural sanitizer, production-parser replay, seeded
sanitizer refusal, and repository-boundary scan.
