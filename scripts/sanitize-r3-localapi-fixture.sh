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
    def status_fields: ["TailscaleIPs", "BackendState", "Self", "Peer", "User", "CurrentTailnet", "Health", "MagicDNSSuffix", "CertDomains", "Version", "TUN", "HaveNodeKey", "AuthURL", "ExitNodeStatus", "ExtraRecords", "ClientVersion"];
    def whois_fields: ["Node", "UserProfile", "CapMap"];
    def only_fields($allowed):
      [keys[] as $key | select($allowed | index($key) | not)] | length == 0;
    if (type == "object") and only_fields(status_fields) and (has("TailscaleIPs")) and (.TailscaleIPs | type == "array") and (.TailscaleIPs | all(type == "string" and length > 0)) then
      {contract_version: "r3-localapi-compatibility-v1", kind: "status", TailscaleIPs: (.TailscaleIPs | map("[redacted]"))}
    elif (type == "object") and only_fields(whois_fields) and (has("Node")) and (.Node | type == "object") and (.Node.StableID | type == "string" and length > 0) and ((has("UserProfile") | not) or (.UserProfile | type == "object")) and ((has("CapMap") | not) or (.CapMap | type == "object")) then
      {contract_version: "r3-localapi-compatibility-v1", kind: "whois", Node: {StableID: "[redacted]"}}
    else error("unexpected LocalAPI schema") end
  ' "$input" >"$output"
else
  echo "jq is required to sanitize LocalAPI fixtures" >&2
  exit 69
fi
