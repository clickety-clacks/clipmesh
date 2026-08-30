# Render-only Ansible packaging

`render.yml` invokes the repository's closed deployment renderer on the
Ansible controller. It writes inactive configuration, systemd, and launchd
files into a new owner-only output directory. It does not install a file or
call a service manager.

Supply every required path, service-user, bind, hub URL, and platform value in
an external inventory or extra-vars file. Keep that file outside this public
repository. The bounded retention, payload, connection, rate, and queue
defaults live in `roles/clipmesh/defaults/main.yml`.

The output remains inert. Installation, service loading, enabling, starting,
listener activation, and enrollment require separate operational authority.
