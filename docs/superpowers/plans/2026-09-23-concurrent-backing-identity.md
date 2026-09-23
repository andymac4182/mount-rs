# Concurrent Backing Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every concurrently writable metadata volume persist the identity of its immutable block backing, and reject a mismatched mount before it can acknowledge a write.

**Architecture:** A block provider creates or reads one random, immutable identity in its own database, FoundationDB keyspace, or object prefix. Metadata atomically binds that identity to a new `MRC2` mode marker and compares it in every revision CAS. An explicit offline migration scans the current `MRC1` namespace and changes the marker only if the exact metadata revision remains unchanged.

**Tech Stack:** Rust 1.95, `async-trait`, SQLite/rusqlite, PGlite/PostgreSQL wire, FoundationDB transactions, `object_store` conditional creates, UUID v4 for non-SQLite identity creation, existing Cargo workspace and native integration harnesses.

## Global Constraints

- Preserve the separate metadata, block, filesystem, SDK, CLI, and binding crates. Do not combine providers into one crate.
- Preserve `BlockId` generation, namespace format version 1, and existing block object paths. `MRC2` changes authority metadata, not file layouts.
- A fresh empty metadata volume enters `MRC2` directly. A normal mount of `MRC1` returns `EBUSY` with the offline migration command; it does not scan the volume or convert it silently.
- Conversion of `MRC1` requires all old mounts and writers to be stopped. The CLI cannot prove that across hosts; the exact-revision CAS and `MRC2` marker still fence a stale old publisher after cutover.
- Conversion reads every distinct block referenced by every `NodeData::File` in the current namespace, including retained unlinked nodes, directly from the selected backing. It checks each extent's `block_offset + length` against the returned bytes and fails on a missing, malformed, or unreadable block. SQLite's random block IDs do not provide a digest; PGlite, FoundationDB, and object blocks can additionally verify their current digest contracts.
- A migration never invents a replacement block, omits an unreadable extent, advances the metadata revision, or changes a mode marker after a revision conflict.
- SQLite concurrent databases remain on a local filesystem on macOS; an NFS/network-backed SQLite database is unsupported. Keep the existing SDK pairing restrictions and signed remote-object preflight.
- Read-only ID verification on an existing `MRC2` volume must enforce the same backing eligibility as a first claim. In particular, reopening SQLite blocks through an NFS path must still fail the local-backing guard, and object backings must use their signed clients.
- The provider-created identity is a nonzero 128-bit identifier generated with UUID v4 randomness for non-SQLite providers (with UUID version and variant bits fixed), or SQLite `randomblob(16)`; SQLite's 16 bytes are random and do not have UUID version/variant bits. It is independent of the public version-history `BlockStoreId` supplied by applications.
- No production block reconciliation deletes the authority marker. Disposable integration fixtures may delete their own marker only after disposing of their metadata volume and clients.
- The only independently mergeable preliminary phase is an additive core contract with default `ENOTSUP` methods and no runtime change. Merge the provider implementations, wrappers, driver switch, migration, and acceptance tests as one protocol PR; no partial provider rollout may make a previously supported concurrent pairing fail on `main`.

---

## File map and contract

| File | Responsibility |
| --- | --- |
| `src/storage.rs` | Typed `ConcurrentBackingId`, fail-closed trait hooks, mode-state type, no backend dependencies. |
| `filesystems/mount-rs-chunked/src/lib.rs` | Store the selected ID, establish `MRC2` before returning a concurrent driver, and check it before each CAS. |
| `filesystems/mount-rs-chunked/src/migration.rs` | One provider-neutral, explicit scan and exact-revision migration algorithm. |
| `providers/mount-rs-sqlite/src/storage.rs` | Authority row in the block DB, `backing_id` in the metadata row, mode and ID predicates in SQLite transactions. |
| `providers/mount-rs-pglite/src/storage.rs` | Authority row per block `volume_key`, metadata binding per `volume_key`, one-statement CAS and migration. |
| `providers/mount-rs-foundationdb/src/lib.rs` | Separate authority and metadata keys under the selected keyspace; mode, ID, fence, and revision in one transaction. |
| `providers/mount-rs-object-store-blocks/src/lib.rs` | Reserved marker object under the exact prefix, conditional creation, direct read-back, direct migration GET, reconciliation exclusion. |
| `providers/mount-rs-rustfs/src/lib.rs`, `providers/mount-rs-r2/src/blocks.rs`, `providers/mount-rs-aws-s3/src/lib.rs` | Require signed configuration and forward marker creation and direct verification through the shared object adapter. |
| `crates/mount-rs-observability/src/lib.rs`, `crates/mount-rs-fault-injection/src/lib.rs`, `crates/mount-rs-sdk/src/stores.rs`, `bindings/mount-rs-napi/src/lib.rs` | Forward every new authority and migration hook; a wrapper must not silently use the trait's default `ENOTSUP`. |
| `crates/mount-rs-sdk/src/filesystem.rs`, `crates/mount-rs-sdk/src/providers.rs` | Reuse split-provider validation, open providers without mounting, run migration, close owned resources. |
| `apps/mount-rs-cli/src/parser.rs`, `apps/mount-rs-cli/src/runtime.rs` | `migrate-concurrent-backing --config <path> --expected-revision <u64>`, with no native mount lifecycle. |
| `tests/concurrent_sqlite_load.rs`, `filesystems/mount-rs-chunked/tests/concurrent_cas.rs` | Reverse the known SQLite mismatch acceptance and prove startup/publication fail closed. |

The final core interfaces are:

```rust
pub struct ConcurrentBackingId([u8; 16]);
pub enum ConcurrentModeState { Legacy, Mrc1, Mrc2(ConcurrentBackingId) }

// BlockStore
async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId>;
async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()>;
async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>>;

// MetadataStore
async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState>;
async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()>;
async fn publish_bound_if_revision(
    &self, backing: ConcurrentBackingId, expected_revision: u64, namespace: Namespace,
) -> Result<u64>;
async fn migrate_mrc1_to_bound_mode(
    &self, backing: ConcurrentBackingId, expected_revision: u64,
) -> Result<()>;
```

All provider paths treat a changed or missing block marker, or a metadata ID different from the caller's ID, as `ESTALE`; `MRC1` normal open is `EBUSY`; a known revision conflict is `EAGAIN`. A backend/transport error with ambiguous commit remains an error and is never turned into a retryable conflict. The old `prepare_concurrent_mode` and `publish_if_revision` signatures can remain during implementation, but remove their provider implementations and update their tests before the protocol PR merges so a new client cannot publish an unbound `MRC1` volume through the core trait.

### Task 1: Add a typed authority ID and fail-closed trait hooks

**Files:**
- Modify: `src/storage.rs:1-12,427-552`
- Test: `src/storage.rs` unit tests

**Interfaces:** Produces `ConcurrentBackingId::from_bytes`, `as_bytes`, `to_hex`, `from_hex`, `ConcurrentModeState`, and the seven trait methods above. Consumers use the ID by value because it is `Copy`.

- [ ] **Step 1: Write failing core tests.** Add a 16-byte round trip and reject zero, uppercase, short, and nonhex persisted IDs:

```rust
#[test]
fn concurrent_backing_id_requires_canonical_nonzero_hex() {
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    assert_eq!(ConcurrentBackingId::from_hex(&id.to_hex()).unwrap(), id);
    for bad in ["00000000000000000000000000000000", "AB" , "gggggggggggggggggggggggggggggggg"] {
        assert!(ConcurrentBackingId::from_hex(bad).is_err());
    }
}
```

- [ ] **Step 2: Run `./scripts/cargo-shared test --locked -p mount-rs-core --lib concurrent_backing_id_requires_canonical_nonzero_hex`.** Expect a compile failure because the type is absent.
- [ ] **Step 3: Add the type and the exact default methods.** `from_bytes` rejects `[0;16]`; `from_hex` accepts exactly 32 lowercase ASCII hex digits, decodes two digits per byte, and delegates to `from_bytes`. Put `ConcurrentModeState` next to `LoadedMetadata`. Each new trait default returns `FsError::new(ErrorCode::Enotsup)` with the corresponding syscall, including `get_for_migration` (a cached custom block implementation is not automatically safe for a migration).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConcurrentBackingId([u8; 16]);

impl ConcurrentBackingId {
    pub fn from_bytes(bytes: [u8; 16]) -> Result<Self> {
        if bytes == [0; 16] { return Err(FsError::new(ErrorCode::Einval)); }
        Ok(Self(bytes))
    }
    pub fn as_bytes(self) -> [u8; 16] { self.0 }
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(32);
        for byte in self.0 { std::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"))
            .expect("writing into a String cannot fail"); }
        text
    }
    pub fn from_hex(text: &str) -> Result<Self> {
        fn nibble(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            }
        }
        let raw = text.as_bytes();
        if raw.len() != 32 { return Err(FsError::new(ErrorCode::Einval)); }
        let mut bytes = [0; 16];
        for index in 0..16 {
            let hi = nibble(raw[2 * index]).ok_or_else(|| FsError::new(ErrorCode::Einval))?;
            let lo = nibble(raw[2 * index + 1]).ok_or_else(|| FsError::new(ErrorCode::Einval))?;
            bytes[index] = (hi << 4) | lo;
        }
        Self::from_bytes(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentModeState { Legacy, Mrc1, Mrc2(ConcurrentBackingId) }
```

- [ ] **Step 4: Run the same core test and `./scripts/cargo-shared clippy --locked -p mount-rs-core --all-targets -- -D warnings`.** Expect both to pass. Commit `src/storage.rs` as `feat(core): add concurrent backing authority contract`. This task alone may merge because no existing runtime path calls the new hooks.

### Task 2: Persist and compare SQLite metadata and block IDs

**Files:**
- Modify: `providers/mount-rs-sqlite/src/storage.rs:168-290,802-1185,1721-1795`
- Test: `providers/mount-rs-sqlite/src/storage.rs` provider tests

**Interfaces:** Implements all Task 1 hooks. The block database owns `mount_rs_block_authority(id=1, backing_id)`; the metadata row owns nullable `backing_id`. Neither derives identity from a path or `volume_id`.

- [ ] **Step 1: Add provider tests before the new methods.** On two independent connections to one block DB, `prepare_concurrent_backing` returns the same ID; a second block DB returns a different ID. Prepare metadata with ID A, then assert `prepare_bound_concurrent_mode(B)` and `publish_bound_if_revision(B,0,namespace())` fail `ESTALE` without changing revision 0. A publishes revision 1. Also inject `write_mode='MRC2', backing_id=NULL` and require open/schema validation to fail.
- [ ] **Step 2: Run `./scripts/cargo-shared test --locked -p mount-rs-sqlite concurrent_backing -- --nocapture`.** Expect RED because the new methods still use `ENOTSUP`.
- [ ] **Step 3: Add checked schema and immutable block identity.** Append this table to `BLOCK_SCHEMA`; in `initialize_version_schema`, add a nullable `backing_id TEXT` column to `mount_rs_metadata` when absent, then accept only `(NULL,NULL)`, `(MRC1,NULL)` with the existing fence, or `(MRC2,valid nonzero ID)` with the fence. Keep existing metadata `volume_id` untouched.

```sql
CREATE TABLE IF NOT EXISTS mount_rs_block_authority (
  id INTEGER PRIMARY KEY CHECK(id=1),
  backing_id TEXT NOT NULL CHECK(length(backing_id)=32 AND backing_id!='00000000000000000000000000000000')
);
ALTER TABLE mount_rs_metadata ADD COLUMN backing_id TEXT;
```

`SqliteBlockStore::prepare_concurrent_backing` runs the existing local-file preflight, then uses one `BEGIN IMMEDIATE` transaction to `INSERT OR IGNORE INTO mount_rs_block_authority(id,backing_id) VALUES(1,lower(hex(randomblob(16))))`, read row 1, parse the ID, and commit. A reopened client reads the same row. `verify_concurrent_backing` repeats the local-file preflight, then selects row 1 without writing; missing or unequal returns `ESTALE`. `get_for_migration` reads the actual block row through `get` without a cache.
- [ ] **Step 4: Bind mode and ID in the current transaction boundaries.** `prepare_bound_concurrent_mode(A)` uses the existing fresh-row guards and one update `SET write_mode='MRC2', backing_id=:a, fence=:sentinel`; an existing `MRC2` row must have exactly A. `MRC1` returns `EBUSY` with `migrate-concurrent-backing` in its message. `publish_bound_if_revision` replaces the current `MRC1` predicate in both its transaction read and update with `write_mode='MRC2' AND backing_id=:a`; classify ID mismatch as `ESTALE` before classifying revision conflict. The migration transaction uses the SQL below and keeps `revision` and `namespace` unchanged. Check zero version rows/pins/head under the same transaction as the CAS.

```sql
UPDATE mount_rs_metadata
SET write_mode='MRC2', backing_id=:backing
WHERE id=1 AND write_mode='MRC1' AND backing_id IS NULL
  AND revision=:expected AND owner IS NULL
  AND fence=:sentinel AND expires=0;
```

- [ ] **Step 5: Run the focused provider tests, `./scripts/cargo-shared test --locked -p mount-rs-sqlite`, and strict provider Clippy.** Expect stable reopen, one authority row, correct fence, no stale publication, and all tests PASS. Commit `providers/mount-rs-sqlite/src/storage.rs` as `feat(sqlite): bind concurrent metadata to block authority`.

### Task 3: Persist and compare PGlite identities per volume key

**Files:**
- Modify: `providers/mount-rs-pglite/src/storage.rs:34-66,634-994,1657-1735`
- Modify: `providers/mount-rs-pglite/Cargo.toml` (add `uuid = { version = "1", features = ["v4"] }`)
- Test: `providers/mount-rs-pglite/src/storage.rs` provider tests, using its existing `Server` helper

**Interfaces:** Same Task 1 hooks. The block authority row is selected by the block store's own `volume_key`; metadata binding uses the metadata store's `volume_key`. Two PGlite wire servers with the same textual key still get different random IDs.

- [ ] **Step 1: Add tests with two block keys and two independently connected clients.** Assert equal identity for the same key/server, unequal identity for a different key, a mismatched metadata prepare and CAS returning `ESTALE`, and normal same-backing CAS preserving one-winner/one-known-`EAGAIN` behavior. Run `./scripts/cargo-shared test --locked -p mount-rs-pglite concurrent_backing -- --nocapture`; expect RED.
- [ ] **Step 2: Extend `BLOCK_SCHEMA` and `METADATA_SCHEMA` and implement the block hooks.** Add `backing_id TEXT` with `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` on metadata and this separate authority table on the block server:

```sql
CREATE TABLE IF NOT EXISTS mount_rs_block_authority (
  volume_key TEXT PRIMARY KEY NOT NULL,
  backing_id TEXT NOT NULL
);
```

Use `uuid::Uuid::new_v4().simple().to_string()` as the candidate in `INSERT ... ON CONFLICT(volume_key) DO NOTHING`, then select and canonical-parse the stored ID. Read the authority row again without writing in `verify_concurrent_backing`. `get_for_migration` performs the current direct SQL block read and verifies the existing `block_id(bytes)` value against the requested ID.
- [ ] **Step 3: Add `MRC2` metadata binding and CAS.** The fresh update is one autocommit statement setting `write_mode=$2, backing_id=$3, fence=$4`, with the existing fresh/version-empty predicates. A current `MRC1` row returns `EBUSY`; current `MRC2` with another ID returns `ESTALE`. Add `AND backing_id=$7` to the existing single-statement revision CAS, and return `ESTALE` when the zero-row read finds another ID. Migration is one update with `WHERE volume_key=$1 AND revision=$expected AND write_mode='MRC1' AND backing_id IS NULL AND owner IS NULL AND fence=$sentinel AND expires=0` plus the existing version-empty predicates; classify a changed revision as `EAGAIN`.
- [ ] **Step 4: Run `./scripts/cargo-shared test --locked -p mount-rs-pglite concurrent_backing -- --nocapture`, `./scripts/cargo-shared test --locked -p mount-rs-pglite`, and strict provider Clippy.** Commit `providers/mount-rs-pglite/Cargo.toml` and storage as `feat(pglite): bind concurrent volume key to block authority`.

### Task 4: Bind FoundationDB mode and keyspace identity transactionally

**Files:**
- Modify: `providers/mount-rs-foundationdb/src/lib.rs:65-82,1639-1668,1686-1738,2041-2108,2352-2450,2469-2567`
- Modify: `providers/mount-rs-foundationdb/Cargo.toml` (add `uuid = { version = "1", features = ["v4"], optional = true }` and `"dep:uuid"` to `foundationdb`)
- Test: `providers/mount-rs-foundationdb/src/lib.rs` decoder tests; `providers/mount-rs-foundationdb/tests/foundationdb.rs` and `tests/block_preflight.rs` with a real cluster

**Interfaces:** Adds `Keyspace::block_authority()` at `prefix + NUL + b"block-authority"` and `Keyspace::metadata_backing()` at `prefix + NUL + b"meta/backing-id"`. Mode key contains exactly `MRC1` or `MRC2`; the new metadata key contains 16 raw ID bytes.

- [ ] **Step 1: Add decoder and real-keyspace tests.** Invalid mode/ID pairs fail; two independently opened handles on one prefix share the block ID; a second prefix differs; metadata with prefix A refuses blocks from prefix B before publication. Run native tests through `./scripts/cargo-shared test --locked -p mount-rs-foundationdb --features foundationdb concurrent_backing -- --nocapture` against the existing disposable cluster harness; expect RED.
- [ ] **Step 2: Create the block authority in one FDB transaction.** Keep the present bounded read-only availability probe before creation. Create one UUID v4 candidate before the transaction retry closure, read `block_authority` with a conflict range, and set the 16-byte value only when missing. A concurrent creator's transaction conflicts and retries by reading the winner. `verify_concurrent_backing` is a bounded, read-only transaction on that key. `get_for_migration` directly reads the existing `block(id)` key and checks `block_id(bytes)==requested` for `sha256:` IDs.
- [ ] **Step 3: Bind metadata in one FDB transaction.** The fresh prepare reads `write_mode`, `metadata_backing`, lease and fence together; it writes mode `MRC2`, 16-byte ID, and the existing `MRCF` fence sentinel atomically. An `MRC1` marker returns `EBUSY`. The revision publication transaction reads the metadata ID in the same read version as mode/fence/manifest and requires the caller ID before comparing revision. Include the added key/read conflict bytes in `metadata_publication_affected_bytes`. Migration reads mode, ID, lease, fence, and manifest; when mode is `MRC1` and manifest revision equals the exact expected value, set `MRC2` and the ID in one commit without replacing manifest or chunks.
- [ ] **Step 4: Update legacy checks and test the fence.** `require_legacy_write_mode` rejects both `MRC1` and `MRC2`; the old decoder rejects `MRC2` as an unknown marker. Make `tests/block_preflight.rs` accept a returned identity type on success while keeping its unavailable-cluster timeout case. Run the FDB real tests with `./scripts/test-foundationdb.sh` in an owned Docker topology, plus strict feature-on Clippy. Commit FDB manifest and source as `feat(foundationdb): bind concurrent keyspace to block authority`.

### Task 5: Claim a prefix identity through signed object-store clients

**Files:**
- Modify: `providers/mount-rs-object-store-blocks/src/lib.rs:25-75,501-556,646-815,959-1240`
- Modify: `providers/mount-rs-object-store-blocks/Cargo.toml` (add UUID v4)
- Modify: `providers/mount-rs-rustfs/src/lib.rs:243-325`
- Modify: `providers/mount-rs-r2/src/blocks.rs:26-120`
- Modify: `providers/mount-rs-aws-s3/src/lib.rs:95-195`
- Test: `providers/mount-rs-object-store-blocks/src/lib.rs`, `providers/mount-rs-rustfs/tests/provider.rs`, `providers/mount-rs-r2/tests/http_interop.rs`

**Interfaces:** Reserved object `<exact-prefix>/_mount-rs-backing-id-v2` has bytes `b"MRC2" + 16-byte ID`. Export a shared `prepare_configured_backing_id(probe: &dyn ObjectStore, blocks: &ObjectStoreBlockStore) -> Result<ConcurrentBackingId>` helper and direct `ObjectStoreBlockStore::verify_concurrent_backing` and `get_for_migration`. A direct `ObjectStoreBlockStore::new` or unsigned provider wrapper still returns `ENOTSUP` on prepare.

- [ ] **Step 1: Add in-memory object tests.** Race 32 clients with distinct candidate IDs against one shared `object_store::memory::InMemory` and prefix; assert that every returned ID equals the single persisted 20-byte marker. A different prefix differs; tampering with or removing the marker makes read-only verification fail `ESTALE`; a second injected store with the same prefix gets a different ID. Run `./scripts/cargo-shared test --locked -p mount-rs-object-store-blocks backing_identity -- --nocapture`; expect RED.
- [ ] **Step 2: Add the marker helper.** Directly GET the marker first, so reopening an established prefix does not PUT the hot key; return an existing ID only after both configured probe and data-block clients read and decode the same exact 20 `MRC2+id` bytes. On `NotFound`, generate one UUID v4 candidate before bounded attempts. The current signed S3 clients use `S3ConditionalPut::ETagMatch`; send `put_opts` with `PutMode::Create` and directly GET after success, `AlreadyExists`, or `Precondition` (including a possible HTTP 409). A conflict followed by GET `NotFound` is not a winner. A lost create response is reconciled only by direct GET. [Cloudflare R2's same-key write limit](https://developers.cloudflare.com/r2/platform/limits/) is one per second; excess concurrent writes can return HTTP 429. Treat 429 as throttle rather than an existing claim: GET to resolve a possible winner, then retry a still-missing marker with bounded backoff, the same candidate, and the existing 20-second probe deadline, or fail closed. Never return the candidate until both clients directly read the stored marker. Keep the marker outside valid `b<digest>` block names, so `reconcile` never scans or deletes it. `get_for_migration` bypasses `ObjectStoreBlockCache` and checks the SHA256 content ID on full-digest block names; a legacy short block ID receives the direct bytes and range validation only.
- [ ] **Step 3: Wire only signed provider constructors.** RustFS, R2, and AWS S3 call their existing signed `probe_configured_concurrent_prefix`, then the shared identity helper; all three verify on both their configured probe client and block-data client. Their `new` constructors preserve `ENOTSUP`. Add a real RustFS test that races two separate signed clients with different candidate IDs, reopens the winning prefix after a service restart and sees the same ID, then tries a sibling prefix and fails metadata pairing before writing.
- [ ] **Step 4: Run object, RustFS, and R2 focused tests; strict Clippy and `./scripts/test-rustfs.sh` in an owned fixture.** Require a two-client create race against the actual RustFS, AWS S3, and Cloudflare R2 services before qualifying each for cross-process marker creation; validate 429/backoff behavior against R2. `providers/mount-rs-r2/tests/http_interop.rs` exercises mount-rs's own S3 gateway, whose per-session conditional lock and check/rename cannot prove atomic Create across independent server sessions or processes. Treat arbitrary R2-configured S3-compatible endpoints as unqualified until their own live two-client race and read-back pass. Update disposable prefix cleanup to remove only the fixture's reserved marker after its metadata volume is discarded; production reconciliation still retains it. Commit shared adapter and the three signed wrappers as `feat(object-blocks): bind signed prefix to immutable authority`.

### Task 6: Forward authority operations through every adapter

**Files:**
- Modify: `crates/mount-rs-observability/src/lib.rs:1092-1140,1217-1250`
- Modify: `crates/mount-rs-fault-injection/src/lib.rs:828-920,939-970`
- Modify: `crates/mount-rs-sdk/src/stores.rs:35-95,204-235`
- Modify: `bindings/mount-rs-napi/src/lib.rs:1369-1505`
- Test: each adapter's existing provider/erasure tests, `bindings/mount-rs-napi/test/chunked.mjs`

**Interfaces:** Forward `concurrent_mode_state`, `prepare_bound_concurrent_mode`, `publish_bound_if_revision`, `migrate_mrc1_to_bound_mode`, `prepare_concurrent_backing`, `verify_concurrent_backing`, and `get_for_migration` without substituting a default implementation. Keep telemetry labels bounded and credential-free.

- [ ] **Step 1: Add forwarding spies.** Wrap a mock with a fixed ID and counters; assert a wrapped prepare returns that exact ID, a wrapped metadata CAS receives it, migration read calls the inner direct reader, and an injected `FaultOperation::Publish` fault prevents a bound CAS from reporting success. Run focused adapter tests and expect RED at the trait defaults.
- [ ] **Step 2: Add direct forwards.** Use the same `observe_fs("provider.blocks", "concurrent.prepare", None, ...)` and metadata operation pattern already in observability. In fault injection, apply existing `FaultOperation::Get` to `get_for_migration` and `FaultOperation::Publish` to `publish_bound_if_revision`; forward identity capability checks without a new fault rule. In SDK erasure, forward to `inner` in both feature configurations. In N-API's manually expanded `async-trait` methods, forward each new by-value `ConcurrentBackingId` argument with the existing boxed-future lifetime pattern.
- [ ] **Step 3: Run strict tests/Clippy for `mount-rs-observability`, `mount-rs-fault-injection`, `mount-rs-sdk`, and `mount-rs-napi`; run `bindings/mount-rs-napi/test/chunked.mjs` with a freshly built addon.** Commit only these adapter files as `feat(adapters): preserve concurrent backing authority through wrappers`.

### Task 7: Make ChunkedFs use `MRC2` at open and at every CAS

**Files:**
- Modify: `filesystems/mount-rs-chunked/src/lib.rs:338-446,891-950`
- Test: `filesystems/mount-rs-chunked/tests/concurrent_cas.rs` and `tests/concurrent_sqlite_load.rs`

**Interfaces:** `ChunkedInner::concurrent_backing: Option<ConcurrentBackingId>` is `Some` only in concurrent mode. Nonconcurrent lease/open paths remain unchanged.

- [ ] **Step 1: Reverse the current mismatch test.** In `different_block_databases_cannot_read_a_shared_metadata_reference`, open A against metadata and blocks A, then attempt B against the same metadata and blocks B. Expect B's `ChunkedFs::open` to fail `ESTALE` before A writes; inspect SQLite metadata as `MRC2` with A's block authority ID; inspect B's block table as empty. A writes, shuts down, reopens with A blocks, and reads exact bytes. Delete the block authority marker from an existing `MRC2` volume and assert that reopen fails `ESTALE` without recreating the marker or advancing metadata. Also change a marker after open and assert that the next attempted write never advances metadata revision.
- [ ] **Step 2: Run `./scripts/cargo-shared test --locked -p mount-rs-core --test concurrent_sqlite_load different_block_databases_cannot_read_a_shared_metadata_reference -- --exact --nocapture`.** Expect RED because B currently opens.
- [ ] **Step 3: Change startup and publication ordering.** Read `metadata.concurrent_mode_state()` before any mutating block identity claim. For `Mrc2(id)`, directly `blocks.verify_concurrent_backing(id)` and `metadata.prepare_bound_concurrent_mode(id)`; do not Create a missing block marker. For `Mrc1`, return `EBUSY` with the explicit migration command before Create. Only for fresh `Legacy` metadata, call `blocks.prepare_concurrent_backing()`, then `metadata.prepare_bound_concurrent_mode(id)` and read-only `blocks.verify_concurrent_backing(id)` before root initialization. Make `prepare_bound_concurrent_mode(id)` idempotent when another fresh opener has already bound that same ID, and return `ESTALE` when it bound another ID. Before a fresh-root CAS and every `publish_namespace` concurrent CAS, flush staged blocks, verify the block ID directly, then call `metadata.publish_bound_if_revision(id, revision, namespace)`. Store `Some(id)` in both concurrent `ChunkedInner` construction sites; store `None` in the legacy path. A failed ID verification is a terminal, fail-closed error, never `EAGAIN` replay.

```rust
let backing = match metadata.concurrent_mode_state().await? {
    ConcurrentModeState::Mrc2(id) => {
        blocks.verify_concurrent_backing(id).await?;
        metadata.prepare_bound_concurrent_mode(id).await?;
        id
    }
    ConcurrentModeState::Mrc1 => {
        return Err(FsError::new(ErrorCode::Ebusy).with_syscall("migrate MRC1 backing"));
    }
    ConcurrentModeState::Legacy => {
        let id = blocks.prepare_concurrent_backing().await?;
        metadata.prepare_bound_concurrent_mode(id).await?;
        blocks.verify_concurrent_backing(id).await?;
        id
    }
};
let namespace = initial_namespace(&options)?;
blocks.flush().await?;
blocks.verify_concurrent_backing(backing).await?;
let revision = metadata.publish_bound_if_revision(backing, 0, namespace).await?;
```

- [ ] **Step 4: Update the `CasMetadata`, `SharedBlocks`, and revision-advancing block test doubles to retain one fixed authority ID and reject a wrong one.** Run concurrent CAS tests, the two/ four-writer SQLite load tests, and strict chunked Clippy. Commit chunked source and tests as `feat(chunked): enforce bound backing before concurrent publication`.

### Task 8: Add explicit offline `MRC1` migration to core, SDK, and CLI

**Files:**
- Create: `filesystems/mount-rs-chunked/src/migration.rs`
- Modify: `filesystems/mount-rs-chunked/src/lib.rs` (export migration function)
- Modify: `crates/mount-rs-sdk/src/filesystem.rs:65-135`
- Modify: `crates/mount-rs-sdk/src/providers.rs:109-180`
- Modify: `apps/mount-rs-cli/src/parser.rs:181-360,612-650`
- Modify: `apps/mount-rs-cli/src/runtime.rs:416-450,238-275`
- Modify: `apps/mount-rs-cli/Cargo.toml` (add `rusqlite` bundled and `tempfile` as development dependencies for the binary migration test)
- Test: `tests/concurrent_sqlite_load.rs`, `apps/mount-rs-cli/src/parser.rs`, `apps/mount-rs-cli/src/runtime.rs`, `apps/mount-rs-cli/tests/cli.rs`

**Interfaces:** `mount_rs_chunked::migrate_mrc1_backing<M: MetadataStore,B: BlockStore>(metadata: &M, blocks: &B, expected_revision: u64) -> Result<ConcurrentBackingId>`; `mount_rs_sdk::Filesystem::migrate_concurrent_backing(options: SplitOptions, expected_revision: u64) -> Result<ConcurrentBackingId>`; CLI `Command::MigrateConcurrentBacking { config: PathBuf, expected_revision: u64 }`.

- [ ] **Step 1: Write migration acceptance tests.** Construct a genuine `MRC1` SQLite volume with the old mode/fence and one referenced file. With correct blocks and exact revision, migration succeeds, keeps revision/namespace bytes, and a new `MRC2` mount reads the file. With a missing block, too-short block, different blocks DB, or changed revision, migration fails and leaves metadata in `MRC1` with no bound ID. With a historical version row, migration refuses until history is separately dealt with. Run the focused test; expect RED because migration API is absent.
- [ ] **Step 2: Implement the scan in `migration.rs`.** Read `metadata.concurrent_mode_state()` and require `Mrc1`; read and validate `metadata.load()` and require its revision equals the CLI's `expected_revision`. Prepare the block authority and collect every unique `BlockId` plus the maximum checked `block_offset + length` from every `NodeData::File` extent, including `nlink=0` nodes. Call `blocks.get_for_migration(id)` once per unique ID; reject if returned length is smaller than the maximum. Reverify the block marker, call `metadata.migrate_mrc1_to_bound_mode(id, expected_revision)`, and return ID only after its provider commit acknowledges. The provider's CAS checks the exact revision again; `EAGAIN` means no mode change.

```rust
let loaded = metadata.load().await?;
loaded.validate()?;
if loaded.revision != expected_revision { return Err(FsError::new(ErrorCode::Eagain)); }
if metadata.concurrent_mode_state().await? != ConcurrentModeState::Mrc1 {
    return Err(FsError::new(ErrorCode::Ebusy).with_syscall("migrate MRC1 backing"));
}
let backing = blocks.prepare_concurrent_backing().await?;
let mut required = BTreeMap::<BlockId, u64>::new();
if let Some(namespace) = &loaded.namespace {
    for node in namespace.nodes.values() {
        if let NodeData::File(layout) = &node.data {
            for extent in &layout.extents {
                let end = extent.block_offset.checked_add(extent.length)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                let prior = required.entry(extent.block.clone()).or_default();
                *prior = (*prior).max(end);
            }
        }
    }
}
for (id, end) in required {
    let bytes = blocks.get_for_migration(&id).await?;
    if u64::try_from(bytes.len()).map_err(|_| FsError::new(ErrorCode::Eoverflow))? < end {
        return Err(FsError::new(ErrorCode::Eio).with_syscall("verify MRC1 block extent"));
    }
}
blocks.verify_concurrent_backing(backing).await?;
metadata.migrate_mrc1_to_bound_mode(backing, expected_revision).await?;
Ok(backing)
```

- [ ] **Step 3: Expose SDK migration without constructing a driver.** Reuse `Filesystem::split`'s current concurrent pairing checks in one `validate_concurrent_split_options(&SplitOptions)` helper, then call `open_storage(&options.metadata, &options.blocks)`, the generic migration function on its erased stores, and `resources.close().await` in both success and error paths. Reject `concurrent_writes=false`. Keep the FoundationDB client network guard alive until both provider handles have dropped, following `ProviderResource::FoundationDb`'s current policy.
- [ ] **Step 4: Add the CLI command parser and runner.** Accept only `migrate-concurrent-backing --config <path> --expected-revision <u64>`; require `DriverChoice::SplitStore` and `storage.concurrent_writes=true`. Resolve the existing JSON config and environment references through `resolve_cli_options`/`split_options`. Resolve `requested_mountpoints` and call `prepare_mountpoints_before_driver` with `preopen=true` before opening providers: refuse an already mounted view and prepare its directory so SQLite placement checks use the actual filesystem's case and normalization identity. Run SDK migration, print the unchanged revision and authority ID, and exit without `AutoMount` or native transport startup. Document the stop-all-old-mounts precondition in `help_text` and `docs/macos-usage.md`.
- [ ] **Step 5: Run CLI parser/runtime tests, migration SQLite tests, strict SDK/CLI Clippy, and an actual disposable CLI invocation.** Add `migrate_concurrent_backing_cli` to `apps/mount-rs-cli/tests/cli.rs`: create a `TempDir`, initialize SQLite metadata/block files, set the fresh metadata row to `write_mode='MRC1', fence=9223372036854775807`, and write JSON `{ "version":1, "mountpoint":"<owned view>", "driver": { "kind":"splitstore", "storage": { "concurrent_writes":true, "metadata": {"kind":"sqlite","path":metadata}, "blocks": {"kind":"sqlite","path":blocks} } } }`. Invoke `env!("CARGO_BIN_EXE_mount-rs")` with `migrate-concurrent-backing --config <owned JSON> --expected-revision 0`; assert exit 0, `MRC2`, an unchanged revision and namespace, and an empty prepared view directory. Add macOS regressions with an absent view and an absent SQLite block directory whose spellings alias through case or normalization: require usage exit 2 before block creation, with the metadata mode, backing ID, revision and namespace unchanged. Run `./scripts/cargo-shared test --locked -p mount-rs-cli --test cli`. Commit core migration, SDK, and CLI files as `feat(cli): migrate verified MRC1 volume to bound MRC2`.

### Task 9: Retire unbound APIs and run provider/runtime gates

**Files:**
- Modify: `src/storage.rs` (remove the two old unbound concurrent methods)
- Modify: all provider and wrapper implementations listed in Tasks 2-6 (remove old unbound implementations; retain the new methods)
- Modify: `tests/concurrent_sqlite_load.rs`, `providers/mount-rs-sqlite/src/storage.rs`, `providers/mount-rs-pglite/src/storage.rs`, `providers/mount-rs-foundationdb/src/lib.rs`, `filesystems/mount-rs-chunked/src/lib.rs`, `filesystems/mount-rs-chunked/tests/concurrent_cas.rs` (direct test calls use bound APIs)
- Modify: `docs/code-architecture.md`, `docs/concurrent-provider-qualification-2026-09-23.md`, `docs/macos-usage.md`, `apps/mount-rs-cli/tests/native_two_process_sql.md`
- Test: workspace, macOS native two-CLI SQLite/Keyspace views, PGlite server, real FoundationDB/RustFS and signed object-store gates

**Interfaces:** The final trait has one way to prepare and publish a concurrent volume: the bound `MRC2` methods. `MRC1` remains readable only by the explicit verified migration path; an old binary's exact `MRC1` decoder rejects `MRC2` and its legacy lease fence remains blocked.

- [ ] **Step 1: Search for remaining unbound calls.** Run `rg -n 'prepare_concurrent_mode|publish_if_revision' src filesystems providers crates bindings apps tests`. Rewrite all direct successful concurrent tests with a provider-created ID and `prepare_bound_concurrent_mode`/`publish_bound_if_revision`; replace unbound provider methods with trait defaults or remove them altogether. Reject direct unbound calls through the default `ENOTSUP` contract if retained for source compatibility.
- [ ] **Step 2: Run format, strict Clippy, and the full workspace suite.** Exact commands: `./scripts/cargo-shared fmt --all -- --check`, `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings`, `./scripts/cargo-shared test --workspace --all-targets --locked`. Expect no formatting difference, no warnings, and zero failed tests. For macOS network/native tests, use the same owned fixtures and opt-in flags in `.github/workflows/ci.yml`; do not treat a fake provider test as cross-process proof.
- [ ] **Step 3: Run bounded adversarial provider gates.** Run `./scripts/cargo-shared test --locked -p mount-rs-core --test concurrent_sqlite_load` in DELETE and opt-in WAL modes; `./scripts/test-pglite.sh`; `./scripts/test-rustfs.sh`; and `MOUNT_RS_FOUNDATIONDB_TOPOLOGY=durable ./scripts/test-foundationdb.sh`. Verify two independent clients on the same identity still publish one revision at a time; wrong backing fails at open, wrong cached ID fails at CAS, marker deletion fails closed, migration missing-block and revision-race tests keep `MRC1`, and each disposable backing reopens exact bytes. Preserve known Ozone 1000-IOPS and Windows S3 latency qualifications as separate performance outcomes.
- [ ] **Step 4: Update docs and review the protocol diff.** Show the marker locations, old-client rejection, explicit migration command, the no-global-live-writer-detection precondition, and the cross-store marker/metadata atomicity limit: remote object marker cannot be committed in the same transaction as PGlite/FDB metadata, so the signed client must read it at open and again before each CAS. Reconcile only content blocks. Use `git diff --check`, inspect `git status --short` for generated files, then commit exact planned files as `docs: qualify MRC2 backing binding and migration`.
- [ ] **Step 5: Integrate one complete protocol PR.** Fetch `origin/main`, rebase the isolated branch, rerun relevant checks on the rebased head, push it, create and attach the PR, review its CI evidence, merge after functional provider/native gates pass or transparently qualify inherited performance-only no-go lanes, then fetch `origin/main` and verify the merge commit contains the protocol head. No individual provider task is merged before Tasks 7-9 finish.

## Self-review and operational limits

- Spec coverage: SQLite, PGlite, FoundationDB metadata and blocks, shared object blocks/RustFS/R2/AWS, SDK/telemetry/fault/N-API wrappers, normal open, every CAS, explicit `MRC1` migration, inverse mismatch acceptance, native/provider/load gates are each assigned above.
- The migration scans only the current `MRC1` namespace. The existing SQLite/PGlite fresh-to-`MRC1` conversion requires empty version history, and their version publication still requires the legacy lease/mode, so supported `MRC1` volumes cannot add version rows. The migration CAS nevertheless rechecks zero history rows/head/pins; a manually corrupted or unsupported volume fails closed.
- An operator must stop old clients before migration. Neither SQLite, PGlite, nor FoundationDB `MRC1` has a distributed active-mount registry; a racing old client is fenced by the atomic mode change and receives a failed publication, but no CLI can promise a graceful response to that client.
- The ID binds a selected logical block namespace. It cannot prove that two separately configured remote clients share network reachability forever. Signed provider probing and direct marker verification cover startup and publication; physical cross-host service replacement and power-loss tests remain distinct runtime qualifications.
