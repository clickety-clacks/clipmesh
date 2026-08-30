#!/usr/bin/env bash
set -euo pipefail

jq -eS '
  if (keys | sort) != ([
    "clipboard_kind",
    "compositor_protocol",
    "invalid_utf8_write_preserved",
    "local_bytes_utf8",
    "local_hint",
    "local_mime_types",
    "local_revision_current_before_remote",
    "lock_state",
    "lock_state_acts_locked",
    "remote_bytes_utf8",
    "remote_hint",
    "remote_revision_current",
    "schema_version"
  ] | sort) then error("capture_shape_unrecognized") else . end |
  if .schema_version != 1 or
     .compositor_protocol != "wlr-data-control-v1" or
     .clipboard_kind != "isolated_headless_wayland" or
     .local_bytes_utf8 != "clipmesh-local-capture" or
     .remote_bytes_utf8 != "clipmesh-remote-capture\nline-two" or
     .local_hint != "Ordinary" or
     .remote_hint != "Ordinary" or
     .local_revision_current_before_remote != true or
     .remote_revision_current != true or
     .invalid_utf8_write_preserved != true or
     ((.lock_state == "Locked" or .lock_state == "Unlocked" or .lock_state == "Unknown") | not) or
     (.lock_state == "Unknown" and .lock_state_acts_locked != true) or
     (.local_mime_types | type) != "array" or
     any(.local_mime_types[]; type != "string")
  then error("capture_value_unrecognized") else . end |
  .local_mime_types |= sort
'
