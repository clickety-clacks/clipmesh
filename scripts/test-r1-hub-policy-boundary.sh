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
