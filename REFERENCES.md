# Compatibility source and architectural inspiration

## Behavioral compatibility target

- [pithings/mountx](https://github.com/pithings/mountx): the TypeScript source
  and test oracle for this Rust port. Differential tests pin revision
  `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.

## Inspiration and design references

- [CrabBuild](https://github.com/crabbuild): review its blob-backed versioning
  designs and bring applicable lessons into the versioned-filesystem proposal.
  Initial candidates are [crab](https://github.com/crabbuild/crab) for large-file
  object storage, chunking/deduplication and lazy hydration;
  [prolly](https://github.com/crabbuild/prolly) for immutable ordered maps,
  structural sharing and diffs; [silo](https://github.com/crabbuild/silo) for an
  object-backed version ledger; and [trail](https://github.com/crabbuild/trail)
  for operation history. Pin inspected revisions, evaluate publication and
  recovery invariants, and record adopt/adapt/reject decisions with tests.
  No dependency or code reuse is implied; inspect licenses before reuse and
  retain fixed-size initial chunking unless a later scope decision changes it.
- [Tensorlake: Firecracker disk snapshots in O(changed bytes)](https://www.tensorlake.ai/blog/firecracker-disk-snapshots-o-changed-bytes):
  storage/diffing reference for versioned filesystems and later copy-on-write.
  Evaluate write-path dirty tracking, explicit zero extents, immutable layers,
  content-addressed blocks, versioned manifests, and layer compaction. Measure
  snapshot pause time separately from durable upload completion, varying total
  state, changed bytes, layer depth, and outstanding dirty data. Its VM block
  design and vendor benchmarks are inspiration, not proof of filesystem-level
  or SQLite snapshot correctness in mount-rs; compare cache and fsync policies.
- [Tensorlake filesystems / TLFS](https://docs.tensorlake.ai/filesystems/introduction):
  versioned-filesystem reference reviewed for the explicit versioning requirement.
  The guide distinguishes retained autosaves from permanent snapshots, supports
  historical reads and forks, and describes pinned versus following read-only
  mounts. Remote recovery reaches the last durable checkpoint, not necessarily
  the last local write. Its shared-writer last-writer-wins policy must not be
  assumed suitable for SQLite files. See versioning acceptance in REQUIREMENTS.md.
- [slatedb/slatedb](https://github.com/slatedb/slatedb): user-selected reference
  for potential S3-backed storage design. Study object-storage LSM layout,
  batched writes, explicit durable-write/flush boundaries, manifest publication,
  compaction, recovery, and memory/disk caching. Evaluate metadata and byte-store
  trade-offs independently, including request cost and latency. This is a
  research reference, not a decision to depend on SlateDB or a claim that its
  storage guarantees satisfy our SQLite-hosting contract. Any adopted design
  must preserve fencing, atomic metadata publication, and honest fsync semantics.
- [tursodatabase/agentfs](https://github.com/tursodatabase/agentfs): the
  user-selected filesystem design reference.
- [Archil introduction](https://docs.archil.com/getting-started/introduction):
  shared storage, object-storage integration, SQLite hosting, and future
  branching/checkpoint design questions. Added at the user's request.
- [tensorlakeai/tensorlake](https://github.com/tensorlakeai/tensorlake): the
  user-selected implementation and architecture reference; its Rust crates
  and macOS filesystem integration are relevant areas for investigation.

## Benchmark alignment

- [Cloudflare Artifact FS](https://github.com/cloudflare/artifact-fs):
  user-selected architecture inspiration, feature comparison, and benchmark
  target. Review lazy Git blob hydration, filesystem generation publication,
  local overlays, prefetch scheduling, and recovery. Track source-pinned review
  and comparable measurements under
  [Artifact FS comparison acceptance](REQUIREMENTS.md#artifact-fs-inspiration-and-benchmark-comparison).
- [RustFS](https://rustfs.com/): required real S3-compatible integration-test
  service and backend for storage benchmarks. Pin the
  tested release/image and verify required semantics rather than inferring
  compatibility from the product description.
- [ComputeSDK storage benchmarks](https://github.com/computesdk/benchmarks/tree/master/benchmarks/storage):
  workload and reporting reference for the required storage benchmark suite.
  See [benchmark acceptance](REQUIREMENTS.md#storage-benchmark-acceptance).

These references inform design, not compatibility promises. They do not add
runtime dependencies or expand the current scope into a hosted compute service.
Copy-on-write remains future work as specified in [REQUIREMENTS.md](REQUIREMENTS.md).
Before reusing implementation code, inspect its license and preserve any
required attribution. A reference project's claims are not evidence that
mount-rs satisfies its own durability or platform acceptance gates.

## End-of-primary-work review

- [Erlang gen_statem behaviour](https://www.erlang.org/doc/system/statem.html):
  review after the primary uses are implemented and verified, as requested.
  Consider lessons from explicit state/event transitions, postponed events,
  timeout lifecycles, and replies for storage coordination and communication.
  Record applicable lessons, concrete code/test gaps, and reasons not to adopt
  unsuitable patterns; this does not require Erlang or a new runtime dependency.
