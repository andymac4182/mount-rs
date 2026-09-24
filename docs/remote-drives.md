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

QUIC uses TLS 1.3 and `mount-rs/1` ALPN. The application protocol and client transport interface permit a future WebSocket implementation; fallback is not implemented yet. Stream, session, frame, I/O and operation limits are enforced. Unknown mutation outcomes close the connection and are never replayed automatically.

## Verification

The signed OIDC QUIC integration test mounts two logical Drives with independent permissions, rejects another Partition and untrusted TLS, checks revocation and reopens durable SQLite data. The optional native qualification also mounts both Drives through NFS, exercises create/read/write/rename/fsync and verifies read-only enforcement:

```sh
MOUNT_RS_REMOTE_NATIVE_NFS=1 ./scripts/cargo-shared test --locked -p mount-rs-remote-client --test quic_mount -- --nocapture
```

Native mounting requires platform NFS support and normal mount privileges. The Remote Drives CI workflow runs protocol tests on Linux and macOS and native NFS qualification on Linux. External issuer availability, cross-host networking, production storage durability and future WebSocket fallback need separate qualification.

See [Remote verification](remote-verification.md) for load runners, failure tests, and bounded proof coverage.
