# Remote Drive mounts over authenticated QUIC

## Goal and scope

A sandbox running the mount-rs CLI can mount a named Drive hosted by a mount-rs service on Linux or macOS. The service owns each Drive's `FsDriver` and its metadata and blob storage. One service hosts multiple Drives. A verified workload identity may have read or write access to one or more of them. The initial network transport is QUIC; a later WebSocket transport must carry the same application protocol. The CLI obtains an OIDC JWT from a configured file or command.

This adds a remote **filesystem driver** to the existing native mount flow. It does not make the sandbox a metadata or block-storage client. Each native mount selects one Drive ID; several mounts may use one server. Existing local drivers and `mount-rs-auto` continue to select Linux/macOS kernel adapters as they do today.

## Components and ownership

1. `mount-rs-remote` contains a client `RemoteFsDriver`, the versioned filesystem request/response types, transport interface, and QUIC implementation. The client exposes `Arc<dyn FsDriver>` to the existing mount path and owns no authoritative namespace or blob cache.
2. A remote service owns a registry of unique Drive IDs, each mapped to an existing server-side `FsDriver`. Each Drive uses the same driver and storage configuration choices already available to the CLI: memory, host, SQLite, or split-store with its supported metadata and block providers. Different Drives may use different choices and separate backing storage. The service reuses the existing driver-opening path instead of creating another storage implementation. Drive IDs are independent of storage paths and never supplied to a backend as unchecked paths.
3. The service has an `Authenticator` boundary producing a verified principal and expiry, and an `Authorizer` boundary mapping that principal plus Drive ID to `Read` or `Write`. The first authenticator verifies third-party OIDC JWTs. Later authentication methods implement the same boundary without changing filesystem or transport code.
4. The CLI's credential source is either `file` or `command`. File mode rereads an atomically replaced file. Command mode executes an argv array directly, with no shell, captures bounded stdout, and treats nonzero exit, timeout, empty output, or malformed token as failure. Secrets are never put in CLI arguments, logs, debug output, or config files.

The existing loopback `serve-http` API and its static per-Drive tokens remain a separate interface. The remote service does not expose it on the QUIC listener or inherit its bearer-token policy.

## Connection and filesystem protocol

The client config supplies a server URL, trusted server name or CA roots, Drive ID, credential source, and native mount options. TLS 1.3 certificate and hostname validation are mandatory; insecure verification and plaintext remote listeners are not supported. QUIC uses a dedicated ALPN value and protocol version. Version negotiation rejects unsupported versions before filesystem requests.

The protocol uses bounded, length-delimited messages with a request ID, operation, payload, and typed success or filesystem error. It covers the `FsDriver` and `FileHandle` operations needed by native mounts, including stat/lookup, bounded directory reads, open/read/write/close, metadata mutations, guarded reads and mutations, and sync barriers. Negotiated capabilities reflect the selected server driver; the client must not advertise atomic guarded operations or durability that the server cannot provide. Open handles are opaque, scoped to a Drive and authenticated session, and invalid after that session ends. Every path is normalized and confined to the selected Drive before dispatch.

One request may be in flight on each QUIC stream; independent streams allow concurrent filesystem calls. Both ends bound message size, read/write chunk size, directory entries, active streams, session count, and operation time. The client maps transport failure to a filesystem I/O error rather than reporting an operation as successful. It never automatically replays a mutation whose outcome is unknown after a broken connection. Reads may retry after a fresh authenticated session. Reopening a file handle requires a new lookup/open and normal identity checks; a stale handle is not silently rebound to a different object.

A `RemoteConnection` interface performs version negotiation, authentication, request exchange, and close. QUIC implements it first. WebSocket can implement the same interface and message encoding later. A future automatic fallback may occur only for an identified reachability failure before authentication or filesystem operations; authentication, certificate, protocol-version, and authorization errors never trigger a downgrade. The first release does not claim WebSocket availability.

## Authentication and authorization

The server configuration has an allowlist of issuer policies. Each policy pins the exact HTTPS issuer, one or more non-wildcard audiences, supported signing algorithms, and exact claim conditions that include a stable workload identity beyond `iss` and `aud`. The server fetches OIDC discovery/JWKS from approved public HTTPS endpoints with bounded responses, no redirects to private destinations, cache expiry, and bounded refresh. It verifies signature and key ID, `iss`, `aud`, `sub`, `exp`, `iat`, optional `nbf`, and maximum token lifetime before evaluating grant conditions. The Kaizen gateway generic workload OIDC implementation is the reference for these checks, not a library dependency on gateway internals.

Each grant binds one issuer policy and exact claim conditions to explicit `{drive_id, permission}` entries. `Read` permits only nonmutating operations. `Write` permits reads and mutations. The service checks the effective permission before every operation, including handle-based writes, truncation, metadata changes, and sync. The grant is never inferred from a client-supplied identity or path. Multiple matching grants may combine permissions for the same principal and Drive, but any ambiguous identity-policy match is rejected. Unknown Drives and unauthorized Drives produce an indistinguishable denial at the protocol boundary.

Authentication happens at session establishment and on renewal. The service checks the verified token expiry and current grant for every request; it does not use a once-authenticated connection indefinitely. The CLI refreshes before expiry through its configured source, then reauthenticates. Renewal by the same verified workload identity preserves open handles, subject to the current Drive permission. A changed identity invalidates them. If refresh fails or a grant is removed, further operations fail closed until a fresh valid token and grant are available. Existing handles cannot outlive the authorized session. Grant changes take effect when the service reloads its validated configuration; a failed reload preserves the last valid policy and reports an error.

## Configuration and operations

The CLI accepts a `remote` driver config rather than adding a new native mount type. It requires one Drive ID and exactly one token source. A command is an argv array, never a shell string. The remote server has a separate `serve-remote` config with listener TLS identity, named Drives using the ordinary CLI driver configurations, issuer policies, and grants. Configuration validation rejects duplicate Drive IDs, unknown grant Drive IDs, wildcard audiences, missing stable identity conditions, unsupported algorithms, and insecure endpoint settings before binding or mounting. Startup opens and validates all configured Drive backends before announcing readiness; shutdown drains sessions and closes owned backends.

Access logs include Drive ID, grant ID, operation class, outcome, and request ID without JWTs, raw file data, or credential-command output. Authentication failures have bounded diagnostic categories. Resource exhaustion rejects new work without unbounded allocation.

## Verification and release boundary

Unit tests cover config rejection, JWT signature/issuer/audience/time/claim failures, key rotation, command and file credential renewal, permission checks on every operation family, and non-leaking errors. Protocol tests cover version mismatch, malformed/oversized frames, concurrent requests, disconnect during mutation, stale handles, and reconnect. Integration tests run a QUIC server with two Drives and principals with read, write, and no access; verify data isolation and persistence after restart with a durable backend. Native tests mount a remote Drive and exercise create/read/write/rename/fsync/unmount on Linux and macOS, using each platform's existing supported adapter and capability gates.

Passing local tests establishes the implemented protocol and platform behavior in those environments. Production storage durability, cross-host network behavior, external issuer availability, and WebSocket fallback require their own qualification before being claimed.
