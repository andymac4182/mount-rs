# Observed SDK construction and local runtime eviction

## Scope

This increment connects the SDK construction journal to actual provider and
filesystem owners. It prepares bounded activation without enabling lazy CLI or
process startup. Live bottleneck diagnostics remain available as described in
[the quickstart](../bottleneck-quickstart.md).

The observed split API retains its provider group before constructor polling,
retains actual PGlite/private TiDB constructor resources and synchronous provider
Arcs, then forwards the same observer to Chunked authority construction. Within
the group, cleanup preserves metadata-before-block ordering and stops on failure.
The outer journal supplies owned cleanup independent of its waiters. Direct
group or SDK shutdown futures alone do not supply that cancellation guarantee.

Observed shutdown and construction-resource close reuse Chunked's full authority
barrier: sticky failure and pending checkout are checked before and after
shutdown, and no writer lease or local grant may remain. Resource close uses
that barrier even for a filesystem opened without an observer. Direct unobserved
shutdown retains its existing behavior. Successful healthy MRC5 opening
captures provider durability flags and immutable backing identity in a bounded
eligibility getter; it performs no allocation, state lock or storage request.
Eligibility does not replace fresh authorization, pin accounting or drain proof.

## Controls

| Surface | Executed cases | What is checked |
| --- | ---: | --- |
| SDK integration | 7 | Invalid options register no owner; decorator failure retains the real provider; actual SQLite full bytes, EOF and backing survive handoff/reopen; volatile/noncompact owners remain ineligible; a real rejected publication prevents provider shutdown; retained ordinary split owners reject failed authority; a snapshot filesystem is retained before application post-open work. |
| Delegated SDK cleanup | 3 | Canonical SQLite checkout commits before its acknowledgement is canceled; the canceled task is joined; observed shutdown and resource close reject that uncertainty; exact persisted grant/backing remain intact. Healthy cleanup retires the exact grant and preserves full bytes/EOF/backing through fresh reopen. |
| Provider group and wire fixture | 15 | Actual PostgreSQL/MySQL constructor boundaries and cancellation retention; metadata-before-block ordering; terminal failure; late registration and poison; shared-context pool ownership; synchronous block retention; an actual early EOF before fixture stop is still joined and does not become a provider cleanup receipt. |
| SQLite runtime pool | 5 | Ten opens/nine evictions across three definitions at capacity one; full bytes and exact/beyond EOF; actual persisted backing markers; cloned request and handle pins; cross-chunk patch and sparse append; failed publication quarantine without replacement; durable MRC4 remains ineligible; a separately owned sibling stays usable. |

The SDK tests first reproduced two semantic ownership failures and a separate
failed-authority shutdown failure. Independent review then found two uncovered
contracts: retained ordinary split cleanup accepted sticky failure, and both
observed/resource cleanup accepted canceled but committed checkout. The new
controls reproduced all three failures before the shared barrier was factored.
Missing-API and test compilation baselines
are recorded separately; they executed no behavioral cases. The service tests
initially used nonexistent convenience APIs; corrections use the actual
open/stat/positioned-read/close and context-close contracts.
The broader run also exposed an early-EOF fixture race during canceled TiDB
construction. The fixture now joins the actual terminal peer after an
undeliverable stop signal; a deterministic control forces EOF before stop.

## Standalone lockfiles

The provider-matrix locked CI build rejected its stale SDK-to-Tokio edge locally
before repair. The provider-matrix and RustFS graphs each require that one edge.
The AWS fixture additionally lacked the existing RustFS-to-Tokio edge; Cargo's
offline resolution changed only that lockfile entry. No registry version was
updated. Final locked build/no-run receipts validate all three graphs.

Hosted job/step metadata identifies failing entry points at the preceding PR
head. It does not expose the exact internal stderr or prove those lockfiles
caused every hosted failure. Hosted Windows, backend runtime and native mount
qualification remain separate gates.

## Limits and next work

The SQLite controls run in one local library process. Weak runtime disappearance
proves release of that Arc, not native SQLite connection/thread/process drain.
These ten SDK/five concrete pool cases do not test canceled cleanup waiters.
Protocol peers qualify construction boundaries, not live PGlite/TiDB databases.
Provider durability flags and local rereads are not power-loss or replicated
durability proof.

SlateDB can start internal tasks before returning its database; this increment
does not own that interval. AWS credential-source selection still needs a frozen
construction choice while preserving token-content refresh. FoundationDB's
process-scoped network guard remains a separate integration seam. No backend is
disabled or silently converted to a different persistence model.

Lazy signed CLI and real-process wiring, actual native-resource observations,
backend comparisons, formal/crash acceptance and the unchanged ten-server,
10,000-client/Drive, 5,000-Partition, 1,000-file/Drive workload remain open.

The source-pinned [receipt report](sdk-observed-construction-20260928/report.json)
records exact selected cases, ordinary default/profiled gates, compilation-only
controls, intentional lock resolution and retained failure baselines.
