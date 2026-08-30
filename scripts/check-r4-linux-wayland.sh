#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != Linux ]]; then
  echo 'R4 Linux Wayland native verification requires Linux' >&2
  exit 1
fi
if [[ -z "${WAYLAND_DISPLAY:-}" || "${CLIPMESH_WAYLAND_CAPTURE_ISOLATED:-}" != 1 ]]; then
  echo 'R4 capture requires an explicitly isolated Wayland display' >&2
  exit 1
fi

cargo test -p clipmesh-agent-core --test desktop_domain
cargo test -p clipmesh-agent-linux --all-targets

capture="$(cargo run --quiet -p clipmesh-agent-linux --example capture_wayland_adapter)"
scripts/sanitize-r4-wayland-fixture.sh <<<"$capture" >/dev/null
jq -e '
  .schema_version == 1 and
  .compositor_protocol == "wlr-data-control-v1" and
  .clipboard_kind == "isolated_headless_wayland" and
  .local_bytes_utf8 == "clipmesh-local-capture" and
  (.local_mime_types | index("text/plain;charset=utf-8") != null) and
  .local_hint == "Ordinary" and
  .local_revision_current_before_remote == true and
  .remote_bytes_utf8 == "clipmesh-remote-capture\nline-two" and
  .remote_hint == "Ordinary" and
  .remote_revision_current == true and
  .invalid_utf8_write_preserved == true and
  (.lock_state == "Locked" or .lock_state == "Unlocked" or .lock_state == "Unknown") and
  (.lock_state != "Unknown" or .lock_state_acts_locked == true)
' <<<"$capture" >/dev/null

rendered_unit="$(mktemp --suffix=.service)"
trap 'rm -f "$rendered_unit"' EXIT
sed \
  -e 's|@@CLIPMESH_AGENT_BINARY@@|/usr/bin/true|g' \
  -e 's|@@CLIPMESH_CONFIG_PATH@@|/tmp/clipmesh-example/config.toml|g' \
  -e 's|@@CLIPMESH_HUB_URL@@|ws://192.0.2.1:4357|g' \
  -e 's|@@CLIPMESH_STATE_PATH@@|/tmp/clipmesh-example/state.sqlite3|g' \
  -e 's|@@CLIPMESH_CONTROL_SOCKET@@|/tmp/clipmesh-example/control.sock|g' \
  deploy/systemd/clipmesh-agent.service >"$rendered_unit"
systemd-analyze verify "$rendered_unit"
! rg -i 'authorization|bearer|credential|password|private key' \
  deploy/systemd/clipmesh-agent.service
test -z "$(find deploy/systemd -type l -print -quit)"

scripts/check-d3-desktop-boundary.sh
scripts/check-repository-boundary.sh
