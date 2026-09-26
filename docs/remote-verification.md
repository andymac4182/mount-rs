# Remote Drive verification

## Gates

Use `scripts/cargo-shared` for normal Rust commands. Run the focused remote
packages and strict Clippy, then the workspace regression suite. The Remote Drives
workflow runs ordinary tests on Linux and macOS, native Linux NFS qualification,
and separately named Kani harnesses pinned to version 0.68.0.

## Security and lifecycle coverage

JWT tests exercise valid ES256 and RS256 signatures, tampering, wrong issuer and
audience, audience arrays, future `iat`/`nbf`, expiry, maximum lifetime, invalid
subjects, critical headers, unsupported algorithms, malformed token segments,
size limits, duplicate signing-key IDs, key rotation throttling, and ambiguous
issuer policies. Catalog tests exercise persistent independent Partitions,
Drive references, exact stable claim conditions, stale CAS, duplicate definitions,
and empty-before-delete Partition transitions.

Handle tests saturate all 1024 slots, release them after catalog revision changes,
and exercise counter overflow cleanup. Regressions require a delayed open to fail
admission after invalidation or terminal shutdown, and prevent an older catalog
snapshot from rolling the session's revision backwards.

Authorization is checked against the catalog snapshot read for a request. A
request already admitted before a concurrent revocation may complete; revocation
is not rollback of backend effects. A catalog revision fences subsequent handle
admission and closes registered handles. Operations on independent backend handles
remain subject to each backend's concurrency and cancellation contract.

## Boundaries

Local loopback measurements do not qualify cross-host networking, production
backends, power-loss durability, or production capacity. Synthetic authentication
in transport stress fixtures excludes external OIDC network latency; OIDC has
separate signature and policy tests. Formal decision proofs exclude cryptographic
correctness, TLS/QUIC implementations, SQL transaction atomicity, JSON pointer
resolution, allocation, native kernels, and async scheduler behavior unless a
particular harness explicitly states otherwise.

Executed results and proof domains are recorded below when verification finishes.

## Running load tests

```sh
./scripts/test-quic-load.sh ci
./scripts/test-quic-load.sh soak
```

Ordinary tests exercise mixed I/O on two independent Drives, reject cross-Drive
and cross-session handles, and require progress with 48 competing streams against
the 32-active-operation limit. The opt-in soak prints completed operations, transferred
bytes, elapsed time, throughput, batch p50/p95 latency, and workload errors.
Assertions check retained file contents; successful requests alone are insufficient.
The manual Remote Drives workflow runs the soak on Linux and macOS and retains
its output as an artifact. Ordinary pushes and pull requests run the shorter load
regressions.

## Running bounded proofs

Install Kani 0.68.0 and run `./scripts/verify-remote-formal`. Every invocation names
one harness; success requires zero failed checks and satisfied cover conditions.
A symbolic decision proof does not prove the whole async service state machine.

| Harness | Input domain | Property and limits |
| --- | --- | --- |
| `remote_grant_requires_exact_scope_and_all_claims` | Two literal scope identities; zero to two resolved conditions, each missing, matching, or different; unwind 5 | Exact Partition and policy, nonempty conditions, and conjunction required. Excludes JSON pointer resolution, arbitrary strings/condition counts, grant aggregation, and JWT signatures. |
| `remote_read_grants_cannot_authorize_mutations` | All four grant/required Read/Write combinations | A Read grant cannot satisfy a Write requirement. Excludes the upstream operation classification and grant resolution. |
| `remote_readonly_flags_require_explicit_nonmutating_values` | All 729 combinations of six absent/invalid, false, or true decoded flag values; unwind 7 | Read classification requires explicit read=true and all mutation flags=false. Excludes JSON parsing and string aliases. |
| `binary_io_lengths_bounded_before_allocation` | Unrestricted `usize` control, payload and count lengths and `u64` request ID, across all six frame kinds; unwind 7 | Accepted control is at most 8 MiB and payload/count at most 1 MiB; noncontrol IDs are nonzero. Read results have equal payload/count and no control; Read/Write control length is 19–82 bytes (18-byte prefix plus a 1–64-byte Drive ID allowance); Write count is zero. Three covers exercise maximum Write payload, oversized rejection and an empty Read result. Excludes allocation execution, payload parsing and transport. |
| `generic_control_charge_cannot_wrap_or_exceed_default_admission` | Arbitrary `usize` length, assumed at most 8 MiB | The production charge equals `64 * length + 1 MiB + 32`, is at least the input length and at most the default 1 GiB admission budget without overflow. One cover reaches the maximum admitted input. Excludes actual JSON heap usage, permit lifetime and concurrent admission. |
| `remote_handle_admission_is_fenced_and_bounded` | Arbitrary closed/present booleans, full-range `u64` revisions/counter, and `usize` count | Only live current-revision admission below 1024 handles with a nonoverflowing ID. Excludes locks, scheduler, and backend close effects. |
| `remote_handles_require_exact_drive_and_revision` | Two literal Drive identities and unrestricted `u64` revisions; unwind 3 | A handle requires exact Drive and catalog revision. Session ownership is exercised by QUIC integration tests; arbitrary strings and table storage are outside this proof. |

Hard expiry or a failed catalog read terminally invalidates the session. A later
renewal requires a new connection; it cannot advertise a session whose old handle
table was already shut down. Expiry coverage uses a synthetic expired identity
and exercises request denial followed by renewal rejection, not a clock-transition
or cancellation proof.

## Executed CI results (2026-09-25 UTC, Linux)

At commit `7eee4a3f2ded67fd01dbc8d388c37e8efd4a83eb`, the
[remote formal job](https://github.com/andymac4182/mount-rs/actions/runs/36164387109/job/108168468016)
executed all seven harnesses in the current runner with Kani 0.68.0 / CBMC
6.11.0 on 64-bit x86_64 Linux. Each reported successful verification: 1,038
checks, zero failed checks, 13 unreachable checks and 25/25 satisfied covers
across the seven runner invocations. The job also ran handle admission
separately; that duplicate is excluded from these totals.

The retained CI log proves these bounded decisions. Verifier binary hashes,
generated proof artifacts and peak RSS were not retained in this record. It
does not prove compact metadata transactions, cache behavior or end-to-end
capacity. The six-harness macOS result below records an earlier runner and is
not a result for the current seven-harness inventory.

## Executed local results (2026-09-24, macOS)

- Kani 0.68.0 / CBMC 6.11.0: all six named harnesses passed, zero failed
  checks, and 25/25 covers satisfied. Kani reported unsupported caller-location
  and foreign-function constructs elsewhere in compiled code; none were reached
  by these harnesses. The runner exits on any failed invocation.
- Default loopback load: eight clients, 100 rounds, 4 KiB payload, 12,800
  completed operations, zero errors, about 3,129 operations/s. Batch p50/p95:
  12/17 ms.
- Extended loopback load: eight clients, 1,000 rounds, 4 KiB payload, 128,000
  completed operations in 41.3 seconds, zero errors, about 3,099 operations/s.
  Batch p50/p95: 12/18 ms. Counted data transfer was 196,608,000 bytes; this
  includes writes and both reads, not unique data or wire overhead.

These are measurements on this machine, not performance thresholds. The workload
uses memory-backed Drives and SQLite catalog reads, and deliberately includes
expected denied cross-Drive handle requests. “Zero errors” means no unexpected
workload failures. The load runner requires dependencies to be cached because it
runs Cargo offline; the workflow first builds/tests the packages.

Soak configuration:

| Environment variable | Default | Bounds |
| --- | --- | --- |
| `MOUNT_RS_QUIC_LOAD_CLIENTS` | 8 | 1–64 |
| `MOUNT_RS_QUIC_LOAD_ROUNDS` | 100 | 1–1,000 |
| `MOUNT_RS_QUIC_LOAD_PAYLOAD_BYTES` | 4,096 | 64–65,536 |

The whole workload has a 300-second timeout. Increase rounds for a longer bounded
run; this does not replace a sustained cross-host production soak.

The connection saturation test retains 128 authenticated sessions, rejects the
129th, verifies an existing session remains usable, then closes one and requires
admission to recover. The stream saturation test holds backend calls behind a
controlled gate: exactly 32 enter, a 33rd cannot enter before release, and all 48
requests subsequently drain with peak concurrency at most 32. Tests use bounded
waits for asynchronous transport behavior; they are executable runtime evidence,
not scheduler proofs. The handshake-credit regression closes the hello stream and reserves one
additional transport credit (33 bidirectional credits), because transport credit
replenishment can be delayed. The request semaphore caps active operations at 32.

Each client reuses one file per Drive across rounds and rewrites it with a unique
client/round/Drive pattern. This keeps retained payloads bounded to approximately
`2 × clients × payload_bytes` (8 MiB at the maximum settings), excluding transient
JSON/transport buffers and metadata. Server handshakes require exactly one framed
hello followed by stream FIN; trailing bytes are rejected and the hello stream
is released before request handling.

Final workspace gates passed:

```sh
./scripts/cargo-shared fmt --all -- --check
./scripts/cargo-shared clippy --workspace --all-targets --offline --locked -- -D warnings
./scripts/cargo-shared test --workspace --all-targets --offline --locked
```

The workspace test run reported 1,158 passed, zero failed, and 86 ignored. The
opt-in soak is executed separately. Existing Unix socket tests required running
outside the Codex filesystem sandbox. Hosted Linux/macOS CI and the new Linux
proof job have not been executed for this change; configured jobs are not hosted
qualification results.

For the actual TiDB 100-client/10-server gate, see [TiDB scale testing](remote-tidb-load.md).
