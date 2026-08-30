#!/usr/bin/env bash
set -euo pipefail

readonly SCANNER_PATH='scripts/check-repository-boundary.sh'
readonly MIT_SHA256='88fd3905e3a737d61cc2e1a31e958b9c1341171ff7245c8eb84eab8daff31262'

cd "$(dirname "$0")/.."

fail() {
  printf 'repository boundary check failed: %s\n' "$1" >&2
  exit 1
}

# Scan the candidate index, excluding this scanner because it must name the
# patterns that it rejects. Output never echoes a matching value.
require_current_match_free() {
  local expression="$1"
  local description="$2"
  local case_insensitive="${3:-}"
  local path custody
  local -a grep_flags=(-E)
  [[ "$case_insensitive" != case_insensitive ]] || grep_flags+=(-i)
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    custody=tracked
    git diff --cached --quiet -- "$path" || custody=staged
    fail "$description in $custody bytes path $path"
  done < <(git grep --cached -l "${grep_flags[@]}" -- "$expression" -- . ":!$SCANNER_PATH" || true)
}

# Architecture 18's historical scope is the candidate index plus every commit
# reachable from HEAD. Only generic secrets and an optional external denylist
# use it. Current-only product-policy concepts must not become a history rail.
require_historical_match_free() {
  local expression="$1"
  local description="$2"
  local case_insensitive="${3:-}"
  local path commit
  local -a grep_flags=(-E)
  [[ "$case_insensitive" != case_insensitive ]] || grep_flags+=(-i)
  require_current_match_free "$expression" "$description" "$case_insensitive"
  while IFS= read -r commit; do
    path=$(git grep -l "${grep_flags[@]}" -e "$expression" "$commit" -- . ":!$SCANNER_PATH" | head -n 1 || true)
    path="${path#"$commit:"}"
    [[ -z "$path" ]] || fail "$description in reachable Git history commit $commit path $path"
  done < <(git rev-list HEAD -- . ":(exclude)$SCANNER_PATH")
}

git cat-file -e :LICENSE 2>/dev/null || fail 'missing MIT license'
test "$(git show :LICENSE | sha256sum | awk '{print $1}')" = "$MIT_SHA256" || fail 'LICENSE does not equal the canonical MIT text'
require_current_match_free 'Apache License|GNU GENERAL PUBLIC LICENSE|Mozilla Public License|BSD [0-9]-Clause' 'contradictory project license'

# Generic secrets use the historical scope. Topology, active surfaces, content
# canaries, and project-license checks use only the current scope.
require_historical_match_free 'cm_(dev|admin|enroll)_v1_[A-Za-z0-9_-]{43}' 'ClipMesh credential'
require_historical_match_free '-----BEGIN( [A-Z]+)? PRIVATE KEY-----' 'private key'
require_historical_match_free '[Aa]uthorization:[[:space:]]*Bearer[[:space:]]+[^[:space:]]+' 'authorization header'
require_historical_match_free 'gh[pousr]_[A-Za-z0-9_]{20,}' 'known token'
require_historical_match_free '(secret|token|password|credential)[[:space:]]*[:=][[:space:]]*[A-Za-z0-9_+/=-]{40,}' 'high-entropy secret assignment'
require_current_match_free '(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[0-1])\.[0-9]{1,3}\.[0-9]{1,3})' 'private network literal'
require_current_match_free '(^|[^0-9])0\.0\.0\.0(:[0-9]+)?([^0-9]|$)' 'active wildcard listener'
require_current_match_free 'CLIPMESH_[A-Z0-9_]*(CONTENT|PAYLOAD)[A-Z0-9_]*CANARY[A-Z0-9_]*' 'clipboard content canary'

normalize_url_host() {
  local url="$1"
  url=$(printf '%s' "$url" | tr '[:upper:]' '[:lower:]')
  url="${url#http://}"
  url="${url#https://}"
  printf '%s\n' "${url%%[/:?#]*}"
}

is_reserved_network_host() {
  case "$1" in
    github.com|example.invalid|*.example.invalid|local-tailscaled) return 0 ;;
    *) return 1 ;;
  esac
}

is_private_network_host() {
  case "$1" in
    internal|local|lan|home|corp|*.internal|*.local|*.lan|*.home|*.corp) return 0 ;;
    *) return 1 ;;
  esac
}

require_reserved_network_hosts() {
  local path url host custody
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    custody=tracked
    git diff --cached --quiet -- "$path" || custody=staged
    while IFS= read -r url; do
      host=$(normalize_url_host "$url")
      is_private_network_host "$host" && fail "private service hostname in $custody bytes path $path"
      is_reserved_network_host "$host" || fail "non-reserved network name in $custody bytes path $path"
    done < <(git show ":$path" 2>/dev/null | grep -Eio 'https?://[^/[:space:]]+')
  done < <(git grep --cached -li -E 'https?://[^/[:space:]]+' -- . ":!$SCANNER_PATH" || true)
}

require_reserved_network_hosts
require_current_match_free '(^|[^A-Za-z0-9_.-])([A-Za-z0-9-]+\.)+(internal|local|lan|home|corp)([^A-Za-z0-9_.-]|$)' 'private service hostname' case_insensitive

# The exact denylist stays owner-only. A hit identifies only its line and the
# public path or commit that needs repair; it never writes the private literal.
denylist_match() {
  local literal="$1"
  local line_number="$2"
  local path commit custody
  path=$(git grep --cached -l -F -e "$literal" -- . ":!$SCANNER_PATH" | head -n 1 || true)
  if [[ -n "$path" ]]; then
    custody=tracked
    git diff --cached --quiet -- "$path" || custody=staged
    printf 'repository boundary check failed: denylist line %s matched %s path %s\n' "$line_number" "$custody" "$path" >&2
    exit 1
  fi
  while IFS= read -r commit; do
    path=$(git grep -l -F "$literal" "$commit" -- . ":!$SCANNER_PATH" | head -n 1 || true)
    path="${path#"$commit:"}"
    [[ -z "$path" ]] || { printf 'repository boundary check failed: denylist line %s matched commit %s path %s\n' "$line_number" "$commit" "$path" >&2; exit 1; }
  done < <(git rev-list HEAD -- . ":(exclude)$SCANNER_PATH")
}

if [[ -n "${CLIPMESH_PRIVATE_DENYLIST_FILE:-}" ]]; then
  test -r "$CLIPMESH_PRIVATE_DENYLIST_FILE" || fail 'external denylist is unreadable'
  line_number=0
  while IFS= read -r literal || [[ -n "$literal" ]]; do
    line_number=$((line_number + 1))
    [[ -z "$literal" ]] || denylist_match "$literal" "$line_number"
  done < "$CLIPMESH_PRIVATE_DENYLIST_FILE"
fi
