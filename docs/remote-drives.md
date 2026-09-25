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

Drive backends are opened at service startup. Adding or changing a Drive definition requires a service restart. An active service returns `ESTALE` when a registered definition changes; it never silently switches an existing mount to another backend. Startup opens every Drive before listener readiness and shutdown ends sessions before closing storage resources.

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

Automatic selection attempts initial QUIC establishment for three seconds. Only timeout or an explicit connection refusal can select TLS WebSocket, before obtaining or sending a token. Certificate, TLS, ALPN, protocol, credential and authentication failures fail closed. Once either transport is selected, requests and renewals never reconnect, switch transport or replay. A connection loss after a backing write can leave its outcome unknown; inspect the backing state before deciding to retry.

WebSocket RPCs are serialized on each connection, including renewal. QUIC retains concurrent streams and remains the throughput-oriented option. WebSocket envelopes contain one exactly 32-byte MRB2 header binary message, zero or more nonempty binary body messages of at most 32 KiB, and an empty binary terminator. Declared lengths and the terminator must be complete before dispatch; text, oversized or surplus body messages are rejected. Library frame/message sizes are capped at 32 KiB, the read buffer at 4 KiB, and queued writes at 64 KiB. The server acquires the existing global operation/decode quotas and an additional raw aggregate byte reservation after validating the header and before receiving any body chunk. Fixed library windows are bounded by the connection cap. Each listener independently applies `max_connections` and its transfer budgets; configuring both creates two sets of capacities.

TCP/TLS/HTTP upgrade and initial hello are limited to ten seconds; body assembly and response sends are limited to ten seconds; authenticated operations and renewals are limited to thirty seconds. Authenticated idle WebSocket sessions retain their bounded connection slot and remain usable without a periodic background reader. Shutdown cancels active requests and then closes session handles, with a thirty-second cleanup bound. Stream, session, frame, I/O and operation limits are enforced. Unknown mutation outcomes close the connection and are never replayed automatically.

## Verification

The signed OIDC QUIC, TLS WebSocket and automatic fallback integration tests mounts two logical Drives with independent permissions, rejects another Partition and untrusted TLS, checks revocation and reopens durable SQLite data. The optional native qualification also mounts both Drives through NFS, exercises create/read/write/rename/fsync and verifies read-only enforcement:

```sh
MOUNT_RS_REMOTE_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-remote-client --test quic_mount -- --nocapture
```

Native mounting requires platform NFS support and normal mount privileges. The Remote Drives CI workflow runs protocol tests on Linux and macOS and native NFS qualification on Linux. External issuer availability, cross-host networking, production storage durability and WebSocket throughput need separate qualification.

See [Remote verification](remote-verification.md) for load runners, failure tests, and bounded proof coverage.

The TLS WebSocket wire tests exercise rejected versions/subprotocols, certificate/authentication failures without fallback, frame limits, malformed and incomplete envelopes, fragmented headers with interleaved Ping, idle sessions, and bounded shutdown. Both generic and binary uncertain-write tests verify backing bytes were committed exactly once before disconnect and that the client refuses subsequent requests. These are local loopback TLS results; they do not establish cross-host or production capacity.
