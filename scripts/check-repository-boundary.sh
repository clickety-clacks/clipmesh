#!/usr/bin/env bash
set -euo pipefail

readonly SCANNER_PATH='scripts/check-repository-boundary.sh'
readonly SCANNER_TEST_PATH='scripts/test-repository-boundary.sh'
readonly MIT_SHA256='88fd3905e3a737d61cc2e1a31e958b9c1341171ff7245c8eb84eab8daff31262'

cd "$(dirname "$0")/.."

fail() {
  printf 'repository boundary check failed: %s\n' "$1" >&2
  exit 1
}

# Scan every public custody surface, excluding this scanner because it must name
# the patterns that it rejects. Output never echoes a matching secret.
require_clean_match_free() {
  local expression="$1"
  local description="$2"
  if git grep -l -E -- "$expression" -- . ":!$SCANNER_PATH" ":!$SCANNER_TEST_PATH" >/dev/null; then
    fail "$description in tracked bytes"
  fi
  if git diff --cached -- . ":!$SCANNER_PATH" ":!$SCANNER_TEST_PATH" | grep -E -- "$expression" >/dev/null; then
    fail "$description in staged diff"
  fi
  if git log --all -p --format= -- . ":(exclude)$SCANNER_PATH" ":(exclude)$SCANNER_TEST_PATH" | grep -E -- "$expression" >/dev/null; then
    fail "$description in reachable Git history"
  fi
}

test -f LICENSE || fail 'missing MIT license'
test "$(sha256sum LICENSE | awk '{print $1}')" = "$MIT_SHA256" || fail 'LICENSE does not equal the canonical MIT text'
require_clean_match_free 'Apache License|GNU GENERAL PUBLIC LICENSE|Mozilla Public License|BSD [0-9]-Clause' 'contradictory project license'

# Generic secret, topology, active-listener, and content-canary rail.
require_clean_match_free 'cm_(dev|admin|enroll)_v1_[A-Za-z0-9_-]{43}' 'ClipMesh credential'
require_clean_match_free '-----BEGIN( [A-Z]+)? PRIVATE KEY-----' 'private key'
require_clean_match_free '[Aa]uthorization:[[:space:]]*Bearer[[:space:]]+[^[:space:]]+' 'authorization header'
require_clean_match_free 'gh[pousr]_[A-Za-z0-9_]{20,}' 'known token'
require_clean_match_free '(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[0-1])\.[0-9]{1,3}\.[0-9]{1,3})' 'private network literal'
require_clean_match_free '([A-Za-z0-9-]+\.)+(internal|local|lan|home|corp)' 'private service hostname'
require_clean_match_free '(^|[^0-9])0\.0\.0\.0(:[0-9]+)?([^0-9]|$)' 'active wildcard listener'
require_clean_match_free 'CLIPMESH_[A-Z0-9_]*(CONTENT|PAYLOAD)[A-Z0-9_]*CANARY[A-Z0-9_]*' 'clipboard content canary'
require_clean_match_free '(secret|token|password|credential)[[:space:]]*[:=][[:space:]]*[A-Za-z0-9_+/=-]{40,}' 'high-entropy secret assignment'

# The exact denylist stays owner-only. A hit identifies only its line and the
# public path or commit that needs repair; it never writes the private literal.
denylist_match() {
  local literal="$1"
  local line_number="$2"
  local path commit
  if path=$(git grep -l -F -- "$literal" -- . ":!$SCANNER_PATH" ":!$SCANNER_TEST_PATH" | head -n 1); then
    [[ -z "$path" ]] || { printf 'repository boundary check failed: denylist line %s matched tracked path %s\n' "$line_number" "$path" >&2; exit 1; }
  fi
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    if git show ":$path" 2>/dev/null | grep -F -- "$literal" >/dev/null; then
      printf 'repository boundary check failed: denylist line %s matched staged path %s\n' "$line_number" "$path" >&2
      exit 1
    fi
  done < <(git diff --cached --name-only --diff-filter=AM -- . ":!$SCANNER_PATH" ":!$SCANNER_TEST_PATH")
  while IFS= read -r commit; do
    path=$(git grep -l -F "$literal" "$commit" -- . ":!$SCANNER_PATH" ":!$SCANNER_TEST_PATH" | head -n 1 || true)
    [[ -z "$path" ]] || { printf 'repository boundary check failed: denylist line %s matched commit %s path %s\n' "$line_number" "$commit" "$path" >&2; exit 1; }
  done < <(git rev-list --all -- . ":(exclude)$SCANNER_PATH" ":(exclude)$SCANNER_TEST_PATH")
}

if [[ -n "${CLIPMESH_PRIVATE_DENYLIST_FILE:-}" ]]; then
  test -r "$CLIPMESH_PRIVATE_DENYLIST_FILE" || fail 'external denylist is unreadable'
  line_number=0
  while IFS= read -r literal || [[ -n "$literal" ]]; do
    line_number=$((line_number + 1))
    [[ -z "$literal" ]] || denylist_match "$literal" "$line_number"
  done < "$CLIPMESH_PRIVATE_DENYLIST_FILE"
fi
