#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
capture_binary="$repo_root/target/debug/examples/capture_logind_transition"

if [[ "$(uname -s)" != Linux ]] ||
  ! command -v bwrap >/dev/null ||
  ! command -v dbus-daemon >/dev/null ||
  [[ ! -x /usr/lib/systemd/systemd-logind ]] ||
  [[ ! -x "$capture_binary" ]] ||
  [[ -z "${WAYLAND_DISPLAY:-}" ]] ||
  [[ "${CLIPMESH_WAYLAND_CAPTURE_ISOLATED:-}" != 1 ]] ||
  [[ ! -d "${XDG_RUNTIME_DIR:-}" ]] ||
  [[ ! -O "$XDG_RUNTIME_DIR" ]] ||
  [[ "$(stat -c '%a' "$XDG_RUNTIME_DIR")" != 700 ]] ||
  [[ ! -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]]; then
  echo 'isolated_logind_capture_unavailable' >&2
  exit 1
fi

bwrap \
  --unshare-user --uid 0 --gid 0 \
  --unshare-pid --unshare-ipc --unshare-uts --unshare-net \
  --ro-bind / / --proc /proc --dev /dev --tmpfs /run --tmpfs /tmp \
  --ro-bind "$XDG_RUNTIME_DIR" "$XDG_RUNTIME_DIR" \
  --setenv XDG_RUNTIME_DIR "$XDG_RUNTIME_DIR" \
  --setenv WAYLAND_DISPLAY "$WAYLAND_DISPLAY" \
  --setenv CLIPMESH_WAYLAND_CAPTURE_ISOLATED 1 \
  -- /bin/bash -c '
    set -euo pipefail
    mkdir -p /run/dbus /run/systemd/system /run/user/0
    printf "isolated\n" >/run/clipmesh-r4-private-logind
    export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket
    dbus-daemon --session --address="$DBUS_SYSTEM_BUS_ADDRESS" \
      --nofork --nopidfile >/tmp/dbus.log 2>&1 &
    dbus_pid=$!
    timeout 10 /bin/bash -c \
      "until busctl --system list >/dev/null 2>&1; do :; done"
    /usr/bin/python3 '"$repo_root"'/scripts/r4-private-systemd1.py \
      >/tmp/systemd1.log 2>&1 &
    manager_pid=$!
    timeout 10 /bin/bash -c \
      "until busctl --system introspect org.freedesktop.systemd1 /org/freedesktop/systemd1 >/dev/null 2>&1; do :; done"
    SYSTEMD_LOG_TARGET=console SYSTEMD_LOG_LEVEL=warning \
      /usr/lib/systemd/systemd-logind >/tmp/logind.log 2>&1 &
    logind_pid=$!
    cleanup() {
      kill "$logind_pid" "$manager_pid" "$dbus_pid" 2>/dev/null || true
      wait "$logind_pid" "$manager_pid" "$dbus_pid" 2>/dev/null || true
    }
    trap cleanup EXIT
    timeout 10 /bin/bash -c \
      "until busctl --system introspect org.freedesktop.login1 /org/freedesktop/login1 >/dev/null 2>&1; do :; done"
    CLIPMESH_LOGIND_CAPTURE_ISOLATED=1 \
      '"$capture_binary"'
  '
