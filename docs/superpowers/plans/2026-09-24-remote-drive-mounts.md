# Remote Drive Mounts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Mount several server-hosted Drives from one Partition in a Linux or macOS sandbox over authenticated QUIC, using the existing native mount adapters and server-side storage backends.

**Architecture:** The server keeps versioned Partition, Drive, issuer, and grant records in a dedicated SQLite metadata catalog, distinct from each Drive's `MetadataStore`. It opens one existing `FsDriver` per Drive. A new remote `FsDriver` forwards bounded filesystem calls over QUIC after OIDC authentication, while the CLI selects one Partition and maps its Drives to native mountpoints. The catalog API permits another durable database adapter later.

**Tech Stack:** Rust 2024, Tokio, `mount-rs-core` driver traits, `mount-rs-sdk` storage factories, Quinn/Rustls, existing native mount facade, asymmetric JWT signature verification, serde.

## Global Constraints

- Preserve the current local mount and `serve-http` behavior. Do not alter the core `FsDriver` contract to add remote-specific details.
- A single CLI invocation selects exactly one Partition. Every remote session is bound to that Partition, and each mountpoint selects one Drive inside it.
- Each Drive has its own existing CLI-supported backend config. The initial service catalog is a separate durable SQLite metadata database and cannot use volatile memory. Never encode catalog JSON into the filesystem `MetadataStore::Namespace` format.
- The server is authoritative for authorization. Grants require exact Partition/Drive permission and a verified issuer, audience, and stable workload identity claim. There is no Partition-wide permission inheritance.
- The initial data transport is QUIC with TLS identity verification. WebSocket is a future implementation of the same versioned protocol; no silent downgrade is allowed for TLS, authentication, or authorization failures.
- Token sources are either a file or an argv command. Never invoke a shell, log a token, or put a token in config or process arguments.
- Reject ambiguous mutation outcomes after connection failure. Never claim a lost write was committed, and never replay it automatically.
- Run Cargo via `./scripts/cargo-shared` from the repository root. Preserve the unrelated dirty formal-verification plan.

---

## File structure

- `crates/mount-rs-remote-protocol/`: versioned bounded messages and operation/error mapping, with no network or storage dependencies.
- `crates/mount-rs-service/`: SQLite catalog adapter, issuer/grant validation, OIDC verifier, operation dispatcher, QUIC listener, and server lifecycle.
- `crates/mount-rs-remote-client/`: credential sources, QUIC connection, remote `FsDriver` and `FileHandle` implementations.
- `crates/mount-rs-sdk/src/filesystem.rs` and `apps/mount-rs-cli/src/runtime.rs`: reuse the ordinary driver-opening path for server Drives and accept a remote driver in CLI runtime without changing native mounting.
- `apps/mount-rs-cli/src/config.rs`, `parser.rs`, `runtime.rs`: remote client and service config, one-Partition multi-Drive mounts, and `serve-remote` command.
- `apps/mount-rs-cli/tests/remote_drive.rs`: two-Partition/two-Drive QUIC, authorization, and native mount qualification.

### Task 1: Durable Partition and Drive catalog

**Files:** Create `crates/mount-rs-service/src/catalog.rs` and tests; modify `Cargo.toml` and `crates/mount-rs-service/Cargo.toml`.

**Interfaces:** `CatalogSnapshot { revision, partitions, issuer_policies, grants }`; `CatalogStore::load_current() -> Result<CatalogSnapshot>`; `CatalogStore::compare_and_swap(expected_revision, next) -> Result<u64>`; `DriveKey { partition_id, drive_id }`; `SqliteCatalog::open(path) -> Result<Self>`.

- [ ] Write a test that creates two Partitions with the same Drive ID, loads them from a fresh store instance, and proves their definitions remain distinct. Add a revision-conflict test and a missing-Drive grant rejection test.
- [ ] Run `./scripts/cargo-shared test -p mount-rs-service catalog --locked`; expect failure because the catalog API is absent.
- [ ] Implement a dedicated SQLite metadata database with a one-row catalog table containing a monotonic revision and bounded JSON document. Use an immediate transaction to validate expected revision and atomically publish the next snapshot. Validate the whole candidate before commit; set a busy timeout and durable SQLite journal/sync settings. Keep an adapter trait so other durable catalog databases can be added without changing the service.
- [ ] Run the catalog tests and strict Clippy for `mount-rs-service`; confirm both pass before proceeding.
- [ ] Commit only this task's files.

### Task 2: OIDC identity and exact Drive grants

**Files:** Create `crates/mount-rs-service/src/auth/{mod.rs,oidc.rs,grants.rs}` and tests; modify crate dependencies.

**Interfaces:** `Authenticator::verify(token) -> VerifiedPrincipal { issuer, subject, stable_claims, expires_at }`; `Authorizer::permission(principal, partition_id, drive_id, catalog_revision) -> Result<Permission>`.

- [ ] Write tests with locally signed RSA and P-256 tokens that accept configured issuer/audience/stable-claim matches and reject wrong signature, algorithm, issuer, audience, time bounds, missing stable identity, unknown Drive, and read-only mutation. Include same Drive ID in another Partition.
- [ ] Run the focused auth tests; expect missing API failures.
- [ ] Implement bounded JWT parsing, pinned signing algorithms, exact issuer/audience and claim matching, and a bounded JWKS cache. Fetch only approved HTTPS discovery/JWKS targets with no redirect or private-address access, following the Kaizen gateway generic workload verifier's checks. Keep its code out of this crate.
- [ ] Implement grant evaluation over a fresh catalog revision on every operation. Loss of catalog access denies new and existing requests. Keep unknown and unauthorized targets indistinguishable externally.
- [ ] Run focused tests and strict Clippy; commit only auth files.

### Task 3: Versioned filesystem wire contract

**Files:** Create `crates/mount-rs-remote-protocol/{Cargo.toml,src/lib.rs,src/messages.rs,src/frame.rs,tests/protocol.rs}`; modify workspace `Cargo.toml`.

**Interfaces:** `PROTOCOL_VERSION`, `Request { request_id, drive_id, operation }`, `Response { request_id, result }`, `Operation` variants for native-needed `FsDriver` and `FileHandle` calls, and bounded `read_frame`/`write_frame`.

- [ ] Write tests for version mismatch, oversized length prefix, malformed body, request/response ID mismatch, filesystem error round-trip, and bounded directory/read/write payloads.
- [ ] Run `./scripts/cargo-shared test -p mount-rs-remote-protocol --locked`; expect missing API failures.
- [ ] Add the minimal versioned encoding and limits. Include guarded reads/mutations and sync barriers; preserve `FsError` codes without exposing backend secrets. Keep the operation set exhaustive for the native adapters actually used by Linux and macOS.
- [ ] Run focused tests, `cargo fmt --check`, and strict Clippy; commit protocol files.

### Task 4: QUIC service and Drive dispatcher

**Files:** Create `crates/mount-rs-service/src/{lib.rs,server.rs,dispatch.rs,registry.rs}` and tests; modify service `Cargo.toml`.

**Interfaces:** `RemoteServer::bind(options, catalog, drive_factory) -> Result<RemoteServer>`; `RemoteServer::close() -> Result<()>`; session state holds one `partition_id`, verified principal, token expiry, and opaque per-Drive handles.

- [ ] Write an in-process QUIC integration test that authenticates into one Partition, accesses two authorized Drives, rejects another Partition, denies writes to a read-only Drive, and sees a grant revocation on the next request.
- [ ] Run the focused service test; expect failure.
- [ ] Implement TLS 1.3/ALPN server setup, bounded streams and frames, version/auth handshake, per-operation catalog revision check, Drive lookup, permission classification, handle scoping, and orderly shutdown. Reuse SDK driver opening and cleanup for each Drive.
- [ ] Add tests for expired token, changed-principal renewal, malformed frame, server restart, and disconnect during an in-flight mutation. Run service tests and strict Clippy; commit service files.

### Task 5: Credential sources and remote `FsDriver`

**Files:** Create `crates/mount-rs-remote-client/{Cargo.toml,src/lib.rs,src/credentials.rs,src/connection.rs,src/driver.rs,src/handle.rs}` and tests; modify workspace `Cargo.toml`.

**Interfaces:** `CredentialSource::{File(PathBuf), Command(Vec<OsString>)}`; `RemoteFsDriver::connect(endpoint, tls, partition_id, drive_id, credentials) -> Result<Self>`; `impl FsDriver for RemoteFsDriver`; `impl FileHandle for RemoteFileHandle`.

- [ ] Write tests for file replacement, argv command capture and timeout, redacted errors, token renewal before expiry, and unchanged-principal handle preservation. Run focused tests and observe failure.
- [ ] Implement credential acquisition without a shell or token logging. Bound file/stdout size and command runtime. Verify server certificate name and configured roots. Connect with QUIC and authenticate the selected Partition.
- [ ] Write a driver test that writes, reads, renames, and syncs via a real in-process server, plus a test that disconnects during a mutation and sees I/O failure without automatic replay. Observe the first failure, then implement `FsDriver`/`FileHandle` forwarding and capability negotiation.
- [ ] Run client tests, `cargo fmt --check`, and strict Clippy; commit client files.

### Task 6: CLI catalog service and one-Partition multi-Drive mount

**Files:** Modify `apps/mount-rs-cli/{Cargo.toml,src/config.rs,src/parser.rs,src/runtime.rs,README.md}`; create `apps/mount-rs-cli/examples/{config-remote-client.json,config-remote-server.json}`.

**Interfaces:** `mount-rs serve-remote --config PATH`; `mount-rs mount --config PATH` with a remote section containing one `partition`, a `drives` array of `{id,mountpoint}`, and exactly one token source.

- [ ] Write config/parser tests that reject mixed or missing Partitions, duplicate Drive IDs/mountpoints, both token sources, insecure URL, and catalog memory provider. Run them and observe failure.
- [ ] Extend config parsing with strict unknown-field rejection and secret references. Add a server command that bootstraps the catalog and opens the Drive registry. Add remote mount runtime that constructs one remote driver per Drive, preflights all authorization before mounting any, then calls the existing native mount facade for every mountpoint.
- [ ] Add a CLI lifecycle test proving failed preflight leaves no mountpoint mounted and successful shutdown unmounts all Drive views. Run focused CLI tests, formatting, and strict Clippy; commit CLI files.

### Task 7: Cross-platform qualification and final verification

**Files:** Add `apps/mount-rs-cli/tests/remote_drive.rs`; update `apps/mount-rs-cli/README.md` with supported and unqualified behavior.

- [ ] Add a durable server restart test with two Drives in one Partition and another Partition reusing one Drive ID; prove isolation, read/write permissions, and persisted bytes.
- [ ] Add opt-in native integration cases on Linux and macOS that mount two Drives from one Partition and exercise create, read, write, rename, fsync, and unmount. Use the existing platform probe and capability gates. Do not label a skipped native case as passed.
- [ ] Run `./scripts/cargo-shared fmt --all -- --check`, then focused client/service/CLI tests and strict Clippy for touched crates. Run `./scripts/cargo-shared test --workspace --all-targets --locked` and `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings` if the environment supports them. Record exact failures and separate environment gates from code failures.
- [ ] Review the spec line by line against tests and the CLI demo, inspect the final diff for token leakage or unauthorized fallback, and commit the qualification/docs changes.

## Execution notes

Use test-first red/green cycles for every behavior. Re-read this plan after each task; adjust a later task only if an earlier verified interface differs, and record that change in the plan. Do not report remote mounts as complete until the real QUIC service, both native platforms, and permission boundaries have been verified. A skipped platform remains an explicit qualification gap.
