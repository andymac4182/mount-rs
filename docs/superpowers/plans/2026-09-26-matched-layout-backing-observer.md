# Fixture-free owned backing observer

2026-09-26. Implements only the agreed four-path slice of the source-only
matched-layout plan (`292298f2eb380c5a4b1a0ce0a46a00c199da8e17c2ac82f258498334e2e515dc`)
and independent protocol review
(`3f270fe6982675f7bacb64dcc0a51680aa1e379fa08a0aa0814ded697fae9725`).

## Scope and acceptance

- New `benchmarks/storage/backing-observer.mjs`: pure projection/accounting and
  bounded observation through an explicitly injected transport and clock.
- New `benchmarks/storage/backing-observer.test.mjs`: Node24 built-in tests with
  injected transports, clocks, observers and in-memory filesystem providers.
- `benchmarks/storage/runner.mjs`: default-off hooks and an outer observer
  finalizer. No CLI/environment activation and no daemon/socket discovery.
- This plan. No Engine adapter, pair coordinator, idle-window wait, compact marker
  reader, fixture labels, native binding, Cargo, threshold or workflow changes.

Retain actual source-bound RED and GREEN receipts for independent review. Only
the final explicit-capture/guarded receipts support fixture-free acceptance.
Historical unpinned loader receipts are retained and their no-addon scope is
unproven (see the loader audit below). Fixture-free acceptance
does not establish live Engine compatibility, backing performance, compact
persisted markers, full capacity, cache causality or formal correctness.

## Closed transport and retention contract

The caller supplies unique full 64-hex CIDs, unique roles and nonempty expected
ownership labels. Approved label keys are `mount-rs.tidb.run`,
`mount-rs.foundationdb.run`, `com.mount-rs.ozone-test` and
`com.mount-rs.ozone-test-run`, plus the existing direct RustFS keys
`com.mount-rs.rustfs-test` and `com.mount-rs.rustfs-test-run`. Expected labels remain
bounded to four per CID. RustFS qualification requires both keys and rejects a
`com.mount-rs.rustfs-test-purpose` label before retention, excluding the fixture's
separate cleanup helper. Exact caller-issued service CID/run values remain required;
these labels alone do not establish a benchmark's backing endpoint binding.
A labelled volume/network does not establish
container ownership; current FDB server creation requires a later explicit seam.

Only GET `/version` and versioned inspect/stats routes for pinned CIDs are
generated. Negotiate Linux API 1.41 through 1.51 within daemon bounds. Require
`stream=false&one-shot=true`; unsupported platform/version is incomplete. No
list, name retarget, overlap or retry. A transport must honor AbortSignal and
return a status plus an async iterable of byte chunks; it never receives secrets
or a socket path from this helper.

Hard ceilings (caller may lower them): 16 containers, 257 requests, 64 boundaries,
1MiB per response, 128 device entries per metric, 32 network interfaces, 8MiB total receipt,
2s request deadline and 60s owner lifetime. Samples of the same CID are at least
1s apart; an early endpoint is unavailable instead of delaying the workload.
This includes immediately adjacent phase boundaries: an end followed by the next
begin can be incomplete, leaving that interval unavailable. Cached endpoint reuse
is not implemented or approved by this slice. Such receipts cannot clear a future
matched-run observer completeness gate.
Abort on limit/deadline failure; a monotonic post-completion check rejects a
late response even if its timer callback has not run. Unsettled aborted requests
remain active and prevent another request for that key. Finalization performs
no network requests, is idempotent, rejects unclosed phases and a spent owner
budget, aborts active requests and freezes the partial receipt against late work. Arbitrary transport/error text is never persisted.

Parse JSON numeric tokens losslessly using Node24 reviver source text, then
validate selected counters as canonical uint64 decimal strings before BigInt
arithmetic. Never convert a rounded Number into a counter. Inspect is projected
before journal retention: CID, role, approved labels, image ID, running/start/
restart identity, and fixed configured limits only. Never retain Env, commands,
mounts, arbitrary labels or complete inspect/error bodies. Stats retain only
selected fields and a body SHA256; additive fields are discarded. Limits retain
their field-specific unset/unlimited sentinels; configured values are distinct
from observed `memory_stats.limit`.

## Accounting and scope

Cumulative Linux CPU usage uses nanoseconds. Per-container CPU rate is the exact
usage-ns/read-interval-ns ratio. CPU core-seconds and exact counter deltas may be
summed; no percentage across unlike windows is presented. API memory values are
gauges, distinct from Docker CLI cache-adjusted display values.

Block byte/operation keys are `(major,minor,Read|Write)` only. Ignore Total,
Sync/Async and other operations. Reject duplicate selected keys, reset counters
and changed device sets. Missing/null/empty metrics are unavailable; explicit
stable zeros are observed zero. cgroup2 operation counts may be unavailable.
An aggregate is complete for a metric only if every required CID has a valid
interval. Publish partial totals separately and set the complete total to null
when any member is missing. Reject duplicate boundary members and identity drift.
Observer receipt completeness means bounded identity/sampling/journal/finalization
completed; each interval/metric separately declares accounting availability.
Unavailable cgroup2 operation counters never become observed zero or throughput.

Network selection is only `networks` interface `rx_bytes` and `tx_bytes`, in
separate maps of exact uint64 decimal inputs. Missing/null/empty networks remains
unavailable; stable explicit zeros are observed zero. Each direction may be null
independently. Interface names use a fixed ASCII policy of one letter followed by
up to 14 letters/digits/underscore/dot/hyphen; reserved names reject. Names are
sorted without alias normalization. The lowerable hard ceiling is 32 interfaces.
Unsafe/malformed keys or counters fail with fixed issues; addresses, packets,
endpoint IDs and additive network fields are discarded before journal retention.

Per-direction intervals require stable interface keys and monotonic known
counters. Missing values, changed keys and resets invalidate that CID's direction;
other directions/metrics may remain available. Complete directions retain exact
per-interface deltas and read-ns ratios. Derived sums use arbitrary precision
decimal strings beyond uint64 without wrapping; inputs remain bounded uint64.
Existing CID aggregates retain separate partial totals/null complete totals.
Network absence affects metric availability separately from bounded receipt
completeness. There is no per-interface partial interval subtotal or aggregate
bandwidth across unlike sample windows.

Network attribution is container-interface accounting. Client/server and virtual
interfaces may count the same traffic more than once; rx and tx remain separate.
Loopback is descriptive, and names/counters alone do not prove continuous
interface lifetime after same-name replacement. No physical link/flow identity,
payload classification, saturation, throughput ranking or bottleneck cause is
established by these observations.

Retain daemon read timestamps at nanosecond precision and local monotonic/UTC
dispatch/response endpoints. Endpoint skew and enclosing windows are distinct
from the unchanged measured workload interval. Idle-labelled endpoints are
descriptive observations only; there is no background subtraction or cache/RCA
claim. Retain observer client wall/CPU, requests/response bytes and Node hook
costs separately. Docker daemon overhead is unisolated.

## Runner ordering and terminal contract

Optional fourth argument to `runBenchmark`:
`{ backingObserver, observerClock?, observerHookTimeoutMs? }`. The observer
implements `beginPhase`, `endPhase`, and `finalize`. All calls are bounded by the
runner wrapper and receive an AbortSignal. Fixed metadata carries provider, phase
name, native quiescence, pending count and measured elapsed time; only an explicit incomplete hook result propagates; arbitrary return
values/error bodies are not embedded in benchmark artifacts.

A module-private registration exports `backing_evidence` only for an exact fresh
factory object claimed once by this runner session. Claiming rejects pre-existing
requests, boundaries, journal, phases, issues, finalization or another session
claim. The original phase/finalization methods must be unchanged; a public
`receipt` override is ignored. Original private finalization must complete for the
same claim before the original lexical receipt accessor can export evidence.
External finalization, no-op replacement hooks and sequential/shared reuse fail
closed without changing the provider outcome or exporting old evidence.
Once claimed, only original hooks invoked with the module-private session
capability can mutate the journal. Public capture/phase/finalize attempts reject
before requests or phase changes and record a fixed incomplete issue while the
session is active. Standalone unclaimed helpers retain their public observation
API; original receipt reads remain available. The capability is never passed to
the transport, provider, public hook metadata or serialized evidence. Future idle
coordination inside a claimed session requires a separately approved protocol.

The final wrapper receipt contains a frozen clone of the bounded selected helper
receipt: identity/limits, CPU/memory/blkio and selected network-byte samples, exact counter deltas, endpoint
time basis, partial/null availability, journal, terminal and observer costs. Its
serialized bytes must fit the factory's registered ceiling. Size/read failure
omits the failed body; helper incompleteness remains bounded partial evidence and
makes wrapper completeness false. Unregistered injected observers remain
wrapper-only, and their arbitrary receipt methods are never queried. Caller
mutation and late helper activity cannot change the session's retained clone.

For entered create/workload/cleanup/shutdown phases:

1. Complete or bound observer begin before the existing native snapshot and timer.
2. Run the existing native operation/timer unchanged.
3. Capture existing native end diagnostics and quiescence before observer end.
4. Keep observer errors separate; always preserve provider result and cleanup.

An outer observer-only `finally` covers success, skipped/revision mismatch,
setup/workload failure and cleanup failure. Finalization occurs once. Disabled
calls add no hooks or receipt fields and preserve existing timing/result shape.
The terminal receipt separates `owned_operations_settled`,
`workload_native_evidence_complete`, `native_profiling_enabled`, actual
`cleanup_complete`, and sticky `operation_deadline_failed`. Native quiescence is
true only with complete enabled workload proof and settled operations, false for
observed pending/native nonquiescence, and null for unobserved/unavailable proof.
Expected live-registry retirement during successful shutdown is retained as
unavailable phase evidence; it does not erase earlier complete workload proof or
claim cleanup uncertainty. Observed native work during cleanup/shutdown does block
continuation. Disabled/unavailable workload proof cannot clear continuation.

The future pair predicate additionally requires a complete observer terminal,
successful cleanup, no deadline failure and no prior-provider uncertainty. It is
a scoped observation, not authorization or workload qualification. A floor-only
failure can meet that predicate with enabled complete native proof. Setup or
workload timeout, pending native work, deferred/failed/unknown cleanup or missing
receipt cannot. This slice does not execute a second member or clear pair safety.
Native completeness covers instrumented/registered diagnostic rows and their
lifetime coverage. Uncovered native operations, global allocator activity,
background maintenance and provider/client C-library work remain unproved.

## Required controls

Actual pre-hook setup-rejection RED; successful/failing/skipped/revision-mismatch
finalization; hook failure never replacing cleanup; disabled shape; controlled
clock hook cost excluded from create/workload/shutdown timings; pending work never
quiescent; exact >2^53 one-unit delta; invalid uint64; inspect/additive-field secret
sentinel absent; identity/role/CID/restart rejection; missing versus zero;
reset/duplicate/changed devices; partial aggregate; missing preread; timestamp
ordering/skew; request late-completion/abort; response/request/journal/cadence caps.

Original legacy/lifecycle W26 floor 1000 and verifier stay unchanged. Compact
constructor selection still reports persisted marker unobserved. Actual marker
reads, real Engine transport, owned fixtures and matched benchmark execution need
a later explicit lease after independent acceptance.

## Implementation checkpoint

The accepted base explicit-capture suite includes the original 24 adversarial
controls plus 13 factory export/ownership controls. The network extension retains
those exact 37 controls and adds 11 projection/availability/ownership controls,
with an actual 37-pass/11-fail source-first RED before implementation. Actual factory-to-runner retention RED
and independent stale/shared/reused/no-op hook REDs precede the export correction.
The export controls exercise selected CPU/memory/blkio, exact >2^53 observations
and one-unit deltas, identity/time/hash provenance, missing operation availability,
unregistered receipt nonexport, public receipt override exclusion, bounded partial
receipt and immutable final clone.
Runner-only journal provenance has an additional actual external capture/begin/
end/finalize mutation RED before its private-capability correction. Guarded behavioral
RED receipts cover phase-specific workload/cleanup/native evidence and observed
shutdown work; historical REDs cover the pre-hook setup failure, incomplete result,
dispatch timestamps, owner deadline, hung-hook finalization, late receipt mutation,
streaming deadline, unclosed phase, receipt size and unavailable native evidence.
Historical controls without a process-wide capture override may have loaded the
existing real addon through `environmentRecord` metadata, called native errno
metadata, and called diagnostics. They cannot support a no-addon claim; no store
or backend activity is reconstructed from them. They remain retained unchanged.
The read-only `historical-loader-audit.md` records this limitation.

Network controls cover exact numeric tokens above 2^53 and one-unit deltas, sums
beyond uint64, independent null/zero directions, key change/reset, partial CID
aggregation, cap lowering, unsafe keys/selected counters, dropped additive fields,
factory-to-runner retention and bounded journals. RustFS controls qualify only
the direct service label pair and reject cleanup purpose/missing/mismatched labels.
All transport/counter and terminal inputs are injected; synthetic native metadata
is not real native proof or real network/physical-flow evidence.

Final guarded GREEN receipts and exact final source identities are frozen under
`/private/tmp/mount-rs-backing-observer-evidence-20260926` (immutable v1) and
`/private/tmp/mount-rs-backing-observer-v2-export-evidence-20260926` (narrow export
correction). The v1 runner wrapper omitted backing samples; v2 closes that
retention gap only for privately claimed fresh factory observers. The final native
diagnostic controls use the existing CommonJS capture binding with restored
synthetic JSON only. Launch requires the explicit override and unset WASI
force/flavor and NODE_PATH. The suite rejects real `.node` loads before benchmark
calls, then asserts the binding marker and empty native require cache. No real
addon, Engine, socket, backing fixture or matched benchmark was executed by the
final guarded controls. Independent acceptance remains separate.
The narrow network correction is separately frozen under
`/private/tmp/mount-rs-backing-observer-network-evidence-20260926`; accepted v2 and
historical packets remain unchanged. No Engine adapter or real request is included.

Primary semantics: [Docker API versions](https://docs.docker.com/reference/api/engine/),
[one-shot version history](https://docs.docker.com/reference/api/engine/version-history/),
[official Engine v1.51 schema](https://raw.githubusercontent.com/moby/moby/v28.3.3/api/swagger.yaml),
[runtime metrics](https://docs.docker.com/engine/containers/runmetrics/),
[Docker stats](https://docs.docker.com/reference/cli/docker/container/stats/).
