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

if command -v jq >/dev/null 2>&1; then
  jq '
    if (keys == ["TailscaleIPs"]) and (.TailscaleIPs | type == "array") then
      {TailscaleIPs: (.TailscaleIPs | map("[redacted]"))}
    elif (keys == ["Node"]) and (.Node | type == "object") and (.Node | keys == ["StableID"]) then
      {Node: {StableID: "[redacted]"}}
    else error("unexpected LocalAPI schema") end
  ' "$input" >"$output"
else
  echo "jq is required to sanitize LocalAPI fixtures" >&2
  exit 69
fi
