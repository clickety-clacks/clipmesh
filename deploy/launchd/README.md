# Generic macOS launchd asset

`com.example.clipmesh-agent.plist` is an inactive template. Replace each
`@@...@@` token outside this repository with a deployment-specific value.
The template contains no application identity or credential. `RunAtLoad` and
`KeepAlive` are false, and this repository does not load or start the job.

The configuration path supplies the remaining generic desktop settings. The
hub URL must use the validated numeric Tailnet `ws://` form. The state and
control-socket parents must be owned by the service user and have mode `0700`.
