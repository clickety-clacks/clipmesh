#!/bin/sh
set -eu

fixture_directory=$(mktemp -d)
trap 'rm -rf "$fixture_directory"' EXIT
contract="$(dirname "$0")/../fixtures/protocol/r3-localapi-compatibility-v1.json"
jq '.status' "$contract" >"$fixture_directory/status.json"
jq '.whois' "$contract" >"$fixture_directory/whois.json"
"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$fixture_directory/status.json" "$fixture_directory/status.sanitized.json"
"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$fixture_directory/whois.json" "$fixture_directory/whois.sanitized.json"
! grep -qE '100\.64\.0\.7|peer-private' "$fixture_directory"/*.sanitized.json

printf '%s\n' '{"Node":{"StableID":"peer-private"},"UnexpectedIdentity":{"LoginName":"secret"}}' >"$fixture_directory/unknown-identity.json"
printf '%s\n' '{"TailscaleIPs":[123]}' >"$fixture_directory/malformed-status.json"
printf '%s\n' '{"Node":{"StableID":"peer-private"},"Payload":"secret"}' >"$fixture_directory/content-shaped.json"
for negative in unknown-identity malformed-status content-shaped; do
  if "$(dirname "$0")/sanitize-r3-localapi-fixture.sh" "$fixture_directory/$negative.json" "$fixture_directory/$negative.sanitized.json"; then
    echo "sanitizer accepted $negative" >&2
    exit 1
  fi
done
