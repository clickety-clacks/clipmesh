#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
check="$repo_root/scripts/check-r1-hub-policy-boundary.sh"

"$check"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/crates/clipmesh-hub-core/src"
cp "$check" "$tmp_root/check.sh"
cp "$repo_root/crates/clipmesh-hub-core/Cargo.toml" "$tmp_root/crates/clipmesh-hub-core/Cargo.toml"
cp "$repo_root/crates/clipmesh-hub-core/src/lib.rs" "$tmp_root/crates/clipmesh-hub-core/src/lib.rs"
sed -i "s#repo_root=.*#repo_root=\"$tmp_root\"#" "$tmp_root/check.sh"
printf '\npub struct AdministratorCredential;\n' >> "$tmp_root/crates/clipmesh-hub-core/src/lib.rs"

if "$tmp_root/check.sh" >/dev/null 2>&1; then
  echo "seeded obsolete surface passed the R1 boundary check" >&2
  exit 1
fi
