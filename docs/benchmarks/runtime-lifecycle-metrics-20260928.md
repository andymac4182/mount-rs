# Drive runtime lifecycle diagnostics

The bounded runtime pool has a separate, explicitly enabled timing bank. Its
fixed labels aggregate within one observer, which can be shared by pools. They
contain no token, path, Drive, Partition, provider endpoint, or error message.
The CLI and real-process fixture
still need their planned lazy-runtime wiring; current eager construction does
not automatically emit this bank.

The existing CLI transport and provider banks, including their activation flags
and output contracts, are described in the [bottleneck guide](../bottleneck-metrics.md).

## Enable and capture

Build `mount-rs-service` with `io-profiling` and construct the observer before
serving requests:

```rust
use mount_rs_service::runtime_diagnostics::RuntimeDiagnostics;
use mount_rs_service::runtime_pool::RuntimePool;

let observer = RuntimeDiagnostics::new(false); // timings, without slow logs
let pool = RuntimePool::with_diagnostics(2_048, observer)?;
// Register immutable factories and use the lazy dispatcher registration API.
let residency = pool.snapshot();
let timings = pool.diagnostics();
```

`RuntimePool::new` uses an unavailable observer even in a profiled build.
Without the feature, `RuntimeDiagnostics::new` is also unavailable. Neither
disabled case constructs a recorder or samples timing clocks. Both snapshots
can be serialized by the caller; they are separate from the existing transport
and storage stage banks.

## Stages and interpretation

| Label | Measured interval |
| --- | --- |
| `runtime.acquire` | One caller's acquisition, including activation/admission waits |
| `runtime.activation_wait` | One caller waiting for an owned activation or eviction admission |
| `runtime.open` | One owned factory task through health/backing checks and final pool transition |
| `runtime.eviction_shutdown` | One owned eviction through health/backing checks and final pool transition |
| `runtime.terminal_drain` | The single owned terminal pool drain |
| `runtime.handle_close` | One owned handle close, including waiting for borrowed I/O |
| `runtime.state_mutex_wait` | Acquiring the pool state mutex |
| `runtime.state_mutex_hold` | Holding the pool state mutex |

Rows contain calls, success/error/cancelled completions, active spans, total and
maximum nanoseconds, and 32 logarithmic microsecond histogram buckets. Spans
overlap and must not be added as disjoint latency components. Mutex intervals
publish after unlocking and are absent from the active gauge while deferred.

An aborted acquisition settles its caller rows as cancelled while the owned
open or eviction continues. Cancelling a shutdown or handle-close waiter does
not finish or replay the owned task. Failed opens, changed backing, failed
health and failed closes preserve the pool's quarantine and retained ownership.
An error or cancelled count is not proof that storage has drained.

Residency includes opening, ready, closing, quarantined and pinned counts,
successful/failed opens and evictions, waits and capacity rejections. High mutex
wait/hold values identify contention; long activation waits with a live open or
eviction identify the owned task keeping admission pending. Capacity rejections
must be compared with pins, quarantines and the configured resident limit.

Timing snapshots use serial atomic reads, rather than a transaction. They report
concurrent activity or saturated counters conservatively. They cannot prove
quiescence, consistency between banks, physical SSD IOPS, or cleanup. Residency
snapshots hold the pool state mutex; they do not validate storage or prove drain.

## Bounded slow logs

`RuntimeDiagnostics::new(true)` also permits slow records at 100 ms or longer,
with at most 16 write attempts per observer lifetime, including failed writes.
Budget reservation happens before obtaining the stderr writer. Each record
uses a fixed 160-byte stack buffer and only stage/outcome/elapsed fields:

```text
mount-rs runtime stage=runtime.open outcome=success elapsed_ns=120000000
```

Publication and logging occur after releasing the state mutex. Stderr writes
are best effort but may block; use timing-only mode for throughput runs and
retain the process controller's output deadline for traced qualification.

## Verification scope

Controlled tests cover cancellation of acquisition, terminal drain and eviction
waiters; Drop-owned handle close; original open errors and panics; retained
failed handles; and observation inside a health callback proving the current
mutex intervals have not published before unlocking. A synchronous admission
ticket waker also verifies that failed or stopped eviction publishes its owned
completion metric before waking the caller. Unit controls cover
idempotent completion, deferred completion, histograms, counter/envelope
saturation, and the finite slow-log budget/writer acquisition.

The strict allocation control runs with observers disabled and explicitly
enabled. In each mode, 1,024 warmed acquire/clone/drop operations allocate zero
times. For 128 read/write pairs, direct and held handles each allocate 256 times
in the underlying async-trait implementation; the holder adds none. Per-method
call and returned-byte checks prevent skipped delegation from passing. These
measurements exclude cold construction, provider/RPC/metadata work, snapshots,
close-task setup, other threads and process RSS. No whole-filesystem zero
allocation or production capacity claim follows from this control.

Run the behavioral and allocation controls through the shared Cargo wrapper:

```sh
CARGO_TARGET_DIR=/your/isolated/target ./scripts/cargo-shared test --locked --offline -p mount-rs-service --features io-profiling --test runtime_pool_diagnostics -- --test-threads=1
CARGO_TARGET_DIR=/your/isolated/target ./scripts/cargo-shared test --locked --offline -p mount-rs-service --features allocation-profiling --test runtime_pool_allocations -- --ignored --exact warmed_runtime_leases_allocate_nothing_and_handle_holder_adds_no_io_boxes --test-threads=1 --nocapture
```

Actual SQLite lifecycle controls separately verify complete bytes, size, EOF,
immutable backing identity and sibling-provider usability after repeated
eviction. Native resource observations, concrete provider construction cleanup,
CLI/process wiring, all backend qualification and the full 10,000-client target
remain separate gates.
