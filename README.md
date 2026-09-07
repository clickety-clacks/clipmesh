# ClipMesh

ClipMesh is a small, private, cross-platform clipboard mesh designed for
machines already connected by a trusted overlay network such as Tailscale.

Desktop agents automatically exchange clipboard text and native file selections
through a hub. macOS reads file URLs from the pasteboard; Linux reads explicit
Wayland `text/uri-list` offers. A path copied as ordinary text remains text. The
iOS/iPadOS app supports text and file selections. Its first row previews the
device clipboard with an inverted background and a Send action. Opening the
app reads that preview but does not send anything or write to the clipboard.
iOS may request paste permission while preparing the preview.

Tap Send to publish the displayed content. The paperclip menu also lets you
choose files. Received files offer Copy and Share, and image/video files show
thumbnails after download. Tapping a downloaded thumbnail copies the selection.
Sending reports success only after the hub accepts the whole selection.
The app uses a scrolling SwiftUI List with native floating toolbars.

File transfer currently supports up to 32 files per clipping, 100 MiB per file,
and 500 MiB per selection. The hub reserves at most 1 GiB of file payloads;
the mobile download cache is limited to 500 MiB. Directories are not accepted.
Files travel as bytes with SHA-256 verification, never as remote filesystem
paths. File metadata and transfers use the negotiated `clipmesh.files.v1`
connection alongside the existing text protocol. Desktop receivers skip initial
file history, suppress source echoes, and check clipboard revisions before
applying downloaded files. Received bytes live in private local directories.
Direct image clipboard formats on desktop are not yet converted into files;
copy an image file in the file manager instead. These source capabilities do
not establish what is installed on any machine.

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
