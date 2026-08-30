# Linux Wayland adapter captures

`r4-wayland-adapter-v1.json` came from `capture_wayland_adapter` on
2026-08-29 at 22:14 PT. The capture ran on x86_64 Ubuntu 24.04 with Linux
6.8.0, Sway 1.9, wlroots 0.17.1, and Rust 1.97.1.

The capture used a new isolated headless Sway session. It wrote fixed
synthetic local and remote UTF-8 text, received real
`wlr-data-control-v1` selection notifications, read back the offered MIME
types, and checked process-lifetime selection revisions. The sanitizer accepts
only the closed synthetic MIME set and removes the raw MIME array from the
public fixture. A seeded unrecognized MIME value must fail sanitization. The
same capture observed an unassociated real logind query as unknown and the
adapter acted locked.

`r4-logind-transition-v1.json` came from `capture_logind_transition` on Gibson.
The capture ran the host's real `systemd-logind` binary inside a private user
and PID namespace with a private D-Bus socket and runtime directory. A minimal
private systemd-manager seam supplied only disposable scope lifecycle. The
capture bound the real `wlr-data-control-v1` interface, then the production
logind adapter read one real private session as it changed from unlocked to
locked to unlocked. The private namespace was destroyed after the capture; the
host system bus and existing sessions were never opened or changed.

It did not access a user's active Wayland display, activate a service, open a
network listener, or use private topology. The raw command outputs and their
SHA-256 values remain in the producer's owner-only evidence manifest. The
capture found no eligible confidential or transient signal, so the checked-in
hint registry remains empty.
