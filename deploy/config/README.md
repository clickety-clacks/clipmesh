# Generic ClipMesh configuration templates

These inactive templates contain only `@@CLIPMESH_...@@` placeholders and
bounded public defaults. Render them outside the source tree with
`scripts/render-r7-packaging.py` or the render-only Ansible playbook.

`clipmesh-hub.toml` covers the explicit Tailnet-only bind, SQLite state,
retention, payload, connection, rate, and queue limits. `clipmesh-agent.toml`
covers the numeric Tailnet hub URL, platform, owner-only state, and owner-only
control socket. Neither template contains an application identity or secret.

Rendering does not install, load, enable, or start a service. A deployment
must validate the rendered values through the ClipMesh startup path before it
separately elects any operational action.
