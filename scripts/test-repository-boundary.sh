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
  printf 'clean fixture\n' > "$target/seed.txt"
  git -C "$target" init -q
  git -C "$target" config user.email 'fixture@example.invalid'
  git -C "$target" config user.name 'fixture'
  git -C "$target" add .
  git -C "$target" commit -qm baseline
  printf '%s\n' "$target"
}

expect_failure() {
  local name="$1"
  local target secret=''
  target=$(make_repo "$name")
  case "$name" in
    hostname) printf '%s\n' "hub.private.$(printf '%s' internal)" > "$target/seed.txt" ;;
    listener) printf '%s\n' "$(printf '0%s0%s0%s0' . . .):443" > "$target/seed.txt" ;;
    canary) printf '%s\n' "CLIPMESH_SECRET_CLIPBOARD_$(printf '%s' CONTENT)_CANARY_7f98d605" > "$target/seed.txt" ;;
    entropy) printf 'service_%s=%s\n' secret "$(printf 'A%.0s' {1..44})" > "$target/seed.txt" ;;
    license) printf '%s %s %s\n' 'This project is also licensed under the' "$(printf '%s%s' A pache)" "$(printf '%s%s.' Lic ense)" >> "$target/LICENSE" ;;
    staged_token) secret="ghp_$(printf 'A%.0s' {1..20})"; printf '%s\n' "$secret" > "$target/seed.txt" ;;
    private_literal) printf '%s.%s.%s.%s\n' 10 1 2 3 > "$target/seed.txt" ;;
    network) printf '%s://%s.%s\n' "$(printf '%s%s' ht tps)" docs example.com > "$target/seed.txt" ;;
    mixed_network) printf '%s://%s.%s %s://hub.%s.example.invalid\n' "$(printf '%s%s' ht tps)" docs example.com "$(printf '%s%s' ht tps)" "$(printf '%s' internal)" > "$target/seed.txt" ;;
    prefixed_network) printf '%s://%s.%s.%s\n' "$(printf '%s%s' ht tps)" example invalid docs.example.com > "$target/seed.txt" ;;
  esac
  git -C "$target" add .
  [[ "$name" != staged_token ]] || printf 'clean fixture\n' > "$target/seed.txt"
  local output
  if output=$("$target/scripts/check-repository-boundary.sh" 2>&1); then
    printf 'expected %s seeded failure\n' "$name" >&2
    exit 1
  fi
  if [[ "$name" == staged_token ]]; then
    [[ "$output" == *'staged bytes path seed.txt'* ]] || { printf 'generic result omitted staged path\n' >&2; exit 1; }
    [[ "$output" != *"$secret"* ]] || { printf 'generic result echoed token\n' >&2; exit 1; }
  fi
}

for seed in hostname listener canary entropy license staged_token private_literal network mixed_network prefixed_network; do expect_failure "$seed"; done

history_target=$(make_repo history)
history_literal="$(printf '%s.%s.%s.%s' 10 1 2 3)"
printf '%s\n' "$history_literal" > "$history_target/seed.txt"
git -C "$history_target" add seed.txt
git -C "$history_target" commit -qm secret-in-history
git -C "$history_target" rm -q seed.txt
git -C "$history_target" commit -qm remove-secret
if history_output=$("$history_target/scripts/check-repository-boundary.sh" 2>&1); then
  printf 'expected history seeded failure\n' >&2
  exit 1
fi
[[ "$history_output" == *'reachable Git history'* ]] || { printf 'history result omitted its custody surface\n' >&2; exit 1; }
[[ "$history_output" != *"$history_literal"* ]] || { printf 'history result echoed private literal\n' >&2; exit 1; }

reserved_target=$(make_repo reserved)
printf '%s://hub.%s.example.invalid\n' "$(printf '%s%s' ht tps)" "$(printf '%s' internal)" > "$reserved_target/seed.txt"
git -C "$reserved_target" add seed.txt
"$reserved_target/scripts/check-repository-boundary.sh"

denylist_target=$(make_repo denylist)
denylist_literal="owner-only-$(printf '%s' marker)"
printf '%s\n' "$denylist_literal" > "$denylist_target/seed.txt"
printf '%s\n' "$denylist_literal" > "$fixture_root/denylist.txt"
git -C "$denylist_target" add seed.txt
if denylist_output=$(CLIPMESH_PRIVATE_DENYLIST_FILE="$fixture_root/denylist.txt" "$denylist_target/scripts/check-repository-boundary.sh" 2>&1); then
  printf 'expected denylist seeded failure\n' >&2
  exit 1
fi
[[ "$denylist_output" == *'staged path seed.txt'* ]] || { printf 'denylist result omitted staged path\n' >&2; exit 1; }
[[ "$denylist_output" != *"$denylist_literal"* ]] || { printf 'denylist result echoed private literal\n' >&2; exit 1; }
