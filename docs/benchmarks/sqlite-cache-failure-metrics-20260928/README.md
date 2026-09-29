# Persistent SQLite cache fault measurements

This component test seeds three distinct 4 KiB files through the MRC5 SDK,
then measures a real SQLite block provider behind RAM, disk and authenticated
QUIC peers. [report.json](report.json) retains numeric observations, source
hashes and the bounded test receipts.

| Phase | Backing GETs / SELECTs | Cache hits | SQLite VFS reads / bytes |
| --- | ---: | ---: | ---: |
| Cold nonholder | 1 | 0 | 4 / 11,888 |
| Disk-only peer | 0 | 1 peer | 0 / 0 |
| Stale holder | 1 | 0 | 2 / 3,696 |
| Holder outage | 1 | 0 | 2 / 3,696 |
| Same holder restart | 0 | 1 peer | 0 / 0 |
| RAM cold | 1 | 0 | 2 / 3,696 |
| Twenty RAM repeats | 0 | 20 RAM | 0 / 0 |
| Disk budget rejection | 1 | 0 | 2 / 3,696 |
| Disk cold | 1 | 0 | 2 / 3,696 |
| Disk restart | 0 | 1 disk | 0 / 0 |
| Corrupt isolated disk | 1 | 0 | 2 / 3,696 |
| First eviction admission | 1 | 0 | 2 / 3,696 |
| Eviction replacement | 1 | 0 | 2 / 3,696 |
| Evicted block after restart | 1 | 0 | 2 / 3,696 |

The 33 full reads require ten backing GETs and ten SQL SELECTs; 23 are cache
hits. The twenty repeated RAM reads are an explicit pattern, so this hit ratio
does not forecast production savings. Each phase resets observations after
setup, checks the real provider connection, observes completed cache workers,
and rejects missing counters, pending work and overflow.

The first cold GET has two SQLite pager misses. Later backing GETs have three
pager hits and no misses, yet still issue two VFS reads. Pager statistics alone
therefore omit some observed SQLite I/O. VFS reads total 45,152 bytes for 40,960
returned backing bytes. These are SQLite callbacks; local cache disk I/O and
physical SSD operations have separate scopes.

## Admission failure exposed by the metrics

The first draft returned correct bytes but failed its disk-entry oracle.
`maintenance_dropped=1` identified refused optional admission. Production pending
reservations charge payload plus 128 bytes, within
`max(memory_bytes, max_blob_bytes)`. A disk-only 4 KiB budget cannot reserve a
4 KiB payload. The corrected disk fixtures allow 8 KiB while keeping RAM at
zero. A separate phase retains the original rejection, one backing GET, no
disk entry and unchanged returned bytes. The production budget policy is unchanged.

The cold, restart, corruption and eviction phases verify complete entries and
checksums where required. Fresh undecorated SDK handles verify complete payloads
and EOF before and after all faults. Independent metadata and raw-block oracles
check MRC5 mode, backing identity, generation and extent identities. All peer,
cache and provider owners must be released before fixture removal.

## Reproduce

```sh
MOUNT_RS_FAILURE_EVIDENCE_ROOT=/absolute/private/evidence \
CARGO_TARGET_DIR=/absolute/isolated/cargo-target \
python3 -B scripts/test-remote-failures.py sqlitecache
```

The fixed owned parent enables profiling, disables tracing, selects exactly one
ignored test, bounds execution and rejects a zero-case pass. CI runs this same
selector. Ordinary cache tests, strict core/cache Clippy and 78 parent controls
also pass. Independent source and retained-evidence review found no remaining
material defect.

This qualifies persistent SQLite cache reads and faults on macOS ARM64. It
does not qualify ten CLI server processes, 10,000 clients, other backing stores,
power-loss recovery, native host mounting or physical device IOPS. The production
capacity goal remains open.
