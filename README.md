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
policy core, and the persistent desktop domain core are present. The desktop
core provides outbox, resume, clear-generation, local-control, revision-marker,
and synthetic adapter seams without opening a network or platform listener.

LocalAPI, transport, platform adapters, mobile, packaging, deployment,
listeners, and private topology remain outside this repository slice.
