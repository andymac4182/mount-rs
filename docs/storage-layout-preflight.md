# Storage layout comparison preflight

`inspectSplitNamespacePresence(metadata, blocks)` inspects an explicitly
configured TiDB metadata namespace and RustFS block prefix before constructing
a filesystem. The benchmark validator accepts its fixed JSON receipt only
when both observations report absence and the TiDB pool has closed.

The metadata query is a single fixed, parameterized SELECT over metadata,
inodes, compact guards, block authority and blocks. Keys use the provider's
binary SQL columns. Inspection can initialize shared schemas and configure
sessions; it inserts no namespace rows. Its additional instrumented source
sites make the TiDB coverage descriptor 35 pool checkout and 57 SQL sites.
These are source counts, rather than observed calls or backend IOPS.

The RustFS query is a signed listing for exactly `prefix/`, requesting at most
one key. Absence requires zero objects, zero common prefixes and no continuation
token. Unexpected, incomplete and failed pages refuse the preflight. The locked
object-store parser buffers responses and does not expose the server truncation
flag. Requesting one key does not guarantee a response byte bound.

The binding accepts canonical URI unreserved ASCII scope components, without
empty, dot or dot-dot components. Metadata keys are capped at 255 bytes and
blob prefixes at 512 bytes. Unsupported provider combinations and fields are
rejected before backend access. Configuration and provider errors are reduced
to fixed categories; receipts contain no keys, endpoints or credentials.

Metadata and blob observations share a 30-second cooperative deadline. Elapsed
checks reject late completed observations as well. The pool shutdown has a
separate 15-second deadline and elapsed check; failed or unconfirmed shutdown
cannot produce an accepted receipt. Cancellation of the overall caller is not
an awaited shutdown receipt and must stop the comparison.

The receipt schema is `mount-rs.split-namespace-presence.v1`. Observation times
are exact decimal nanoseconds elapsed from one native monotonic start. The
consumer caps receipt size at 4096 UTF-8 bytes, rejects duplicate JSON members,
unknown fields, wrong types, invalid clock order and contaminated scopes, and
returns a deeply frozen copy. It performs one native dispatch without retry.

These are separate API observations, with no reservation or atomic exclusion.
A writer can populate either scope afterward. The comparison must also own its
cohorts, verify the selected persisted layout and canary bytes after reopening,
and stop on pending work or cleanup failure. Shutdown does not purge namespaces.

Local controls exercise SQL source selection, listing behavior through a fake
page store, the native lifecycle through private operation callbacks, and the
public JavaScript error/receipt adapters. They provide no live TiDB/RustFS
absence or throughput evidence. The regular CI controls keep those evidence
levels separate.

## Isolated layout arm and runner evidence

`createOwnedLayoutArm` pins a private TiDB/RustFS configuration and a fresh owned
cohort. Before handing its original filesystem to the timed runner, it checks
absence, constructs with all three layout flags explicitly false or true,
checks an empty root, and writes, syncs and closes a fixed 4096-byte canary.
Compact arms query the persisted MRC5 receipt. A legacy `null` observation
establishes recognized non-MRC5 state, rather than an exact legacy marker.

The arm captures the drained layout and awaits original shutdown once. It then
opens two fresh handles from configuration: the first verifies all canary bytes
and EOF, removes the canary and closes; the second verifies the persisted layout
and empty logical root before closing. Compact backing identity must remain
stable. Structural generation can increase across filesystem mutations and must
match exactly across a drained-to-fresh-reopen boundary. Failed or pending
shutdown cannot permit another reopen.

`assessOwnedLayoutRunnerOutcome` separately checks the original fixed
400-iteration, 64-concurrency, 4096-byte workload, its 1200 successful operations
and 400 verified reads, every sample, cleanup and original status/failure set.
It validates the explicit RustFS diagnostic family, compact proof when selected,
native quiescence and independently recomputes the backing interval. Sparse
samples, accessors, inconsistent counters and incomplete observations refuse
continuation. The projection retains fixed categories and numbers, excluding
credentials, raw errors and layout identities.

`floor_qualified` and `runner_safe_to_continue` are separate fields. A sole
original `IOPS_TARGET_NOT_MET` failure keeps the original failed status and
unqualified floor. It can be safe to collect another arm only if all other
runner evidence passes. The enclosing comparison must still verify arm
persistence, native identity, fixture ownership and identity across arms, and
stop on uncertain work. These helpers add no operation timeout or capacity
override. Their tests use modeled evidence; no live paired performance result
is implied.
