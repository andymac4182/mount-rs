# Concurrent provider qualification, 2026-09-23

This record separates provider unit checks, independent coordinator load,
native NFS behavior, and cross-host claims. The feature uses opt-in
`splitstore` + `concurrent_writes`; legacy snapshot filesystems have no shared
writer claim. The branch began at fetched `origin/main`
`8a629287104fcdd6d4c334378eccfcab005817ec`.

## Current scope at a glance

| Storage / workload | Current evidence and limits |
| --- | --- |
| Memory | Sharing is within one filesystem instance in one process. Independent memory-backed CLIs do not share state. |
| SQLite provider backing | Same host and the same canonical local metadata/block files. Bundled SQLite 3.51.3 passed native DELETE and WAL two-CLI checks, one CLI with two views, fresh reopen and integrity checks. Concurrent Windows SQLite backing returns `ENOTSUP`. |
| PGlite provider backing | Clients share one persisted engine through its socket server; two split-store CLIs need four socket slots. Native bidirectional/disjoint writes and fresh engine/CLI reopen passed. Three slots rejected the second CLI before mount while the first remained readable. |
| FoundationDB metadata + RustFS blocks | Two independent macOS loopback CLIs passed one current 400-write packet: 400 acknowledged writes, 400 checks through each live view and 400 after fresh CLI reopen. Earlier OOM, sync errors and per-OPEN timeouts remain retained and unexplained where stated below. |
| Concurrent online reclamation | Disabled / `ENOTSUP`; distributed handle pins and a capacity policy are still needed. Retained immutable conflict blocks and tombstones can grow. |
| Application SQLite inside shared NFS views | Unqualified: a retained cross-view locking probe allowed the contender to acquire `BEGIN IMMEDIATE`. Local SQLite provider backing is a separate qualified scope. |
| Cross-host and recovery | These native packets use one host. Physical cross-host mounts, power-loss recovery and production operation need separate evidence. |
| Runtime and performance | Current macOS CLI/addon measurements use debug builds and Node 24.18. The latest packet enabled bounded failure-only diagnostics, with request tracing and profiling off. Sustained production performance and the configured IOPS floor are not qualified by these samples. |

The detailed history below retains each original failure and its source,
runtime, measurement and cleanup boundary.

## Current bundled SQLite engine

The workspace pins `rusqlite =0.39.0` and `libsqlite3-sys 0.37.0`, bundling
SQLite 3.51.3 with the upstream
[WAL-reset fix](https://www.sqlite.org/releaselog/3_51_3.html).
The SQLite 3.46.0 coordinator, stress, and native measurements below remain
historical evidence. The upgraded engine passed the local and native gates
below.

Fresh provider databases default to DELETE journaling, and provider
connections set `synchronous=FULL`. Existing WAL databases remain WAL when
opened for `concurrent_writes`; provider configuration has no journal mode
option and provider open does not reset the mode. Concurrent SQLite backing
still requires the same canonical local files on one host. NFS/SMB backing
and cross-host sharing of those SQLite files remain unsupported.

SQLite application databases stored inside shared NFS views remain
unqualified because the application packet observed a cross-view locking
defect. This boundary is separate from local SQLite backing and the bundled
engine fix.

The version regression failed with the expected missing-fix error on 3.46.0
before the upgrade, then passed with runtime/header version agreement.
The checkpoint regression runs 64 checkpoint rounds while a provider
connection holds a reserved writer transaction, interleaving 66 acknowledged
block writes. It verifies the pinned reader snapshot, WAL reset after release,
fresh exact block reads, row count and integrity. This ordinary bounded edge
test does not reproduce SQLite's rare upstream race or impose a hard worker
join deadline.

Local macOS gates on the upgraded bundle:

| Gate | Result |
| --- | --- |
| SQLite provider library | 64 passed, including version and checkpoint regressions |
| SQLite VFS all targets | 5 library + 16 engine + 16 storage bridge tests passed |
| CLI all targets | 66 library + 29 integration tests passed; opt-in native/service cases stayed ignored |
| Core local SQLite integration | 12 concurrent coordinator + 1 backend fault tests passed; 5 opt-in cases stayed ignored |
| Opt-in local WAL load | 8 × 100 lifecycles; 2,008 acknowledged operations, fresh exact reads and both integrity checks passed in 52,436 ms; 833 block rows and 383,934 namespace bytes |
| Serial native SQLite packet, WAL enabled | All 4 cases passed in 16.18 s: two CLIs in DELETE and WAL, one CLI with two views, and both NFS backing rejection cases |
| SQLite provider, VFS and CLI all-target strict Clippy; workspace formatter | Passed |
| Full offline locked metadata resolution | Workspace plus all 5 excluded packets passed; unrelated package pins and Windows edges retained |

The direct dependencies enable `fallible_uint` to retain the previous checked
unsigned column conversions. Two VFS test calls now pass `&str` explicitly
for rusqlite's generic VFS-name argument; the production path already did so.

The fixed-engine native two-CLI 2 × 12 loads took 934 ms in DELETE and 836 ms
in WAL. Both modes retained exact data after fresh process reopen and passed
metadata/block integrity checks. NFS blocks were rejected with local metadata
still at revision zero, without a namespace or concurrent-mode marker. The
owned root, mounts, ten observed child PIDs and five observed listener ports
were independently confirmed absent; the pre-existing user mount, PID 66542
and listener 55309 remained unchanged. This opt-in WAL run supplements the
default native CI gate, which still runs DELETE. The short inner NFS lock
probe reported blocked in both modes; it does not qualify SQLite application
files inside shared NFS views or replace the retained adversarial failures.

Raw upgrade logs are retained under
`/private/tmp/mount-rs-sqlite-wal-fix-{provider-green,vfs,cli,core-local,clippy,wal-8x100,native}-20260924.log`.
Native ownership and independent cleanup evidence are in
`/private/tmp/mount-rs-sqlite-wal-fix-native-20260924.json` and
`/private/tmp/mount-rs-sqlite-wal-fix-native-cleanup-20260924.json`.

Focused checks:

```sh
./scripts/cargo-shared test --locked -p mount-rs-sqlite \
  bundled_sqlite_has_wal_reset_fix -- --nocapture
./scripts/cargo-shared test --locked -p mount-rs-sqlite \
  wal_checkpoint_contention_preserves_acknowledged_blocks -- --nocapture
```

## Current-source PGlite native requalification

On merged main `d983be89`, the three PGlite-only cases in
`apps/mount-rs-cli/tests/native_two_process_pglite.rs` passed serially on macOS
in 20.51 seconds. The isolated checkout was `25d793f6`, with the same complete
source tree as that main revision. Node 24.18.0, PGlite 0.5.8 and socket server
0.2.11 were used; the outer 300-second watchdog did not fire.

| Case | Result |
| --- | --- |
| One CLI, two writable views | Bidirectional bytes, rename/unlink and clean shutdown passed |
| Two CLIs, one persisted engine | 20 files per writer; 160 acknowledged write, local read, cross-view read and unlink calls in 12,768 ms; disjoint-range writes and fresh engine/CLI reopen assertions passed |
| Three socket slots | The second CLI was rejected before mounting; the first CLI's existing file remained readable |

The slot case does not test subsequent slot reuse or a new write from the
first CLI. The RustFS-block variant was excluded from this packet. This is
same-host loopback acceptance against one engine, not physical cross-host
or sustained performance qualification. Each engine owns its data directory;
the reopen case stops the original engine before opening its replacement.

The exact temporary root and mounts were removed, the entire dedicated
process group was empty, five sampled test/CLI PIDs and two observed listener
ports were absent, and the owned dependency symlink was removed. The original
dependency directory and user's mount/PID 66542/listener 55309 were unchanged.
Short-lived Node PIDs were not captured by the observer's raw argv match;
engine restart/stop is asserted by the passing fixture, with the empty process
group verified independently afterward.

Immutable raw log:
`/private/tmp/mount-rs-current-pglite-native-20260924.log` (925 bytes; SHA-256
`a9d16d13ea0c0de60b26bf9aafa82f84c306e2f01d5a2424266a03c17cb8e14d`).
The command, source/CLI/test hashes and sampled resource registry are in
`/private/tmp/mount-rs-current-pglite-native-20260924.json`; independent cleanup
evidence is in `/private/tmp/mount-rs-current-pglite-native-cleanup-20260924.json`.

## Current CI checkpoint

For PR #20 head `25d793f6`, fault workflow `35878531285` passed on Ubuntu,
macOS and Windows. Primary CI `35878531432` also passed Windows Rust and
Ubuntu native NFS. The latter freshly built rusqlite 0.39/libsqlite3-sys 0.37,
rejected NFS metadata and blocks before any marker or namespace publication,
and passed privileged checkpointed/active-WAL file-alias rejection. Its
single-view Python application-SQLite restart probe used host SQLite 3.45.1;
that probe is separate from the bundled 3.51.3 engine gates above.

The same CI run's Ozone compositions completed all 400 exact-byte lifecycles
(1,200 acknowledged operations), with zero timeouts and cleanup failures,
but failed the configured 1,000 IOPS floor: TiDB measured 369.94 IOPS and
FoundationDB 597.83 IOPS. Earlier baseline floor failures remain recorded
below. These isolated CI windows are not controlled performance comparisons,
and this checkpoint does not claim all repository CI is green. Windows Node
job `107240666347` also failed the snapshot-SQLite S3 fixture's 32-request
fan-out: PUT request 5 hit its 15-second client deadline. Its preceding
WebDAV SQLite network/crash checks passed. This is an unresolved Windows
load result; the log does not establish an engine regression or data loss.

Exact current job logs are retained at
`/private/tmp/mount-rs-pr20-native-nfs-ubuntu-107240666428.log`,
`/private/tmp/mount-rs-pr20-ozone-tidb-107240666108.log` and
`/private/tmp/mount-rs-pr20-ozone-foundationdb-107240666543.log` and
`/private/tmp/mount-rs-pr20-windows-node-107240666347.log`.

## SQLite coordinator load

`tests/concurrent_sqlite_load.rs` opens independently connected metadata and
block stores over the same disposable local SQLite files. It performs whole
file create/rename/unlink lifecycles, simultaneous writes to disjoint ranges
of one file, fresh reopen, exact byte checks, and `PRAGMA integrity_check` on
both databases.

| Code at run | Journal | Writers × lifecycles | Result | Elapsed | Block rows | Namespace bytes |
| --- | --- | ---: | --- | ---: | ---: | ---: |
| New SQL CAS, core retry limit 32 | DELETE | 2 × 12 | 62 acknowledged lifecycle operations; integrity OK | 131 ms | 39 | 12,447 |
| New SQL CAS, core retry limit 32 | DELETE | 4 × 12 | Repeatable `EAGAIN` during one writer's whole-file publication | about 300 ms to failure | Not measured | Not measured |
| Temporary core retry limit 128 | DELETE | 4 × 12 | 124 acknowledged lifecycle operations; integrity OK | 474 ms | 170 | 24,105 |
| Temporary core retry limit 128 | DELETE | 8 × 100 | 2,008 acknowledged lifecycle operations; integrity OK | 70,951 ms | 10,019 | 383,934 |
| Rebased whole-file layout, original retry limit 32 | DELETE | 4 × 12 | 124 acknowledged lifecycle operations; integrity OK; eight repeats passed | 349–519 ms | 55–57 | about 24,106 |
| Rebased whole-file layout, original retry limit 32 | DELETE | 8 × 100 | `EAGAIN` during write and rename under high contention; load stopped | 14,250 ms to failure | Not measured | Not measured |
| Rebased layout plus async backoff, retry limit 64 | DELETE | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK in one run | 55,130 ms | 834 | 383,935 |
| Rebased layout plus async backoff, retry limit 128 | DELETE | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK in two runs | 76,250 and 58,470 ms | 835 and 834 | 383,936 and 383,934 |
| Rebased layout plus async backoff, retry limit 128 | WAL | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK in one disposable run | 53,570 ms | 834 | about 383,934 |
| Checked SQLite block and lease commits | DELETE | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK | 61,671 ms | 834 | 383,933 |
| Checked SQLite block and lease commits | WAL | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK, diagnostic mode | 63,871 ms | 834 | 383,936 |
| Checked commits plus rollback-confirmed BUSY retry | DELETE | 8 × 100 | 2,008 acknowledged operations; fresh reopen and integrity OK | 52,978 ms | 832 | 383,935 |

The short fixed retry budget failed because local SQLite writers can advance
the revision while another writer repeatedly prepares new immutable blocks.
Increasing only the retry budget let the larger load finish but retained many
staged block rows. Whole-file replay now reuses its prepared immutable layout
and refreshes immediately before CAS. A deterministic revision-after-block-put
test was red before this change and green afterward; block rows in eight
repeated small loads fell to 55–57. The original 32-attempt budget still
starved under eight writers, including namespace-only rename. A bounded
async timer with jitter now staggers known conflicts; the 64-attempt policy
passed one 8 × 100 run but failed a deterministic 1.5-second sustained
conflict rename test at about 1.35 seconds. The final 128-attempt policy
passed that test and two 8 × 100 DELETE runs. The first temporary 128 run
above is a measured *before-rebase* baseline with substantially more staged
blocks. Both final runs retained about 835 blocks instead of 10,019; online
reclamation is still missing, so capacity can grow indefinitely.

A rollback-journal reader holding a SHARED lock exposed a separate SQLite
acknowledgement bug. The old `INSERT ... RETURNING` block write returned a
block ID even though a fresh connection found zero committed rows. SQLite
[documents this one-row `RETURNING`/reset failure](https://www.sqlite.org/c3ref/reset.html).
The old lease acquisition and renewal statements could likewise return a
fence or expiry while the durable row stayed unchanged. Three reader-lock
regressions failed before the provider change and passed after block IDs and
legacy leases were returned only after a checked transaction commit. The
SQLite provider suite passed 21/21. The two new eight-writer timings above
measure this completed-insert implementation; they are isolated local runs,
not a controlled IOPS comparison.

On diagnostic head `3ed628e2`, the four-writer DELETE coordinator test passed
on a Windows push runner but one PR runner returned `database is locked` on its
first write after about 33 seconds. The same test passed on both push and PR
for head `d57df80a`, so the intermittent contention remains an edge case.
Disposable DELETE-journal probes then held a competing writer at `BEGIN
IMMEDIATE` and a SHARED reader across `COMMIT`. The provider originally
returned `EIO` after its five-second busy timeout in each case; fresh reopen
confirmed no publication or block write had committed. Four focused
regressions were red before the retry and green afterward. Only structured
`SQLITE_BUSY` with a verified inactive transaction is now retryable. A
concurrent metadata publication temporarily uses a 250 ms busy timeout and
restores the connection's original timeout after the operation; block writes
retry outside the connection lock with a 16-attempt, 30-second cap. No block
ID is returned until its INSERT and COMMIT complete. The provider suite
passed 28/28 and the independent two/four-writer coordinator suite passed
4/4. Windows verification was pending at this diagnostic stage. The final
core PR-event head `688ff38a7823e1615fcfb5ac3389e0200932c33d` later passed
[Windows Rust CI job 107196626751](https://github.com/andymac4182/mount-rs/actions/runs/35865709669/job/107196626751).
Windows concurrent SQLite backing still returns `ENOTSUP`. The test also
reports the provider stage on any future CI failure.

Reproduction of the bounded load uses only test-owned files:

```sh
MOUNT_RS_SQLITE_MULTIWRITER_LOAD=1 \
  ./scripts/cargo-shared test --locked -p mount-rs-core \
  --test concurrent_sqlite_load \
  independent_sqlite_coordinators_load_and_check_integrity \
  -- --ignored --nocapture
```

Set `MOUNT_RS_SQLITE_MULTIWRITER_JOURNAL=WAL` for an isolated WAL experiment.
The opt-in `MOUNT_RS_SQLITE_MULTIWRITER_JOURNAL_MATRIX=1` test checks local
SQLite backing in DELETE and WAL. An initial probe requested TRUNCATE and
PERSIST from a preparatory connection and completed 4 × 12 lifecycles with
integrity OK, but those results are **not** TRUNCATE/PERSIST provider evidence:
SQLite reverts those rollback modes to DELETE on connection reopen. The
test now selects only modes that survive independently opened provider
connections. TRUNCATE and PERSIST are tested separately by application
SQLite connections placed *inside* an NFS mount.
The final 4 × 12 journal matrix passed again with 124 acknowledged operations
per mode, 56 DELETE block rows and 54 WAL block rows, exact fresh reads, and
both backing databases reporting `integrity_check=ok`.
The bounded 8 × 100 WAL experiment also passed on disposable local files,
but a successful stress run cannot rule out a rare timing bug. Those runs
used bundled SQLite 3.46.0, whose WAL mode was unqualified for multi-process
checkpoint contention because it lacked the WAL-reset fix. The current
3.51.3 bundle and its verification gates are described
[above](#current-bundled-sqlite-engine). See
[SQLite's WAL note](https://www.sqlite.org/wal.html#the_wal_reset_bug).

## SQLite database files hosted inside NFS

This is a different workload from SQLite as the mount-rs backing provider.
The detailed executable packet and observed JSON outcomes are in
[`tests/sqlite_nfs_adversarial.md`](../tests/sqlite_nfs_adversarial.md).

The test-owned native Mac NFS run used Python SQLite 3.53.4 and passed DELETE, TRUNCATE, and PERSIST
lock/recovery/integrity probes. WAL requested through the NFS view fell back
to DELETE and is unsupported in that profile. A 2 × 8 DELETE load committed
16/16 exact 4 KiB rows at 14.082 transactions/second with p95 commit time
72.066 ms and integrity OK.
A stronger single-view DELETE load with four SQLite application processes
committed 100/100 transactions in 4,523 ms (22.11 transactions/second), had
11 SQLite busy retries and p95 commit time 111.175 ms, and reopened with
integrity OK. These loads use the `sqlite_single_host` local-lock profile on
one NFS view; they do not establish safe locking across independent views.

Two independent NFS listeners in one Rust process, sharing one filesystem
driver but using separate local-lock tables, gave a
concrete unsafe result: a contender acquired `BEGIN IMMEDIATE` through view B
while view A held an uncommitted SQLite update. It was rolled back before any
write, and integrity remained OK. This demonstrates why
`--sqlite-single-host` is rejected with shared views; a good single-view lock
probe is not proof for two views or two hosts. The test cleaned its exact
temporary paths and left the independent live Finder demo mount alone.

## SQLite provider files placed on NFS

An additional disposable backing-file probe placed SQLite metadata and block
databases on two native NFS client views of one in-memory export. Before the
local-filesystem guard, both concurrent SQLite CLIs mounted; simultaneous
first writes gave A=`EIO` and B=`Ok(())`, while the test-owned databases
still reported `integrity_check=ok` after clean stop. That asymmetric result
does not make NFS backing safe. A macOS `statfs` `MNT_LOCAL` check in
concurrent SQLite metadata preparation is now red/green qualified: the same
native setup rejects both CLIs before mount with an explicit NFS/network
backing error, and both backing databases remain valid. The nonconcurrent
SQLite route retains its prior behavior. After block preflight was added, the
executable metadata negative isolates that check with NFS metadata and
local blocks, so startup must report the metadata-specific rejection. A
mixed local-metadata/NFS-block
negative case was red before the block-provider guard: a concurrent CLI
mounted with NFS blocks and promoted fresh local metadata to `MRC1`, revision
1, with a namespace. The first provider-guard run still failed because SDK
`ErasedBlockStore` did not forward the new `prepare_concurrent_mode` hook.
After forwarding, the same native case rejected NFS blocks **before** the CLI
mounted, with the explicit `concurrent SQLite blocks require a local
filesystem; NFS/network-backed databases are unsupported` error. The local
metadata row remained fresh: revision 0, fence 0, no namespace/owner, and no
`MRC1` marker; both databases reported `integrity_check=ok`. The executable
packet and exact opt-in commands are in
[`apps/mount-rs-cli/tests/native_two_process_sql.md`](../apps/mount-rs-cli/tests/native_two_process_sql.md).

## Qualification boundary

SQLite metadata CAS can coordinate independent CLIs using the same local
database files on one host. SQLite's guidance does not support directly
sharing those files among hosts over NFS/SMB. PGlite multiwriter clients must
reach one PGlite socket server, which multiplexes multiple connections over
one embedded engine; it is not two engines opening one data directory. RustFS
is shared immutable blocks, paired with FoundationDB or PGlite metadata CAS.
The test-owned native two-CLI macOS SQLite split-store case passed in both
DELETE and WAL backing modes: bidirectional creates, 4 KiB disjoint same-file
writes, rename/unlink, 2 × 12 lifecycles (988 and 861 ms), clean SIGINT,
fresh CLI reopen, exact bytes, and metadata/block `integrity_check=ok`.
The native positive gate runs DELETE by default; WAL requires
`MOUNT_RS_CLI_NATIVE_SQLITE_WAL=1`. At these checkpoints WAL was diagnostic
because the bundled SQLite 3.46.0 had the WAL-reset issue.
After block-hook forwarding, the same local two-CLI case passed again (2 × 12
loads in 969 and 828 ms for DELETE and WAL), and the one-CLI/two-view local
DELETE case passed (2 × 12 load in 1,561 ms). All runs checked exact data,
clean shutdown/reopen, and backing integrity; these timings are bounded test
measurements, not production throughput.
After the CLI's pre-provider mountpoint path guard changed, two fresh
test-owned DELETE runs passed again: the separate-CLI case completed its
2 × 12 lifecycle load in 1,798 ms and the single-CLI/two-view case in
3,342 ms. Both reopened exact data and reported `integrity_check=ok` for
metadata and blocks. The separate-CLI inner SQLite lock probe was blocked;
that single observation does not qualify application SQLite across views.
One SQLite-backed CLI also served two writable native NFS views and passed
bidirectional visibility, disjoint ranges, rename/unlink, 2 × 12 lifecycles
in 1,376 ms, clean unmount of both paths, fresh reopen and integrity checks.
The PGlite case used one persisted socket server and passed 2 × 20 lifecycles
(160 acknowledged high-level calls in 4,924 ms), disjoint same-file writes,
bidirectional namespace changes, server restart and fresh CLI reopen. An
intentional socket limit of three admitted one split-store CLI but rejected
the second before NFS mount while the first stayed readable. PGlite also
passed a one-CLI/two-writable-view native case and a live N-API two-writer
smoke against a max-eight TCP loopback server. The SQLite and PGlite native
lanes removed their test-owned mounts; the existing interactive Finder demo
mount was untouched. The FoundationDB/RustFS lane also passed with two
independent CLI/NFS mounts: 40/40 synchronized 4 KiB file writes in 11,716 ms,
exact readback from both live mounts, merged disjoint same-file ranges,
rename/unlink visibility, and fresh third-CLI reopen. The full disposable
RustFS harness stopped/restarted its service on a new loopback port and
reopened its blocks; the PGlite VFS restart lane passed. Test-owned NFS
mounts, RustFS Docker container and FoundationDB server were removed.
The PGlite-metadata/RustFS-blocks pairing then passed a separate disposable
two-CLI/two-writable-NFS-view run: 160 acknowledged lifecycle calls in
5,660 ms, merged disjoint same-file writes, rename/unlink visibility, clean
unmount, and exact RustFS bytes after a fresh PGlite server and CLI reopen.
The runner and RustFS harness both exited successfully and removed their
owned mounts, test roots, and service. Both services used macOS loopback;
this is same-host separate-process evidence.
After the signed startup probe landed, the full disposable RustFS service
harness passed again. Its real-service negative lane rejected wrong signing
credentials, a missing bucket, and a changed reserved probe before fresh
SQLite metadata entered concurrent mode; a valid client reused the probe.
The native FoundationDB/RustFS lane then passed with two separate writable
NFS CLIs, 40 acknowledged writes in 17,310 ms, same-file merge,
rename/unlink, and exact fresh third-CLI reopen. Both owned NFS mounts, the
test-owned RustFS container, FoundationDB process, sparse APFS device, and
temporary roots were cleaned. The harness also restarted RustFS and
reopened exact blocks from a fresh client.
That fixture needed a private 1 GiB sparse APFS image because the host Data
volume was 97% full: FoundationDB's storage server reported it was near its
5% free-space margin when run directly on Data. The temporary APFS image
provided enough free percentage for a real FDB transaction readiness check;
the runner never accepted a merely connected client or a status message that
said the database had issues as proof of writable readiness. This is a
test-environment capacity finding, and production FoundationDB data volumes
also need operating free space.
The standalone Linux FoundationDB/RustFS composition now imports the named
RustFS crate rather than the R2 compatibility adapter. A disposable durable
three-node FoundationDB run passed initial chunked composition, its provider
contracts, a separate-container transaction readiness check after service
restart, fresh-client authority publication, and RustFS block reopen. The
first single-node restart attempt timed out with FoundationDB error 1031 in
the unchanged authority publisher before the RustFS reopen test could start;
the preceding RustFS composition had passed. This is retained as a fixture
restart observation, not a RustFS block failure. Both owned Docker runs were
cleaned by their harnesses.
Physical cross-host and production operation still need separate evidence.
The `native-concurrent-sqlite` macOS CI job runs the local DELETE shared-mount
cases and both NFS backing rejection cases serially. WAL stays opt-in in this
fixture; its earlier diagnostic status reflected the SQLite 3.46.0 WAL-reset
issue. At that checkpoint the local-filesystem provider guard was macOS
specific. The later bound-backing checks below add Linux filesystem and
individual-file mount rejection; network-backed SQLite remains unsupported.

At the first concurrent-writer checkpoint, the mode kept detached file tombstones and staged blocks
and had no distributed open-handle pin or online collector. The block-row
growth measured above is a capacity limit even when acknowledged data is
correct. Every writable CLI must select the same block database or remote
bucket/prefix; its `MRC1` metadata marker did not bind a block-backing
identity. An explicit mismatch test shared metadata but gave writers different
SQLite block files: one writer published a file, the other received an error
on read, and the correct backing still read exact bytes. A second writer could
also acknowledge a new file into its different block store, leaving one
metadata namespace with chunks split across the two backings. A later missing
backing would make that acknowledged file inaccessible. Matching the block
backing in every writer's configuration was therefore a required volume
invariant. The `MRC2` work below binds a stable backing identity before
publication. Cross-host physical
mounts, power-loss recovery, and sustained
multi-endpoint RustFS visibility are separate qualification gates.

An independent review also found a direct generic API hazard: local SQLite
metadata with process-private memory blocks could previously prepare `MRC1`
and mount, then publish block IDs another writer cannot read. A new regression
was red on that behavior and green after `BlockStore` concurrent preparation
became fail closed by default. Shared providers now explicitly opt in; rejected
memory blocks leave the SQLite row at revision 0 with no namespace or mode
marker, and an exclusive legacy writer can still acquire its lease.

The object-block adapter's content-addressed collision check previously
returned an error for replaced remote bytes while its same-client cache still
served the old bytes. The regression was red before cache invalidation and
green afterward: a failed conditional put now removes the affected cache
entry, and a later get rechecks the remote digest. This protects an already
open client from treating known bad remote bytes as a healthy cached block.

The same review found that the generic public object-store adapter previously
accepted any injected `ObjectStore` for concurrent mode, including two
distinct process-local `InMemory` instances. A red regression demonstrated
this bypass. Its preflight now returns `ENOTSUP` regardless of a caller's
durability declaration. Named R2, RustFS, and AWS S3 config-built signed
clients retain a bounded create/read service probe under a reserved object
in the selected prefix. This object is reused by later mounts and excluded
from ordinary block reconciliation. An unavailable service, rejected write
credential, missing bucket, or mismatched probe bytes must reject startup
before a fresh metadata authority enters `MRC1`.
Remote policy grants must include the reserved
`<prefix>/_mount-rs-concurrent-probe-v1` key for create and read. A policy
limited to `<prefix>/b*` for content-addressed block names would let ordinary
block operations through but reject concurrent startup. The repository's AWS
policy example grants `<prefix>/*`; account-specific narrower policies need
the extra probe key. This is a startup availability boundary.
The configured R2 HTTP fixture passed two successful probes and rejected an
invalid signature, missing bucket, and stopped service. Its failure message
did not contain the test credential. Generic adapter tests passed 13/13,
R2 provider tests passed eight unit and three HTTP checks, and AWS S3's
offline/config tests passed 3/3. These do not establish live AWS S3 account
access.

The Node example CLI now uses the same concurrent SQLite backing placement
rule as the Rust CLI before opening its driver. Its regression covers backing
metadata or blocks under the mountpoint, missing path components, `..`,
symlink aliases in both directions, a dangling SQLite-file symlink into an
absent view, a safe outside dangling symlink, and an actual mount command
before SDK import. On macOS it materializes the unmounted view directory and
compares filesystem identity of backing ancestors before provider open. This
caught the tested APFS aliases `ſ`/`s` and `ᾀ`/`ἀι`, while allowing distinct
`cafe`/`café` and `α`/`β` siblings; rejected commands left no backing database
or `MRC1` marker. Node `--check` briefly creates any missing empty view
directories and removes only those it created. The Rust CLI likewise follows
dangling backing symlink targets before provider open and waits for real
volume identity before rejecting absent name aliases, so case-sensitive APFS
siblings remain available. A separately formatted case-sensitive APFS volume
was not used in this test pass. Omitted RustFS `durable` now defaults to false in
the Node CLI, matching the Rust CLI and N-API default. The direct N-API
`createChunkedDriver` path receives the mountpoint later in `mount()` and
cannot enforce this placement check before provider construction; its callers
must keep concurrent SQLite backing files outside their mountpoints.

FoundationDB block preflight also had a startup availability gap: creating a
client database handle does not contact its cluster. A RED native macOS
unavailable-loopback regression accepted its old no-op block preflight.
After a bounded read-only transaction on a key in the selected block keyspace,
the same test rejected startup in under one second (GREEN). This transaction
does not create a block or change metadata. Existing ChunkedFs ordering tests
cover block preflight before metadata mode conversion; the focused FDB gate
does not claim a live healthy cluster when its cluster environment is absent.

Both native RustFS combination runners had a cleanup gap when a supervised
process group leader exited before its child. Dry negative probes reproduced
the orphan and then passed after exact test-owned process/session discovery,
signaling, and absence checks were added. An unrelated session survived the
PGlite probe; the FDB probe preserved an attached marked temporary root when
an exact unmount failed. The runners leave an unresolved owned root in place
for diagnosis instead of deleting a still-mounted directory. These are
failure-path checks; the earlier successful native service measurements above
were not repeated solely for runner cleanup.
An outer-watchdog force-kill probe then found that the PGlite runner's
detached Cargo session escaped the combination supervisor. The probe was red
before Cargo joined the private supervisor group and green afterward; an
unrelated session and the Finder demo mount stayed untouched. The inner
runner now signals only its exact Cargo leader and preserves its marked root
if descendants cannot be proven stopped.
Another dry failure-path review substituted a symlink at each owned
`mount-a`/`mount-b` path to an unrelated mounted location. The original PGlite
runner reached mount lookup through that link, and the Rust native TestScope
accepted the changed path. The RustFS and PGlite Python runners now reject
non-directory or changed inode paths before any mount lookup or `umount`; the
Rust TestScope rejects them before its cleanup unmount. RED/GREEN probes for
both names preserved the marked root and left the foreign path untouched.
No live service run was repeated solely for this cleanup hardening.
The FoundationDB/RustFS combination watchdog allows a 45-second TERM grace
for the runner's exact unmount, service stop, and sparse-image detach. A
hard SIGKILL after that grace cannot run the inner Python `finally` block;
it may leave a marked test-owned APFS image attached. The watchdog reports
this as cleanup failure and the runner preserves any uncertain mounted root
for diagnosis. Normal success, injected setup failures, and scoped TERM
cleanup passed; hard-KILL image reaping was not qualified.

## First checkpoint workspace gates

After the path-guard and exact cleanup fixes,
`./scripts/cargo-shared fmt --all -- --check` and strict
`./scripts/cargo-shared clippy --locked --workspace --all-targets -- -D warnings`
passed. The complete local `cargo test --locked --workspace --all-targets`
run passed 798 tests with zero failures and 61 ignored tests across 123 test
binaries. The ignored native and service acceptance tests are separate gates
described above. Node addon build, typecheck, CLI/chunked suites, Python script
AST, changed shell syntax, CI workflow YAML parse, and `git diff --check` also
passed. The full Cargo suite ran with macOS test-owned socket permissions so
its 9P tests could bind Unix sockets.

## Bound backing protocol qualification

The coordinated protocol branch changes the concurrent metadata mode to `MRC2`. Metadata stores
persist a 16-byte backing ID and check it in the same transaction as each
revision CAS. Block stores persist their own stable authority and verify it
read-only at mount open and again after the block flush, before each metadata
CAS. An existing `MRC1` volume returns `EBUSY` with the offline migration command
before it creates a block marker. Migration directly reads every referenced
block, checks that each extent fits the returned bytes, and rechecks metadata
revision before changing the mode without changing namespace bytes.
Content-addressed object IDs with a full digest also check their bytes against
that digest. SQLite random IDs and legacy short object IDs cannot detect a
same-length replacement because the old metadata has no checksum to compare.

The inverse SQLite mismatch regression now rejects a second physical block
database against one bound metadata file with `ESTALE` before a write or marker
claim. Deleting the marker makes reopen fail without recreating it or advancing
metadata; changing it after open stops the next publication with `ESTALE` and
no metadata revision advance. Migration rejects missing and short blocks,
references selected from a different SQLite block file, retained unlinked-node
block damage, stale revisions, and version
history. A revision change during the direct block scan returns `EAGAIN` while
the volume stays `MRC1`.

SQLite block and metadata files use separate Unix physical dev/inode and
canonical pathname stamps to reject a copied backing with the same persisted
authority or volume ID. The
metadata stamp is seeded only when the provider exclusively creates a new
file; a preclaim copy of that file cannot enroll independently. Historical
unstamped Legacy SQLite metadata can continue in exclusive-writer mode but
cannot automatically enter `MRC2`. An already `MRC1` SQLite file cannot mount
with the upgraded binary: it cannot acquire a legacy writer, and its implicit
`MRC2` migration is refused. Explicit trusted recovery is available only for a
completely unstamped, fenced MRC1 SQLite metadata file. It requires the exact
revision and volume ID and an operator assertion that every writer is stopped
and the selected file is the sole authoritative metadata copy. The tool cannot
prove those conditions; a copied database retains its logical volume ID.
Recovery stamps the current device, inode and canonical pathname while
preserving the volume ID, revision and namespace. See the
[operator procedure](concurrent-backing-reenrollment.md). Partially stamped
MRC1 files and physical pathless MRC2 prototypes remain unsupported by trusted
recovery; an unstamped `MRC2` file refuses startup. On other
platforms, including Windows, concurrent SQLite backing returns `ENOTSUP`
because a copied file could carry the same marker while its contents diverge.
The GNU Linux guard uses `fstatfs` on an `O_PATH` inspection descriptor and allows ext-family,
XFS, Btrfs, F2FS and tmpfs; it rejects NFS, overlayfs, CIFS, 9P, FUSE and
unknown types before concurrent claim. tmpfs passes locality but does not
survive host reboot. The Linux native NFS acceptance fixture places both
provider database files on its owned NFS view and asserts `ENOTSUP` with no
metadata revision or block marker change. The final core PR-event head
`688ff38a7823e1615fcfb5ac3389e0200932c33d` passed the
[native-nfs Ubuntu CI job 107196627065](https://github.com/andymac4182/mount-rs/actions/runs/35865709669/job/107196627065).
That workflow used PR merge checkout `3552df110e08d934e96a10ef07366a894e576f7e`
into main `a47fafbe`. Its owned NFS fixture emitted
`SQLITE_LINUX_NFS_BACKING_REJECT_PASS metadata=ENOTSUP blocks=ENOTSUP markers=0 revision=0`.
Closing a regular descriptor for the same inode releases process POSIX locks,
including SQLite's locks. `O_PATH` avoids that close path. A new distinct-process
regression holds `BEGIN IMMEDIATE`, requires another process to receive
`SQLITE_BUSY` before and after inspection, and permits its write after rollback.
That reserved-lock regression passed on the same final PR-event head in
[Rust Ubuntu CI job 107196626837](https://github.com/andymac4182/mount-rs/actions/runs/35865709669/job/107196626837).
Its SQLite suite reported 63 passed, zero failed and three ignored; the outer
reserved-lock test explicitly invokes its distinct-process probe worker.
The privileged file-bind fixture runs in the native NFS job. macOS does not
execute the reserved-lock test.
The combined revised checkout passed SQLite 61 tests, CLI 65 library tests,
16 CLI integration tests, workspace formatting, and strict all-target Clippy
for both packages. The remote-only offline migration regression went red
because it created a view directory before provider connection failed; it now
skips SQLite placement preparation when neither backing is SQLite.

Concurrent SQLite requires exactly one hard link for each metadata and block
inode at claim, reopen and publication/authority verification. Different
hard-link names share dev/inode but select different SQLite WAL sidecars. A
disposable APFS WAL repro acknowledged an A write, then B hit
`SQLITE_IOERR_SHORT_READ` through the alias; DELETE, TRUNCATE and PERSIST
appeared usable in a bounded sequential probe but did not establish safety.
New provider regressions reject aliases before a claim and stop publication
if a link appears after open. Linux file-only bind mounts can also expose one
inode at different database paths with link count one and different WAL
sidecars. GNU Linux now also checks the selected descriptor's
`STATX_ATTR_MOUNT_ROOT` attribute, before authority insertion or publication.
An individually mounted database file returns `ENOTSUP`, even when an active
canonical WAL hides the authority row from the alias. Directory mounts remain
usable. The attribute's supported mask is required: concurrent SQLite needs
GNU Linux with kernel 5.8 or newer; other Linux targets fail closed for this
opt-in mode. The direct `statx` syscall adds no glibc wrapper version requirement.
The canonical pathname stamp rejects alternate auxiliary paths; symlinks
resolving to the same canonical path remain usable. The revised SQLite provider
suite passed 61/61 on macOS, including alternate path, symlink, hard-link and
portable mount-attribute regressions. The final core head `688ff38a` passed
the privileged Linux file-bind fixture in native-nfs Ubuntu CI job
`107196627065`, workflow `35865709669`. The checkpointed case closes both
stores before binding. The extended case keeps canonical WALs open and
verifies in a distinct process that an alias seeing an empty authority table
cannot install a second authority or change metadata; the canonical providers
still verify their original MRC2 authority. This qualifies the fixture's owned
local backing and file-only aliases, without qualifying arbitrary filesystems
or adversarial path swaps.
Use the same canonical database paths in every SQLite process.
Pathless development MRC2 prototypes fail closed before release; see the
[auxiliary path authority](sqlite-auxiliary-path-authority.md) for the format
and recovery limits.

The earlier offline migration path prepared a block authority before directly
reading referenced blocks and attempting the metadata transition. In that
version, a disposable historical unstamped SQLite MRC1 CLI fixture rejected
migration with metadata still `MRC1`, revision zero and no backing ID, but left
one authority row in the selected block database. That row alone did not
publish a namespace.

The recovery/preflight follow-up checks metadata eligibility, the loaded
namespace and expected revision, then directly reads every referenced block
before claiming block authority. Ordinary migration still refuses historical
unstamped SQLite MRC1 metadata. Failures found during these checks, including
missing or short blocks, wrong expectations, version state and invalid
existing metadata stamps, leave the block marker unclaimed. Fresh Legacy
enrollment also performs read-only metadata preflight
before claiming block authority. A peer that completes MRC2 enrollment during
that preflight is accepted only after verifying its existing block marker;
missing or wrong markers are never recreated.

Preflight is advisory, and each provider rechecks the transition conditions
during its final conditional update. A concurrent change or failure after a
successful block claim can still leave an unused marker because separate
metadata and block providers have no shared transaction. Keep all writers
stopped through migration or trusted recovery. Both offline CLI handlers
prepare missing SQLite view directories and recheck their filesystem identity
before provider open, including absent APFS case and normalization aliases.
They do not start native mounts; prepared directories can remain after failure.

The bounded local SQLite load completed 8 × 100 independent mount lifecycles
in both modes: DELETE took 48,047 ms and WAL took 41,993 ms. Each run
acknowledged 2,008 lifecycle operations, reopened exact bytes, and reported
`PRAGMA integrity_check=ok` on both backing files. Each retained 834 block
rows and a namespace of about 383,935 bytes. The smaller 4 × 12 DELETE/WAL
journal matrix passed too. These measurements used bundled SQLite 3.46.0,
with WAL diagnostic because of the WAL-reset issue described above.

On this branch, the disposable macOS NFS two-CLI SQLite case passed in DELETE
and WAL: both independently mounted CLIs saw the other's creates, merged
disjoint same-file ranges, renamed and unlinked files, and a fresh CLI reopened
the result. Its 2 × 12 loads took 778 and 634 ms, and both backing databases
passed integrity checks after clean unmount. One CLI serving two writable
views passed its 2 × 12 load in 1,068 ms. The two-view inner SQLite application
lock probe returned `blocked` in these runs; it does not qualify SQLite
application databases across independent NFS views. Concurrent SQLite
provider files placed on NFS still fail before a volume is created, including
the mixed local-metadata/NFS-block path. The first sandboxed native attempt
could not load `mount_nfs` and returned status 5; the same owned cases passed
with host mount permissions, with no test mount left behind.

A later full SQLite application matrix through two separate CLI mounts
reproduced the shared-view NFS limit three times. DELETE, TRUNCATE, and
PERSIST recovered neither the killed uncommitted writer's lock nor a clean
reopen (`database is locked`); one DELETE load worker failed at commit with
a disk I/O error, and WAL selected DELETE. This is the CLI's `soft,nolocks`
shared-view profile, not the separate `locallocks,hard` single-view SQLite
profile that passed its rollback modes and 100-row load. The mount-rs two-CLI
file lifecycle load, cross-view visibility, fresh reopen, and both local
backing databases' integrity checks passed around the failing application
packet. Three macOS `.nfs.*` deferred unlink names stayed visible for ten
seconds and were removed with the entire disposable backing after unmount.
The application packet is diagnostic and does not qualify SQLite files inside
two independent NFS mountpoints.

Real PGlite socket-server authority and MRC1 migration tests passed 2/2.
Its disposable macOS two-CLI NFS case passed 160 acknowledged calls in a
3,721 ms load, cross-view writes, and fresh reopen against one PGlite engine.
The owned real FoundationDB provider gate passed on both single-node and
durable three-node clusters: five authority, migration and contract tests plus
the terminal native network shutdown test each time. The durable readiness
probe received an ambiguous transaction acknowledgement and confirmed its
value by readback before accepting the cluster as ready. A separate native
macOS two-CLI test with FoundationDB serving both metadata and blocks passed
bidirectional visibility, concurrent writes and fresh reopen; its runner
verified exact NFS mount and sparse-image cleanup. These are one-host and
disposable-service results. Physical cross-host mounts, power-loss recovery,
online reclamation, distributed open-handle pins and sustained production
performance remain separate qualifications.

## Rebased integrity and service checkpoint

Ordinary FoundationDB block reads now verify the full SHA-256 content ID, as
the migration read already did. A direct same-length key overwrite is rejected
with `EIO` in the real durable three-node provider test (five live provider
tests passed, one ignored, followed by the native shutdown test). Ordinary
PGlite reads likewise verify their existing legacy MD5-derived content ID;
the isolated Node-backed same-length BYTEA tamper test failed before the
change and passed with `EIO` afterward. MD5 checks ordinary accidental
replacement, not adversarial collision resistance. SQLite's older random
block IDs do not provide a comparable read digest.

The first PR #13-rebased full RustFS fixture passed signed authority,
provider, SDK and VFS stages, then one native two-CLI load write returned macOS NFS code 60
(`ETIMEDOUT`) after 34,280 ms on the shared soft mount. Its exact stalled
backend request was not retained in that first run. A repeat with bounded
failure-only CLI and operation diagnostics passed 40/40 native acknowledgements
in 12,672 ms, and the complete signed provider, SDK, N-API, SQLite VFS,
service restart and reopen fixture ended with `RUSTFS_INTEGRATION_PASS`.
The exact post-guard head `ff58b97d` reproduced the timeout at writer A's
`open /load-a-07`, while writer B acknowledged all 20 operations. A completed
its filesystem create/open and a later lstat before progress stopped; the
existing trace did not identify the pending publication or NFS response.
The disposable mount, image, FoundationDB process and RustFS container were
removed after the failure. Request-phase tracing is being used to diagnose
this intermittent soft-NFS timeout; it is not a measured production
reliability rate. The combined traced head `042734b6` then passed the full
FoundationDB/RustFS fixture: 40/40 native acknowledgements took 27,200 ms,
followed by `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. The runner
verified removal of its exact mounts, images, processes, containers and temp
roots, and closure of all four owned service ports. This pass does not explain
the earlier failures. The extended traced fixture on test-only head
`e4489179` also passed 80/80 acknowledgements, both live verification passes,
fresh reopen, owned resource cleanup and the complete service fixture.
Its 94,537 ms native timer includes the live verification passes; it is not
a write-only throughput measurement. No pending spans were captured at the
end of either writer's load window.

| Longest traced phase | Writer A, ms | Writer B, ms |
| --- | ---: | ---: |
| Local filesystem gate wait | 19.599 | 22.249 |
| Metadata load | 83.640 | 18.401 |
| Backing authority verification | 16.216 | 21.187 |
| FoundationDB metadata CAS | 510.396 | 484.629 |
| CREATE ownership claim | 271.992 | 326.332 |
| NFS dispatch | 761.390 | 753.642 |

These maxima come from different requests and are not additive. They identify
costs in this passing window, not the cause of the earlier timeout. Tracing
can affect scheduling through stderr I/O; an untraced reliability qualification
and an explanation of the intermittent failure remain outstanding.

The same combined source passed locked workspace all-targets tests after
cleaning the local workspace package artifacts from its isolated target:
1,045 tests across 128 passing suites, zero failures, 75 ignored opt-in tests.
Workspace formatting and strict all-targets Clippy passed. A shared target
had previously supplied a CLI binary containing a diagnostic absent from
the selected checkout; those stale artifacts were removed before these gates.

The corrected native SQLite fixture also passed all four owned macOS cases
on that source: two CLIs in DELETE and WAL, one CLI with two writable views,
NFS metadata rejection and mixed local-metadata/NFS-block rejection. The
two-CLI 2 × 12 loads took 879 ms in DELETE and 710 ms in WAL; the one-CLI
two-view load took 1,205 ms. Both local backing files passed integrity checks
and fresh reopen returned exact bytes. The test runner removed its exact
temporary root and left no matching mount. The native concurrent SQLite CI
job passed on `2ccf9579` as well.

An earlier CI comparison of main `1a8dac33` and PR head `ff58b97d` used
400 iterations, concurrency 64, 4 KiB payloads and 64 KiB chunks against the
disposable Ozone object service. Every provider completed all 1,200 successful
write/read/delete operations with no timeout and owned cleanup. The metric
called "successful lifecycle IOPS" counts those three operations per
lifecycle divided by measured wall time; its configured target is 1,000.

| Metadata provider | Main IOPS | PR IOPS | Target result, main / PR |
| --- | ---: | ---: | --- |
| SQLite | 1,010.95 | 944.94 | pass / fail |
| PGlite | 993.62 | 677.65 | fail / fail |
| TiDB | 314.74 | 291.69 | fail / fail |
| FoundationDB | 505.07 | 493.33 | fail / fail |

These single CI samples do not establish a controlled performance comparison.
SQLite crossed the threshold in the PR sample, and a performance regression
has not been excluded. The three aggregate Ozone performance jobs failed on
both revisions. Their functional completion does not qualify the configured
IOPS target. The Windows Node structural open-flags fixture also failed on
both revisions with `EINVAL` when opening its writable host file; this is a
separate baseline failure, not evidence that the full Node test chain passed.

## Native create ownership and bounded RustFS follow-up

The guarded CREATE helper now omits internal uid/gid updates when the newly
created inode already has the requested AUTH_SYS owner. Its deterministic
EXCLUSIVE-create regression observed two metadata publications before the
change and one afterward. Wrong owners, parent setgid inheritance, unknown
credentials, stale inode identities and explicit client SETATTR remain
covered separately. This removes a redundant publication; it does not
establish the cause of the earlier per-OPEN NFS timeouts.

The debug, request-traced source `d6e242f1` ran 200 writes per independent
writer against disposable native FoundationDB and RustFS. Both writer loops
completed their 400 acknowledged writes, and their bounded phase summaries
were retained. The fixed 1,200-second harness deadline then expired before
`NATIVE_FDB_RUSTFS_LOAD_PASS`. No individual load operation reported
`ETIMEDOUT` in this attempt. The live checks and fresh reopen were not fully
qualified, and the complete service packet ended with `RUSTFS_COMBO_FAIL`.
The mount request deadline was unchanged.

The older output did not retain a completed verification-file count. The
planned live checks were 400 files through each view, followed by 400 files
after fresh reopen. Fresh reopen was not reached because it follows the
missing live-load PASS marker. The writer snapshots retained 20 longest-phase
entries each and six fresh GETATTR/lstat pending spans in total. These were
load-window snapshots, not timeout-time verification snapshots.

| Longest traced writer phase | Writer A, ms | Writer B, ms |
| --- | ---: | ---: |
| Local filesystem gate wait | 242.591 | 220.801 |
| Metadata load | 102.185 | 215.969 |
| Backing authority verification | 43.374 | 13.000 |
| FoundationDB metadata CAS | 481.139 | 514.557 |
| CREATE ownership claim | 142.312 | 147.143 |
| NFS dispatch | 1,223.824 | 1,164.755 |

The traced open/mutate records counted 202 and 195 CAS backoff events, with
largest zero-based attempt numbers of three and two. These are observed
metadata conflict retries, not NFS RPC retransmission counts. The maxima
come from different requests and are not additive; verification-stage
phase maxima and RPC retry counts were not retained.

One approved read-only sample of the exact owned writer-A CLI requested one
second of profiling. Its UTC start/end were 2026-09-23 13:29:47.551 and
13:29:50.451. The sampled guarded-read/lstat stacks showed
`refresh_concurrent_namespace`, FoundationDB metadata loading, JSON
deserialization and namespace validation. This matches the full namespace
reload path, but does not exclude a stalled or retried verification RPC in
the capped attempt. Both stderr request tracing and stack sampling can
affect scheduling. These debug diagnostic totals are not release-build
production throughput measurements.

The retained capped log is
`/private/tmp/mount-rs-native-stress400-full-traced-d6e242f1-20260923.log`
(52,548 bytes; SHA-256
`30e39f70af53c120209ce12e0a9c9a095e6c01f0bda774407fa25f21a488e25e`).
The full stack sample and UTC metadata are retained beside it as
`mount-rs-native-stress400-cli-a-sample-d6e242f1-20260923.txt` and `.json`.
The original failure evidence is unchanged.

After merging the recovery API source, exact debug source `300cd931` on
merged main `a3e05036` passed one untraced 20-write-per-writer packet.
`MOUNT_RS_TRACE_REQUESTS` was explicitly unset with `env -u`; only value `1`
enables tracing. CLI verbose diagnostics were disabled and no profiling was
performed. The full log contains no request-trace markers. CAS retry counts
and internal phase metrics are unavailable for this run; marker absence
does not establish zero contention.

| Completed stage | Count | Elapsed, ms |
| --- | ---: | ---: |
| Writes acknowledged | 40 | 17,288 |
| Live view A file checks | 40 / 40 | 4,499 |
| Live view B file checks | 40 / 40 | 4,294 |
| Fresh reopen file checks | 40 / 40 | 4,996 |

The combined writes-plus-live-checks timer was 26,082 ms, and the entire
native test took 37.92 seconds. The genuine signed provider, SDK, addon,
SQLite VFS, service fault/restart and reopen packet ended with
`RUSTFS_INTEGRATION_PASS`. The fixed runner/combo budgets were 600 and 900
seconds. This single untraced pass does not explain the earlier per-OPEN
timeouts or complete the capped 400-write qualification.

The immutable successful log is
`/private/tmp/mount-rs-final-native40-untraced-300cd931-20260923.log`
(14,198 bytes; SHA-256
`32b2282098358a4bf5291d1c828e9faa15730a672cfc3a79f9cb3cd6aada9709`).
Both attempts independently verified removal of their exact containers,
mounts, sparse images, processes and temporary roots, closure of their
observed endpoint ports, and preservation of the user's existing NFS mount.
Their exact transient dependency links were removed too.

Before the final packet, all 32 local workspace package artifacts were
cleaned in both host and explicit arm64 target layouts, retaining dependency
caches. Fresh CLI and addon builds used the selected source; the addon
loaded 259 exports. All nine full offline locked standalone dependency
graphs passed without lock repairs. The NFS suite passed 156 tests with two
ignored; feature-enabled CLI checks passed 100 with 19 ignored. Formatting
and strict feature-enabled CLI/NFS all-targets Clippy passed.

The test now records acknowledged writes separately from verification,
names each live/reopen view, prints each 25-file checkpoint and exact final
or error counts, and labels tracing availability. Panic cleanup retains
bounded pending traces with best-effort output when tracing is enabled.
An external harness deadline need not unwind Rust, so its last printed
checkpoint remains the retained verification progress boundary.

## Current 400-write attempts and native harness corrections

After the conditional metadata refresh and bundled SQLite 3.51.3 changes
merged, three further genuine RustFS plus native FoundationDB packets used
200 files per independent writer. All used debug CLI/addon artifacts,
`MOUNT_RS_TRACE_REQUESTS=0`, disabled CLI verbose output, no profiling, and
the same fixed 1,200-second native / 1,500-second combo budgets. These are
diagnostic qualification attempts, not release-build throughput samples.

| Source | Native duration | Earlier complete write-plus-sync loops | Failure |
| --- | ---: | ---: | --- |
| `d983be89` | 101.81 s | 294 | Writer A open 148 and writer B open 146 returned `EIO`; native FoundationDB reported fatal `OutOfMemory`. |
| `0d683812` | 0.00 s | 0 | Setup could not copy the feature-enabled CLI from a deleted target alias (`ENOENT`), before any native mount or load. |
| `4e4dce76` on merged main `b22637f2` | 103.73 s | 354 | Writer A `sync_all` 180 returned `EIO`; writer B `sync_all` 174 returned `ESTALE`. |

The completed-loop counts are inferred from the zero-based failure indices;
each earlier loop returned successfully from both `write_all` and `sync_all`.
The last two writes in the third attempt returned from `write_all` but failed
at `sync_all`, so they are not counted as acknowledged. None of these attempts
printed the full 400-write acknowledgment marker, started live-view checks,
or reached fresh reopen. Each full service packet failed. No runner or combo
deadline expired. Request-trace marker counts were zero; CAS retry counts,
RPC retransmissions and internal phase timings are unavailable.

The first packet used the helper's former 512 MiB FoundationDB process cap.
Its retained server output reported `OutOfMemory` at 15:10:59 UTC, but did
not retain RSS metrics or database size. FoundationDB's fatal handler can
report this event for an exceeded resident-memory cap or an allocation
failure, so the exact trigger is unconfirmed. The post-cleanup host snapshot
showed 48 GiB physical RAM and 62% free memory; it does not measure host
memory at the failure time.

The native helper now restores FoundationDB 7.4.7's documented 8 GiB process
default and accepts an ASCII decimal test-only override from 256 through
16,384 MiB. It validates the override before allocating resources and prints
the selected process, storage-memory and cache parameters. The retained
storage-memory option is relevant to the memory engine, not this SSD engine.
No product deadline changed. The helper also retains bounded existing
`ProgramStart`, peak/latest sampled `ProcessMetrics.ResidentMemory` and
latest allocator `MemoryMetrics` before cleanup; best-effort diagnostic
output cannot bypass cleanup on a broken pipe or `KeyboardInterrupt`.
These parameter meanings are described in the [pinned FoundationDB
configuration source](https://github.com/apple/foundationdb/blob/7.4.7/documentation/sphinx/source/configuration.rst).

The second failure had a reproducible harness cause: Cargo reused a native
test executable containing `CARGO_BIN_EXE_mount-rs` from the previous run's
deleted target alias. The fixture now resolves its CLI from the running
test's current Cargo `deps` / profile cohort, rejects an invalid layout,
and pins that feature-enabled binary for the entire mount/reopen sequence.
The old cached executable failed after the alias moved; the corrected
cached executable passed the same relocation check without rebuilding it
for the second alias. Six pure fixture tests, strict feature-enabled CLI
Clippy, formatting and the Python cleanup/parameter/evidence probes passed.
The helper now prints its native root, service root and container identity
before entering Cargo, retaining those identities even on immediate failure.

The third packet confirmed an 8 GiB `ProgramStart.MemoryLimit`
(`8,589,934,592` bytes). Its greatest retained periodic RSS sample was
`712,671,232` bytes at 15:35:05 UTC. `ProcessMetrics.Memory` was virtual
memory, not RSS, and the allocator pools are not a resident-memory total.
No fatal OOM event appeared in the retained server output. The bounded
trace scan read 2,003,387 bytes without truncation or parsing errors. These
periodic samples do not establish a continuous peak or identify the new
`sync_all` error's originating provider/RPC stage. Both owned CLIs remained
alive when the writers failed.

Before these packets, all 32 local workspace packages were cleaned in the
exclusive target's host and explicit arm64 layouts, retaining dependency
caches. All nine full offline locked standalone dependency graphs passed.
The relevant feature-enabled suites passed 601 tests across 68 executables,
with zero failures and 45 ignored opt-in tests; formatting and strict
all-targets Clippy passed. Fresh builds selected the combined production
runtime source `d983be89`; subsequent changes were confined to documentation
and harness/fixture code. The current copied native feature CLI was
54,745,336 bytes with SHA-256
`0af3419bc55a995851000546a7cfc3bf647da9ab90767cd00a5d076c73684552`.
The fresh default-feature arm64 addon was 94,782,888 bytes with SHA-256
`fb015abd4de60e325ab442890906c5d78f83d896198107c6b55102f6fdb73c0c`;
it loaded 254 exports and the required `Filesystem` and
`createChunkedDriver` APIs. The export count is this artifact's observation.

The original logs are immutable and retained separately:

| Attempt | Raw log under `/private/tmp/` | Bytes | SHA-256 |
| --- | --- | ---: | --- |
| First | `mount-rs-native-untraced400-d983be8999a569ab1905fb082404ffde3045b910-20260923.log` | 13,325 | `e20db2992f0e44d3ea25e1c46c8e044b5e043398f14818bcee3cb40f8f172806` |
| Second | `mount-rs-native-untraced400-0d683812e6002202a56b8cd4675cac035c1aea3c-20260923.log` | 7,830 | `a31480f375b50997587cc2529aec3ff5f7bfb8e5d1fc966e394062742d0f6bef` |
| Third | `mount-rs-native-untraced400-4e4dce762092f13e1e0ed01709f2891e5a0d52e6-20260923.log` | 9,625 | `a4d455e70571c9a369602f9dc5c8db9e69e80a08aaccc1b0b47f11a359f79dcf` |

Structured result, artifact and cleanup proofs are retained beside the logs
with prefixes `mount-rs-native400-d983`, `mount-rs-native400-0d683` and
`mount-rs-native400-4e4dc`. Exact owned cleanup was independently checked:
containers, native roots, mounts, images, processes and observed ports were
absent, and the two exact borrowed dependency links were removed. The first
and third service roots were identified and verified absent. The second
immediate failure did not capture its exact service-root identity before
exit; its owned container was recovered from timestamped Docker events and
verified absent, and no matching temporary service root remained. This is
an identity gap, not an independently verified exact service-root claim.
The existing user CLI PID 66542, NFS mount and listener 55309 were preserved
through every attempt.

Before the third packet, one existing macOS S3 network fixture passed in
4.793 seconds with Node 24.18 and the same fresh addon. It kept the existing
32-request fanout and 15-second request timeout, covered NodeFs and SQLite
GET bytes/streamed/stats, and verified its own root/process-group/observed
port cleanup. Its raw log is
`/private/tmp/mount-rs-current-macos-s3-network-20260924.log`, with a sibling
JSON proof. This single macOS pass does not explain the retained Windows CI
15-second S3 timeout.

These three attempts did not complete the 400-write native qualification.
The 8 GiB helper correction and cached-artifact correction address observed
harness defects; neither explains the third attempt's sync errors or the original
per-OPEN timeouts. The earlier traced 400-write acknowledgment followed by
a verification deadline, and the complete untraced 40-write pass, remain
separate evidence boundaries.

## Completed 400-write packet with bounded NFS failure diagnostics

Merged source `44aa06f4f3bed86c46bab68799de24413614235a` preserves backend
path-resolution errors in NFSv3 WRITE and COMMIT. The prior `.ok()` paths
could turn an original `EIO`, `EFBIG` or `EACCES` into `ESTALE`. Two shared
wire regressions were red before that correction and green afterward,
checking unchanged file contents on resolution failure and recovery through
the same valid handle. Genuinely stale handles retain their stale status.
This is a proven error-mapping defect; it does not identify what caused the
earlier native sync errors.

That source passed one complete genuine native FoundationDB 7.4.7 plus
RustFS packet on macOS. Both independent writers used 200 files, with the
same 8 GiB server cap, 1 GiB APFS data image, five-second FoundationDB
transaction setting and 1,200 / 1,500-second native/combo budgets. No capacity
or deadline was increased for this attempt. Request tracing and CLI verbose
output were explicitly off, no profiling was performed, and
`MOUNT_RS_TRACE_FAILURES=1` enabled only bounded exceptional records.

| Completed stage | Count | Elapsed, ms |
| --- | ---: | ---: |
| Writes acknowledged | 400 | 132,143 |
| Live view A file checks | 400 / 400 | 11,563 |
| Live view B file checks | 400 / 400 | 11,878 |
| Fresh CLI reopen file checks | 400 / 400 | 13,623 |

The writes-plus-live-checks timer was 155,585 ms; the entire native test took
173.59 seconds. The complete signed provider, SDK, addon, SQLite VFS and
service fault/restart/reopen packet ended with both `RUSTFS_COMBO_PASS` and
`RUSTFS_INTEGRATION_PASS`. Checkpoints retained every 25 verified files.
These debug diagnostic times are not production-release throughput.

The successful fixture buffers owned CLI stderr and does not print those
buffers on normal completion. The raw log contains zero request/failure
trace markers, but exceptional-record availability is therefore unknown;
zero raw failure markers do not establish zero internal failures. CAS retry
counts, RPC retransmission counts and internal phase metrics are unavailable.
The approved failure-only disk watcher did not trigger, so no `statvfs` or
disk-capacity measurement was retained. The unchanged 1 GiB image passed
this packet; that result does not establish or dismiss capacity pressure in
the original failed attempts, and does not justify a capacity change.

Existing FoundationDB trace evidence retained the 8,589,934,592-byte process
cap, a greatest periodic RSS sample of 679,329,792 bytes and a final sampled
RSS of 240,418,816 bytes. The bounded scan read 3,161,172 bytes without
truncation or parsing errors. These sampled resident values are distinct
from virtual memory and allocator-pool totals, and do not establish a
continuous peak.

Before the packet, the isolated checkout's complete tree was confirmed
identical to reviewed/gated source `7bf72807`. Core/NFS all-target tests,
four bounded-diagnostic checks, six pure native-fixture checks, strict
feature-enabled core/NFS/CLI Clippy and formatting passed; a disabled-env
check produced no failure marker. The affected core/NFS/CLI/NAPI packages
were cleaned in both host and arm64 target layouts. All nine full offline
locked standalone dependency graphs passed again. Fresh feature CLI and
default addon builds took 5.15 and 16.09 seconds, and the native test cohort
build took 3.30 seconds; its six pure checks passed with two native cases
ignored. The selected addon loaded 254 exports and the required APIs under
Node 24.18.0.

The fixture's actual copied CLI was 54,624,360 bytes with SHA-256
`8c46753a676741e4b3782e9b9ae237cb5226cadd4611f1031a979116cb018db1`.
The addon was 94,809,624 bytes with SHA-256
`6285d489ab77f62c2bbaa07d5a27a3fd53920e7f6f5672e3f837dd8ee5003ee8`.
The immutable raw log is
`/private/tmp/mount-rs-native-failures400-44aa06f4f3bed86c46bab68799de24413614235a-20260923.log`
(19,589 bytes, 298 lines; SHA-256
`f1c0462ea18852ff3487a9654841ce492cc6079c0c1687c3473316f5fb1926b5`).
Result, artifact, ownership, memory and independent cleanup JSON proofs use
the prefix `/private/tmp/mount-rs-native-failures400-44aa06f4-`.

Independent cleanup confirmed the exact native and service roots,
containers, mounts, images and matching processes absent. Observed ports
54318, 54319, 54485, 54631 and 54648 were closed, including the restarted
RustFS endpoint. The exact PGlite helper command and owned diagnostic watcher
were absent. Both borrowed dependency links were verified and removed,
preserving their original directories. The user's existing PID 66542,
NFS mount and listener 55309 remained unchanged.

Separately, the unchanged PR #20 Windows Node isolated rerun
`107254968301` completed successfully, including its 32-request S3 network
fixture and the concurrent-SQLite `ENOTSUP` assertion. Its immutable log is
`/private/tmp/mount-rs-pr20-windows-node-isolated-rerun-107254968301.log`
(77,008 bytes; SHA-256
`7fd9ecd0a1ee167fa535dff24ac9e3959de58e4dcb3982ccca4ebd756df96eac`).
The original 15-second Windows timeout remains recorded; the rerun does
not establish its cause. PR #23 fault workflow `35884548013` passed on
Ubuntu, macOS and Windows. This fault-workflow result is separate from the
primary Rust/native CI status and the retained IOPS-floor failures.

This completes one bounded current-source 400-write native packet with
live and fresh-reopen exact bytes. It does not turn the earlier OOM,
`EIO`/`ESTALE`, per-OPEN timeouts or capped verification into passing runs,
and it does not establish an RCA or sustained reliability guarantee.
