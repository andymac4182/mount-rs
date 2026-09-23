# Concurrent provider qualification, 2026-09-23

This record separates provider unit checks, independent coordinator load,
native NFS behavior, and cross-host claims. The feature uses opt-in
`splitstore` + `concurrent_writes`; legacy snapshot filesystems have no shared
writer claim. The branch began at fetched `origin/main`
`8a629287104fcdd6d4c334378eccfcab005817ec`.

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
4/4. Windows verification of this retry head is still required. The test
also reports the provider stage on any future CI failure.

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
but a successful stress run cannot rule out a rare timing bug. The currently
bundled SQLite version is 3.46.0; its WAL mode cannot be
qualified as safe for multi-process checkpoint contention until upgraded to
a version with the WAL-reset fix. The default provider uses rollback/DELETE
journaling and FULL synchronization. See [SQLite's WAL note](https://www.sqlite.org/wal.html#the_wal_reset_bug).

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
The native positive gate now runs DELETE by default; WAL requires
`MOUNT_RS_CLI_NATIVE_SQLITE_WAL=1` as a diagnostic because of the bundled
SQLite 3.46.0 WAL-reset issue.
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
cases and both NFS backing rejection cases serially. WAL stays an opt-in
diagnostic because the bundled SQLite release has the documented WAL reset
issue. The current local-filesystem provider guard is macOS specific; Linux
network-backed SQLite files have not been qualified for concurrent mode.

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
`MRC2` migration is refused. Trusted offline re-enrollment has not been
implemented. An unstamped `MRC2` file refuses startup. On other
platforms, including Windows, concurrent SQLite backing returns `ENOTSUP`
because a copied file could carry the same marker while its contents diverge.
The Linux guard uses `fstatfs` on the selected file and allows ext-family,
XFS, Btrfs, F2FS and tmpfs; it rejects NFS, overlayfs, CIFS, 9P, FUSE and
unknown types before concurrent claim. tmpfs passes locality but does not
survive host reboot. The Linux native NFS acceptance fixture places both
provider database files on its owned NFS view and asserts `ENOTSUP` with no
metadata revision or block marker change. The Linux native NFS CI job passed
that owned fixture on head `2ccf9579` with both markers absent and revision zero.

Concurrent SQLite requires exactly one hard link for each metadata and block
inode at claim, reopen and publication/authority verification. Different
hard-link names share dev/inode but select different SQLite WAL sidecars. A
disposable APFS WAL repro acknowledged an A write, then B hit
`SQLITE_IOERR_SHORT_READ` through the alias; DELETE, TRUNCATE and PERSIST
appeared usable in a bounded sequential probe but did not establish safety.
New provider regressions reject aliases before a claim and stop publication
if a link appears after open. Linux file-only bind mounts can also expose one
inode at different database paths with link count one and different WAL
sidecars. The canonical pathname stamp now rejects those aliases; symlinks
resolving to the same canonical path remain usable. The SQLite provider suite
passed 60/60, including alternate path marker and symlink regressions. The
owned Linux file bind regression passed in the same native NFS CI job on
`2ccf9579`. That bounded fixture closes both stores before binding; it does
not qualify a file-only alias opened while a canonical WAL remains active.
Use the same canonical database paths in every SQLite process.
Pathless development MRC2 prototypes fail closed before release; see the
[auxiliary path authority](sqlite-auxiliary-path-authority.md) for the format
and recovery limits.

The offline migration path currently prepares a block authority before it
directly reads referenced blocks and attempts the metadata transition. A
disposable historical unstamped SQLite MRC1 CLI fixture rejected migration
with metadata still `MRC1`, revision zero and no backing ID, but left one
authority row in the selected block database. That row alone does not publish
a namespace. Read-only transition and extent preflight before authority claim
is tracked as the immediate follow-up; until then a rejected historical
migration can leave this unused marker.

The bounded local SQLite load completed 8 × 100 independent mount lifecycles
in both modes: DELETE took 48,047 ms and WAL took 41,993 ms. Each run
acknowledged 2,008 lifecycle operations, reopened exact bytes, and reported
`PRAGMA integrity_check=ok` on both backing files. Each retained 834 block
rows and a namespace of about 383,935 bytes. The smaller 4 × 12 DELETE/WAL
journal matrix passed too. WAL remains a diagnostic because the bundled
SQLite 3.46 release has the WAL-reset issue described above.

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
the earlier failures. An extended 80-write traced load is the next transport
diagnostic.

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
