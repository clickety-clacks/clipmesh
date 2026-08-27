#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != Darwin ]]; then
  echo 'R5 macOS native verification requires macOS' >&2
  exit 1
fi

cargo test -p clipmesh-agent-core --test desktop_domain
cargo test -p clipmesh-agent-macos --all-targets

capture="$(cargo run --quiet -p clipmesh-agent-macos --example capture_macos_adapter)"
jq -e '
  .schema_version == 1 and
  .pasteboard_kind == "isolated_unique_native" and
  .local_bytes_utf8 == "clipmesh-local-capture" and
  (.local_declared_types | index("public.utf8-plain-text") != null) and
  .local_hint == "Ordinary" and
  .local_revision_current_before_remote == true and
  .remote_bytes_utf8 == "clipmesh-remote-capture\nline-two" and
  .remote_revision_current == true and
  .change_count_monotonic == true and
  (.lock_state == "Locked" or .lock_state == "Unlocked")
' <<<"$capture" >/dev/null

plutil -lint deploy/launchd/com.example.clipmesh-agent.plist
test "$(plutil -extract RunAtLoad raw -o - deploy/launchd/com.example.clipmesh-agent.plist)" = false
test "$(plutil -extract KeepAlive raw -o - deploy/launchd/com.example.clipmesh-agent.plist)" = false
! rg -i 'authorization|bearer|credential|password|private key' \
  deploy/launchd/com.example.clipmesh-agent.plist

scripts/check-d3-desktop-boundary.sh
scripts/check-repository-boundary.sh
