#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
hub_root="$repo_root/crates/clipmesh-hub-core"

obsolete='HistoryMode|AdministratorCredential|DeviceCredential|EnrollmentArtifact|PresentedCredential|DeviceRecord|create_managed_device|issue_enrollment|exchange_enrollment|rotate_credential|revoke_device|purge_history|memory_history'

if rg -n "$obsolete" "$hub_root"; then
  echo "obsolete application-authority or selectable-history surface remains in hub core" >&2
  exit 1
fi

if rg -n 'clipmesh-protocol' "$hub_root/Cargo.toml"; then
  echo "hub core still depends on superseded D0 application-authority types" >&2
  exit 1
fi

if ! rg -q 'StablePeerId' "$hub_root/src/lib.rs"; then
  echo "stable peer input contract is missing" >&2
  exit 1
fi

if ! rg -q 'ClipContentV1' "$hub_root/src/lib.rs"; then
  echo "canonical clip-content seam is missing" >&2
  exit 1
fi
