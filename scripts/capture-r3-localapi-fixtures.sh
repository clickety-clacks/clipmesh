#!/bin/sh
# Capture only from the test-local daemon simulator. This script never starts
# a listener and refuses the system Tailscale socket. Fresh Tailnet captures
# are deferred to R7 by the R3 boundary ruling.
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: CLIPMESH_TEST_LOCALAPI_SOCKET=/path/to/simulator.sock CLIPMESH_TEST_REMOTE_ADDR=127.0.0.1:1234 $0 OUTPUT_DIRECTORY" >&2
  exit 64
fi

: "${CLIPMESH_TEST_LOCALAPI_SOCKET:?set the test-local daemon socket}"
: "${CLIPMESH_TEST_REMOTE_ADDR:?set the accepted process-local remote address}"

case "$CLIPMESH_TEST_LOCALAPI_SOCKET" in
  /var/run/tailscale/*) echo "refusing the system Tailscale socket" >&2; exit 65 ;;
esac

output_directory=$1
umask 077
mkdir -p "$output_directory"

curl --fail --silent --show-error --unix-socket "$CLIPMESH_TEST_LOCALAPI_SOCKET" \
  http://example.invalid/localapi/v0/status >"$output_directory/status.raw.json"
curl --fail --silent --show-error --unix-socket "$CLIPMESH_TEST_LOCALAPI_SOCKET" \
  "http://example.invalid/localapi/v0/whois?addr=$CLIPMESH_TEST_REMOTE_ADDR" \
  >"$output_directory/whois.raw.json"

"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$output_directory/status.raw.json" "$output_directory/status.sanitized.json"
"$(dirname "$0")/sanitize-r3-localapi-fixture.sh" \
  "$output_directory/whois.raw.json" "$output_directory/whois.sanitized.json"
rm "$output_directory/status.raw.json" "$output_directory/whois.raw.json"
