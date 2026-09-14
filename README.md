# ClipMesh

Copy on one device, paste on another. ClipMesh shares text, images, and files
between your Linux, Mac, iPhone, and iPad devices over your private Tailscale network.

## What you need

Run the ClipMesh server, called the hub, on one computer and a client on each
device you want to use. All devices need access to the same hub through Tailscale.
The hub keeps shared clipboard history and transfers files between your devices.
Linux requires a Wayland desktop.

## Getting started

1. Get ClipMesh from the [project repository](https://github.com/clickety-clacks/clipmesh).
   Installation currently requires building from source; there is no packaged download yet.
2. Connect your devices to the same Tailscale network.
3. Set up the hub on a computer that will stay available. Configure its Tailscale
   address and storage location, then start it with `clipmesh-hub --config <hub.toml>`.
4. Connect each client to that hub using `ws://<hub-tailscale-ip>:<port>/v1/stream`.
   On Linux and Mac, set `hub_url` in the client configuration and start
   `clipmesh-agent --config <agent.toml>`. On iPhone and iPad, enter the URL in
   ClipMesh's Connection settings and tap Save.
5. Copy a short piece of text on one desktop and paste it on another. On iPhone
   or iPad, tap Send to share text, or tap a history entry to copy it.

Setup references: [hub and client configuration](https://github.com/clickety-clacks/clipmesh/tree/main/deploy/config),
[Linux service setup](https://github.com/clickety-clacks/clipmesh/tree/main/deploy/systemd),
[Mac service setup](https://github.com/clickety-clacks/clipmesh/tree/main/deploy/launchd),
and [iPhone and iPad build instructions](https://github.com/clickety-clacks/clipmesh/tree/main/mobile/ClipMesh).
The desktop references provide service templates, not automatic installers.

## On Linux and Mac

Copy text, an image, or files as you normally would. ClipMesh sends the item to
your other connected desktops so you can paste it there.

Copy the files themselves in your file manager to send them. Copying a filename
or path as text sends only that text. Connecting a desktop does not replace its
clipboard with previously shared files.

## On iPhone and iPad

Open ClipMesh and tap Send to share the clipboard preview. You can also choose
files from the paperclip menu. Opening the app does not send anything or replace
your clipboard, though iOS may ask for permission to read the preview.

To receive something, find it in the shared history and tap it to copy. Files
have Copy and Share actions after downloading. You can also tap an image thumbnail
to copy it. Entries show the device they came from, and search finds text,
filenames, and device names.
