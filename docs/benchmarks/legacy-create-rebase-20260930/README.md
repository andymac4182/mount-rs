# Legacy fresh-create amplification — 2026-09-30

## Measured change

A deterministic durable SQLite control pauses a fresh create after preparation,
commits an unrelated file, then resumes the create. The unchanged implementation
discards its prepared blocks when the namespace revision changes. The new code
uses the existing guarded create rebase for legacy fresh creates too.

| Observed provider calls | Before | After |
| --- | ---: | ---: |
| Total blob PUTs for both files | 3 | 2 |
| Metadata publications for the affected create | 2 | 1 |

The baseline actually failed the two-PUT assertion; the candidate passed both
count assertions. Both executions first verified complete payloads, membership,
sizes and EOF through a fresh SQLite reopen. The occupied-path control passes
before and after, retaining the existing replacement fallback.

These are logical provider calls in a controlled race. They are not HTTP request
counts, physical IOPS or a TiDB/RustFS throughput benchmark.

[observations.json](observations.json) retains the actual count lines, source
hashes and capture hashes. The later baseline/candidate counter receipts are
`0f1947cb56a4bd80295c4d18a6325ede44a8d1f8f16e0d656ae589a28964ffe4` /
`ff68a931aa104e1fa4ac35e2ce5463c8fe9259a3adff048f9fbb5dc62f021725`.

## Relation to the failed performance gate

The retained legacy TiDB gate completed 1,200 API operations at 388.69 ops/s;
PGlite completed them at 718.52 ops/s. Its 400 fresh creates incurred 299 / 357
stale-create fallbacks, 699 / 757 logical PUTs and 633 / 757 metadata publications.
Identical payloads coalesced behind adapter flights, so those PUT totals are not
backing HTTP totals. Indexed MRC5 was not selected by that gate.

The gate still requires 1,000 successful API operations/s, the same legacy layout,
4 KiB payloads and concurrency 64. It must run again to establish any throughput
benefit. Native indexed-drive results are a separate workload.

## Verification

The candidate passes 239 ordinary ChunkedFs tests, with 25 opt-ins ignored.
Seventeen relevant opt-ins were then explicitly enabled and passed: current-path
create, parent/symlink/occupied-path guards, cancellation, uncertain outcomes,
lease recovery, changed chunkers and causal counters. Strict all-target Clippy
passes after formatting.

The production change preserves operation gates, leases, authority and chunker
checks, publication fencing, uncertain-write handling and acknowledgement
barriers. It changes which fresh creates can reuse already prepared blocks.

- Baseline regression receipt SHA-256: `c2dac972a0d18758f8e01b4c0732439dbd3f38a7fb54f409fe5c51cd664affbc`.
- Candidate regression: `89f225a1f538063a36cea5672b2a7a93d3544f73f8a25f2b16388f6b426cee2b`.
- Ordinary tests: `5af2d2ca0393392644b0dd1fcc6f1761679e7a6ae90910547225de2fa2de0cbc`.
- Enabled opt-ins: `99dd3046a4da9957ed6eaf9695b1d7136ed3e1bf6a927e3033386ccec261581e`.
- Strict Clippy: `1f694b0f350e1b3518922b20cf4996d92bc7323a0c21e781bf058eab85e72927`.

The benchmark goal remains active; the production client target is unqualified.
