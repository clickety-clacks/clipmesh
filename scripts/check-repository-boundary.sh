#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'repository boundary check failed: %s\n' "$1" >&2
  exit 1
}

require_clean_match_free() {
  local expression="$1"
  local description="$2"
  if git grep -n -E -- "$expression" -- . ':!scripts/check-repository-boundary.sh' >/dev/null; then
    fail "$description in tracked bytes"
  fi
  if git diff --cached -U0 | grep -E -- "$expression" >/dev/null; then
    fail "$description in staged diff"
  fi
  if git log --all -p --format= -- . ':(exclude)scripts/check-repository-boundary.sh' | grep -E -- "$expression" >/dev/null; then
    fail "$description in reachable Git history"
  fi
}

test -f LICENSE || fail 'missing MIT license'
grep -qx 'MIT License' LICENSE || fail 'non-canonical license'

# The script itself names the patterns it enforces; scan every other tracked byte.
require_clean_match_free 'cm_(dev|admin|enroll)_v1_[A-Za-z0-9_-]{43}' 'ClipMesh credential'
require_clean_match_free '-----BEGIN( [A-Z]+)? PRIVATE KEY-----' 'private key'
require_clean_match_free '[Aa]uthorization:[[:space:]]*Bearer[[:space:]]+[^[:space:]]+' 'authorization header'
require_clean_match_free 'gh[pousr]_[A-Za-z0-9_]{20,}' 'known token'
require_clean_match_free '(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[0-1])\.[0-9]{1,3}\.[0-9]{1,3})' 'private network literal'

if git grep -n -E 'https?://[^/[:space:]]+' -- . ':!scripts/check-repository-boundary.sh' | \
  grep -Ev 'example\.invalid|github\.com/clickety-clacks/clipmesh|github\.com/actions|github\.com/dtolnay|github\.com/rust-lang/crates\.io-index' >/dev/null; then
  fail 'non-reserved network name in tracked bytes'
fi

if [[ -n "${CLIPMESH_PRIVATE_DENYLIST_FILE:-}" ]]; then
  test -r "$CLIPMESH_PRIVATE_DENYLIST_FILE" || fail 'external denylist is unreadable'
  line_number=0
  while IFS= read -r literal || [[ -n "$literal" ]]; do
    line_number=$((line_number + 1))
    [[ -z "$literal" ]] && continue
    if git grep -l -F -- "$literal" -- . >/dev/null; then
      printf 'repository boundary check failed: denylist line %s matched tracked path\n' "$line_number" >&2
      exit 1
    fi
    if git diff --cached -- . | grep -F -- "$literal" >/dev/null; then
      printf 'repository boundary check failed: denylist line %s matched staged diff\n' "$line_number" >&2
      exit 1
    fi
    if git log --all -p --format= -- . ':(exclude)scripts/check-repository-boundary.sh' | grep -F -- "$literal" >/dev/null; then
      printf 'repository boundary check failed: denylist line %s matched reachable history\n' "$line_number" >&2
      exit 1
    fi
  done < "$CLIPMESH_PRIVATE_DENYLIST_FILE"
fi
