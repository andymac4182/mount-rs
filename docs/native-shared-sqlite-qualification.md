# Native SQLite with shared directory checkout

The opt-in `shared_checkout_native_sqlite_disjoint_handoff_and_crash_recovery`
test in `tests/native_shared_sqlite.rs` qualifies SQLite through real Linux
kernel FUSE mounts backed by shared SQLite metadata/block providers using
MRC3 directory authority. Each service has explicit
`OwnershipMode::Shared` and `with_checkout_path("/left")` or `/right`.

The harness creates disjoint directories using an offline Legacy bootstrap,
closes that driver, then explicitly enrolls the existing initialized namespace
with the providers' `prepare_concurrent_backing` and
`prepare_delegated_mode` operations. It starts separate child-process FUSE
services for the two disjoint directory grants. Overlapping exact, ancestor,
and descendant checkouts must be rejected before another mount can start.
Each mounted client also verifies that SQLite cannot create a database in
the other client's checked-out scope.

Each directory runs real Python SQLite DELETE and WAL workloads concurrently:
`synchronous=FULL`, separate-process lock contention, uncommitted spilled-page
rollback after killing a writer, committed killed-writer recovery, integrity,
and checkpointing. The verifier compares every ordered row ID and every
payload byte. The left service then unmounts and checks in; a fresh service
checks out and remounts that directory and verifies the committed databases.
Publication using the retired grant must fail with `ESTALE`.

The fresh left service is killed with SIGKILL. After cleanup of its dead
kernel mount, a fresh checkout must remain blocked by the crashed grant.
Explicit recovery with an incorrect expected fence must fail;
recovery with the exact crashed grant fence must succeed. Publication using
the crashed token must fail. A new left checkout/remount verifies SQLite
while the disjoint right service retains its exact original grant and verifies
its databases. Both services then unmount and check in, leaving no grants.

Native ownership handoff uses unmount/checkin followed by checkout/remount.
The test does not claim live revocation of a kernel mount or cached kernel
handles. SIGKILL recovery is not power-loss durability or independent-host
SQLite qualification.

```sh
MOUNT_RS_RUN_NATIVE_FUSE=1 cargo test -p mount-rs-core \
  --test native_shared_sqlite --locked -- \
  --ignored --exact shared_checkout_native_sqlite_disjoint_handoff_and_crash_recovery --nocapture
```

Run on Linux with `/dev/fuse`, `fusermount3`, `mountpoint`, Python 3, a C
compiler, and Rust 1.95. Use provider backing files on a qualified local
filesystem; the SQLite authority guard intentionally refuses remote, FUSE,
and unknown backing filesystems. The child helper test is private: invoke the
main test with the exact filter above.

## Execution evidence

The initial actual Linux FUSE run passed 1/1 in 28.23 seconds with Python
SQLite 3.45.1. Strict Linux focused Clippy passed. The existing Colima
`kernel-mount` VM was started without resetting or deleting state: Ubuntu
24.04.4 LTS, arm64, Linux `6.8.0-100-generic`, Rust 1.95.0, Python 3.12.3,
4 CPUs and 4 GiB RAM. Guest builds used
`CARGO_TARGET_DIR=/tmp/mount-rs-native-target`; providers were on guest-local
storage, with only source shared through virtiofs.

Intermediate revisions exposed native WAL I/O failures and an exceptional
`invalid orphan ownership provenance` local-validation trace. The final
engine checks fresh publication revision after reading authority, so remote
orphan publication causes known conflict/rebase instead of false scope denial.
After that fix, the complete strengthened native workload passed three
consecutive times: 14.69, 18.82, and 20.66 seconds. Diagnostics were enabled
with `MOUNT_RS_TRACE_FAILURES=1`; no invalid-orphan-provenance trace occurred.
Strict Linux focused Clippy passed. Engine and SQLite provider source hashes
were unchanged across those three runs:

```text
filesystems/mount-rs-chunked/src/lib.rs: 2045a6315fbfbb0478578265f0fa3ad2a7ce81d5595ad7f88eb5e86dd52defec
providers/mount-rs-sqlite/src/storage.rs: 16e5ec391a6a8ab71327d16684441fdd2c2ab0eae1c7937d17192cbd1f022446
```

These are local Ubuntu arm64 kernel/client observations, not hosted CI,
other distributions/architectures, independent-host SQLite, or power-loss
durability. Host compilation alone is not a native pass.

Final-source exclusive ownership regressions also passed: the native
exclusive-writeback SQLite process/reopen case in 6.66 seconds, and the
exclusive-writeback mount-service SIGKILL/lease-expiry restart case in 16.55
seconds. Strict Linux Clippy passed for all three native targets and the
service example. No FUSE mounts or test child processes remained; only known,
verified-unmounted failed-harness temporary roots were cleaned. The original
Docker `desktop-linux` context was restored and the existing Colima profile
was returned to its initially stopped state.
