#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

render_values() {
  python3 - "$1" <<'PY'
import json
from pathlib import Path
import sys
scratch = Path(sys.argv[1])
print(json.dumps({
    "CLIPMESH_HUB_BINARY": str(scratch / "bin" / "clipmesh-hub"),
    "CLIPMESH_HUB_CONFIG_PATH": "/opt/clipmesh/config/hub.toml",
    "CLIPMESH_HUB_LISTEN_ADDRESS": "192.0.2.1:4357",
    "CLIPMESH_HUB_STATE_DIRECTORY": "/opt/clipmesh/state/hub",
    "CLIPMESH_SERVICE_USER": "root",
    "CLIPMESH_SERVICE_GROUP": "root",
    "CLIPMESH_AGENT_BINARY": str(scratch / "bin" / "clipmesh-agent"),
    "CLIPMESH_CONFIG_PATH": "/opt/clipmesh/config/agent.toml",
    "CLIPMESH_HUB_URL": "ws://192.0.2.1:4357/v1/stream",
    "CLIPMESH_AGENT_PLATFORM": "reserved-example",
    "CLIPMESH_STATE_PATH": "/opt/clipmesh/state/agent.sqlite3",
    "CLIPMESH_CONTROL_SOCKET": "/opt/clipmesh/state/control.sock",
    "CLIPMESH_RETENTION_SECONDS": 604800,
    "CLIPMESH_HISTORY_MAX_ENTRIES": 500,
    "CLIPMESH_MAX_PAYLOAD_BYTES": 262144,
    "CLIPMESH_MAX_CONNECTIONS": 64,
    "CLIPMESH_MAX_CONNECTIONS_PER_PEER": 2,
    "CLIPMESH_PUBLISH_TOKENS_PER_MINUTE": 60,
    "CLIPMESH_PUBLISH_BURST": 10,
    "CLIPMESH_OUTBOUND_QUEUE_MESSAGES": 64,
    "CLIPMESH_OUTBOUND_QUEUE_BYTES": 2097152,
}, sort_keys=True))
PY
}

mkdir "$scratch/bin"
install -m 0755 /usr/bin/true "$scratch/bin/clipmesh-hub"
install -m 0755 /usr/bin/true "$scratch/bin/clipmesh-agent"
render_values "$scratch" >"$scratch/render-values.json"
scripts/render-r7-packaging.py "$scratch/direct" <"$scratch/render-values.json"

jq 'del(.CLIPMESH_HUB_URL)' "$scratch/render-values.json" >"$scratch/missing-values.json"
if scripts/render-r7-packaging.py "$scratch/missing" <"$scratch/missing-values.json"; then
  printf 'R7 renderer accepted a missing variable\n' >&2
  exit 1
fi

jq '.CLIPMESH_UNEXPECTED = "rejected"' \
  "$scratch/render-values.json" >"$scratch/unknown-values.json"
if scripts/render-r7-packaging.py "$scratch/unknown" <"$scratch/unknown-values.json"; then
  printf 'R7 renderer accepted an unknown variable\n' >&2
  exit 1
fi

mkdir "$scratch/existing"
if scripts/render-r7-packaging.py "$scratch/existing" <"$scratch/render-values.json"; then
  printf 'R7 renderer overwrote an existing directory\n' >&2
  exit 1
fi

python3 - "$scratch/direct" <<'PY'
import os
import plistlib
from pathlib import Path
import stat
import sys
import tomllib

root = Path(sys.argv[1])
expected_modes = {
    "clipmesh-hub.toml": 0o600,
    "clipmesh-agent.toml": 0o600,
    "clipmesh-hub.service": 0o644,
    "clipmesh-agent.service": 0o644,
    "com.example.clipmesh-agent.plist": 0o644,
}
assert set(path.name for path in root.iterdir()) == set(expected_modes)
for name, expected in expected_modes.items():
    path = root / name
    assert stat.S_IMODE(path.stat().st_mode) == expected
    assert "@@CLIPMESH_" not in path.read_text(encoding="utf-8")

with (root / "clipmesh-hub.toml").open("rb") as handle:
    hub = tomllib.load(handle)
assert hub == {
    "config_version": 1,
    "listen_address": "192.0.2.1:4357",
    "tailscale_localapi": "system",
    "state_directory": "/opt/clipmesh/state/hub",
    "retention_seconds": 604800,
    "history_max_entries": 500,
    "max_payload_bytes": 262144,
    "max_connections": 64,
    "max_connections_per_peer": 2,
    "publish_tokens_per_minute": 60,
    "publish_burst": 10,
    "outbound_queue_messages": 64,
    "outbound_queue_bytes": 2097152,
}
with (root / "clipmesh-agent.toml").open("rb") as handle:
    agent = tomllib.load(handle)
assert agent["config_version"] == 1
assert agent["hub_url"] == "ws://192.0.2.1:4357/v1/stream"
assert agent["platform"] == "reserved-example"

with (root / "com.example.clipmesh-agent.plist").open("rb") as handle:
    launchd = plistlib.load(handle)
assert launchd["RunAtLoad"] is False
assert launchd["KeepAlive"] is False
assert launchd["ProgramArguments"][0].endswith("/bin/clipmesh-agent")
PY

if command -v systemd-analyze >/dev/null 2>&1; then
  systemd-analyze verify \
    "$scratch/direct/clipmesh-hub.service" \
    "$scratch/direct/clipmesh-agent.service"
fi

cargo run --quiet -p clipmesh-hub-edge --example validate_config -- \
  "$scratch/direct/clipmesh-hub.toml"

if rg -i 'authorization|bearer|credential|password|private key' "$scratch/direct"; then
  printf 'R7 rendered assets contain an application identity or secret surface\n' >&2
  exit 1
fi

if rg -n 'systemctl|launchctl|enabled:[[:space:]]*true|state:[[:space:]]*started' deploy/ansible; then
  printf 'R7 render-only Ansible assets contain service activation\n' >&2
  exit 1
fi

if command -v ansible-playbook >/dev/null 2>&1; then
  python3 - "$scratch/render-values.json" "$scratch/ansible-vars.json" "$scratch/ansible" <<'PY'
import json
from pathlib import Path
import sys

values = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
mapping = {
    "clipmesh_render_output_directory": sys.argv[3],
    "clipmesh_hub_binary": values["CLIPMESH_HUB_BINARY"],
    "clipmesh_hub_config_path": values["CLIPMESH_HUB_CONFIG_PATH"],
    "clipmesh_hub_listen_address": values["CLIPMESH_HUB_LISTEN_ADDRESS"],
    "clipmesh_hub_state_directory": values["CLIPMESH_HUB_STATE_DIRECTORY"],
    "clipmesh_service_user": values["CLIPMESH_SERVICE_USER"],
    "clipmesh_service_group": values["CLIPMESH_SERVICE_GROUP"],
    "clipmesh_agent_binary": values["CLIPMESH_AGENT_BINARY"],
    "clipmesh_agent_config_path": values["CLIPMESH_CONFIG_PATH"],
    "clipmesh_agent_hub_url": values["CLIPMESH_HUB_URL"],
    "clipmesh_agent_platform": values["CLIPMESH_AGENT_PLATFORM"],
    "clipmesh_agent_state_path": values["CLIPMESH_STATE_PATH"],
    "clipmesh_agent_control_socket": values["CLIPMESH_CONTROL_SOCKET"],
}
Path(sys.argv[2]).write_text(json.dumps(mapping), encoding="utf-8")
PY
  ansible-playbook -i localhost, --syntax-check deploy/ansible/render.yml
  ansible-playbook -i localhost, deploy/ansible/render.yml -e "@$scratch/ansible-vars.json"
  diff -ru "$scratch/direct" "$scratch/ansible"
else
  printf 'R7 packaging note: ansible-playbook unavailable; canonical renderer passed\n'
fi

cargo metadata --locked --no-deps --format-version 1 >"$scratch/cargo-metadata.json"
jq -e '
  [.workspace_members[] as $member | .packages[] | select(.id == $member) |
    select(.version != "0.1.0")] | length == 0
' "$scratch/cargo-metadata.json" >/dev/null

jq -e '
  any(.packages[] | select(.name == "clipmesh-hub-edge") | .targets[];
    .name == "clipmesh-hub" and any(.kind[]; . == "bin")) and
  any(.packages[] | select(.name == "clipmesh-agent") | .targets[];
    .name == "clipmesh-agent" and any(.kind[]; . == "bin"))
' "$scratch/cargo-metadata.json" >/dev/null

if cargo run --quiet -p clipmesh-agent -- --config \
  "$scratch/direct/clipmesh-agent.toml" 2>"$scratch/agent-invalid.err"; then
  printf 'R7 agent accepted a reserved-example endpoint or platform\n' >&2
  exit 1
fi
test "$(cat "$scratch/agent-invalid.err")" = "config_value_invalid"

cargo package --workspace --locked --allow-dirty --no-verify >/dev/null
find target/package -maxdepth 1 -type f -name 'clipmesh-*-0.1.0.crate' \
  -printf '%f\n' | sort >"$scratch/actual-packages"
printf '%s\n' \
  clipmesh-agent-0.1.0.crate \
  clipmesh-agent-core-0.1.0.crate \
  clipmesh-agent-linux-0.1.0.crate \
  clipmesh-agent-macos-0.1.0.crate \
  clipmesh-hub-core-0.1.0.crate \
  clipmesh-hub-edge-0.1.0.crate \
  clipmesh-protocol-0.1.0.crate | sort >"$scratch/expected-packages"
diff -u "$scratch/expected-packages" "$scratch/actual-packages"

printf 'R7 packaging check passed: 5 inactive renders, 7 version-0.1.0 crates, 2 executable targets\n'
