# ClipMesh

ClipMesh is a small, private, cross-platform clipboard mesh designed for
machines already connected by a trusted overlay network such as Tailscale.

The project is intentionally text-first and topology-neutral. Desktop agents
automatically exchange clipboard text through a hub, while the iOS/iPadOS app
shows recent entries and lets the user explicitly copy one into the system
pasteboard.

Rust is the default implementation language for the hub, protocol, and desktop
agents. The Apple mobile client uses SwiftUI and native platform APIs.

See [the initial spirit document](docs/initial-spirit.md) for the product intent,
architecture, security model, scope, and unresolved decisions.

## Status

The Rust protocol-v1 foundation and transport-free hub state core are present.
Hub transport, desktop, mobile, deployment, live enrollment, and private
topology remain outside this repository slice.
