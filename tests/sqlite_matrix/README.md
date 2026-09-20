# SQLite reliability matrix packet

This is an intentionally isolated, bounded runner for the current
file-backed `mount-rs` SQLite metadata/block providers. It uses the existing
`SqliteMetadataStore`, `SqliteBlockStore`, `ChunkedFs`, and
`mount-rs-fault-injection` APIs. It does not modify the root workspace or any
production source.

The runner executes eight fixed cells:

| Cells | Coverage |
| --- | --- |
| `delete_persistence_reopen`, `wal_persistence_reopen` | Configure each provider database, write through `ChunkedFs`, close, reopen, read the acknowledged payload, and verify journal mode, `FULL` synchronization, and `integrity_check`. |
| `delete_concurrent_reader_writer`, `wal_concurrent_reader_writer` | Run eight barrier-bounded reader/writer rounds against independent metadata connections. The writer uses the supported single-writer lease; a competing writer must receive `EAGAIN`. |
| `delete_fault_block_put_before`, `wal_fault_block_put_before` | Inject `ENOSPC` before an immutable block put and require the baseline payload after fresh reopen. |
| `delete_fault_metadata_publish_after_unknown`, `wal_fault_metadata_publish_after_unknown` | Inject the existing post-publish lost-acknowledgment hook and require a commit-unknown error, a whole allowed state after reopen, and valid SQLite integrity. |

The output is newline-delimited JSON: one object per case followed by one
summary object. It contains no temporary paths or payload bytes. The fault
seed is fixed at `0x5a172026`; the concurrency round count is fixed at eight.

Run the packet from the repository root:

```sh
cargo fmt --manifest-path tests/sqlite_matrix/Cargo.toml -- --check
cargo test --manifest-path tests/sqlite_matrix/Cargo.toml --quiet
cargo run --manifest-path tests/sqlite_matrix/Cargo.toml --quiet
```

The standalone child workspace is deliberate: the root `Cargo.toml` remains
unchanged. `Cargo.lock` generated beside this manifest belongs only to this
packet if the runner is built locally.

## Classification and gaps

`pass` is direct provider evidence only. WAL is explicitly requested and must
be observed as `WAL`; a rollback journal silently substituted for WAL fails the
cell. The persistence cells query SQLite's actual journal, synchronization, and
integrity state after a fresh provider reopen.

The concurrency cells cover the API's current single-writer contract. They do
not claim that multiple writers should succeed concurrently. The two fault
cells use the reusable optional fault-injection crate and record the selected
boundary, operation, phase, action, outcome, and fixed seed in the JSON line.

This packet does not simulate native FUSE/NFS SQLite hosting, the mount-free
SQLite VFS, process termination, power loss, or distributed multi-host locking.
Those remain separate acceptance boundaries; this packet must not be promoted
to evidence for them.
