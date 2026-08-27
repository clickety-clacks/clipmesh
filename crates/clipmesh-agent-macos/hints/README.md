# macOS explicit-hint registry

`registry-v1.json` is the complete macOS confidential/transient hint registry.
An entry can be added only with a sanitized real capture that proves the
operating system or source application explicitly attached that meaning to
the pasteboard entry. The entry must name that fixture in its `evidence`
field.

The registry is intentionally empty. The R5 capture found no qualifying
signal. Unknown, absent, ambiguous, source-name-only, content-derived, and
otherwise unverified declared types therefore remain ordinary.
