# Generic ClipMesh configuration templates

These inactive templates contain only `@@CLIPMESH_...@@` placeholders and
bounded public defaults. Render them outside the source tree with
`scripts/render-r7-packaging.py` or the render-only Ansible playbook.

`clipmesh-hub.toml` covers the explicit Tailnet-only bind, SQLite state,
retention, payload, connection, rate, and queue limits. `clipmesh-agent.toml`
covers the numeric Tailnet hub URL, platform, owner-only state, and owner-only
control socket. Neither template contains an application identity or secret.

The agent also accepts an optional `hub_silence_timeout_seconds` (default 45,
allowed 35 to 3600). If the hub sends nothing for that long, not even its
30-second ping, the agent drops the connection, logs `hub_silent`, and
reconnects. The template leaves it unset.

Rendering does not install, load, enable, or start a service. A deployment
must validate the rendered values through the ClipMesh startup path before it
separately elects any operational action.
