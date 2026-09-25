# W21 storage, versioning, and communication reference review

Reviewed 2026-09-20 against the source list in [`REFERENCES.md`](../../REFERENCES.md) and the W21 checklist in [`WORK_TRACKER.md`](../../WORK_TRACKER.md). This is a source-backed review packet, not a change to either tracking file. A W21 task ID below is a follow-up mapping; it is not a claim that the tracker item is complete.

The compatibility baseline is mount-rs's existing Apache-2.0 project license. No reference source was added as a dependency, and no reference source code was copied, translated, vendored, or committed. Where a source revision, license file, implementation, or benchmark could not be verified, the status is marked explicitly rather than inferred.

## Status and decision summary

| Reference | Review pin or version | License evidence and compatibility | Review status | Main follow-ups |
| --- | --- | --- | --- | --- |
| [agentfs](https://github.com/tursodatabase/agentfs/tree/0a014ebd4918615baff589ed17486e557e7c6a23) | Commit `0a014ebd4918615baff589ed17486e557e7c6a23` | README states MIT; the advertised `LICENSE.md` was not present at this pin, so code reuse is not license-cleared. | Reviewed with license verification gap | W14, W15, W23, W28 |
| [Archil](https://docs.archil.com/getting-started/introduction) | Public documentation fetched 2026-09-20; no public revision exposed | Service documentation was reviewed; no code license was identified and no code reuse is proposed. | Reviewed, unversioned documentation | W12, W14, W15, W17, W22 |
| [Tensorlake repository](https://github.com/tensorlakeai/tensorlake/tree/b397fbb7e73a168b4eb9df45216464f4cdac3c43) / TLFS | Repository commit `b397fbb7e73a168b4eb9df45216464f4cdac3c43`; public TLFS docs unversioned; macOS TLFS implementation says private | Repository LICENSE is Apache-2.0. Product docs/article are evidence only, not code permission. | Repository/CLI reviewed; TLFS implementation blocked | W12, W14, W15, W23, W28 |
| [Changed-byte snapshots](https://www.tensorlake.ai/blog/firecracker-disk-snapshots-o-changed-bytes) | Article fetched 2026-09-20; published/modified 2026-07-13 | Vendor article; no code reuse. | Reviewed as a vendor benchmark claim, not independently verified | W14, W23, W28 |
| [CrabBuild](https://github.com/crabbuild) | `crab` `f9495a2cf4ea0e9245cfc645f02c027ae37298c3`; `prolly` `6ee959eaed2625bf086ae0c4d1a2b5934f7e3872`; `silo` `7f71a06b0560fb1ef85c3aa57bcac365dbe9d7be`; `trail` `9823ed7551a1c53bb1a2c1dc329d9faf30e6f460` | `crab` Apache-2.0; `prolly`, `silo`, and `trail` MIT at the reviewed pins. Compatible in principle; no code/dependency reuse. | Reviewed at pinned revisions | W02, W14, W18, W23, W28 |
| [SlateDB](https://github.com/slatedb/slatedb/tree/85199863ffc140a69ab5616c01e8e2d557b2ca89) | Commit `85199863ffc140a69ab5616c01e8e2d557b2ca89` | Pinned LICENSE is Apache-2.0. | Reviewed at pinned revision | W02, W03, W14, W18, W28 |
| [SQLite VFS](https://www.sqlite.org/vfs.html) | Official documentation fetched 2026-09-20; SQLite documentation is rolling rather than a repository commit | SQLite is public-domain software according to its [official copyright page](https://www.sqlite.org/copyright.html). No SQLite code was copied. | Reviewed from primary API/WAL documentation | W03, W12, W15, W28 |
| [Cloudflare ArtifactFS](https://github.com/cloudflare/artifact-fs/tree/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7) | Commit `2b87a48691ef4ae82d391b7bbe4976c06c7fadf7` | Pinned LICENSE is Apache-2.0. | Feature/source review complete; comparable benchmark blocked | W10, W14, W18.4, W23, W28 |
| [Erlang/OTP gen_statem](https://www.erlang.org/doc/system/statem.html) | OTP `OTP-29.1` / documentation version 29.1 | Pinned `LICENSE.txt` is Apache-2.0. No Erlang code was copied. | Reviewed at pinned documentation/source tag | W12, W15, W17, W22, W28 |
| [Rivet Actors](https://github.com/rivet-dev/actors/tree/78336a1a0ee33bb45e6b15e89963cbe91713353b) | Commit `78336a1a0ee33bb45e6b15e89963cbe91713353b` | Pinned LICENSE is Apache-2.0; the existing review confirms no code copied. | Reviewed; detailed packet is [`docs/reviews/rivet-actors.md`](rivet-actors.md) | W07, W12, W15, W28 |
| [Apache Ozone](https://github.com/apache/ozone/tree/a86b889dd539a927c9f5d6ff7a4bd0dfdf04a84b) | Release tag `ozone-2.2.1`, peeled commit `a86b889dd539a927c9f5d6ff7a4bd0dfdf04a84b`; harness image is architecture-digest pinned | Release LICENSE is Apache-2.0. No Ozone code copied. | Service contract and release/license reviewed; broader Ozone internals not claimed | W02, W06, W07, W08, W26, W28 |
| [RustFS](https://github.com/rustfs/rustfs/tree/3738b32df14ab42021cd0ca2f282d67430c5ec87) | Source commit `3738b32df14ab42021cd0ca2f282d67430c5ec87`; harness image `rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff` | Pinned source LICENSE is Apache-2.0. The image is treated as a service fixture, not as source-license proof. | Service contract reviewed; source and image pins are deliberately recorded separately | W02, W06, W07, W08, W26, W28 |
| ZeroFS | No review pin; the attempted repository URL returned `Repository not found` | Explicitly excluded per scope as AGPL; no code, dependency, or license text may be reused. | Excluded / unreviewed / blocked | None; do not reopen without a verified canonical source and license review |

## Review method and constraints

- A primary source means the upstream repository, upstream API/documentation, or the project's own pinned integration fixture. Marketing and vendor benchmark claims are labeled as such.
- A commit or release pin is used whenever the upstream exposes one. Live documentation without a revision is labeled unversioned and is not treated as an immutable contract.
- “Adopt” means a design lesson is suitable for mount-rs. “Adapt” means it requires mount-rs's existing split metadata/block, lease, SQLite, or provider semantics. “Reject” means the reference's behavior would be unsafe or out of scope for mount-rs.
- The review does not close W21 by editing the tracker. It supplies the evidence and follow-up IDs needed for that decision.

## 1. agentfs

Primary sources: [pinned README](https://github.com/tursodatabase/agentfs/blob/0a014ebd4918615baff589ed17486e557e7c6a23/README.md), [specification](https://github.com/tursodatabase/agentfs/blob/0a014ebd4918615baff589ed17486e557e7c6a23/SPEC.md), and the [Rust overlay implementation](https://github.com/tursodatabase/agentfs/blob/0a014ebd4918615baff589ed17486e557e7c6a23/sdk/rust/src/filesystem/overlayfs.rs).

### Verified evidence

- The spec identifies a SQLite-backed AgentFS format, audit/tool-call records, separate namespace/data structures, and fixed-size data chunks whose configuration is persisted. The tool-call audit log is insert-only rather than an update/delete history.
- The overlay combines a read-only base with a writable delta. Copy-up, whiteouts, origin metadata, and stable inode/path mappings are explicit implementation concepts; the Go overlay also documents a bounded path-cache option.
- The README describes a portable SQLite database, queryable history, and snapshot/fork workflows. Those are useful product properties, but copying a SQLite file is not by itself proof of an atomic snapshot across mount-rs's split metadata and block providers.

### Decisions

- **Adopt:** explicit namespace/data separation; immutable persisted format/chunk configuration; insert-only provenance/audit records where mount-rs needs auditability; and a clear base-plus-delta model for a future overlay.
- **Adapt:** whiteouts, origins, and copy-up to version-layer metadata only after the complete-manifest version protocol is correct. Map this to W14/W23 and test cancellation/recovery under W28.
- **Reject:** treating one SQLite file or `cp agent.db snapshot.db` as a durable split-store publication; treating in-memory overlay maps as authority; and labeling a copied live WAL database an application-consistent SQLite snapshot. SQLite-specific work remains under W12/W15.

### License status

The pinned README says MIT and links to `LICENSE.md`, but that file was not present at the pinned commit. MIT is therefore a stated upstream claim, not a verified file-level clearance for code reuse. No AgentFS code or dependency is used by mount-rs. Recheck the license file before any future incorporation (W21.8).

## 2. Archil

Primary sources: [introduction](https://docs.archil.com/getting-started/introduction), [architecture](https://docs.archil.com/details/architecture), [branches and checkpoints](https://docs.archil.com/concepts/branches-and-checkpoints), [shared disks](https://docs.archil.com/concepts/shared-disks), and [S3 API](https://docs.archil.com/protocols/s3-api).

### Verified evidence

- Archil presents a centralized durable cache between clients and an S3 data source, shared disks, strong read-after-write behavior for connected clients, and asynchronous source synchronization.
- A checkpoint is an immutable filesystem state and a branch is a writable fork from a checkpoint. The documentation says branching/checkpointing is not generally available on disks synchronized to a data source such as S3.
- Shared-disk write delegation has explicit exclusive/shared/conditional modes. Delegations are path locks; force checkout after a crash can lose in-progress writes. The S3 API documents `PutObject` commit-before-response, active-mount conflict responses, and no S3 object-versioning/object-lock substitute.

### Decisions

- **Adopt:** explicit checkpoint IDs, immutable historical reads, branch/fork ancestry, and writer/read delegation boundaries. Map to W14 and the lease/lock coverage in W12/W15.
- **Adapt:** source synchronization and remote write visibility into mount-rs's pending/active publication protocol; “source sync completed” cannot stand in for durable metadata activation. Map to W17/W22.
- **Reject:** global same-path last-writer-wins as the default for SQLite or other application transactions; treating a branch name or shared credential as a security boundary; and assuming a connected-client read-after-write guarantee is a crash/restart guarantee.

### License/status boundary

The fetched material is live service documentation, not a pinned source repository. No public documentation license suitable for code reuse was verified. It is used only for behavioral comparison; no Archil code or dependency is incorporated.

## 3. Tensorlake, TLFS, and changed-byte snapshots

Primary sources: [Tensorlake repository at the reviewed commit](https://github.com/tensorlakeai/tensorlake/tree/b397fbb7e73a168b4eb9df45216464f4cdac3c43), its [Apache-2.0 LICENSE](https://github.com/tensorlakeai/tensorlake/blob/b397fbb7e73a168b4eb9df45216464f4cdac3c43/LICENSE), the public [TLFS introduction](https://docs.tensorlake.ai/filesystems/introduction), [core concepts](https://docs.tensorlake.ai/filesystems/core-concepts), [concurrent writes](https://docs.tensorlake.ai/filesystems/concurrent-writes), [architecture](https://docs.tensorlake.ai/filesystems/architecture), the pinned [snapshot CLI command](https://github.com/tensorlakeai/tensorlake/blob/b397fbb7e73a168b4eb9df45216464f4cdac3c43/crates/cli/src/commands/sbx/snapshot.rs), and the [changed-byte Firecracker article](https://www.tensorlake.ai/blog/firecracker-disk-snapshots-o-changed-bytes).

### Verified evidence and blocked surface

- Public TLFS documentation describes versioned/time-travelled cloud volumes, server WAL/autosave checkpoints, durable snapshot promotion, immutable content sharing, historical reads, and read-only pinned views. It documents a split control plane (metadata/history) and data plane (content-addressed blobs with lazy reads/prefetch).
- The public concurrent-write policy says disjoint paths merge while overlapping same-path writes are ordered last-writer-wins. That policy is a useful contrast, not a mount-rs decision: SQLite needs an explicit conflict or participant-level commit result.
- The pinned CLI snapshot command distinguishes local readiness from server completion and polls the operation status; a completed snapshot requires a snapshot URI. This is evidence for an explicit asynchronous operation state, not proof of the private filesystem implementation.
- The pinned repository's `platform/macos/tlfs/README.md` says the macOS TLFS portion is private and not part of the repository. The implementation was therefore not source-reviewed. Public docs are live and unversioned.
- The changed-byte article describes a paused Firecracker VM sealing an overlay, tracking dirty and known-zero 4 KiB blocks, uploading immutable content-addressed layers/manifests, and compacting layer depth. Its numerical results are Tensorlake's own benchmark; no independent reproduction was run here. A VM/device dirty bitmap does not cover filesystem namespace completeness or SQLite application consistency.

### Decisions

- **Adopt:** separate operation states for sealed/local-ready, durably published, and acknowledged; immutable layers/manifests; operation IDs for ambiguous outcomes; and measurements that distinguish pause time, durable completion, changed bytes, total state, backlog, and layer depth. Map to W14/W23/W28.
- **Adapt:** dirty/zero tracking only as a future physical optimization after complete namespace manifests and SQLite participant barriers are correct. Map to W12/W15 and W23.
- **Reject:** treating a private/unversioned TLFS implementation as verified; using a Firecracker block bitmap as a filesystem snapshot authority; claiming the vendor benchmark as mount-rs performance evidence; and adopting silent same-path LWW for the core publication path.

### License/status boundary

The public repository is Apache-2.0. The TLFS implementation and product documentation/article are not additional code-reuse grants. No Tensorlake code is incorporated. The changed-byte benchmark remains a source-backed vendor claim only.

## 4. CrabBuild: `crab`, `prolly`, `silo`, and `trail`

The following is the bounded review already aligned with the versioned-filesystem design; each row is pinned to a primary repository revision and has a verified license file.

| Project and primary pin | License | Verified lesson | Mount-rs decision and task IDs |
| --- | --- | --- | --- |
| [`crab` at `f9495a2`](https://github.com/crabbuild/crab/tree/f9495a2cf4ea0e9245cfc645f02c027ae37298c3) ([LICENSE](https://github.com/crabbuild/crab/blob/f9495a2cf4ea0e9245cfc645f02c027ae37298c3/LICENSE)) | Apache-2.0 | Immutable chunks/manifests precede mutable references; publication validates generation/digest and uses leases, CAS, release-after-commit, and explicit GC/doctor roots. | **Adopt** immutable-before-mutable ordering, digest validation, CAS, fencing, and explicit roots. **Adapt** to `BlockId`, `Namespace`, provider flush, and `WriterLease`. **Reject** Git-ref/pointer-clone semantics and CDC for the first fixed-size format. W02/W14/W23/W28. |
| [`prolly` at `6ee959e`](https://github.com/crabbuild/prolly/tree/6ee959eaed2625bf086ae0c4d1a2b5934f7e3872) ([LICENSE](https://github.com/crabbuild/prolly/blob/6ee959eaed2625bf086ae0c4d1a2b5934f7e3872/LICENSE)) | MIT | Immutable ordered-map roots, format/config digests, structural patches, named roots, prune-before-reachability-GC, and stable cursors. | **Adopt** root separation, format validation, and prune/GC ordering. **Adapt** because an FS namespace includes inode/link/sparse/layout semantics. **Reject** a map-only snapshot and a new Prolly/CDC dependency now. W14/W18/W23. |
| [`silo` at `7f71a06`](https://github.com/crabbuild/silo/tree/7f71a06b0560fb1ef85c3aa57bcac365dbe9d7be) ([LICENSE](https://github.com/crabbuild/silo/blob/7f71a06b0560fb1ef85c3aa57bcac365dbe9d7be/LICENSE)) | MIT | Immutable payload/root/commit records sit behind a mutable branch ref; authority stamps fence writers; operation IDs reconcile ambiguous commits; historical reads use explicit commits. | **Adopt** immutable versions, mutable head separation, operation identity, fencing, historical selection, and authoritative GC roots. **Adapt** to one volume timeline and split providers. **Reject** whole-file payload assumptions, implicit merges, and LWW publication. W02/W14/W28. |
| [`trail` at `9823ed7`](https://github.com/crabbuild/trail/tree/9823ed7551a1c53bb1a2c1dc329d9faf30e6f460) ([LICENSE](https://github.com/crabbuild/trail/blob/9823ed7551a1c53bb1a2c1dc329d9faf30e6f460/LICENSE)) | MIT | A changed-path ledger separates trusted scope from untrusted callbacks and models prepared/applied/published/acknowledged/aborted intents, snapshot/restore fences, active views, and in-flight GC roots. | **Adopt** cut/fence/operation states, unknown-outcome recovery, active roots, and restore fencing. **Adapt** to `ChunkedFs` plus a database-aware SQLite participant. **Reject** watchers/dirty paths as snapshot authority and importing a CDC schema. W12/W14/W23/W28. |

The common result is narrow: durable immutable data must be validated before an authoritative mutable head points at it, and GC must follow authoritative roots. These sources do not justify a new dependency, CDC pipeline, content-defined chunker, or automatic merge policy in mount-rs. No CrabBuild source code was copied.

## 5. SlateDB

Primary sources: [pinned repository](https://github.com/slatedb/slatedb/tree/85199863ffc140a69ab5616c01e8e2d557b2ca89), [README](https://github.com/slatedb/slatedb/blob/85199863ffc140a69ab5616c01e8e2d557b2ca89/README.md), [manifest store](https://github.com/slatedb/slatedb/blob/85199863ffc140a69ab5616c01e8e2d557b2ca89/slatedb/src/manifest/store.rs), [manifest invariants](https://github.com/slatedb/slatedb/blob/85199863ffc140a69ab5616c01e8e2d557b2ca89/slatedb/src/manifest/invariants.rs), [cache policy](https://github.com/slatedb/slatedb/blob/85199863ffc140a69ab5616c01e8e2d557b2ca89/slatedb/src/cached_object_store/policy.rs), and [cache storage](https://github.com/slatedb/slatedb/blob/85199863ffc140a69ab5616c01e8e2d557b2ca89/slatedb/src/cached_object_store/storage.rs).

### Verified evidence

- SlateDB is an embedded LSM using object storage, with a memory/WAL durability boundary distinct from later SST/manifest flushes. The README documents `await_durable()` and `flush()` as different levels of completion.
- `FenceableManifest` initializes writer/compactor epochs and uses conditional manifest publication. Checkpoints reference a manifest ID and update the manifest core atomically; manifest/GC invariants define a safe cutoff rather than deleting arbitrary history.
- Cache policy intentionally bypasses or invalidates caching for manifests, WAL, coordination, and retrying reads, while allowing read-through caching for immutable compacted SSTs. This is a direct cache-coherence lesson for mutable heads and provider coordination.

### Decisions

- **Adopt:** an explicit durable boundary, fenced publication, immutable-object versus mutable-head cache separation, and retry refetch after uncertain reads. Map to W02/W03/W14/W28.
- **Adapt:** LSM/compaction ideas only to provider metadata and future physical layout; mount-rs's namespace manifests and block references remain the logical version contract. Map to W14/W18.
- **Reject:** using SlateDB as a filesystem namespace or dependency; equating an object-store upload with SQLite `fsync`; and caching mutable head/WAL/coordination state as if it were immutable SST data.

This was the 2026-09-20 reference-review decision. The later, explicit
SlateDB integration adds `mount-rs-slatedb` as a single-writer metadata
provider and a key-value filesystem store. Its RustFS benchmark and current
limitations are recorded in `benchmarks/slatedb-rustfs/README.md`; the older
review does not describe the newer implementation status.

### License status

The pinned LICENSE is Apache-2.0 and is compatible in principle with mount-rs.
At the time of this reference review, no SlateDB code or dependency was incorporated.

## 6. SQLite VFS and WAL boundary

Primary sources: SQLite's [VFS overview](https://www.sqlite.org/vfs.html), [`sqlite3_vfs`](https://www.sqlite.org/c3ref/vfs.html), [`sqlite3_io_methods`](https://www.sqlite.org/c3ref/io_methods.html), [WAL documentation](https://www.sqlite.org/wal.html), and [copyright/licensing](https://www.sqlite.org/copyright.html).

### Verified evidence

- The VFS is a thin operating-system-facing interface; SQLite's pager owns transactions and journal logic. VFS shims can layer behavior, and registered VFS objects have lifecycle/name/thread-safety constraints.
- `sqlite3_io_methods` requires deliberate handling of reads, writes, sync, lock/unlock, and WAL shared-memory methods. A short read must zero-fill the remainder and return `SQLITE_IOERR_SHORT_READ`; no-lock behavior is unsafe for multi-connection databases.
- WAL uses `-wal` and `-shm` state, one writer, and ordered checkpoint syncs. The official documentation says WAL normally requires shared memory and that VFS `xShm*` methods are required for WAL. `synchronous=FULL` and `NORMAL` have different sync guarantees.

### Decisions

- **Adopt:** keep logical filesystem/version APIs above a strict mount-free VFS contract; test lifecycle, random/short reads, zero-fill, truncate, sync, locks, access/delete, error mapping, and registration/shim behavior. Map to W03/W12/W15/W28.
- **Adapt:** expose only the journal modes and durability guarantees the selected backend can actually provide; treat backend fencing and ambiguous commit as explicit outcomes.
- **Reject:** no-op locks, claiming an asynchronous/remote backend is SQLite-safe merely because it implements reads/writes, and claiming WAL support without real `xShm*` and checkpoint/reopen tests. No SQLite code was copied.

## 7. Cloudflare ArtifactFS

Primary sources: [pinned repository](https://github.com/cloudflare/artifact-fs/tree/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7), [README](https://github.com/cloudflare/artifact-fs/blob/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7/README.md), [generation publication](https://github.com/cloudflare/artifact-fs/blob/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7/internal/snapshot/store.go), [overlay/COW](https://github.com/cloudflare/artifact-fs/blob/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7/internal/overlay/store.go), [hydrator](https://github.com/cloudflare/artifact-fs/blob/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7/internal/hydrator/hydrator.go), and [watcher](https://github.com/cloudflare/artifact-fs/blob/2b87a48691ef4ae82d391b7bbe4976c06c7fadf7/internal/watcher/watcher.go).

### Verified evidence

- ArtifactFS is a beta Go/FUSE Git-backed filesystem. Its SQLite snapshot metadata records a base commit/generation; a local overlay stores writes, copy-on-write blobs, and whiteouts.
- Hydration has a priority queue, bounded workers, cancellation, and deduplicated waiters. The README describes manifest/source-first availability, verified commit/digest acquisition, and an explicit readiness gate before hydration.
- `PublishGeneration` writes base nodes and current-generation/head metadata in one SQLite transaction. The watcher polls a Git ref; that is a source-refresh mechanism, not a durable version authority for mount-rs.

### Decisions and benchmark boundary

- **Adopt:** generation records, metadata-readiness before byte hydration, priority/prefetch with deduplicated waiters, local COW/whiteouts, verified source commit/digest, and an explicit readiness state. Map to W10/W14/W23/W28.
- **Adapt:** replace Git commit/ref semantics with mount-rs immutable namespace manifests, block digests, and provider publication. Keep FUSE availability separate from mount-free/remote consumers.
- **Reject:** Git HEAD polling as the authoritative version commit, FUSE-only availability, and the inference that metadata readiness means durable content bytes are already present.
- **Unreviewed/blocked:** no comparable ArtifactFS benchmark was fetched or run with a mount-rs dataset, hardware, filesystem, or runtime. W18.4 must not claim a performance win; a future benchmark must publish dataset, source revision, hydration/prep cost, bytes, memory, latency, and cache state.

### License status

The pinned LICENSE is Apache-2.0 and is compatible in principle with mount-rs. No ArtifactFS code or dependency is incorporated.

## 8. Erlang `gen_statem`

Primary sources: the [OTP 29.1 `gen_statem` documentation](https://www.erlang.org/doc/system/statem.html), its pinned [OTP source](https://github.com/erlang/otp/blob/OTP-29.1/system/doc/design_principles/statem.md), and the [Apache-2.0 license](https://github.com/erlang/otp/blob/OTP-29.1/LICENSE.txt).

### Verified evidence

The behavior is an explicit event-driven state machine: state plus event produces actions and a next state. The engine owns state/data, timers, postponed events, internal events, and replies. The documentation distinguishes state-enter, state/event/generic timeouts, cancellation, `postpone`, `hibernate`, immediate replies, and later transition actions.

### Decisions

- **Adopt:** write an explicit operation lifecycle and event matrix for capture, flush, publish, ambiguous result, lease expiry, restart, and recovery. Keep replies distinct from durable state transitions and test timeout cancellation/expiry deterministically. Map to W12/W15/W17/W22/W28.
- **Adapt:** use a Rust coordinator/state enum and bounded event queues; translate timeout/lease identities into mount-rs operation IDs and fences.
- **Reject:** depending on the Erlang runtime, treating a timeout as evidence that a commit did or did not reach durable storage, or using postponed messages as unbounded persistence. Communication events still need durable recovery records where the outcome is ambiguous.

## 9. Rivet Actors

The detailed source review is [`docs/reviews/rivet-actors.md`](rivet-actors.md). Primary source pin: [Rivet Actors at `78336a1a0ee33bb45e6b15e89963cbe91713353b`](https://github.com/rivet-dev/actors/tree/78336a1a0ee33bb45e6b15e89963cbe91713353b), with the [pinned Apache-2.0 license](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/LICENSE).

### Verified source paths and findings

The existing review inspected [`depot-client/src/vfs.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot-client/src/vfs.rs), [`depot-client-embedded/src/lib.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot-client-embedded/src/lib.rs), commit [`apply.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot/src/conveyer/commit/apply.rs), [`publish.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot/src/conveyer/commit/publish.rs), [`stage.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot/src/conveyer/commit/stage.rs), [`universaldb/src/database.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/universaldb/src/database.rs), [`universaldb/src/error.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/universaldb/src/error.rs), the Postgres driver commit/transport/resolver, and [`depot/src/recovery.rs`](https://github.com/rivet-dev/actors/blob/78336a1a0ee33bb45e6b15e89963cbe91713353b/engine/packages/depot/src/recovery.rs).

- Rivet's relevant abstraction is UniversalDB, not a direct FoundationDB client. The inspected manifests include `foundationdb-tuple`, while the reviewed UDB implementation uses Postgres/RocksDB drivers.
- `io_sync` waits for dirty-page flush and backend commit. Staged commit thresholds, finalization, PIDX/head/versionstamp/quota publication, stable operation IDs, deduplication, epoch fences, and `NotCommitted`/retry/transaction-too-old error paths are explicit.
- The lost-commit-response test models an unknown outcome that later fails at a head-fence check. Recovery is bounded and fails closed. The no-op SQLite `xLock` shape is only safe under a narrow single-actor invariant and is not portable to mount-rs.

### Decisions

- **Adopt:** stable operation IDs, explicit sync/commit boundaries, fencing, unknown-outcome reconciliation, and bounded fail-closed recovery. Map to W07/W12/W15/W28.
- **Reject:** adopting no-op SQLite locks outside a proven single-actor/single-connection invariant, or treating a successful local response as proof that a remote publication is visible.
- **License:** Apache-2.0 is compatible in principle; the detailed review confirms no Rivet code or dependency was copied or added.

## 10. Apache Ozone and RustFS service references

These references are included as real S3-compatible service evidence, not as a claim that S3 semantics replace mount-rs's SQLite/versioning contracts.

### Apache Ozone

Primary sources: [Ozone 2.2.1 source release](https://github.com/apache/ozone/tree/a86b889dd539a927c9f5d6ff7a4bd0dfdf04a84b), its [Apache-2.0 license](https://github.com/apache/ozone/blob/a86b889dd539a927c9f5d6ff7a4bd0dfdf04a84b/LICENSE.txt), and the [official Ozone Docker package](https://github.com/apache/ozone-docker/pkgs/container/ozone).

The mount-rs harness pins `ghcr.io/apache/ozone:2.2.1-all-in-one` to `sha256:88cf042bc3b810a66a85ab3fcd7b1558a44bb9d914a28d64e30338ac6780b9a6` on `linux/amd64` and `sha256:c7ba6ee740323de7da970d8b8ea373d43c42fe22d31d72077092f5511c5d83ed` on `linux/arm64`. The repository's [`tests/ozone/README.md`](../../tests/ozone/README.md) and [`scripts/test-ozone.sh`](../../scripts/test-ozone.sh) record the exact fixture and local-loopback boundary.

The real gateway contract covers full/range reads, missing-object mapping, create-only publication, stale conditional read/write rejection, successful ETag CAS, concurrent puts, bounded stopped-service failure, and fresh-client reopen after service restart. The separate mixed metadata-provider composition lane remains independently gated. This is strong provider-contract evidence; it is not a review of every Ozone internal durability mode.

### RustFS

Primary sources: [RustFS source at the reviewed commit](https://github.com/rustfs/rustfs/tree/3738b32df14ab42021cd0ca2f282d67430c5ec87), its [Apache-2.0 license](https://github.com/rustfs/rustfs/blob/3738b32df14ab42021cd0ca2f282d67430c5ec87/LICENSE), the [S3 compatibility matrix](https://github.com/rustfs/rustfs/blob/3738b32df14ab42021cd0ca2f282d67430c5ec87/docs/architecture/s3-compatibility-matrix.md), and the [release image listing](https://hub.docker.com/r/rustfs/rustfs/tags).

The mount-rs harness pins `rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, then tests the real loopback service for immutable publication, stale conditionals, full/range reads, missing-object errors, concurrent puts, exact-ID cleanup, and persistence across a service restart. The harness explicitly calls this single-node/single-disk evidence, not a power-loss or distributed durability claim; see [`tests/rustfs/README.md`](../../tests/rustfs/README.md) and [`scripts/test-rustfs.sh`](../../scripts/test-rustfs.sh).

The source commit and image release pin are intentionally separate. A current source checkout proves the Apache-2.0 license and source context; the harness image digest proves which service artifact the integration gate uses. No unverified claim is made that the image digest corresponds to the source commit, and no code is copied.

### Decisions

- **Adopt:** real-provider tests for create-only immutable blocks, conditional CAS, range reads, bounded errors, restart/reopen, and explicit provider capability reporting. Map to W02/W06/W07/W08/W26/W28.
- **Reject:** treating S3 compatibility as SQLite locking/WAL durability, treating a successful object response as a complete namespace publication, or treating a single-node restart as crash/power-loss evidence.

## 11. Explicit ZeroFS exclusion

ZeroFS is explicitly excluded from this review and from all mount-rs design/code/dependency reuse because the requested scope identifies it as AGPL. The [ZeroFS site](https://www.zerofs.net/) itself identifies the repository's AGPL-3.0 licensing, but the attempted canonical repository lookup at `https://github.com/zerofs/zerofs.git` returned `Repository not found` on 2026-09-20. No revision, source tree, or standalone license text was therefore verified here. This is deliberately recorded as **excluded / unreviewed / blocked**, not filled in from assumptions.

## Cross-source decisions and follow-up ledger

| Cross-source decision | Concrete evidence | Follow-up task IDs |
| --- | --- | --- |
| Publish immutable blocks/manifests before a mutable head; use digest validation, CAS, operation IDs, and fencing. | CrabBuild, SlateDB, Rivet, Ozone/RustFS conditional contracts | W02, W07, W14, W26, W28 |
| Keep version identity/history separate from physical COW or changed-byte optimization. | AgentFS overlays, Tensorlake layers, ArtifactFS generations, CrabBuild roots | W14, W18.4, W23, W28 |
| Treat SQLite application consistency as a participant/barrier problem, not a generic filesystem snapshot claim. | SQLite VFS/WAL docs; AgentFS, Tensorlake, ArtifactFS, and Rivet boundaries | W03, W12, W15, W28 |
| Model communication as explicit durable state plus bounded recovery, not merely a response or timeout. | gen_statem transitions/timeouts; Tensorlake snapshot polling; Rivet lost-ack/recovery; SlateDB fencing | W12, W15, W17, W22, W28 |
| Make lazy hydration/readiness/cache policy explicit and measurable. | ArtifactFS hydrator/readiness; Tensorlake lazy data plane; SlateDB cache bypass rules | W10, W18.4, W23, W28 |
| Qualify provider semantics with real pinned services and keep service evidence separate from hosted/release acceptance. | Ozone 2.2.1 and RustFS 1.0.0 harnesses; existing W26 composition boundary | W06, W07, W08, W26, W28 |
| Do not incorporate any reference code until the license file and attribution obligations are verified at the exact pin. | AgentFS license-file gap; Apache/MIT/public-domain sources; explicit ZeroFS exclusion | W21.8, W18, W26 |

## Unreviewed or blocked items that remain open

- AgentFS's MIT claim is present in its pinned README, but the advertised `LICENSE.md` is absent at that pin; no code reuse is cleared.
- Archil documentation and Tensorlake public TLFS documentation are live/unversioned; the private TLFS implementation was not available for review.
- The changed-byte article is a vendor benchmark, not an independently reproduced mount-rs result.
- ArtifactFS feature/source comparison is reviewed, but W18.4's comparable benchmark is blocked until a named dataset, hardware/runtime, source revision, and measurement protocol exist.
- Ozone/RustFS service harness evidence does not qualify hosted, power-loss, or distributed durability beyond the exact tests described above.
- ZeroFS remains explicitly excluded and unreviewed.

## Verification ledger

The source fetches used for this packet included pinned repository/tree/blob requests, official SQLite/Erlang documentation requests, and `git ls-remote` tag checks. Exact notable outcomes were:

- `agentfs` commit `0a014ebd4918615baff589ed17486e557e7c6a23`, Tensorlake commit `b397fbb7e73a168b4eb9df45216464f4cdac3c43`, SlateDB commit `85199863ffc140a69ab5616c01e8e2d557b2ca89`, ArtifactFS commit `2b87a48691ef4ae82d391b7bbe4976c06c7fadf7`, Rivet commit `78336a1a0ee33bb45e6b15e89963cbe91713353b`, and the four CrabBuild pins above were fetched or resolved.
- Ozone tag `ozone-2.2.1` resolved to `a86b889dd539a927c9f5d6ff7a4bd0dfdf04a84b`; the release `LICENSE.txt` begins with Apache License 2.0. RustFS source `LICENSE` at the reviewed commit begins with Apache License 2.0; its exact service image digest is taken from the repository harness.
- Erlang `OTP-29.1/LICENSE.txt` begins with Apache License 2.0. SQLite VFS/WAL/API/copyright pages were fetched from `sqlite.org`.
- The expected AgentFS `LICENSE.md` endpoint returned HTTP 404 at the reviewed commit. The attempted `https://github.com/zerofs/zerofs.git` lookup returned `Repository not found`. Those failures are reflected above rather than hidden.
- No benchmark, Docker service run, code build, or tracker mutation was performed as part of this packet. The provider behavior statements above are source/harness evidence, not a new pass claim.
