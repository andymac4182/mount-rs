# Phase-bound OS I/O metrics

## Contract

Add opt-in test-only process and selected Linux block-device snapshots. The
existing100 ms resource sampler keeps its current behavior. An explicit boundary
capture uses `MOUNT_RS_PROFILE_IO=1`; a Linux device is sampled only when
`MOUNT_RS_PROFILE_BLOCK_DEVICE` names one device under `/sys/block`.

Retain raw before/after identity and exact decimal counters, checked deltas,
sample duration and interval duration. Missing, disabled, unsupported, unselected,
reset or changed-identity observations remain unavailable rather than becoming
zero. Never sum devices or infer physical flash, remote datastore or workload
attribution from host counters.

Linux process collection retains all seven `/proc/self/io` fields. Its identity
is own PID plus `/proc/self/stat` start ticks. The selected device records the
device name, major/minor, disk sequence and boot identity; sectors mean512 bytes.
Bracket each device counter read with two identical identity reads, then also
check identity across the phase. This detects reattachment during a sample.
Darwin process collection uses libc's `proc_pid_rusage` and `RUSAGE_INFO_V2` ABI,
retaining own PID, process start time and disk-accounted read/write bytes.

On macOS, `MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID` selects one canonical nonzero
numeric IORegistry entry ID. Public IOKit exact-ID matching avoids inventory or a
subprocess. Check the selected ID and `IOBlockStorageDriver` class, read only its
`Statistics` property, and require all four operation/byte values to be CFNumbers
that convert exactly to nonnegative64-bit signed integers. Values outside that
API conversion range remain unavailable. Retain the ID and observer PID/start
generation: entry IDs are only valid until reboot, and a live observer generation
cannot span a reboot. Missing selection remains unselected, never automatic.
No partition/device/file/VM mapping is inferred from an ID.
Linux fields label completed block operations. macOS fields label driver
operations processed, matching the public IOKit definition without implying
Linux completion semantics.

## Implementation and verification

1. Add pure parser, exact counter, identity/reset and disabled-observer contract
   tests with forwarding stubs. Run focused RED under the root-owned Cargo lease.
2. Implement bounded parsers and checked deltas, then platform collection without
   dependencies, subprocesses, device inventory or host configuration changes.
3. Wire explicit `Snapshot::capture_process_io_boundary` and
   `capture_connections_io_boundary` methods. Leave ordinary capture methods and
   continuous sampling free from the new I/O observer.
4. Run focused GREEN, touched-file formatting and strict scoped Clippy. Run a
   separately authorized own-process native smoke; Linux/device fixture tests on
   macOS do not qualify actual Linux collection. Preserve exact source receipts.

The sampler itself performs I/O and consumes CPU. Boundary observer time is
retained, and host device counters include other processes and asynchronous
writeback. Rate calculations use a caller-visible checked interval and do not
rename OS operation counts as application IOPS. This change adds measurement and
does not change resource floors, datastore configuration or workload behavior.
Enabled OS reads, CF property copies and JSON snapshots can allocate and consume
CPU/I/O. This boundary observer makes no zero-allocation or performance claim.
Normal captures retain a disabled OS envelope and never select/query a device.
