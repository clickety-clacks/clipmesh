# Generic Linux systemd assets

`clipmesh-agent.service` is an inactive per-user template.
`clipmesh-hub.service` is an inactive system template. Render each `@@...@@`
token outside this repository with `scripts/render-r7-packaging.py` or the
render-only Ansible playbook. The templates contain no application identity or
credential. This repository does not install, enable, or start either unit.

The desktop hub URL must use the validated numeric Tailnet `ws://` form. The
hub bind must equal a current self Tailnet address. State and control-socket
parents must be owned by the applicable service user and have mode `0700`.
