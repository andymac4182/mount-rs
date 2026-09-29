# Remote Drives

The remote provider mounts server-owned filesystems through the existing Linux and macOS native adapters. One client connection selects one Partition, then mounts one or more explicitly granted Drives from it. The server can host multiple Partitions. Each Drive independently uses the normal CLI storage configuration: memory, host, SQLite, or splitstore with its existing metadata and blob providers.

## Configure and run

See [example configurations](../apps/mount-rs-cli/examples/remote/). Replace the issuer, audience and exact workload claim conditions with your third-party OIDC policy. Supply a TLS certificate and private key for the service. The client verifies the certificate's server name and trusted roots; custom CA certificates supplement the public roots. `endpoint` is currently a resolved IP address and port.

```sh
mount-rs catalog-apply --config apply.json
mount-rs serve-remote --config server.json
mount-rs validate-config --config client.json
mount-rs mount --config client.json
```

`mount-remote --config client.json` is an equivalent explicit command. Remote mount options live in the config; additional mount CLI overrides are rejected. The bootstrap server config locates a dedicated SQLite service metadata catalog and its TLS listener. Drive definitions, issuer policies and explicit per-Drive grants reside in that catalog. The local `catalog-apply` command validates all backend definitions using the normal CLI parser and atomically updates the catalog only when `expected_revision` matches. It prints the new revision. Backend paths are resolved relative to the catalog document and stored as absolute paths. Storage secrets use the existing environment references or provider credentials chain.

The bootstrap server config accepts `"max_connections": 1024` for a server expecting 1,000 clients. The compatible default is 128; supported values are 1 through 16,384. This controls connection admission, while the per-connection active request limit remains 32. Size this alongside datastore pools and memory; it is not a throughput guarantee.

An optional positive `"max_active_drives"` bounds resident Drive runtimes on each server. When omitted, capacity equals the number of registered Drives (at least one). The server parses and registers all Drive plans before listener readiness, then opens a backend on its first authorized request. Backend capability and availability errors can therefore occur on that first request. Open handles and requests pin their runtime. At capacity, the server can evict an unpinned, healthy persistent compact runtime only after its actual shutdown succeeds. If no eligible runtime is available, activation returns `EBUSY`; construction or shutdown failures preserve their errors and retain affected owners. Memory and ordinary SQLite runtimes remain resident once opened. Size this limit for the storage modes and simultaneous active Drives in your deployment.

Token credentials are exactly one of:

```json
{"file":"token.jwt"}
```

```json
{"command":["token-helper","--audience","mount-rs"]}
```

The command runs directly as argv with no shell, a ten-second limit and bounded stdout. Token files are freshly read for renewal; atomically replace the file. The server verifies signatures with configured RS256/ES256 allowlists, exact issuer and audience, standard time claims and exact workload grant conditions. Renewal with a changed workload identity or failure closes the session.

## Catalog updates and handles

Every operation checks current authorization metadata. Revocation takes effect on the next operation; metadata unavailability denies access. A catalog revision change invalidates previously opened remote handles, even when the change affects another Drive; reopen files explicitly. Ordinary credential renewal preserves handles when identity and catalog revision remain stable. Catalog updates must first empty a Partition and remove its grants before deleting it.

Adding or changing a Drive definition requires a service restart. An active service returns `ESTALE` when a registered definition changes; it never silently switches an existing mount to another backend. Authorization is checked before cold activation and again before using the opened backend. Shutdown ends sessions, drains active runtimes and construction owners, then closes shared storage resources and the server cache. Failed or uncertain cleanup retains its owners and prevents a replacement service invocation in that process.

QUIC uses TLS 1.3 and `mount-rs/2` ALPN. TLS WebSocket uses the `/mount-rs` HTTP upgrade path and `mount-rs.v2` subprotocol. Both require the current v2 ClientHello; there is no older-version compatibility implementation. WebSocket uses the same credentials, dispatcher, authoritative Drive permissions, renewal, revision fencing and session handles.

### TLS WebSocket and initial fallback

Add an optional TCP listener to the service config alongside its QUIC UDP listener:

```json
{"listen":"0.0.0.0:4433","websocket_listen":"0.0.0.0:4434"}
```

Add connection selection inside the client's `driver` object:

```json
{"connection_transport":"auto","websocket_endpoint":"192.0.2.10:4434"}
```

`connection_transport` is `quic` (default), `websocket`, or `auto`. WebSocket and automatic selection require an explicit resolved `websocket_endpoint`; both use the existing `server_name` and verifying public/custom CA roots. The separate top-level `transport` still selects the native mount adapter (`auto`, `fuse`, `nfs`, `9p`). The existing Rust `RemoteConnection::connect` API remains QUIC; `connect_with_transport` accepts `ConnectionTransport` selection. `WebSocketServer` exposes the same bind/options/transfer-limits/local-address/close pattern as `RemoteServer`.

Automatic selection attempts initial QUIC establishment for three seconds. Fallback requires an initial timeout with no received UDP datagrams, before obtaining or sending a token. Any received datagram makes a later timeout fatal, including a partial or stalled TLS handshake; this deliberately fails closed even for early peer responses. Certificate, TLS, ALPN, protocol, credential and authentication failures fail closed. Once either transport is selected, requests and renewals never reconnect, switch transport or replay. A connection loss after a backing write can leave its outcome unknown; inspect the backing state before deciding to retry.

WebSocket RPCs are serialized on each connection, including renewal. QUIC retains concurrent streams and remains the throughput-oriented option. WebSocket envelopes contain one exactly 32-byte MRB2 header binary message, zero or more nonempty binary body messages of at most 32 KiB, and an empty binary terminator. Declared lengths and the terminator must be complete before dispatch; text, oversized or surplus body messages are rejected. Library frame/message sizes are capped at 32 KiB, the read buffer at 4 KiB, and queued writes at 64 KiB. The server acquires the existing global operation/decode quotas and an additional raw aggregate byte reservation after validating the header and before receiving any body chunk. Fixed library windows are bounded by the connection cap. Each listener independently applies `max_connections` and its transfer budgets; configuring both creates two sets of capacities.

TCP/TLS/HTTP upgrade and initial hello are limited to ten seconds; body assembly and response sends are limited to ten seconds; authenticated operations and renewals are limited to thirty seconds. Authenticated idle WebSocket sessions retain their bounded connection slot and remain usable without a periodic background reader. Shutdown cancels active requests and then waits for every session handle close to finish. Explicit close, revision invalidation and shutdown retain each actual asynchronous close task across caller cancellation; they never restart a partially applied close. A backing handle that delays close also delays shutdown, rather than losing ownership at a silent cleanup deadline. Stream, session, frame, I/O and operation limits are enforced. Unknown mutation outcomes close the connection and are never replayed automatically.

## Verification

The signed OIDC QUIC, TLS WebSocket and automatic fallback integration tests mounts two logical Drives with independent permissions, rejects another Partition and untrusted TLS, checks revocation and reopens durable SQLite data. The optional native qualification also mounts both Drives through NFS, exercises create/read/write/rename/fsync and verifies read-only enforcement:

```sh
MOUNT_RS_REMOTE_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-remote-client --test quic_mount -- --nocapture
```

Native mounting requires platform NFS support and normal mount privileges. The Remote Drives CI workflow runs protocol tests on Linux and macOS and native NFS qualification on Linux. External issuer availability, cross-host networking, production storage durability and WebSocket throughput need separate qualification.

See [Remote verification](remote-verification.md) for load runners, failure tests, and bounded proof coverage.

The TLS WebSocket wire tests exercise rejected versions/subprotocols, certificate/authentication failures without fallback, frame limits, malformed and incomplete envelopes, fragmented headers with interleaved Ping, idle sessions, and bounded shutdown. Both generic and binary uncertain-write tests verify backing bytes were committed exactly once before disconnect and that the client refuses subsequent requests. These are local loopback TLS results; they do not establish cross-host or production capacity.

The isolated SQLite reply loss control forwards a signed OIDC session through a TLS WebSocket relay to the actual service. After receiving the complete successful binary write reply and its terminator, the relay holds both sockets open and waits for an explicit release. While the caller still waits and before any connection, server or write-handle close, an independent fresh SQLite reader checks metadata, permissions, every byte and exact EOF. It drops its read-only handle and filesystem without close or sync, because persisted handle close itself saves a snapshot. Only after this observation does the relay suppress the whole reply and disconnect both sockets. The caller must remain uncertain with one submitted write; subsequent control, binary write and binary read calls fail closed, and the failed read leaves its buffer unchanged. After successful close-future completion and dropping the original SQLite owners, a second fresh filesystem repeats the metadata, byte and EOF checks. The fixed `MOUNT_RS_SQLITE_REPLY_LOSS` receipt includes `held_reply`, `preclose_verified_bytes` and `preclose_eof`; it qualifies local SQLite commit before disconnect and subsequent reopen behavior, without a process crash or power loss.

The automatic-selection variant composes the same fault with a real unavailable QUIC attempt. Its bound UDP socket receives initial client datagrams and sends no replies. The fixture observes the credential file absent at the first probe, then creates it only inside the validated WebSocket upgrade callback, before sending the upgrade response. Successful signed authentication therefore requires credential retrieval after transport selection. A timestamp captured at relay TCP acceptance must be at least three seconds after selection starts, preserving the existing initial QUIC deadline. Initial datagrams are counted separately and settled through a bounded 100 ms quiet interval. A contact-first UDP observer then remains active across filesystem requests, the held reply, the independent preclose check, and a 100 ms post-loss observation window. Ready UDP/TCP contact takes precedence over simultaneous completion; a terminal nonblocking UDP check also rejects queued traffic. A real ready-datagram control verifies the observation cannot hide contact behind ready success.

Schema version 2 distinguishes `initial_quic_datagrams` from `quic_datagrams=0`, whose fixed scope is `after_initial_settlement_through_post_loss_quiet`. The credential counter measures file issuance, not credential reads. The owned parent requires exactly one complete, valid receipt as well as the exact named test pass. Both variants use the SDK snapshot SQLite filesystem; compact MRC5 selection has separate signed CLI coverage and is not established by this reply-loss fixture. Cross-host behavior, unbounded future absence of traffic, process crash, power loss and production capacity remain outside these local checks.

Run it serially in an owned process gate with a private `TMPDIR` (mode `0700` on Unix) and privately captured output. Before its first asynchronous operation, the helper retains its fixture directory and writes a private `owner.json`; it never removes the directory, including on success, error, timeout or panic. The numeric receipt reports `server_close_wait_completed`, `directory_retained=1`, `directory_removed=0` and `process_cleanup_observed=0`. Canceling a close wrapper does not establish completion of the actual server or its handle close tasks. The gate may remove retained fixture directories only after observing the owned Cargo/test process reap, owned process group absence and output EOF:

```sh
CARGO_TARGET_DIR=/path/to/dedicated-cargo-cache python3 -B scripts/test-remote-failures.py wsloss
CARGO_TARGET_DIR=/path/to/dedicated-cargo-cache python3 -B scripts/test-remote-failures.py wsautoloss
```

The fixed parent sets the private fixture `TMPDIR` and opt-in flag, pins source before and after, bounds output and execution, and removes retained fixture directories only after its process barrier. Its process-group sweep is containment; it does not replace the test's close-completion or persistence observations.
