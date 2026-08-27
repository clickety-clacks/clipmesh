#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
agent_root="$repo_root/crates/clipmesh-agent-core"

forbidden='source_device_id|source_seq|AdministrativelyPaused|DeviceCredential|Enrollment|TlsListener|std::net|tokio|axum|wayland|pasteboard|wl-copy|wl-paste'

if rg -n "$forbidden" "$agent_root"; then
  echo "desktop domain contains a removed authority or out-of-scope adapter surface" >&2
  exit 1
fi

if rg -n 'clipmesh-protocol' "$agent_root/Cargo.toml"; then
  echo "desktop domain depends on immutable superseded D0 types" >&2
  exit 1
fi

for seam in ClipContentV1 clear_generation PlatformRevision is_current local_only_next; do
  if ! rg -q "$seam" "$agent_root/src"; then
    echo "desktop domain is missing required seam: $seam" >&2
    exit 1
  fi
done
