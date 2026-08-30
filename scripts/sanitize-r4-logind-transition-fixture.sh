#!/usr/bin/env bash
set -euo pipefail

jq -eS '
  if (keys | sort) != ([
    "compositor_protocol",
    "locked_acts_locked",
    "platform",
    "provider",
    "schema_version",
    "session_kind",
    "states",
    "unlocked_acts_locked"
  ] | sort) then error("capture_shape_unrecognized") else . end |
  if .schema_version != 1 or
     .platform != "linux-wayland" or
     .compositor_protocol != "wlr-data-control-v1" or
     .provider != "systemd-logind" or
     .session_kind != "isolated_private_bus" or
     .states != ["Unlocked", "Locked", "Unlocked"] or
     .locked_acts_locked != true or
     .unlocked_acts_locked != false
  then error("capture_value_unrecognized") else . end
'
