# ClipMesh

ClipMesh is a small, private, cross-platform clipboard mesh designed for
machines already connected by a trusted overlay network such as Tailscale.

The project is intentionally text-first and topology-neutral. Desktop agents
automatically exchange clipboard text through a hub, while the iOS/iPadOS app
leaves the system clipboard untouched when opened or when clips arrive.
Tap **Copy to ClipMesh** to send clipboard text. Tap the latest clip preview
or a history entry to copy it to the device. Sending reports success only
after the hub accepts the clip. iOS may ask permission to paste when sending.

Rust is the default implementation language for the hub, protocol, and desktop
agents. The Apple mobile client uses SwiftUI and native platform APIs.

See [the product intent](docs/initial-spirit.md) for the accepted MVP policy and
its canonical reviewed specification reference.

## Status

`main` contains the current implementation and is the default development
target. The former `0.1.0` quarantine-only policy and separate quarantine-to-main
approval gate no longer apply (Mike's source-development ruling, 2026-09-05).
Existing branches remain preserved. Source development does not authorize
release publication, permanent installation, or deployment.

The mobile project is [mobile/ClipMesh/ClipMesh.xcodeproj](mobile/ClipMesh/ClipMesh.xcodeproj).
Live acceptance remains partial; availability on `main` is not a full-acceptance
or release claim.

The immutable Rust protocol foundation, the remediated transport-neutral hub
policy core, the persistent desktop domain core, and the explicit Tailnet hub
and desktop agent executables are present. The desktop core provides outbox,
resume, clear-generation, local-control, revision-marker, and synthetic
adapter seams without opening a network or platform listener. The edge validates a
configured Tailnet self address through LocalAPI, resolves each accepted
socket with WhoIs before HTTP parsing, and holds the hub event lease through
complete WebSocket-frame output. The `clipmesh-hub` binary is its only explicit
bind-and-serve boundary. The `clipmesh-agent` binary admits a numeric Tailnet
endpoint before transport, composes the native platform adapter, resumes to
live, and reconnects with full jitter. The hub core provides SQLite-only
ordering, retry, resume, acknowledgement, retention, shared clear, and
canonical clip-content custody.

The Linux Wayland and macOS native clipboard and lock-state adapters,
owner-only Unix control seams, inactive generic systemd and launchd templates,
closed configuration templates, and render-only Ansible assets are present.
Installation, service loading or activation, deployment, listener activation,
and private topology remain outside this repository slice.
