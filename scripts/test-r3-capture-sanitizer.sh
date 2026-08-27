#!/bin/sh
set -eu

fixture_directory=$(mktemp -d)
trap 'rm -rf "$fixture_directory"' EXIT
contract="$(dirname "$0")/../crates/clipmesh-hub-edge/src/localapi-compatibility-v1.json"
jq -e '
  .upstream == {
    "release": "v1.100.0",
    "revision": "c811bb19bf3b0c89061ac7b7a073f6cd23b504d0",
    "status_type": "ipnstate.Status",
    "whois_type": "apitype.WhoIsResponse"
  }
' "$contract" >/dev/null
jq '.status' "$contract" >"$fixture_directory/status.json"
jq '.whois' "$contract" >"$fixture_directory/whois.json"
"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$fixture_directory/status.json" "$fixture_directory/status.sanitized.json"
"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$fixture_directory/whois.json" "$fixture_directory/whois.sanitized.json"
! grep -qE '100\.64\.0\.7|peer-private' "$fixture_directory"/*.sanitized.json
grep -q 'tailscale-v1.100.0-c811bb19bf3b0c89061ac7b7a073f6cd23b504d0' "$fixture_directory"/*.sanitized.json

printf '%s\n' '{"Node":{"StableID":"peer-private"},"UnexpectedIdentity":{"LoginName":"secret"}}' >"$fixture_directory/unknown-identity.json"
printf '%s\n' '{"Node":{"StableID":"peer-private"},"UserProfile":"malformed","CapMap":[]}' >"$fixture_directory/malformed-whois-types.json"
printf '%s\n' '{"TailscaleIPs":[123]}' >"$fixture_directory/malformed-status.json"
printf '%s\n' '{"Node":{"StableID":"peer-private"},"Payload":"secret"}' >"$fixture_directory/content-shaped.json"
for negative in unknown-identity malformed-whois-types malformed-status content-shaped; do
  if "$(dirname "$0")/sanitize-r3-localapi-fixture.sh" "$fixture_directory/$negative.json" "$fixture_directory/$negative.sanitized.json"; then
    echo "sanitizer accepted $negative" >&2
    exit 1
  fi
done
