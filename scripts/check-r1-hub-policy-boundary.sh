#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
hub_root="$repo_root/crates/clipmesh-hub-core"

obsolete='HistoryMode|AdministratorCredential|DeviceCredential|EnrollmentArtifact|PresentedCredential|DeviceRecord|device_registry|create_managed_device|issue_enrollment|exchange_enrollment|rotate_credential|credential_(rotation|expiry)|revoke_device|purge_history|memory_history|Pairing|pairing_(code|route)|Bearer|bearer_token|TlsListener|tls_listener|admin_route|enrollment_route'

current_files=()
while IFS= read -r -d '' path; do
  case "$path" in
    docs/* | crates/clipmesh-protocol/* | scripts/check-*.sh | scripts/test-*.sh)
      ;;
    *.rs | *.toml | *.sql | *.yaml | *.yml | *.json | *.service | *.plist | *.sh)
      current_files+=("$repo_root/$path")
      ;;
  esac
done < <(git -C "$repo_root" ls-files --cached -z)

if ((${#current_files[@]} == 0)); then
  echo "current-source census resolved no tracked source files" >&2
  exit 1
fi

# The accepted R1 card keeps the exact D0 protocol crate byte-identical. Scan
# every other tracked executable, schema, configuration, route, and deployment
# surface in the current tree. Documentation and the census scripts may name
# removed concepts only to specify or detect them.
if rg -n "$obsolete" "${current_files[@]}"; then
  echo "obsolete application-authority or selectable-history surface remains in the current source scope" >&2
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
