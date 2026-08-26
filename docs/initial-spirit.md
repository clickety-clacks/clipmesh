# ClipMesh — MVP product intent

ClipMesh gives one person one text clipboard across devices admitted to the
same Tailnet. A trusted self-hosted hub can read clip text while it validates,
orders, stores, and distributes it. Tailscale WireGuard protects transit.

The local Tailscale boundary supplies the stable peer identity for each
connection. ClipMesh does not create a second membership system. Every
admitted peer has the same publish, resume, acknowledgement, history, and
shared-clear authority.

The hub keeps SQLite history for seven days or 500 clips by default. It removes
the oldest rows first when either limit applies. Shared clear deletes retained
clips and advances one durable generation without changing any system
clipboard.

Resume material updates product history only. After catch-up, a new live clip
from another peer directly overwrites an eligible system clipboard.

`ClipContentV1` is the one content serialization boundary for ingress, SQLite,
egress, previews, and platform writes. Diagnostics contain no content or
Tailnet identity material.

The MVP has no application account, device registry, administrator role,
enrollment or pairing flow, application credential, application TLS, E2E,
memory-only hub history, public listener, or private topology in public bytes.

The canonical reviewed Spirit and technical specification live in the
`tightbeam-specs` repository at exact commit
`26fd8011bf46dd50484988a1fc20e1258c3899a0`.
