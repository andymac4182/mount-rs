# W01 core concurrency parity packet

This is a bounded, mount-free concurrency packet for the in-memory core. It
replays the same JSON schedule against `mount-rs-core::MemoryFs` and the pinned
`mountx` TypeScript memory driver, then compares stable JSON observations.
Every round launches its listed operations concurrently; rounds and observation
steps are awaited in fixture order.

The packet covers:

- concurrent disjoint creates and binary writes;
- two open handles performing fixed positional writes and reads;
- rename/unlink while a held handle continues to access its inode;
- concurrent append writes, compared as an exact fixed-size record multiset;
- recursive `mkdir` idempotence under a concurrent call group; and
- exclusive-create atomicity, requiring one success and one `EEXIST`.

Inodes and wall-clock timestamps are intentionally omitted from observations.
The append record order and the identity of the winner in the exclusive-create
race are scheduler-dependent, so those round results are compared as
multisets. The packet does not claim cross-process atomicity, crash/durability
behavior, cancellation/close races, or transport/native concurrency; those
stable scope gaps are emitted in the machine-readable report.

## Exact oracle-enabled command

From the mount-rs checkout:

```sh
cd /Users/andrewmcclenaghan/github/andymac4182/mount-rs
CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-core-concurrency-target \
MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX \
node tests/core_concurrency/check.mjs
```

The command verifies that `MOUNTX_SOURCE` is exactly revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. Success is one JSON line such as:

```json
{"packet":"W01-core-concurrency-v1","status":"PASS","scenarios":6,"unsupported":5,"failures":0,"oracle":"85361a8212ff9bff8e69f62fa8993ef2c2ec51e8"}
```

If `MOUNTX_SOURCE` is unset, the Node command emits a JSON `SKIP` result and
does not claim parity. A configured but non-pinned source is a failure.

## Focused Rust-only command

This runs the same Rust side without requiring the oracle:

```sh
cd /Users/andrewmcclenaghan/github/andymac4182/mount-rs
CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-core-concurrency-target \
cargo run --quiet --locked \
  --manifest-path tests/core_concurrency/Cargo.toml
```

It prints one JSON report containing each setup result, parallel round result,
observation, and the explicit unsupported-scope inventory. The standalone
manifest and lockfile keep this packet out of the root workspace manifest.

## Files and boundaries

- `Cargo.toml`, `Cargo.lock`: standalone Rust harness dependencies only.
- `src/main.rs`: deterministic round runner and stable result projection.
- `scenarios.json`: shared schedule and scope classifications.
- `check.mjs`: pinned-revision check, TypeScript replay, Rust invocation, and
  machine-readable pass/fail result.

No source, root manifest, tracker, site, benchmark, or git-history files are
part of this packet.
