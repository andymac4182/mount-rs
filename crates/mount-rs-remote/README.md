# mount-rs-remote

Initial public-server session transport. Runtime dependencies contain no Redis,
TiDB or object-store client. `RemoteSession::connect` takes a server origin,
volume, client bearer credential and request timeout. HTTPS is required except
for loopback HTTP testing. URL credentials, query, fragments and path prefixes
are rejected; redirects and reqwest retries are disabled.

The transport supports bounded session controls, raw writes, capability queries
and explicit close. It preserves recognized filesystem error codes. Response
bodies are capped at 1MiB, control bodies at 8KiB and raw writes at 64KiB. Secret
headers and session Debug output are redacted. Transport failure or cancellation
after dispatch closes future admission because the operation outcome may be
unknown. Cleanup remains available explicitly; no request is automatically
replayed. Abandoned sessions depend on server idle expiry.

`RemoteFs::connect` implements `FsDriver` and returns retained remote
`FileHandle` objects. Reads and writes return partial counts at the 64KiB wire
limit; the adapter does not split or replay operations. Handle operations are
serialized, closed handles return EBADF, and decoded replies are validated.
Call `RemoteFs::shutdown` explicitly after draining the mount.

Six focused tests pass, including actual loopback HTTP trait operations,
rename/unlink handle identity, partial I/O, cancellation, redirects and truncated
responses. The CLI now selects this adapter from remote JSON configuration; a subprocess
self-test validates public-endpoint I/O and cleanup. This is not native-mount
or production TLS/AWS qualification.
Cross-ingress adapter tests, credential rotation, idle-session keepalive and
performance qualification remain open. Not every path operation has executed
network coverage.
