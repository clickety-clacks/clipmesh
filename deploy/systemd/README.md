# Generic Linux systemd user asset

`clipmesh-agent.service` is an inactive template for a per-user service.
Replace each `@@...@@` token outside this repository with a deployment-specific
value. The template contains no application identity or credential. This
repository does not install, enable, or start the unit.

The configuration path supplies the remaining generic desktop settings. The
hub URL must use the validated numeric Tailnet `ws://` form. The state and
control-socket parents must be owned by the service user and have mode `0700`.
