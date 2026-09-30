# CLI observed construction controls — 2026-09-28

The [exact report](report.json) binds eight final gates to the corrected CLI
source, immutable owned parent, local logs and terminal receipts. No throughput
was measured by this change.

## Change and scope

`DriverRuntimePlan` now has an explicit observer-aware open path. Split storage
passes the same observer into the SDK. Every successful constructor registers
the actual `Arc<Filesystem>` synchronously before telemetry wrapping or snapshot
SQLite root stat/chown awaits. The runtime retains that same filesystem and its
cached driver. Existing unobserved entrypoints retain their behavior.

The caller owns its construction journal and attempt before polling. It settles
errors through journal-owned cleanup or hands off while retaining the returned
runtime. The API creates no cancellation-independent worker or admission fence.
Native work before a constructor returns requires its own provider ownership.

## Verification

- Three actual SQLite ownership cases passed with profiling disabled and enabled.
- Default and observability/profiling CLI all-target suites each passed **177
  tests, zero failures, 18 ignored**. Ignored cases are not claimed executed.
- Strict default/profiled Clippy and workspace formatting passed.
- The signed configured CLI fixture passed QUIC and WebSocket MRC5 selection,
  live payload checks and clean persistent reopen. This remains an eager fixture.

All eight final parent receipts have unchanged source pins, no timeout, overflow
or unknown lifecycle state, EOF, reaped owned children and absent owned groups.
There are 540 repository source/control pins plus one private immutable parent
runner pin; those counts do not mean 541 compiled or committed files.

The initial RED was three missing-API compilation errors and **zero behavioral
tests executed**. Independent review corrected two test gaps before final GREEN:
a writable reopen could repair a wrong root owner, and sequentially ordered
reads could hide ignored explicit offsets. Final controls reopen read-only and
use nonmonotonic offsets with exact slices and an independent sequential cursor.

## Data oracles and remaining work

The cases verify all 8,193 acknowledged bytes, stat size, chunk-crossing and
backward positioned reads, exact/beyond EOF, unchanged root UID/GID, actual
registered owner identity, SDK group/authority handoff, unchanged MRC5 backing,
and validation refusal before provider creation. Journal/reference retirement
is not presented as native SQLite resource accounting.

Bounded runtime activation is still preparatory. A source audit found the pool
can publish successful close/start replacement before its final driver/runtime
aliases drop; concrete connection/VFS retirement and canceled waiter controls
are next. AWS source/proxy freezing and supported SDK SlateDB pre-return task
drain remain open. SlateDB is outside the current configurable CLI and
SQLite/TiDB process-target factory domain.

This increment does not establish ten independent servers, the full production
population, new backend throughput, native I/O cancellation, crash durability,
changed-source formal qualification, physical device IOPS or whole-CI success.

## Reproduce the assertions

```sh
./scripts/cargo-shared test --locked -p mount-rs-cli --lib runtime::construction_tests -- --test-threads=1
MOUNT_RS_PROFILE_IO=1 ./scripts/cargo-shared test --locked -p mount-rs-cli --features observability,io-profiling --lib runtime::construction_tests -- --test-threads=1
./scripts/cargo-shared test --locked -p mount-rs-cli --all-targets -- --test-threads=1
MOUNT_RS_PROFILE_IO=1 ./scripts/cargo-shared test --locked -p mount-rs-cli --features observability,io-profiling --all-targets -- --test-threads=1
```

Use a checkout-isolated writable `CARGO_TARGET_DIR`. The report records complete
commands for lint, formatting and the signed configured fixture. Private owned
parent/log paths are local evidence; ordinary Cargo commands reproduce the
assertions without claiming the parent's containment observations.
