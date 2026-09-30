# Filesystem block write timings

The filesystem block provider can record fixed process counters for PUT and
flush. Build the provider, SDK or CLI with `io-profiling`, then select
`MOUNT_RS_PROFILE_IO=1` before starting the process. The service forwards this
feature to an enabled SDK runtime. Builds without the provider feature leave
its recording disabled even when the environment flag is set.

## Measurements

The bank schema is `mount-rs.filesystem-blocks-bank.v1`. Its fixed labels contain
no paths, block identifiers, Drive identifiers, credentials or payloads.

| Rows | Observed boundary |
| --- | --- |
| PUT/flush waiter | Waiting for owned blocking work; dropping this waiter leaves admitted work running |
| PUT/flush queue | Admission to actual blocking closure entry |
| PUT/flush worker | The existing work closure and its actual result |
| `input_copy`, `content_id` | Owned input copy and content hashing/identifier construction |
| `initial_authority`, `shard_open`, `existing_verify` | Root/marker checks, shard access and full existing-content verification |
| `stage_create_write`, `before_publish_authority`, `publish_name` | Staging write, authority checks and create-only publication attempt |
| `file_sync`, `file_device_sync` | Regular-file sync and the subsequent device helper |
| `shard_sync`, `root_sync`, `post_directory_device_sync` | Directory syncs and the final winning-file device helper |
| `final_authority` | The existing checks after persistence barriers |

Waiter timing begins after input copy and admission checks. Constructors and
their backing-marker barriers are excluded. Flush still performs its two
authority checks without replaying PUT barriers or draining outstanding PUTs.
All persistence operations retain their existing order.

Each row retains starts, absolute inflight work, success/error/abandoned
terminals, cumulative elapsed nanoseconds, lifetime maximum nanoseconds and
offered bytes. Abandoned describes an unfinished observation; a dropped
waiter does not imply abandoned worker publication. PUT branch counts describe
selection of existing, created or race-existing paths, which can subsequently
fail. A failed create-only rename can lead to a successful race-existing PUT.

Durations overlap: the worker contains most PUT stages. They are wall time,
not exclusive CPU time. Offered bytes count starts, including failure and
abandonment; they are not bytes durably written. Device rows count helper
invocations. `device_barrier_supported` distinguishes macOS's device helper
from Linux's no-op helper. Neither row is physical device IOPS.

## Snapshot quality and allocation scope

Snapshots are fixed Copy values. Saturation is sticky; concurrent updates are
disclosed. A snapshot is process cumulative and is not a transactionally
consistent cut, a configured-provider census or evidence of worker drain.
Observed zero work is distinct from a disabled or unavailable observer.

Warmed recorder updates, guards, handle clones and fixed snapshots add no heap
allocations in the allocation controls. Observer construction and JSON export
are outside that window. Existing filesystem buffers, identifier strings and
Tokio worker allocations remain outside this claim.

For interval analysis bind both snapshots to the same process, generation and
built sources. Check schema/label rosters and availability, reject saturation
or decreasing cumulative counters, and disclose concurrent activity. Inflight
and lifetime maxima are absolute observations and must not be subtracted.
The existing 118 storage and 136 profile rows retain their previous meanings.

## Export locations

CLI periodic and shutdown records include
`process_diagnostics.filesystem_blocks`. An enabled capture samples the bank
once and shares that value between both listener records. An absent observer
has `available: false` and a reason; an observed zero snapshot remains available.
Periodic capture uses the existing `MOUNT_RS_DIAGNOSTIC_INTERVAL_MS` setting.

Native production-target worker records include
`filesystem_blocks_observation`, with configuration, availability, scope and
the cumulative snapshot. The snapshot is inside the existing capture deadline.
Its `complete` field remains false: this addition does not provide checked
per-cell deltas or qualify the bank through legacy `metrics_complete` or
`delta_from_previous` fields. Neither shutdown export nor a zero inflight count
establishes application drain.

## TiDB and filesystem restart test

The owned durable TiDB harness selects the SDK and CLI pairing with
`MOUNT_RS_TIDB_FILESYSTEM=1`. The Linux TiDB CI job enables it before and after
the existing TiDB/PD/TiKV restart. Both phases use the same private filesystem
root, metadata volume, backing marker and retained SDK payloads. The test covers
independent SDK contexts, full reads and EOF, shutdown/reopen, SQL backing
identity, filesystem block selection and rejection of changed root authority.

The harness requires one named passed SDK test, its exact phase marker and
Cargo status zero before the CLI self-test. Its stdout capture is capped at
64 KiB and refuses skipped or filtered tests. The CLI performs its own write,
sync, truncate and reopen round trip in each phase. This route is mount free;
it does not qualify mounted traffic, cross-host roots or physical power loss.
