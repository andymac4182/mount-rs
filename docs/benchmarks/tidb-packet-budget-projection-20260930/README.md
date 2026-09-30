# TiDB selected-write packet-budget projection

The selected inode writer now returns `@@SESSION.max_allowed_packet` in its required fresh authority/member query. It retains the raw value until after update validation and guard encoding, then applies the existing minimum of client and server limits before DML. Guard locking still precedes the fresh Read Committed authority read. Structural publication and read-point SQL are unchanged.

## Actual before/after query control

The existing owned TiDB `actual_indexed_write_cycle_uses_two_inode_reads` gate was extended first. Before production edits it reached cleanup and the complete persistent successor oracle, then failed as intended: one standalone packet query instead of zero. After the change the same gate passed.

| Warm file read + selected publication | Before | After |
|---|---:|---:|
| SQL commands observed by the proxy, including transaction controls | 8 | 7 |
| Standalone packet-limit queries | 1 | 0 |
| Complete guard rows returned | 2 | 2 |
| Selected guard UPDATEs | 1 | 1 |
| COMMITs | 1 | 1 |

The authority/member query now includes the cap. Warm `tidb.sql.session` calls are zero. The complete fresh snapshot matches the acknowledged selected guard, unchanged authority and every untouched root/sibling guard. This is a real provider query-count result; it is not a throughput or physical IOPS measurement.

## Verification

- All six actual indexed-write controls passed: the query budget, authority and generation precedence, missing membership, retained tombstone bytes, and publication while another file guard remains locked.
- Three additional actual controls passed: oversized bodies return EFBIG before mutation; authority is rechecked after waiting for the selected guard; a lost real COMMIT acknowledgement returns an unknown outcome without replay, with the exact durable successor visible independently.
- Formatting and strict all-target Clippy passed. The ordinary all-target run passed 58 tests; 97 opt-in tests were ignored in that general run. The nine actual controls above were executed separately against the owned TiDB fixture.
- Every command owner settled without a deadline/capture/host-floor failure, with its child reaped and process group absent. Fresh read-only checks before and after confirmed all original eight containers, lifecycle/configuration identities and six volumes were preserved. Unique test-volume rows are retained.

The [verification data](verification.json) records source hashes, actual owner/capture hashes and the scoped query contract. Production source was unchanged after the actual controls; the final formatting adjustment only wrapped a test string expression. The preceding repository commit is `be510e4bd638504306eca37ec63f9379fdb6e8e9`.

Typical actual commands used Rust 1.95.0, an isolated shared Cargo target, `--locked --offline`, serial ignored cases and `MOUNT_RS_PROFILE_IO=1`. TiDB configuration came from the already authorized fixture; no global/session configuration or container changes were made by this optimization.

## Remaining scope

The [fresh matched native comparison](../tidb-packet-paired-20260930/README.md) now confirms six to five classified SQL calls per overwrite. Its single instrumented source pair showed no all-active overwrite throughput or process-wide allocation gain; it does not isolate metadata allocation or a causal performance change. The [previous native profile](../tidb-rustfs-current-scale-20260930/README.md) measured source 642 and therefore still reports six SQL calls per overwrite cycle. This selected-write change leaves the 44-call structural churn path and its full directory/member reads intact.

The full ten-server / 10,000-client / 10,000-Drive / 5,000-Partition / 1,000-files-per-Drive target, composed peer cache qualification and final CI/merge remain outstanding.
