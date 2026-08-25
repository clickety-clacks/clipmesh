# ClipMesh — Initial Spirit

## Purpose

ClipMesh should make copying short-lived text between a person's computers feel
like using one clipboard, without requiring a cloud account, pairwise GUI setup,
or a heavyweight desktop application.

It is intended for a private fleet already joined by a secure overlay network
such as Tailscale. Installation and enrollment should be automatable, including
through Ansible. A new managed desktop should join the clipboard pool using its
own provisioned identity rather than requiring every existing device to approve
it interactively.

The project must remain independent of any particular user's topology. Source,
examples, tests, and defaults must not embed real hostnames, usernames, tailnet
names, IP addresses, filesystem layouts, secrets, or deployment boundaries.
Topology-specific values belong in deployment inventory or runtime
configuration outside this public repository.

## Product shape

ClipMesh is a hub-and-spoke clipboard pool with optional direct-delivery
evolution later:

1. A lightweight hub accepts authenticated connections, retains a small recent
   history, and broadcasts new clipboard entries.
2. A daemon on each Linux or macOS desktop watches the local clipboard, submits
   changes, receives remote changes, and writes them locally.
3. A SwiftUI iOS/iPadOS app displays recent entries and copies a selected entry
   into `UIPasteboard.general` on explicit user action.
4. A Share extension may let iOS/iPadOS users send selected text into ClipMesh
   from another app.

There is no expectation that iOS or iPadOS can run a permanent clipboard daemon.
Apple suspends ordinary background applications and restricts passive clipboard
access. The mobile experience should embrace explicit interaction: fetch recent
history when active, display it clearly, and copy or share on request.

## Experience principles

- Installation is automatable; trust enrollment is not a GUI ceremony for
  managed desktops.
- Every device has a distinct, revocable identity.
- Copying ordinary text on one desktop should make it available on the other
  online desktops within roughly one second.
- Mobile behavior should be honest about platform constraints rather than
  pretending background synchronization is reliable.
- Clipboard contents are unusually sensitive. The system should collect less,
  retain less, expose less, and log no content.
- Operational behavior should be understandable from normal logs and health
  endpoints without revealing clipboard payloads or credentials.
- Prefer small native components and platform APIs over embedded browser or JVM
  runtimes.

## Implementation preferences

Rust is preferred everywhere it is practical:

- Hub: Tokio, Axum, rustls, and SQLite through SQLx or rusqlite.
- Shared protocol and domain model: a versioned Rust crate with a deliberately
  small wire schema.
- Linux agent: Rust orchestration around `wl-paste --watch` and `wl-copy` for
  the first Wayland implementation. A native backend can follow if justified.
- macOS agent: Rust with native pasteboard integration. A narrow Swift or
  Objective-C bridge is acceptable where it is safer than fragile FFI.
- iOS/iPadOS: SwiftUI, URLSession/WebSocket APIs, UIPasteboard, Keychain, and an
  optional Share extension.

Avoid designing new cryptographic primitives. Use established protocols and
well-reviewed libraries.

## Logical components

### `clipmesh-hub`

- Listens only on configured interfaces; deployment should normally bind it to
  an overlay-network address.
- Authenticates each device independently.
- Maintains device metadata and revocation state.
- Accepts clipboard events and broadcasts them to eligible devices.
- Stores a bounded history in SQLite, or optionally memory only.
- Exposes content-free health and readiness endpoints.
- Rejects oversized, expired, duplicate, replayed, and malformed events.

### `clipmesh-agent`

- Runs as a systemd user service on Linux and a launchd agent on macOS.
- Watches plain-text clipboard changes.
- Sends local changes to the hub.
- Applies remote changes to the system clipboard.
- Suppresses feedback loops using message IDs and content hashes.
- Stops publishing while paused or while the desktop is locked.
- Reconnects with bounded exponential backoff and resumes from a cursor.
- Stores credentials with restrictive permissions or in the native keychain.

### `ClipMesh` for iOS/iPadOS

- Connects while foregrounded and fetches bounded recent history.
- Shows source device, age, and a safe text preview.
- Copies a selected entry to the global system pasteboard.
- Stores credentials in Keychain.
- Provides a manual refresh and clear-history control.
- May include a Share extension for sending selected text to the hub.
- Must not claim dependable passive background clipboard monitoring.

### Deployment assets

- Generic systemd and launchd templates.
- An Ansible role or documented variables for binaries, hub URL, device ID,
  credential material, retention, and startup behavior.
- No real inventory or secrets in the public repository.

## Wire-event baseline

The first protocol should be deliberately boring and versioned. Each clipboard
event should include at least:

- Protocol version
- Globally unique message ID
- Source device ID
- Per-device monotonic sequence number
- Creation timestamp
- Expiration timestamp
- Content type (`text/plain` initially)
- Payload length
- Content hash
- Payload or encrypted payload envelope

The hub and agents should use the identifiers to reject replay, deduplicate
delivery, resume after reconnect, and prevent clipboard echo loops. Protocol
compatibility and migrations must be explicit.

## Security model

The overlay network is one security layer, not the entire authorization model.
ClipMesh assumes a tailnet device or local process may still be compromised.

### Required for the first usable release

1. **Network confinement**
   - Bind the hub only to configured private/overlay interfaces.
   - Document a least-privilege Tailscale ACL allowing only clipboard devices
     to reach the service port.
   - Do not expose a public listener by default.

2. **Per-device authentication**
   - Provision a unique credential for every device.
   - Never ship a fleet-wide secret in a public repository or binary.
   - Support individual revocation and rotation.
   - Managed desktops may receive credentials through Ansible or another secret
     delivery mechanism.
   - Mobile enrollment should use a short-lived, single-use enrollment artifact.

3. **Transport protection**
   - Use rustls/TLS in addition to the overlay network.
   - Authenticate WebSocket or streaming sessions before accepting content.
   - Prefer mutual TLS if its operational cost remains reasonable; otherwise
     use unique high-entropy bearer credentials over TLS for the MVP.

4. **Replay resistance and validation**
   - Validate sequence numbers, timestamps, expiry, message IDs, sizes, and
     content type.
   - Apply strict request and connection limits.
   - Treat all remote clipboard content as untrusted data.

5. **Minimal persistence**
   - Text only for the MVP.
   - Default maximum item size around 256 KiB.
   - Default history between 20 and 50 items.
   - Default expiration measured in hours, not indefinite retention.
   - Restrictive database and credential permissions.
   - An optional memory-only hub mode.

6. **Clipboard-specific controls**
   - Pause globally or per device.
   - Do not publish while a desktop is locked.
   - Respect password-manager or sensitive clipboard MIME hints where the
     platform exposes them.
   - Provide local-only copy behavior and an emergency clear operation.
   - Never include clipboard payloads in logs, metrics, crash reports, or error
     messages.

### End-to-end encryption direction

The MVP may allow the trusted self-hosted hub to see plaintext if it already has
TLS, unique device authentication, strict network confinement, and short
retention. The architecture should not prevent a later zero-knowledge mode.

For zero-knowledge delivery, a sender would encrypt an event separately to each
eligible recipient identity using an established recipient-encryption scheme
such as HPKE or an age-style X25519 construction. The hub would route and store
only ciphertext. This should be adopted only with a clear key-distribution and
device-revocation design; it must not be improvised.

## MVP scope

- Rust hub and Rust Linux/macOS agents.
- SwiftUI iOS/iPadOS history-and-copy client.
- Plain UTF-8 text only.
- One hub and a small personal fleet.
- Authenticated TLS connections over a private overlay network.
- Unique device credentials and revocation.
- Bounded SQLite history plus optional memory-only mode.
- Automatic desktop synchronization.
- Foreground mobile refresh and explicit pasteboard writes.
- Loop suppression, replay protection, pause, clear, and lock-state behavior.
- Generic deployment documentation and Ansible-friendly configuration.

## Explicit non-goals for the MVP

- Images, files, HTML, RTF, or arbitrary MIME replication.
- Public-internet discovery or operation without a private network.
- Accounts, billing, social sharing, or multi-tenant hosting.
- Reliable background execution on iOS/iPadOS.
- A full clipboard manager UI on desktop.
- Pairwise GUI enrollment between every machine.
- Invented cryptography.
- Embedding a specific private topology in source control.

## Initial acceptance outcomes

1. A newly provisioned managed desktop can join the pool without touching every
   existing desktop.
2. Copying text on one unlocked desktop updates other connected unlocked
   desktops within one second under normal conditions.
3. An iPad can open the app, see recent entries, tap one, and paste it into
   another app through the global pasteboard.
4. The iPad Share extension, if included in the first milestone, can send
   selected text into the pool.
5. Disconnecting and reconnecting does not create loops or duplicate history.
6. Revoking one device prevents it from reconnecting without disturbing other
   devices.
7. The hub is unreachable outside its intended private interface and contains
   no clipboard payloads in logs.
8. Repository tests and examples contain no private topology identifiers or
   credentials.

## Spirit questions requiring explicit decisions

The Product Owner should settle these before implementation commits the project
to difficult-to-change behavior:

1. Is hub-readable plaintext acceptable for the first release, or is
   end-to-end encryption a launch requirement?
2. Should history persist in SQLite by default, or should memory-only operation
   be the default posture?
3. What are the desired default retention duration, history count, and maximum
   item size?
4. Is the iOS Share extension part of the MVP or the next milestone?
5. Is mutual TLS worth the enrollment complexity, or should the MVP use unique
   bearer credentials over TLS and Tailscale?
6. Which deployment owns the canonical hub role, without encoding that private
   choice in the public product?
7. What open-source license should govern the public repository?

If product intent or spirit remains ambiguous, file Tightbeam decision requests
for the operator rather than silently choosing an enduring product policy.

