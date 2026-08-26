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

The immutable Rust protocol foundation and the remediated transport-neutral hub
policy core are present. The hub core accepts a stable peer ID from a later
Tailnet edge and provides SQLite-only ordering, retry, resume, acknowledgement,
retention, shared clear, and canonical clip-content custody.

LocalAPI, transport, desktop, mobile, packaging, deployment, listeners, and
private topology remain outside this repository slice.
