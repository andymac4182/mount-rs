# Native exclusive-writeback SQLite qualification

This lane exercises SQLite through an actual Linux kernel FUSE mount with
split SQLite metadata/block providers and explicit exclusive writeback:

```rust
ChunkedOptions::fixed("owned-native", 4096)?
    .with_ownership_mode(OwnershipMode::Exclusive)
    .with_writeback(true)
```

One mount service owns the stores. Multiple SQLite processes on that same
Linux host use the mounted directory. Independent mounts/hosts and shared
ownership are outside this qualification.

`tests/native_exclusive_sqlite.rs` executes the existing real Python SQLite
process workload for both DELETE rollback journals and WAL with
`synchronous=FULL`. It checks transaction rollback, separate-process writer
lock contention, WAL readers during an active writer, dirty-page spills,
killed uncommitted-writer recovery, killed committed-writer recovery,
integrity, and WAL checkpointing. It then unmounts, shuts down the driver,
reopens both backing stores, remounts, and checks integrity and every committed
row/payload through Python SQLite. The reopen uses `mode=rw`, so a missing
database cannot silently be recreated.

Run on a Linux host with `/dev/fuse`, `fusermount3`, Python 3, a C compiler,
and Rust 1.95:

```sh
MOUNT_RS_RUN_NATIVE_FUSE=1 ./scripts/cargo-shared test -p mount-rs-core \
  --test native_exclusive_sqlite --locked -- \
  --ignored --exact exclusive_writeback_sqlite_processes_and_durable_reopen --nocapture
```

The ignored tests are opt-in; ordinary host compilation or mock tests do not
provide native acceptance. The Python process-kill checks terminate SQLite
clients. A separate service crash test terminates the mount service itself.

`tests/sqlite_service_crash.rs` adds
`exclusive_writeback_service_sigkill_preserves_synced_delete_and_wal`. It
starts `examples/sqlite_mount_service.rs` with
`MOUNT_RS_SQLITE_EXCLUSIVE_WRITEBACK=1`; the example otherwise retains its
original synchronous default. For DELETE and WAL it commits a 1 MiB payload
with SQLite `synchronous=FULL`, sends SIGKILL to the service, observes the
existing live writer lease reject a fresh owner, cleans up the dead FUSE
mount, retries the public ownership acquisition until lease expiry, and
verifies the committed payload/integrity through the restarted mount before
committing another row. It never edits provider leases or bypasses fencing.

```sh
./scripts/cargo-shared build -p mount-rs-core --example sqlite_mount_service --locked
MOUNT_RS_RUN_NATIVE_FUSE=1 ./scripts/cargo-shared test -p mount-rs-core \
  --test sqlite_service_crash --locked -- \
  --ignored --exact exclusive_writeback_service_sigkill_preserves_synced_delete_and_wal --nocapture
```

SIGKILL service recovery and orderly shutdown/reopen are not power-loss proof.

## Local Linux environment

The existing stopped Colima `kernel-mount` profile was started without deleting
or resetting VM/container state. Configuration: aarch64, 4 CPUs, 4 GiB memory,
20 GiB disk, VZ with virtiofs; Ubuntu 24.04.4 LTS, kernel
`6.8.0-100-generic`. The VM had `/dev/fuse`, `fusermount3`, and Python 3.12.3.
Guest-local build prerequisites and Rust 1.95.0 were installed. Linux builds
use `CARGO_TARGET_DIR=/tmp/mount-rs-native-target` to avoid mixing host and
guest build outputs.

The local host Python control passed DELETE/WAL using SQLite 3.53.4, including
the new durable-data verifier. The focused Rust test compiled on macOS; that
is a compile/control result only.

Native Linux execution on 2026-09-24 passed 1/1 in 3.49 seconds using Python
SQLite 3.45.1. Both journal modes reported process locking/recovery success;
both durable-data verification passes completed before and after
shutdown/store reopen/remount. The final core source with exact ordered IDs
and byte equality for every payload passed 1/1 in 15.58 seconds. The separate
exclusive-writeback mount-service SIGKILL/restart case passed 1/1 in 36.96
seconds, observing the live lease fence and successful recovery for both
journal modes. The existing `mounted_split_stores_host_sqlite_and_reopen`
native baseline also passed 1/1 in 69.74 seconds. These test timings cover
different workloads and are not a controlled performance comparison.
Focused strict Clippy passed on both macOS and Linux, including a final Linux
rerun. No FUSE mounts or test SQLite subprocesses remained after qualification.
The original Docker `desktop-linux` context was restored and the existing VM
was returned to its initially stopped state.

Exact guest command:

```sh
colima ssh --profile kernel-mount -- sh -lc '
  . "$HOME/.cargo/env"
  cd /Users/amcclenaghan/github/andymac4182/mount-rs
  PYTHONDONTWRITEBYTECODE=1 MOUNT_RS_RUN_NATIVE_FUSE=1 \
    CARGO_TARGET_DIR=/tmp/mount-rs-native-target \
    cargo test -p mount-rs-core --test native_exclusive_sqlite --locked -- \
      --ignored --exact exclusive_writeback_sqlite_processes_and_durable_reopen --nocapture
'
```

This proves the observed local Ubuntu arm64 kernel/client behavior. It does
not establish other distributions/architectures, independent-host SQLite,
VM power loss, or production durability.
