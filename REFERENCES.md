# Compatibility source and architectural inspiration

## Behavioral compatibility target

- [pithings/mountx](https://github.com/pithings/mountx): the TypeScript source
  and test oracle for this Rust port. Differential tests pin revision
  `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.

## Inspiration and design references

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

- [Barre/ZeroFS](https://github.com/Barre/ZeroFS): user-selected feature and
  benchmark comparison target for object-backed mounted filesystems. Review
  its configuration, NFS/9P access, durability and recovery tests, caching,
  and storage behavior. Track comparisons under
  [ZeroFS comparison acceptance](REQUIREMENTS.md#zerofs-feature-and-benchmark-comparison).
- [ComputeSDK storage benchmarks](https://github.com/computesdk/benchmarks/tree/master/benchmarks/storage):
  workload and reporting reference for the required storage benchmark suite.
  See [benchmark acceptance](REQUIREMENTS.md#storage-benchmark-acceptance).

These references inform design, not compatibility promises. They do not add
runtime dependencies or expand the current scope into a hosted compute service.
Copy-on-write remains future work as specified in [REQUIREMENTS.md](REQUIREMENTS.md).
Before reusing implementation code, inspect its license and preserve any
required attribution. A reference project's claims are not evidence that
mount-rs satisfies its own durability or platform acceptance gates.
