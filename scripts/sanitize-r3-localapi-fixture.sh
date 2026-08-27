#!/bin/sh
# Emit a schema-only LocalAPI fixture. Stable IDs, addresses, host names, and
# every unknown value are replaced; raw captures never leave the output dir.
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 INPUT_JSON OUTPUT_JSON" >&2
  exit 64
fi

input=$1
output=$2
contract="$(dirname "$0")/../crates/clipmesh-hub-edge/src/localapi-compatibility-v1.json"

if command -v jq >/dev/null 2>&1; then
  jq --slurpfile contracts "$contract" '
    $contracts[0] as $contract |
    def only_fields($allowed):
      [keys[] as $key | select($allowed | index($key) | not)] | length == 0;
    if (type == "object") and only_fields($contract.schema.status.allowed_fields) and (has("TailscaleIPs")) and (.TailscaleIPs | type == "array") and (.TailscaleIPs | all(type == "string" and length > 0)) then
      {upstream_compatibility: "tailscale-v1.100.0-c811bb19bf3b0c89061ac7b7a073f6cd23b504d0", kind: "status", TailscaleIPs: (.TailscaleIPs | map("[redacted]"))}
    elif (type == "object") and only_fields($contract.schema.whois.allowed_fields) and (has("Node")) and (.Node | type == "object") and (.Node.StableID | type == "string" and length > 0) and (. as $response | [$contract.schema.whois.object_fields[] as $field | select(($response | has($field)) and ($response[$field] | type != "object"))] | length == 0) then
      {upstream_compatibility: "tailscale-v1.100.0-c811bb19bf3b0c89061ac7b7a073f6cd23b504d0", kind: "whois", Node: {StableID: "[redacted]"}}
    else error("unexpected LocalAPI schema") end
  ' "$input" >"$output"
else
  echo "jq is required to sanitize LocalAPI fixtures" >&2
  exit 69
fi
