# ClipMesh

ClipMesh is a small, private, cross-platform clipboard mesh designed for
machines already connected by a trusted overlay network such as Tailscale.

The project is intentionally text-first and topology-neutral. Desktop agents
automatically exchange clipboard text through a hub, while the iOS/iPadOS app
shows recent entries and lets the user explicitly copy one into the system
pasteboard.

Rust is the default implementation language for the hub, protocol, and desktop
agents. The Apple mobile client uses SwiftUI and native platform APIs.

See [the product intent](docs/initial-spirit.md) for the accepted MVP policy and
its canonical reviewed specification reference.

## Status

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
