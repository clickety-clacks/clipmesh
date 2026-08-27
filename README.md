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
policy core, and a dormant Tailnet edge are present. The edge validates a
configured Tailnet self address through LocalAPI, resolves each accepted
socket with WhoIs before HTTP parsing, and holds the hub event lease through
complete WebSocket-frame output. It has no executable or default listener.
The hub core provides SQLite-only ordering, retry, resume, acknowledgement,
retention, shared clear, and canonical clip-content custody.

Desktop, mobile, packaging, deployment, listener activation, and private
topology remain outside this repository slice.
