# Conditional catalog reads

This slice changes Unix service catalog reads after a catalog has been loaded. It does not change the direct SDK storage path or establish the cause of the application's 1000 ops/s CI floor. The service catalog is an authorization input for remote requests.

## Authority protocol

Each of the eight pooled SQLite connections owns a certificate containing that exact connection's `PRAGMA data_version` result, the shared cache's checked generation, and a retained `Arc` of the decoded catalog. A hit requires all three to match the currently installed shared snapshot. The retained `Arc` prevents an old control block address from being reused as proof. Up to eight previous snapshots can remain retained until their handles are revisited.

Reads and own CAS acquire the selected connection mutex before the shared cache mutex. No path acquires a second connection while holding the cache. Both guards live entirely in a `spawn_blocking` closure. Own CAS invalidates the shared cache before beginning its transaction and retains the cache guard through commit, rollback by transaction drop, and final backing check. An aborted async waiter cannot release this serialization early. CAS conflicts and uncertain results are not replayed.

A read verifies backing identity and autocommit state, then probes `data_version` on the selected connection. A certified hit avoids selecting the BLOB, while still sampling pager availability and verifying backing again. On a miss it uses the existing typed, bounded full-row selector and decoder, releases the row statement, confirms autocommit, then probes the same connection again. Equal probes permit a new certificate. Unequal probes return the validated row at its SELECT observation point without a reusable certificate; the next call must query. Errors invalidate the shared cache and fail closed. A checked generation never wraps; exhaustion is terminal for that catalog instance. Non-Unix continues its full-row path.

The token covers supported SQLite-managed commits observed by one exact handle; it is not a catalog revision, a token comparable across connections, cryptographic integrity, or a detector for unsupported raw file/WAL tampering. A commit overlapping a read can occur after the read's observation point; the next nonoverlapping read must observe the committed authority. Path device/inode checks retain the prior contract and do not make replace-and-restore races descriptor atomic. The existing 5-second SQLite busy timeout and request caps remain.

## Diagnostics and controls

Local `CatalogReadDiagnostics` counts same-handle token probes/errors, certified hits, full-BLOB query calls and logical returned bytes, pager sample calls/availability, and observations on each fixed pool slot. Counters are active only when the existing profile switch is enabled. The existing `catalog.query_document_bytes` event still means a full-BLOB select. An absent delta row is interpreted as zero only alongside an enabled profile, positive `catalog.load` calls, a positive private full-row query control, and successful pager/slot coverage.

The private unit selector runs the same full-row query and decoder, without certificate probes or a production override. The exact 10,000-Drive/5,000-Partition/10,000-grant document is 2,271,257 bytes. Same-binary, serial 40-read windows compare that full-row control with warmed conditional reads. Rust allocation observations count only the current thread inside the synchronous SQLite loader closure; process CPU includes other process work. SQLite C allocations, OS device operations, physical flash I/O, async scheduling, and end-to-end RPC throughput are outside those measurements. Profiling adds pager sampling and JSON observer allocations, so enabled and disabled windows are reported separately.

The signed wire control uses one authenticated QUIC connection and one opened read handle. An independent committed same-revision grant revocation precedes the next RPC; all eight observed pool slots reject with `EACCES` without invoking the backend read. Issuer, driver-definition, and own-CAS revocations are separate assertions. Already-authorized in-flight operations are outside this next-RPC guarantee.

## Evidence boundary

The original 40-read target-shape RED, native correctness matrix, signed wire result, paired control/candidate logs, formatting, scoped Clippy, affected tests, exact source/binary hashes, and protected-file manifest belong to the immutable packet under `/private/tmp/mount-rs-catalog-conditional-read-evidence-20260926`. This document describes the source contract; independent SPEC and QUALITY acceptance and a final root delivery remain separate gates.
