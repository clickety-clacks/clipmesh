#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT

make_repo() {
  local target="$fixture_root/$1"
  mkdir -p "$target/scripts"
  cp "$repo_root/scripts/check-repository-boundary.sh" "$target/scripts/"
  cp "$repo_root/LICENSE" "$target/LICENSE"
  git -C "$target" init -q
  git -C "$target" config user.email 'fixture@example.invalid'
  git -C "$target" config user.name 'fixture'
  git -C "$target" add .
  git -C "$target" commit -qm baseline
  printf '%s\n' "$target"
}

expect_failure() {
  local name="$1"
  local target
  target=$(make_repo "$name")
  case "$name" in
    hostname) printf '%s\n' "hub.private.$(printf '%s' internal)" > "$target/seed.txt" ;;
    listener) printf '%s\n' "0.0.0.0:443" > "$target/seed.txt" ;;
    canary) printf '%s\n' "CLIPMESH_SECRET_CLIPBOARD_$(printf '%s' CONTENT)_CANARY_7f98d605" > "$target/seed.txt" ;;
    entropy) printf 'service_%s=%s\n' secret "$(printf 'A%.0s' {1..44})" > "$target/seed.txt" ;;
    license) printf '%s\n' 'This project is also licensed under the Apache License.' >> "$target/LICENSE" ;;
  esac
  git -C "$target" add .
  if "$target/scripts/check-repository-boundary.sh"; then
    printf 'expected %s seeded failure\n' "$name" >&2
    exit 1
  fi
}

for seed in hostname listener canary entropy license; do expect_failure "$seed"; done
