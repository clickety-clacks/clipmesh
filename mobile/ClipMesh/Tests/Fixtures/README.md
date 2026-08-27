# R6 Rust frame provenance

`rust-hub-frames-v1.jsonl` was captured from the production Rust serializers at
ClipMesh commit `95745a285c109260edcf6655e72d5c0e68dce6c3` on 2026-08-27 at 02:06 PT.
The capture ran an ignored test at the production hub-edge serializer seam
with Cargo 1.93.1. It printed the real `server_hello` and `event_frame`
outputs without binding a listener or contacting a hub.

Structural sanitization replaced the two random process UUIDs with the reserved
ClipMesh fixture UUID namespace. The peer ID and clip text were already
synthetic. No private topology, credential, live response, or raw identity value
entered this fixture.
