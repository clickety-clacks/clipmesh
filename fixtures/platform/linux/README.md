# Linux Wayland adapter capture

`r4-wayland-adapter-v1.json` came from `capture_wayland_adapter` on
2026-08-29 at 22:14 PT. The capture ran on x86_64 Ubuntu 24.04 with Linux
6.8.0, Sway 1.9, wlroots 0.17.1, and Rust 1.97.1.

The capture used a new isolated headless Sway session. It wrote fixed
synthetic local and remote UTF-8 text, received real
`wlr-data-control-v1` selection notifications, read back the offered MIME
types, and checked process-lifetime selection revisions. It queried the
current process through logind. Because the isolated process had no logind
session association, that real query returned unknown and the adapter acted
locked.

It did not access a user's active Wayland display, activate a service, open a
network listener, or use private topology. The raw command output and its
SHA-256 remain in the producer's owner-only evidence manifest. The sanitized
fixture SHA-256 is
`306c81b16fc194b8208866146782f014cf4aeb79cddeb47f66cc7011c24a65a2`.
The capture found no eligible confidential or transient signal, so the
checked-in hint registry remains empty.
