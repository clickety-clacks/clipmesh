# Source metadata endpoint

An admitted Tailnet client can fetch source labels with:

```http
GET /v1/source-metadata
```

The hub authenticates the socket with Tailscale WhoIs before reading HTTP,
applies the normal per-peer HTTP request bucket, and then returns:

```json
{
  "protocol_version": 1,
  "type": "source_metadata",
  "peers": [
    {"id": "<stable-id>", "display_name": "gibson"}
  ],
  "file_sources": [
    {"clip_id": "<uuid>", "source_peer_id": "<stable-id>"}
  ]
}
```

`id` is `Self.ID` or `PeerStatus.ID` from the hub host's Tailscale
`/localapi/v0/status` response. The map key is a public key and is not used as
an identity. `display_name` is the first label of the corresponding
`DNSName`, with the daemon's trailing dot removed first. The hub includes only
IDs represented by retained text or file history, so an unavailable or
unrecognized source remains an honest unknown label on the client.

The peer directory is cached for 30 seconds and capped at 1,024 entries. File
source mappings use the existing 500-entry retained file-history limit. This
endpoint does not change `clipmesh.v1` event frames or strict
`clipmesh.files.v1` history replies.
