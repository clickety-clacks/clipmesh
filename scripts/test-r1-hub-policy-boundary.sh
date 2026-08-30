#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
check="$repo_root/scripts/check-r1-hub-policy-boundary.sh"

"$check"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

(cd "$repo_root" && git ls-files -z | tar --null --files-from=- -cf -) | tar -xf - -C "$tmp_root"
git -C "$tmp_root" init -q
git -C "$tmp_root" add .
mkdir -p "$tmp_root/crates/seeded-current-scope/src"
printf 'pub struct AdministratorCredential;\n' > "$tmp_root/crates/seeded-current-scope/src/lib.rs"
git -C "$tmp_root" add crates/seeded-current-scope/src/lib.rs

if "$tmp_root/scripts/check-r1-hub-policy-boundary.sh" >/dev/null 2>&1; then
  echo "seeded obsolete surface outside the hub crate passed the current-tree census" >&2
  exit 1
fi

history_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root" "$history_root"' EXIT
(cd "$repo_root" && git ls-files -z | tar --null --files-from=- -cf -) | tar -xf - -C "$history_root"
git -C "$history_root" init -q
git -C "$history_root" config user.email 'fixture@example.invalid'
git -C "$history_root" config user.name 'fixture'
git -C "$history_root" add .
git -C "$history_root" commit -qm baseline
mkdir -p "$history_root/crates/removed-authority/src"
printf 'pub struct AdministratorCredential;\n' > "$history_root/crates/removed-authority/src/lib.rs"
git -C "$history_root" add crates/removed-authority/src/lib.rs
git -C "$history_root" commit -qm obsolete-authority
rm "$history_root/crates/removed-authority/src/lib.rs"
git -C "$history_root" add -u
git -C "$history_root" commit -qm remove-obsolete-authority
"$history_root/scripts/check-r1-hub-policy-boundary.sh"
