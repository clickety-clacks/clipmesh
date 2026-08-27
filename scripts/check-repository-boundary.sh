#!/usr/bin/env bash
set -euo pipefail

readonly SCANNER_PATH='scripts/check-repository-boundary.sh'
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
  local case_insensitive="${3:-}"
  local path commit
  local -a grep_flags=(-E)
  [[ "$case_insensitive" != case_insensitive ]] || grep_flags+=(-i)
  while IFS= read -r path; do
    [[ -z "$path" ]] || fail "$description in tracked bytes path $path"
  done < <(git grep -l "${grep_flags[@]}" -- "$expression" -- . ":!$SCANNER_PATH" || true)
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    if git show ":$path" 2>/dev/null | grep "${grep_flags[@]}" -- "$expression" >/dev/null; then
      fail "$description in staged bytes path $path"
    fi
  done < <(git diff --cached --name-only --diff-filter=AM -- . ":!$SCANNER_PATH")
  while IFS= read -r commit; do
    path=$(git grep -l "${grep_flags[@]}" -e "$expression" "$commit" -- . ":!$SCANNER_PATH" | head -n 1 || true)
    path="${path#"$commit:"}"
    [[ -z "$path" ]] || fail "$description in reachable Git history commit $commit path $path"
  done < <(git rev-list HEAD -- . ":(exclude)$SCANNER_PATH")
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
require_clean_match_free '(^|[^0-9])0\.0\.0\.0(:[0-9]+)?([^0-9]|$)' 'active wildcard listener'
require_clean_match_free 'CLIPMESH_[A-Z0-9_]*(CONTENT|PAYLOAD)[A-Z0-9_]*CANARY[A-Z0-9_]*' 'clipboard content canary'
require_clean_match_free '(secret|token|password|credential)[[:space:]]*[:=][[:space:]]*[A-Za-z0-9_+/=-]{40,}' 'high-entropy secret assignment'

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
  local path commit url host
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    while IFS= read -r url; do
      host=$(normalize_url_host "$url")
      is_private_network_host "$host" && fail "private service hostname in tracked bytes path $path"
      is_reserved_network_host "$host" || fail "non-reserved network name in tracked bytes path $path"
    done < <(grep -Eio 'https?://[^/[:space:]]+' -- "$path")
  done < <(git grep -li -E 'https?://[^/[:space:]]+' -- . ":!$SCANNER_PATH" || true)
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    while IFS= read -r url; do
      host=$(normalize_url_host "$url")
      is_private_network_host "$host" && fail "private service hostname in staged bytes path $path"
      is_reserved_network_host "$host" || fail "non-reserved network name in staged bytes path $path"
    done < <(git show ":$path" 2>/dev/null | grep -Eio 'https?://[^/[:space:]]+')
  done < <(git diff --cached --name-only --diff-filter=AM -- . ":!$SCANNER_PATH")
  while IFS= read -r commit; do
    while IFS= read -r path; do
      path="${path#"$commit:"}"
      [[ -z "$path" ]] && continue
      while IFS= read -r url; do
        host=$(normalize_url_host "$url")
        is_private_network_host "$host" && fail "private service hostname in reachable Git history commit $commit path $path"
        is_reserved_network_host "$host" || fail "non-reserved network name in reachable Git history commit $commit path $path"
      done < <(git show "$commit:$path" | grep -Eio 'https?://[^/[:space:]]+')
    done < <(git grep -li -E 'https?://[^/[:space:]]+' "$commit" -- . ":!$SCANNER_PATH" || true)
  done < <(git rev-list HEAD -- . ":(exclude)$SCANNER_PATH")
}

require_reserved_network_hosts
require_clean_match_free '(^|[^A-Za-z0-9_.-])([A-Za-z0-9-]+\.)+(internal|local|lan|home|corp)([^A-Za-z0-9_.-]|$)' 'private service hostname' case_insensitive

# The exact denylist stays owner-only. A hit identifies only its line and the
# public path or commit that needs repair; it never writes the private literal.
denylist_match() {
  local literal="$1"
  local line_number="$2"
  local path commit
  if path=$(git grep -l -F HEAD -- "$literal" -- . ":!$SCANNER_PATH" | head -n 1); then
    [[ -z "$path" ]] || { printf 'repository boundary check failed: denylist line %s matched tracked path %s\n' "$line_number" "$path" >&2; exit 1; }
  fi
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    if git show ":$path" 2>/dev/null | grep -F -- "$literal" >/dev/null; then
      printf 'repository boundary check failed: denylist line %s matched staged path %s\n' "$line_number" "$path" >&2
      exit 1
    fi
  done < <(git diff --cached --name-only --diff-filter=AM -- . ":!$SCANNER_PATH")
  while IFS= read -r commit; do
    path=$(git grep -l -F "$literal" "$commit" -- . ":!$SCANNER_PATH" | head -n 1 || true)
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
