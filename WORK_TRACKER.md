# Workstream and task tracker

Updated: 2026-09-21. Baseline: local commit `21803fd` plus the sequentially
published `main` updates listed below. Overall status: **in progress;
not release-ready**.

This is the delivery dashboard. [Requirements](REQUIREMENTS.md) define scope;
[porting evidence](PORTING_STATUS.md) and the [API parity ledger](docs/public-api-parity.md)
retain detailed results. A passing component test is not end-to-end acceptance.

Current local acceptance: on 2026-09-20, `scripts/test-all.sh` exited 0 at
`73c33e0` with the pinned mountx checkout and live, bucket-scoped Cloudflare R2
credentials held outside the repository. The run passed the complete Rust and
Node suites, memfs/SQLite/PGlite parity, live PGlite lifecycle and split-store
gates, authenticated R2 contract and five-seed R2 differential traces, the
configuration-driven R2 CLI (including both metadata-provider compositions),
HTTP/remote reopen, and R2 cleanup. Native privileged mounts, FSKit signing and
hosted Windows/macOS CI remain separate evidence boundaries.

Latest hosted evidence: [CI run 35499717435](https://github.com/andymac4182/mount-rs/actions/runs/35499717435)
at `37e9ba1` passed RustFS, Ozone, Linux Rust, Linux x64/arm64 Node,
Windows Node, and Linux native FUSE/NFS/9P/WebDAV jobs. Windows Rust failed
the HostFs lexical-root and directory-open tests; macOS jobs remain queued.
This evidence predates `3f52b44` and does not qualify that newer revision.
Its separate fault-injection workflow also passed (run `35499717417`).

Windows HostFs follow-up: directory handles now use backup semantics, raw
Win32 error mappings are separate from platform libc errno values, and root
tests cover drive/UNC paths. Main passed 14 macOS tests and strict Clippy;
post-fix Windows runtime CI is still required. Directory behavior was checked
against [libuv's Windows implementation](https://github.com/libuv/libuv/blob/v1.x/src/win/fs.c).
Run `35500474806` reached a different failure first: SQLite fencing setup
exhausted a 75 ms lease before publication. The test now uses long-lived setup
leases and explicitly expires the persisted lease before takeover, retaining
busy-owner, stale-writer, and winner-persistence assertions. Five local chunked
concurrency tests passed; Windows runtime confirmation remains required.
At `83bca86`, Windows job `106052286866` passed those fencing and HostFs unit
tests, then failed five HostFs integration tests: unsupported symlinks,
read-only create flags, and hard-link metadata. Lagrange is implementing these
without skipping the tests. Windows Node job `106052286914` passed.

Main ran `cargo test --workspace --all-targets --all-features --locked --offline`
against an isolated committed `cf3c485` snapshot on macOS: exit 0. Explicitly
ignored remote/native gates are not acceptance evidence from that run.
Main also reran the full oracle-enabled Node package suite after `753df86`:
exit 0, including harness/structural drivers, server protocols, 44 typed 9P
cases, memory parity, distribution and artifact aggregation. PGlite/R2 factory
and native-mount opt-ins were skipped in this run and remain separate gates.

Current-cycle evidence on 2026-09-21 adds a mount-free W01 concurrency packet,
an isolated provider/consumer matrix, and an eight-cell SQLite reliability
matrix. The dedicated PGlite lifecycle gate passed the Rust SDK rows for memfs,
memory/memory, SQLite/SQLite and PGlite/PGlite; the Node SDK rows for memfs,
SQLite, chunked-memory, chunked-SQLite and chunked-PGlite; and the five CLI
config/runtime rows. R2 rows remained explicit skips because this shell did not
have the credential variables. The post-fix full gate also passed the Rust and
Node suites, five-seed traces across memory/SQLite/object-store/chunked/PGlite
backends, and the N-API distribution checks. The full root gate exited 0 with
1,194 upstream tests passed and 40/40 seeded trace lanes passed; native mounts,
hosted Windows/Linux runs and live R2 remain separate acceptance gates.

Fresh current-tree `scripts/test-pglite.sh` acceptance also exited 0: real
PGlite provider parity/reconnect/fencing/cancellation, disk-server restart,
split metadata/blocks, N-API, chunked, userspace FUSE, Rust/Node/CLI provider
matrices, the 1,194-test upstream suite, and all 40 PGlite-inclusive seeded
trace lanes passed. R2 rows were explicit skips because this shell still lacks
the required scoped credentials.

The exact published tree at `747b160` was rerun on 2026-09-21 with the same
gate and exited 0: PGlite lifecycle/reconnect/fencing/cancellation, SQLite VFS
round-trip and fresh-provider reconnect, disk-server restart, split stores,
N-API, chunked, userspace FUSE, Rust SDK 6/6, Node SDK 5/5, CLI 11/11, the
upstream suite 1,200 passed/82 skipped, and all 40 seeded oracle lanes passed.
R2 factory/runtime rows remained explicit credential-gated skips; this is not
live R2 acceptance.

The latest published tree `f07579e` was rerun with the oracle-enabled N-API
package suite on 2026-09-21 and exited 0. Harness, factory, lifecycle, server,
JS-driver, FUSE/NFS/9P codecs, Unstorage, chunked, differential, restart and
distribution/artifact packets all passed; PGlite/R2 factories and native-mount
opt-ins remained explicit skips. The authorized macOS native NFS N-API gate
then passed real mounted write/read/unmount, and the Node CLI native gate passed
with independent Rust and Node clients writing to the same mount, Rust reading
Node bytes, clean unmount, and both payloads retained in the backing root.

The same tree also passed `./scripts/cargo-shared test --locked --workspace
--all-targets --all-features --offline` on macOS. Core, every registered
integration/transport crate, the SDK/CLI/HTTP/N-API paths, SQLite/VFS,
chunked, versioned and observability tests completed with exit 0; live R2,
PGlite, TiDB, RustFS and privileged native tests remained explicit prerequisite
skips rather than being counted as service acceptance.

The follow-up PGlite CLI consumer check in `dcc3aa4` also exited 0: the Rust
CLI now performs the same configured PGlite split-store write, shutdown,
reopen and readback that the Node CLI already performed. The live CLI matrix
is now 10 passes and one explicit R2 skip; no config-validation row is being
counted as live R2 evidence.

The bounded RustFS run on the current tree also exited 0. It passed real
immutable block writes/reopens, SQLite and PGlite metadata compositions, N-API
factories, remote CLI HTTP graceful-reopen, the SQLite VFS over RustFS blocks,
fault recovery, service restart/reopen, and the RustFS block benchmark. This
is strong S3-compatible evidence, but it is intentionally not counted as live
Cloudflare R2 evidence.

Latest SDK-consumer slice at local `21803fd` was published to `main` as the
sequential API commits `9868e93`, `83153ef`, `b8a49a0`, `9163a39`, `b09965c`,
`cc605c7` and `b43a4e9`. The Rust provider matrix now opens PGlite/R2 through
the public Rust SDK facade and verifies clean shutdown/reopen; the Node matrix
does the same through `createChunkedDriver`; and the Node CLI accepts the same
versioned provider config shape as the Rust CLI. The local PGlite run passed
Rust SDK 4/4, Node SDK 5/5 and CLI 7/7 gated cases, while R2 remained an
explicit credential skip. The CLI also rejects `--reopen` for process-local
memory rather than reporting a false durability result. Full offline workspace
tests and the N-API package gate passed after this slice; hosted CI is still
queued and does not count as green evidence.

The follow-up structural-driver slice at local `fd5eb04` was published as
`88f8f9b` and `f7e630e`. It forwards normalized numeric `open_flags` with the
Linux/macOS/Windows Node flag namespaces, and the rebuilt addon passed the
opt-in macOS NFS structural mount lifecycle with mounted write/readback,
callback reachability, unmount and cleanup. Linux, non-NFS transports, hosted
Windows execution and full FUSE/FSKit acceptance remain separate gates.
The full locked offline workspace gate also exited 0 after this patch.

Focused current-head acceptance after the follow-up packets: `mount-rs-sdk`
unit tests passed (2/2), the Rust provider matrix passed memfs, memory/memory,
SQLite/SQLite and PGlite/PGlite (4/4, with R2 explicit skips), the Node SDK CLI
direct driver self-test passed, the complete N-API suite passed, chunked tests
passed (12/12 plus 7/7 concurrency), SQLite VFS tests passed (4/4 unit, 16/16
engine and 16/16 bridge), and local HostFs tests passed (14/14 including the
Windows oracle cases where runnable). These are focused local gates at
`3042d09`; the current shell still lacks live R2 credentials and hosted
Windows/macOS and privileged native-mount runs remain unqualified.

The current W01 mount-free FUSE packet adds typed napi-rs `GETLK`, `SETLK` and
`SETLKW` request codecs plus the typed `GETLK` reply codec. The generated root
bindings, explicit CommonJS/ESM barrel exports and `./fuse` declarations are
covered by a typecheck and a pinned-oracle differential covering the 48-byte
request, 24-byte typed reply, truncation, trailing-byte and empty status-reply
boundaries. The release N-API build, oracle-enabled package suite and focused
locked `mount-rs-fuse` tests passed. This is mount-free wire evidence only;
native FUSE lock/session semantics remain an explicit boundary.

Parallel W01 sidecars completed on 2026-09-21 and were published to `main`:

- `ccaf8f5` + `4cdeb58` correct the Windows SQLite WAL shared-memory access mask
  and add a Windows-only mapping regression test. The focused SQLite VFS gate
  passed 5 unit, 16 engine and 16 storage-bridge tests, but hosted Windows
  rerun is still required.
- `7236e0a` + `4422cd0` add oracle-backed Unstorage capability, unsupported
  operation, metadata and ownership-overlay parity. Direct and N-API tests
  pass; the broader W01 ledger remains open.
- `57632c6` + `e213856` add structural N-API adapter checks and an opt-in native
  lifecycle test. The authorized macOS NFS run passed kernel read, callback
  reachability, unmount and cleanup; native structural writes remain an explicit
  `EIO`/unsupported boundary until decoded `open_flags` are exposed.
- `1af1986` + `8ac77f3` + `9215ba3` + `a1ec0c5` retain NFSv3/v4 backend handles
  across unlink/rename and classify the pinned TypeScript control separately.
  Focused Rust tests passed 30 unit, 8 integration and 266 oracle cases with
  18 capability-gated skips; privileged Linux/macOS NFS qualification remains
  separate.

These packets reduce the W01 queue but do not close W01.1-W01.4: the complete
parity ledger, all classified skips, cross-backend seeded evidence, live R2,
hosted Windows/macOS and privileged native transport gates remain required.

The FUSE session follow-up `1cf0350` corrects the support matrix: plain
`RENAME2` is dispatched as a normal rename, while exchange/whiteout flag
variants return `ENOSYS` without mutation. The README now states that boundary
and the focused session test covers all three nonzero flag variants. The
16-test session suite, strict Clippy, formatting and diff checks passed.

Final integrated local gate after `192749e`: `cargo test --workspace
--all-targets --all-features --locked --offline` exited 0, and the
oracle-enabled `pnpm test` in `integrations/mount-rs-napi` exited 0 with the
new Unstorage parity included. R2/PGlite factory and privileged native-mount
rows remain explicit skips without credentials or opt-in host prerequisites.

Latest W01 rotation completed with three disjoint Luna Max packets. Local
commits `31d62df`, `3355cc1` and `323231f` were each validated and published
to `origin/main` through the GitHub contents API. The Linux packet wires an
explicit `/dev/fuse`/`fuse3` structural-driver mount/read/write/unmount job;
the hosted Linux result is still required. The Unstorage packet adds oracle
parity for atime, mtime, ctime and birthtime (11/11 Rust tests plus direct and
N-API parity). The FUSE packet exposes and differentially tests the `READDIR`
body codec, including UTF-8 names, alignment, bounded packing and malformed
input. The full locked Rust workspace gate, oracle-enabled N-API gate and the
real macOS Rust+Node CLI NFS demo all passed after the rotation.

The next SDK-consumer packet is now implemented and locally verified. The
Rust CLI exposes `sdk-self-test`, a mount-free process-level example that
constructs memory or configured durable filesystems through
`mount-rs-sdk::Filesystem`, writes/readbacks through the shared driver, and
supports a shutdown/reopen assertion. The Node CLI has the matching SQLite
split-store shutdown/reopen integration row. The provider matrix runs both
real CLI processes: offline results are 8 CLI passes and 2 explicit
PGlite/R2 skips; the Rust SDK matrix is 4 passes and 4 explicit external-gate
skips. The oracle-enabled N-API suite passed, including the public
`READDIRPLUS` body differential and artifact aggregation. This is a focused
consumer-path checkpoint, not closure of W01 or live-provider/native-mount
acceptance.

The latest parallel acceptance rotation added three bounded packets and
published them to `origin/main`. `3b59dc8` exposes typed FUSE `GETATTR` and
`SETATTR` request/reply codecs with pinned 7.41/7.8 differential coverage;
`7da18fb` verifies exact prefix cleanup and sibling/parent sentinel
preservation in the real FoundationDB metadata + RustFS chunk harness; and
`d6c8a29` hardens the real TiDB + RustFS composition fixture with explicit
durable scope, block absence checks, metadata-row cleanup and symlink-path
rejection. The combined locked offline Rust workspace gate and oracle-enabled
N-API suite both exited 0. The FoundationDB and TiDB mixed-provider harnesses
passed real service runs; TiDB used the available single-node v8.5.7 topology,
so replicated/durable capacity and provider restart promotion remain open
acceptance boundaries rather than being implied by these passes.

The following W01 rotation then completed three more disjoint Luna Max packets
and published them to `main`: `a31880d` adds seeded positional write,
truncate, flush and reopen coverage across the Rust SDK, Node SDK, Rust CLI and
Node CLI provider matrix; `cc73ad5` adds 11 Unstorage path/metadata/handle
oracle rows with zero mismatches or skips; and `a3795f0` exposes typed FUSE
`OPEN`/`OPENDIR` request codecs across protocol 7.8, 7.39 and 7.41. The
focused provider matrix passed 5 Rust SDK, 4 Node SDK and 9 CLI cases with
explicit PGlite/R2 skips; the Unstorage packet passed its direct, capability,
edge, N-API and upstream conformance gates; and the full locked Rust workspace
plus oracle-enabled N-API suite passed. These packets further reduce W01 but
do not claim native FUSE session/mount or live-provider acceptance.

The latest W01 rotation completed three additional disjoint Luna Max packets
and published them to `main` on 2026-09-21:

- `80f2efd` (published as `e5902bed`) fixes Windows HostFs symlink creation to
  use normal Win32 paths with a long-path fallback. The focused HostFs suite
  passed 15 tests, the Windows target check and strict target Clippy passed;
  hosted Windows runtime confirmation, read-only create flags and hard-link
  metadata remain open.
- `16b2180` (published as `9832b60`) adds typed Rust FUSE `ACCESS` session
  dispatch with exact 8-byte request validation, root handling, owner/group/
  other permission checks, invalid-mask errors and focused session tests.
- `7dd9a60` (published as `83466d7f0be058390f5c951a58d04d3a67f4923e`)
  exposes Rust-backed FUSE `LOOKUP` request/reply codecs through the N-API
  package, including declarations, generated artifacts, protocol 7.8/7.39/
  7.41 differentials and malformed/truncated/trailing-input checks.

The combined local gate for this rotation passed `cargo fmt --all -- --check`,
the locked offline all-feature Rust workspace suite, and the oracle-enabled
N-API suite. The N-API run passed the FUSE `READDIR`, `READDIRPLUS`, `READ`,
`WRITE`, `GETATTR`, `SETATTR`, `OPEN`/`OPENDIR`, `CREATE` and `LOOKUP`
families, 9P, NFS, WebDAV, Unstorage, chunked storage and distribution checks;
PGlite/R2 factories and native mounts remained explicit prerequisite skips.
The newest CI and fault-injection runs for `83466d7` are queued, not green
evidence. These packets further reduce W01 but do not close W01 or establish a
privileged native FUSE mount, FSKit activation, live R2, or hosted Windows run.

The Windows HostFs follow-up was then published as `44cef6e` (from sidecar
`d64d3bb`). It aligns long-path symlink fallback, Windows unlink disposition,
and read-only hard-link metadata/lifetime behavior, with Windows-gated
regressions. The focused macOS suite passed 10/10, the installed
`x86_64-pc-windows-gnu` target compiled all HostFs targets, strict Clippy and
format/diff checks passed. Hosted Windows runtime execution remains open, so
this packet is target-compile evidence rather than Windows platform acceptance.

Fresh macOS demo evidence on this checkout: `bash scripts/demo-end-to-end.sh`
exited 0 after building the Rust CLI. The CLI mounted the local HostFs through
native NFS; an independent Rust process wrote/read `rust-process.txt`, an
independent Node `fs/promises` process wrote/read `javascript-process.txt`, a
second Rust process verified the JavaScript bytes, and the CLI unmounted cleanly
with the bytes still present in the host-backed directory. This is a real
single-host NFS mount demonstration, not FUSE/FSKit or a remote-provider result.

The matching Node CLI native path also passed on macOS: `node
examples/node-cli/index.mjs --driver host --root <temporary backing>
--transport nfs --mountpoint <temporary mount> --self-test` exited 0, with the
Node SDK mounting through native NFS, writing and reading its file, unmounting,
and leaving the temporary backing directory clean. This is a separate Node CLI
consumer check; it does not qualify Linux FUSE, FSKit or live providers.

The latest W01 acceptance packets were published to `origin/main` at
`25644c5`. `462d54b` closes the remaining Unstorage evidence harness gap:
preparations now execute for both oracle and native adapters, refusals assert
state preservation, hidden hardlink/mknod boundaries are split into separate
rows, and the packet is part of the N-API chain. The direct and N-API gates
passed 31 rows (5 PASS, 26 exact ENOSYS, zero ENOTSUP or skipped rows).
`a3d980d` extends the opt-in Node SDK CLI native gate so the mounted path is
exercised by an independent Rust client and an independent Node client, with
Rust verifying the bytes written by Node. The authorized macOS NFS run passed
mount, Rust read/write, Node read/write, cross-client readback, unmount and
backing-root persistence. `25644c5` adds plain-flag FUSE `RENAME2` session
support while keeping unsupported flags fail-closed; 16 focused FUSE tests,
strict Clippy and formatting passed. FALLOCATE, LSEEK and COPY_FILE_RANGE
remain explicit unsupported boundaries, and Linux hosted FUSE remains a
separate acceptance gate.
The follow-up `7183778` strengthens the same native gate by asserting that
both Rust- and Node-written payloads remain in the backing root after unmount.

The third W01 implementation rotation was validated locally and published
sequentially to `origin/main` (verified remote ref
`10666e8dcca35c0cb39483317272de6832acb984`). The Rust FUSE packet is
`15026f2`, the napi-rs packet is `5b7f982`, and the CI packet is `d28f31a`.
Rust now validates the fixed-width FUSE `POLL` request at
the session boundary, rejects malformed/truncated/trailing frames with
`EINVAL`, and keeps valid `POLL` at an explicit safe `ENOSYS` boundary until a
driver supplies poll semantics. napi-rs exposes the matching request/reply
codecs, generated declarations/artifacts, protocol-minor oracle coverage and
malformed-frame tests. CI now runs the opt-in Node SDK CLI native mount gate on
Linux and macOS when the required host prerequisites are present. The combined
locked workspace gate, scoped FUSE tests/Clippy and oracle-enabled N-API suite
passed; hosted Linux/macOS results and actual kernel POLL support remain open.

The next three implementation packets were validated and published to
`origin/main` on 2026-09-21. The HostFs packet is local `72d570f` and finished
publishing at remote commit `1352fa9`; the N-API FUSE lifecycle packet is local
`13c3acb` and finished at remote commit `5ba69dd`; and the pure Rust FUSE INIT
packet is local `b2040f6` and finished at remote commit `63c4264`. The HostFs
packet adds libuv-aligned Windows read-only creation/unlink behavior and
Win32 hard-link lifetime coverage. The N-API packet adds pinned-oracle
`RELEASE`/`RELEASEDIR`, `FLUSH`, and `FSYNC`/`FSYNCDIR` request/status codecs.
The Rust packet hardens version-gated `FUSE_INIT_EXT`/`flags2` handling and
adds six wire-negotiation integration tests. The combined locked Rust suite
and full pinned-oracle N-API suite passed; the N-API packet's scoped Clippy
gate still excludes the pre-existing `js_driver.rs` type-complexity warning.
Hosted Windows runtime, live R2/PGlite, FSKit activation and privileged native
FUSE acceptance remain separate gates.

The fourth W01 implementation packet is locally validated in `1a8b112` and
published through remote commit `7c6186f`. Rust FUSE now strictly validates
the `FALLOCATE`, `RENAME2`, `LSEEK`, and `COPY_FILE_RANGE` request bodies,
rejects malformed/trailing frames with `EINVAL`, and keeps valid operations at
an explicit `ENOSYS` boundary without mutating the session. The focused FUSE
tests and strict scoped Clippy gate passed; hosted/native acceptance remains a
separate gate.

The W01 follow-up packets are now published sequentially. The N-API xattr
packet is local `cf7a132` and finished at remote commit `4f484ad`; it adds
pinned-oracle `SETXATTR`, `GETXATTR`, `LISTXATTR`, and `REMOVEXATTR` codecs,
generated declarations/artifacts and malformed/truncated/trailing coverage.
The Unstorage packet is local `e0e8195` and finished at remote commit
`9b74c87`; its 14 capability-boundary rows passed with 5 supported results, 9
explicit `ENOSYS` results, zero `ENOTSUP` mismatches and zero skips.

The Unstorage hardlink follow-up is local `99c7d32` and published through
`2bce444`: four hardlink capability rows passed with zero supported results,
four exact `ENOSYS` results, zero `ENOTSUP` mismatches and zero skips; every
row preserved errno `-38`, syscall `link`, and null path/destination fields.

The W04.3 PGlite packet was rebased onto remote `8ea3dc8` and published as a
seven-file sequence ending at `90c33949`. It adds durable version metadata,
real-server reconnect/version tests, mount-free SQLite VFS coverage and the
configuration-driven PGlite gate. Focused versioning/VFS tests, locked
compilation, formatting, Clippy and script checks passed; hosted reconnect
acceptance remains open.

The provider packets subsequently published on the current remote are W08
TiDB/RustFS hardening (rebased local tip `6327857`, published through
`bbe8ebb`), W07 FoundationDB/RustFS restart-gate hardening (rebased local tip
`629c2f6`, published through `ffd06bf`) and W30 observability (published at
`6ce8228`). W26's nine-file Ozone packet remains byte-for-byte present in the
current branch at its verified `a936eba` content. The W26 follow-up now adds a
dedicated hosted `ozone-compositions` CI job for the real SQLite/PGlite mixed
metadata gate; its hosted result remains pending until that job runs. W08 and
W07 service runs remain explicitly blocked by unavailable Docker/libfdb runtime
prerequisites;
W30 external collector reachability and Linux/Windows qualification remain
open; the W30.5 local loopback collector, exporter-failure/shutdown tests,
macOS qualification packet, and no-exporter facade benchmark are now verified
on the current macOS arm64 host.

## How to read and maintain this tracker

- **Landed:** committed implementation, not necessarily full acceptance.
- **Implementing:** active, uncommitted work; not delivered.
- **Verifying:** implementation exists, acceptance evidence is incomplete.
- **Planned:** required but not yet assigned or implemented.
- **Deferred:** deliberately sequenced later, not forgotten or completed.
- Checked tasks describe only the precise scope stated. Unchecked tasks remain open.
- Main maintains this file after handoffs, validated commits, failures and new
  findings. Workers report task IDs, changed files, commands/results and blockers
  to main rather than concurrently editing it. Split newly discovered work into
  named tasks below. Keep failed, skipped and unavailable evidence visible.
- Commit and push validated chunks to `origin/main`; do not call a workstream
  complete until its integration and platform acceptance gates pass.

### Delegated execution and rotation

`Main` in the dashboard means the coordinator owns cross-stream integration,
acceptance evidence, tracker updates and commit/push; it does not mean that
every implementation task is being done serially. Workers receive disjoint
write scopes, return exact paths and test evidence, and are closed after their
patch is integrated. A completed worker is then rotated into the next open,
non-overlapping packet. The current bounded allocation is:

| Worker | Packet | Write scope | Handoff state |
| --- | --- | --- | --- |
| `dbfa2ea` | N-API strict-Clippy type cleanup | Factors the structural-driver open future into a named alias; full scoped N-API Clippy now passes with `-D warnings` and no type-complexity exclusion. This is lint evidence, not native-mount or hosted-platform acceptance |
| `66c79e0` | Rust FUSE session parity for simple namespace operations | 16 frame-level session tests cover SYMLINK/MKNOD/MKDIR/UNLINK/RMDIR/RENAME/LINK/ACCESS, error/state cleanup, MKNOD fallback and POSIX name limits; focused FUSE tests, strict Clippy and formatting passed. Native device/mount and advanced FALLOCATE/LSEEK semantics remain open |
| `6972fbe` | Unstorage hardlink alias capability boundary | Direct and N-API packets now cover six exact `ENOSYS` hardlink rows, including alias write-through and unlink-lifetime cases, with zero `ENOTSUP` mismatches and zero skips; generic hardlink inode support remains open |
| `96bbbd9` | Structural-driver native lifecycle assertion hardening | Explicit transport identity is asserted and the PASS marker is emitted only after unmount, live-mount cleanup and mountpoint teardown; macOS NFS structural mount/read/write/unmount passed. Linux FUSE hosted execution remains unverified |
| `24c8a6b` | Public napi-rs FUSE request/reply codecs for SYMLINK/MKNOD/MKDIR/UNLINK/RMDIR/RENAME/RENAME2/LINK/ACCESS/FALLOCATE/LSEEK | Pinned mountx differential plus typed round-trip coverage passed; full oracle-enabled N-API test chain, typecheck and artifact aggregation passed. Native FUSE session/device/mount remains open |
| Peirce | W12/W15 SQLite VFS and WAL/reliability seam | `integrations/mount-rs-sqlite-vfs/**`, related VFS plan | Integrated |
| Mill | W08 TiDB provider and RustFS composition harness | `integrations/mount-rs-tidb/**`, `tests/tidb/**`, TiDB harness | Integrated; bounded TiDB + RustFS composition passed |
| Aristotle | W13 macOS FSKit seam | `integrations/mount-rs-fskit/**` | Integrated checkpoint |
| Meitner | W24 TanStack Start marketing/docs site | `apps/site/**` | Child task complete; custom domain live |
| Ohm | W18.6 storage-dispatch draft review | `benchmarks/storage/dispatch/**` | Closed; no change recommended |

Completed W01 rotation packets (closed after main reviewed and published each
patch):

| Worker | Packet | Write scope | Handoff state |
| --- | --- | --- | --- |
| Hume | W01 Linux structural FUSE acceptance wiring | `.github/workflows/ci.yml`, `integrations/mount-rs-napi/{README.md,test/structural-native.mjs}` | Integrated as `31d62df`; hosted Linux result pending |
| Gibbs | W01 Unstorage timestamp metadata parity | `integrations/mount-rs-kv/tests/driver.rs`, Unstorage parity tests | Integrated as `3355cc1`; focused oracle gates passed |
| Euler | W01 FUSE READDIR body codec | `integrations/mount-rs-napi/{fuse.cjs,postlude-fuse-codec.cjs,index.d.ts,test/fuse-codec.mjs}` | Integrated as `323231f`; pinned differential passed |
| Confucius | W01 FUSE READDIRPLUS body codec | `integrations/mount-rs-napi/{fuse.cjs,postlude-fuse-codec.cjs,index.d.ts,test/fuse-codec.mjs}` | Integrated in the current SDK/transport packet; pinned differential and aggregation passed |
| James | W01 Unstorage capability classification | `tests/unstorage/capability-parity.mjs` | Integrated in the current packet; 5 oracle-classified unsupported rows passed |
| Curie | W01 provider-consumer durability expansion | `tests/provider_matrix/src/main.rs` | Integrated in the current packet; direct and split SQLite reopen rows passed |
| Galileo | W01 seeded provider/consumer lifecycle parity | `tests/provider_matrix/**` | Integrated as `a31880d`; 5 Rust SDK, 4 Node SDK and 9 CLI passes; PGlite/R2 explicit skips |
| Poincare | W01 Unstorage path/metadata/handle parity | `tests/unstorage/**` | Integrated as `cc73ad5`; 11 rows, zero mismatches/skips; remaining capability limits explicit |
| Socrates | W01 FUSE OPEN/OPENDIR request codecs | `integrations/mount-rs-napi/**` | Integrated as `a3795f0`; protocols 7.8/7.39/7.41 differential passed; native mount remains open |
| Lagrange | W27 Windows HostFs symlink portability | `crates/mount-rs-host/**` | Integrated as `80f2efd` / published `e5902bed`; focused macOS and Windows-target gates passed; hosted Windows runtime remains open |
| Mendel | W10 Rust FUSE ACCESS session dispatch | `transports/mount-rs-fuse/**` | Integrated as `16b2180` / published `9832b60`; focused locked session tests passed; native device/mount remains open |
| Euler | W01 napi-rs FUSE LOOKUP request/reply codecs | `integrations/mount-rs-napi/**` | Integrated as `7dd9a60` / published `83466d7`; pinned protocol differential and full N-API gates passed |
| Locke | W27 Windows HostFs read-only/link lifetime parity | `integrations/mount-rs-host/**` | Integrated as `72d570f` / published through `1352fa9`; 15 macOS tests, Windows-target check and target Clippy passed; hosted Windows runtime remains open |
| Singer | W01 napi-rs FUSE lifecycle codecs | `integrations/mount-rs-napi/**` | Integrated as `13c3acb` / published through `5ba69dd`; pinned differential and full N-API suite passed; strict pre-existing Clippy lint remains |
| Zeno | W01 Rust FUSE INIT wire negotiation | `transports/mount-rs-fuse/{src/init.rs,tests/init.rs}` | Integrated as `b2040f6` / published through `63c4264`; six focused tests and strict scoped Clippy passed |
| Schrodinger | W01 napi-rs FUSE READLINK/STATFS codecs | `integrations/mount-rs-napi/**` | Integrated as `054fb95`; pinned protocol differential, declarations/artifacts, typecheck, build and full N-API suite passed; scoped Clippy retains the known pre-existing lint exclusion |
| Pasteur | W01 Node SDK CLI native integration | `examples/node-cli/**` | Integrated as `8e1f218`; opt-in macOS NFS mount/read/write/unmount/persistence passed; Linux FUSE and unavailable-platform paths remain explicit skips |
| Pascal | W01 Rust FUSE READLINK/STATFS wire/session behavior | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `08731ed`; full FUSE suite and strict scoped Clippy passed; native kernel/session acceptance remains separate |
| Nietzsche | W01 napi-rs FUSE BATCH_FORGET/INTERRUPT codecs | `integrations/mount-rs-napi/**` | Integrated as `4dc90d5`; generated bindings, pinned-oracle protocol-minor tests, typecheck, build and full N-API suite passed; scoped Clippy retains the known pre-existing lint exclusion |
| Nash the 2nd | W01 Rust FUSE forget/interrupt validation | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `7934062`; strict BATCH_FORGET validation, fail-closed INTERRUPT semantics, 48 package tests and strict scoped Clippy passed; native kernel cancellation remains open |
| Kant | W01 Unstorage remaining capability parity | `tests/unstorage/**` | Integrated as `b13350f`; 25 oracle rows passed with 4 supported, 21 explicit ENOSYS classifications, 0 ENOTSUP mismatches and 0 skips |
| Aquinas the 2nd | W01 Rust FUSE POLL session validation | `transports/mount-rs-fuse/{src/session.rs,tests/**}` | Integrated as `15026f2`; strict 24-byte framing, malformed/trailing rejection, explicit ENOSYS boundary, 50 package tests and strict scoped Clippy passed |
| Euclid the 2nd | W01 napi-rs FUSE POLL request/reply codecs | `integrations/mount-rs-napi/**` | Integrated as `5b7f982`; generated artifacts/declarations, protocol-minor oracle differentials, malformed/truncated/trailing tests, build/typecheck and full N-API suite passed |
| Dalton the 2nd | W01 Node SDK CLI native CI gate | `.github/workflows/ci.yml` | Integrated as `d28f31a`; opt-in Linux/macOS native gate with bounded timeouts and prerequisite probes; hosted results remain unverified |
| Mendel the 2nd | W01 Rust FUSE advanced-operation validation | `transports/mount-rs-fuse/{src/session.rs,tests/session.rs}` | Integrated as `1a8b112`; published through `7c6186f`; strict FALLOCATE/RENAME2/LSEEK/COPY_FILE_RANGE framing, explicit EINVAL/ENOSYS boundaries, no-mutation/session-survival tests and strict Clippy passed |
| Ramanujan the 2nd | W01 napi-rs FUSE xattr codecs | `integrations/mount-rs-napi/**` | Integrated as `cf7a132`; published through `4f484ad`; SETXATTR/GETXATTR/LISTXATTR/REMOVEXATTR pinned-oracle differentials, generated artifacts/declarations, typecheck, build and full focused suite passed |
| Zeno the 2nd | W01 Unstorage capability boundary parity | `tests/unstorage/**` | Integrated as `e0e8195`; published through `9b74c87`; 14 rows passed with 5 supported, 9 explicit ENOSYS, zero ENOTSUP mismatches and zero skips |
| Kant the 2nd | W01 Unstorage hardlink capability boundary | `tests/unstorage/**` | Integrated as `99c7d32`; published through `2bce444`; 4 rows passed with 0 supported, 4 exact ENOSYS, zero ENOTSUP mismatches and zero skips |
| Main | W01 Rust FUSE IOCTL session framing | `transports/mount-rs-fuse/{src/session.rs,tests/session.rs}` | Integrated as `f1872f8`; live source/test blobs verified; strict 32-byte header and declared-input-size framing, malformed/trailing `EINVAL`, valid-request `ENOSYS`, and no-mutation coverage passed in 12 focused tests and strict scoped Clippy |
| Meitner the 2nd | W01 napi-rs FUSE IOCTL codecs | `integrations/mount-rs-napi/**` | Integrated as `32ddee3`; published sequentially through `8ea5f38`; build, typecheck, focused pinned-oracle raw-layout differential, and the full oracle-enabled N-API suite passed |
| Pasteur the 2nd | W01 napi-rs FUSE BMAP codecs | `integrations/mount-rs-napi/**` | Integrated as `387940b`; published sequentially through `091ddcf`; Rust/N-API release build, typecheck, protocol-minor/truncation/trailing/wrong-shape oracle differentials, and the full oracle-enabled N-API suite passed |
| Main | W01 napi-rs FUSE GETLK/SETLK/SETLKW codecs | `integrations/mount-rs-napi/**` | Current packet: generated bindings/declarations, explicit ESM/CommonJS exports, typecheck, pinned-oracle request/reply/error-boundary differential, release build, focused locked FUSE tests and full oracle-enabled N-API suite passed; native FUSE session/mount remains open |

Closed packets already integrated this cycle include Mendel (FUSE), Lagrange
(Windows host), Epicurus (CLI), Maxwell (FoundationDB), Newton/Astra (R2
design review), Raman (scoped napi-rs package distribution), Aristotle (the
unsigned FSKit bridge checkpoint), Mill (the TiDB provider/harness checkpoint),
Peirce (the SQLite VFS/WAL checkpoint), Russell (CLI/HTTP edge coverage),
Aquinas (Windows CI parity), Cicero (parity audit), and Ohm (storage-dispatch
review). Main
rotates those slots rather than assigning multiple workers to the same files.

### Narrow-band completion order

To reduce half-finished breadth, new worker packets are not opened outside the
current vertical slice until its acceptance gate is green:

1. **P0 runtime path:** core mountx parity, independent metadata/block stores,
   fixed-size chunking, and the shared contract through memfs, SQLite, PGlite
   and authenticated Cloudflare R2.
2. **P0 consumer path:** the same configured drives through napi-rs, the
   config-file CLI and the multi-drive HTTP API, with reopen, range, partial
   write, truncate, concurrency, fault-injection and cleanup evidence.
3. **P0 reliability/platform path:** SQLite hosting/VFS/WAL matrix and
   macOS/Linux qualification, while Windows CI remains a required parallel
   signal and is never treated as passed from Unix evidence.
4. **P1 integration path:** TiDB and FoundationDB metadata with RustFS blocks,
   then FSKit and remaining native acceptance. Existing bounded workers may
   finish their current packets, but they must not grow scope.
5. **P2 evidence/product path:** versioning, benchmarks/compression, AWS/Ozone,
   reference reviews, site/domain and publication hardening.

Distributed cache (W22), physical copy-on-write (W23), lifecycle hooks (W29)
and OTel (W30) remain deferred discussions/features until the P0/P1 gates are
complete.

## Workstream dashboard

| ID | Stream | Status | Current owner |
| --- | --- | --- | --- |
| W01 | Core and mountx parity | Active simple-first; parallel sidecars are integrated as bounded packets; full parity remains open | Main (integration/acceptance) |
| W02 | Metadata/block split and chunking | Verifying; persisted chunker metadata and partial-write/reopen gates landed | Main |
| W03 | Memory and SQLite stores | Landed; extending | Main |
| W04 | PGlite | Verifying | Main |
| W05 | Cloudflare R2 | Complete for requested Rust/Node SDK and CLI hosted acceptance; native/platform gates remain separate | Main |
| W06 | RustFS integration service | Landed; extending | Lagrange (complete slice) / Main |
| W07 | FoundationDB | Provider/composition passed; target-gated root member and Rust SDK/CLI selection landed; production authority, Node/native, hosted acceptance and the W07.7 production rollout gate remain open | Maxwell (complete slice) / Main |
| W08 | TiDB | Functional hosted acceptance complete for the defined scope: durable 3PD/3TiKV restart, provider fencing/ambiguous commit, live TiDB/RustFS Node/CLI/FUSE, ARM and macOS/Ubuntu native rows passed; production rollout remains NO-GO with P01–P09 open | Mill (functional checkpoint) / Main; production ownership TBD |
| W09 | Node / napi-rs and public API | Verifying; public Rust SDK, Rust-backed FUSE state, and Node SDK CLI landed; platform/package gaps remain | Main (packets integrated) |
| W10 | FUSE, NFS, 9P, WebDAV, S3 | FUSE codec, lifecycle, ACCESS, INIT and session packets landed; native and cross-platform transport acceptance remains open | Main (packets integrated) |
| W11 | Config-driven CLI | Rust CLI now consumes the Rust SDK; Node CLI consumes the Node SDK; provider/remote/hosted gates remain | Main |
| W12 | Safely hosting SQLite files | Local journal/WAL matrix, fail-closed bridge, and recovery gates landed; hosted/Windows gates pending | Peirce (checkpoint) / Main |
| W13 | macOS FSKit | Unsigned bridge checkpoint passed; activation/signing pending | Aristotle (checkpoint) / Main |
| W14 | Versioned filesystems | Local foundation landed; integration pending | Main |
| W15 | Mount-free SQLite VFS | Rollback, process-local and host-local WAL gates landed; remote/Node/Windows acceptance pending | Peirce (checkpoint) / Main |
| W16 | just-bash / Mastra adapters | Landed locally; hosted verification pending | Main |
| W17 | Multi-drive HTTP server | Server/CLI and local edge-case coverage landed; RustFS remote passed, Cloudflare acceptance pending | Russell (complete slice) / Main |
| W18 | Benchmarks and dependency budget | Partial implementation | Ohm / Main |
| W19 | Compression | Design review recorded | Main |
| W20 | CI, packaging and final acceptance | Verifying | Main |
| W21 | Reference review and learnings | Ongoing | Main |
| W22 | Distributed caching | Deferred for discussion | User / Main |
| W23 | Physical copy-on-write | Future requirement | Unassigned |
| W24 | Domain and marketing site | TanStack Start site deployed; `mount-rs.com` and `www.mount-rs.com` live on Vercel | Meitner (complete slice) / Main |
| W25 | Actual AWS S3 integration | Complete for the myroot private bucket, scoped role, live Rust gate, and owned-prefix cleanup | Main |
| W26 | Apache Ozone S3 backend | W26 qualification complete within documented scope: local and hosted Ozone gateway, SQLite/PGlite composition, durable FoundationDB, and Ozone-backed durable TiDB gates passed; post-demo production rollout track is open and currently NO-GO | Main |
| W27 | Native Windows support and CI | HostFs symlink, read-only create/unlink and hard-link packets landed; hosted runtime and mount qualification pending | Main |
| W28 | Deterministic fault injection | Implementing | Main integration |
| W29 | User-configurable lifecycle hooks | Deferred for later | Unassigned |
| W30 | OpenTelemetry traces, metrics and logs | Implementing: opt-in facade, boundary wiring, local collector/failure tests, benchmark and macOS qualification packet landed; external collector/Linux/Windows evidence pending | Main |
| W31 | Per-drive mounts from one backing datastore | Deferred for future design | Unassigned |

## Decisions and external prerequisites

- After the app restart, the nine prior worker handles were missing. Their
  checkout edits were preserved. Seven replacement Luna Max workers were
  started and verified running; current ownership is in the dashboard above.
  Historical worker names in individual evidence entries identify earlier
  work, not live sessions. Main owns integration, root manifests, and commits.

- [x] **D01 — Live R2 credentials:** security-provisioned bucket-scoped Object
  Read & Write credentials for `mount-rs-integration-tests` are stored as
  encrypted GitHub `r2-ci` environment secrets, using a one-week Cloudflare
  token TTL through 2026-09-28. The final hosted run `35579757447` passed the
  budget guard, Rust/Node SDK and CLI matrices, live R2 trace/CLI/N-API,
  service evidence and benchmark artifact. Credential values are not committed
  or recorded; rotation remains scheduled maintenance.
- [x] **D02 — License:** user selected Apache-2.0 for mount-rs on 2026-09-20.
  First-party package declarations use Apache-2.0; preserve third-party notices,
  including MIT attribution for upstream-derived portions.
- [ ] **D03 — FSKit:** obtain signing/install/activation authorization and host
  prerequisites when executable extension testing is ready.
- [ ] **D04 — Distributed cache:** discuss topology, consistency and service
  choice with the user after the primary implementation is accepted.
- [x] **D05 — Launch:** user authorized the existing Vercel Hobby plan and the
  `mount-rs.com` launch. Route 53 and Vercel access are verified; no duplicate
  purchase, paid upgrade or additional paid resources were used.
- [x] **D06 — Rust crate publication authorization:** user authorized publication
  from CI on `main` when ready. npm publication must use `@mount-rs`, with secure
  release controls for both registries. Verify namespace ownership, OIDC trust,
  protected main-only release jobs, provenance/package integrity and readiness
  before publishing. This authorization is not release-readiness evidence.

## W01 — Core and mountx behavioral parity

Current priority is simple-to-complex execution. Close the mount-free
in-memory Rust/Node/oracle contract first, then add persistence, remote
providers, transports, and native mounts only as separate later gates. A
passing remote/provider or native test cannot close a simpler parity gap, and
the W01 stream must remain open until its skipped behavior is classified and
the applicable oracle-backed cases are covered.

- [x] Land Rust filesystem contract and implementations, with separate crates.
- [x] Pin mountx oracle to `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.
- [x] Latest local upstream-suite stage: 1,194 passed, 88 skipped; not full parity
  acceptance and not evidence that skipped behavior works.
- [ ] W01.1 Complete every applicable export/behavior in the API parity ledger.
- [ ] W01.2 Classify all upstream skips and add coverage for required behavior.
- [ ] W01.3 Finish seeded cross-engine traces and retain reproducible failures,
  seeds and revision-matched results for every required backend.
- [ ] W01.4 Verify errors, paths, binary data, links, timestamps, handles,
  concurrency and lifecycle on supported macOS and Linux configurations.
- [x] W01.5 Add a bounded deterministic concurrency packet: six scenarios,
  five explicit unsupported classifications, zero oracle mismatches, and five
  consecutive pinned-oracle runs. Cross-process/crash, exact append ordering,
  cancellation/close races, durability/restart and transport/native concurrency
  remain open by classification.

Evidence landed without closing the remaining W01 acceptance gates:

- [x] `0de1832` plus `6ba3d62` now provide a mount-free core parity harness:
  101 operations, 79 successful results, 22 stable expected errors and zero
  mismatches/skips against the pinned oracle. The added `lchown` symlink
  lifecycle row closes a concrete core-ledger gap. This still does not claim
  concurrency, persistence, providers, transports or native mounts.
- [x] `ae2f12d` adds the complete 88-row upstream skip inventory and a
  revision-checked trace-evidence runner. Five pinned seeds passed on the
  memory/SQLite/object-store/chunked six-backend lane, and the dedicated
  PGlite lifecycle extended the same five-seed, 621-operation trace to
  `pglite` and `chunked-pglite`; live R2 remains credential-gated.
- [x] `dd65770` adds the Rust-backed N-API FUSE codec subpath and declarations;
  it is mount-free protocol coverage, not native FUSE session acceptance.
- [x] `0d7f1f4` proves the Rust CLI's real macOS NFS mount path with independent
  Rust and Node filesystem clients; `0dca1d1` adds a separate Node SDK CLI and
  its opt-in real macOS NFS self-test. Linux and provider-backed SDK matrices
  remain unverified here.
- [x] The isolated provider/consumer matrix exercises Rust SDK, Node SDK and
  both real CLI consumers with machine-readable PASS/SKIP/FAIL output. The
  current offline Rust matrix is 4/4, the process-level CLI matrix is 8/8
  with two explicit provider-gated skips, and the Node CLI includes a durable
  SQLite shutdown/reopen row. The local PGlite lifecycle remains the next
  rerun target for the new CLI rows; R2 rows remain explicit skips without
  credentials and do not count as live-provider acceptance.
- [x] `29337f7` makes the Rust CLI construct all local/provider drivers through
  the public `mount-rs-sdk` facade and adds a public Node CLI SDK self-test. The
  Rust SDK example, Rust CLI, Node CLI and provider matrix now exercise the same
  SDK contract; native mount and live remote-provider acceptance remain separate.
- [x] `71e826f` pins the parity checker to the oracle revision, requires locked
  Cargo execution, and records the next simple-first W01 closure queue. The
  pinned 101-step core trace and six-scenario concurrency trace still pass with
  zero mismatches; the listed lifecycle, capability and native gaps remain open.
- [x] `7236e0a`, `4422cd0`, `57632c6`, `e213856`, `1af1986`, `8ac77f3`,
  `9215ba3` and `a1ec0c5` integrate parallel W01 capability, structural native,
  Windows SQLite and NFS held-handle sidecars. Their focused evidence is listed
  above; these commits do not close the remaining end-to-end parity gates.
- [x] `21803fd` adds the SDK-backed Node CLI provider-config/reopen path, extends
  the Rust and Node provider matrices to public-SDK PGlite/R2 compositions, adds
  the memory durability guard, and wires structural-driver coverage into the
  Windows/macOS Node CI jobs. Focused local gates passed; live R2 and hosted CI
  remain open.
- [x] The current SDK CLI packet adds the actual Rust binary `sdk-self-test`
  command and process-level memory/SQLite reopen integration checks, extends
  the Node CLI with the matching SQLite reopen example, and adds public
  `READDIRPLUS` codec differential coverage. The focused CLI/N-API/provider
  gates pass; PGlite, R2, native and hosted platform gates remain open.
- [x] `655cf19` records the SDK-backed Rust and Node CLI consumers as runnable
  examples and process-level integration tests. The Rust CLI constructs its
  filesystem through the public SDK, the Node CLI does the same through the
  napi-rs package, and both exercise durable SQLite shutdown/reopen paths.
- [x] `fd5eb04` forwards decoded numeric `open_flags` through the structural
  N-API adapter and changes the opt-in native lifecycle from an expected write
  refusal to mounted write/readback. The local macOS NFS run passed; other
  transport/platform acceptance remains open.
- [x] `31d62df` wires the Linux structural-driver FUSE mount/read/write/unmount
  acceptance job with explicit `/dev/fuse` prerequisites. The local harness and
  YAML validation passed; hosted Linux execution remains unverified.
- [x] `3355cc1` adds Unstorage timestamp metadata parity for atime, mtime, ctime
  and birthtime. Focused Rust, direct oracle and N-API tests passed; the other
  capability-limited rows remain open.
- [x] `323231f` exposes the FUSE `READDIR` body pack/unpack codec with UTF-8,
  alignment, bounded-packing and malformed-input coverage against the pinned
  oracle.
- [x] The current FUSE follow-up exposes and differentially tests the public
  `READDIRPLUS` body codec, including legacy/default layouts, integer coercion,
  truncation, padding and missing-entry errors. FUSE session and native-mount
  surfaces remain open.
- [x] The next disjoint W01 packets are also integrated: `750484d` adds 11
  oracle-classified Unstorage edge rows with zero skips; `26427f8` adds a
  72-step core handle/lifecycle trace with 63 successes, 9 expected errors and
  zero mismatches; and `8eac5cb` exposes the N-API FUSE `READ` request/raw-reply
  codec with protocol 7.41/7.8 differential and malformed-input coverage.
  The combined N-API suite passed; native FUSE session/device/mount and the
  remaining capability-limited rows remain open.
- [x] The latest W01 rotation is also integrated: `6ba3d62` adds `lchown`
  symlink lifecycle parity (101 steps, 79 successes, 22 expected errors,
  zero mismatches/skips); `58144fd`/`7238c829` classify the remaining
  Unstorage inventory (14 rows, 4 PASS, 10 ENOSYS, zero skips); and
  `63d9585`/`5c16164` expose the Rust-backed FUSE `CREATE` request/reply
  codecs across protocol 7.41/7.39/7.12/7.8. The full locked Rust and
  oracle-enabled N-API gates passed; native FUSE session/mount and live
  provider gates remain separate.
- [x] The next W01 rotation is integrated: `80f2efd` / `e5902bed` fixes
  Windows HostFs symlink path handling with a long-path fallback; `16b2180` /
  `9832b60` adds Rust FUSE `ACCESS` session dispatch and permission checks; and
  `7dd9a60` / `83466d7` exposes the Rust-backed N-API FUSE `LOOKUP` codecs.
  The combined locked Rust and oracle-enabled N-API gates passed. Hosted
  Windows, live R2, FSKit activation and privileged native mount evidence
  remain open.
- [x] The current implementation rotation is integrated: `72d570f` adds
  Windows read-only create/unlink and hard-link lifetime parity; `13c3acb`
  exposes the Rust-backed N-API `RELEASE`/`RELEASEDIR`, `FLUSH`, and
  `FSYNC`/`FSYNCDIR` codecs; and `b2040f6` hardens pure Rust FUSE INIT
  negotiation with six integration tests. The combined locked Rust workspace
  and pinned-oracle N-API gates passed. The N-API scoped Clippy run retains
  the pre-existing `js_driver.rs` type-complexity exclusion; hosted Windows,
  live providers, FSKit and privileged native mount evidence remain open.
- [x] The next three-way W01 rotation is integrated: `08731ed` adds typed
  Rust FUSE `READLINK`/`STATFS` wire/session behavior; `054fb95` exposes the
  matching napi-rs codecs, generated declarations/artifacts and pinned-oracle
  differentials; and `8e1f218` adds the opt-in Node SDK CLI native mount
  integration. The current-tree locked Rust workspace, oracle-enabled N-API
  suite, and macOS NFS Node CLI mount/read/write/unmount/persistence test all
  passed. Hosted Linux FUSE, hosted Windows, FSKit, live R2/PGlite and the
  remaining FUSE session/mount surfaces remain open.
- [x] The second W01 rotation is integrated: `7934062` validates FUSE
  `BATCH_FORGET` and fail-closed `INTERRUPT` sessions; `4dc90d5` exposes the
  matching napi-rs codecs and pinned-oracle tests; and `b13350f` replaces the
  remaining Unstorage capability skips with exact supported/`ENOSYS` assertions.
  The full locked Rust workspace, oracle-enabled N-API suite, 25-row Unstorage
  packet and previous native Node CLI acceptance all passed. Hosted Linux and
  Windows, FSKit, live R2/PGlite, and kernel-level cancellation remain open.
- [x] The third W01 rotation is integrated and published sequentially: `15026f2`
  validates FUSE `POLL` frames at the Rust session boundary; `5b7f982` exposes
  the matching napi-rs request/reply codecs and pinned-oracle tests; and
  `d28f31a` adds the opt-in Linux/macOS Node SDK CLI native gate to CI. The
  locked all-feature workspace, 50-test FUSE package gate, scoped Clippy,
  build/typecheck and full oracle-enabled N-API suite passed. The verified
  remote ref after publication is
  `10666e8dcca35c0cb39483317272de6832acb984`. Hosted CI and kernel poll
  semantics remain explicit acceptance boundaries.
- [x] The fourth W01 Rust FUSE packet is integrated locally as `1a8b112`:
  advanced-operation request framing (`FALLOCATE`, `RENAME2`, `LSEEK`, and
  `COPY_FILE_RANGE`) rejects malformed/trailing input with `EINVAL`, returns
  explicit `ENOSYS` for valid-but-unsupported operations, and preserves
  session state without mutation. Focused FUSE tests and strict scoped Clippy
  passed; sequential remote publication is pending.
- [x] The second-rotation implementation and documentation files were published
  sequentially to `origin/main`; the verified remote ref after that packet was
  `9c5f910489741c169031f2f737c3eb51ed427c89`. The local checkout remains
  intentionally divergent because the Contents API creates one remote commit
  per file.
- [x] Current-tree demo rerun after the second rotation also exited 0:
  `bash scripts/demo-end-to-end.sh` mounted the Rust CLI through macOS NFS,
  exercised independent Rust and Node clients on one mount, verified the
  cross-process bytes after unmount, and retained the backing data; the Node
  SDK CLI native self-test independently passed its NFS mount/read/write,
  unmount and persistence checks.
- [x] The three implementation packets were published sequentially to
  `origin/main`; their verified implementation ref before the tracker-only
  publication was `bcf465334273c8701161c0fb35be7bfc4ddfb538`, followed by
  tracker publication `b04f6ca4979cf632d74d31d9f6b55792f3f86f98`. The local
  checkout remains intentionally divergent because Contents-API publication
  creates one remote commit per file; no force-push or destructive
  synchronization was used.

- [x] `f1872f8` adds the Rust FUSE IOCTL session packet: exact 32-byte header/input-size framing, `EINVAL` for truncated/declared-size/trailing payloads, explicit `ENOSYS` for valid requests, and no state mutation. The isolated 12-test FUSE gate and strict scoped Clippy passed. The elevated macOS N-API regression suite also passed, including native NFS server integration and the pinned-oracle/Unstorage/distribution gates; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] `32ddee3` adds the N-API FUSE IOCTL codec packet: 32-byte request and 16-byte reply layouts, declared input-size framing, protocol-context handling, signed results, malformed/trailing rejection, and pinned-oracle differential coverage. The full oracle-enabled N-API suite passed after publication; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] `387940b` adds the N-API FUSE BMAP codec packet: typed request/reply layouts, protocol-minor coverage, all truncation boundaries, trailing-byte rejection and wrong-shape errors against the pinned oracle. The post-publication full oracle-enabled N-API suite passed; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] Final combined N-API verification on 2026-09-21 passed after rebuilding the release native binding: elevated `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX pnpm test` reported both IOCTL and BMAP pinned-oracle differentials, plus the full harness/server/CLI/NFS/9P/Unstorage/chunked/distribution gates. The first rerun correctly exposed a stale local native artifact missing the new exports; PGlite/R2 and native-mount opt-ins remain explicit skips.

- [x] Current-tree W01/PGlite verification on 2026-09-21 passed: `cargo test --workspace --all-targets --all-features --locked --offline` exited 0; `MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX sh scripts/test-pglite.sh` exited 0 with real provider parity, reconnect, version history, SQLite VFS reconnect, N-API/chunked/FUSE rows, Rust SDK 6 pass/3 R2 skips, Node SDK 5 pass/1 R2 skip, CLI 11 pass/1 R2 skip, and 40/40 PGlite-inclusive 621-operation trace lanes. The upstream suite reported 1,200 passed/82 skipped; skipped behavior and hosted platform/native gates remain open.

## W02 — Independent metadata, blocks and chunking

- [x] Land metadata/block contracts, fixed-size chunking, immutable blocks,
  fenced publication and ordered durability barriers.
- [x] Exercise local mixed-provider compositions.
- [x] `d6b80f4` persists chunker configuration/version metadata and covers partial
  writes, truncation, close/reopen and stale publication in the chunked store;
  the focused chunked suite passed 12/12 plus 7/7 concurrency cases locally.
- [ ] W02.1 Complete live mixed-provider matrix, including Node and CLI paths.
- [ ] W02.2 Verify stale writers, CAS conflicts, partial uploads, retry ambiguity,
  crash/reopen and provider capability failures across remote combinations.
- [ ] W02.3 Document format/version migration and chunk-size compatibility.
- [ ] W02.4 Define safe orphan cleanup and garbage collection before enabling it.
- [ ] W02.5 Preserve an extensible chunker interface. Fixed-size is initial scope;
  additional algorithms need explicit implementation and compatibility tests.

## W03 — Memory and SQLite persistence

- [x] The standalone SQLite reliability packet passes 8/8 deterministic cells
  across DELETE/WAL persistence reopen, supported single-writer contention,
  ENOSPC block-put injection, post-publish unknown-commit recovery, FULL
  synchronization and `integrity_check`.
- [x] Land memory and SQLite filesystem, metadata and block integrations.
- [x] Local provider run passed five memory and four SQLite library tests.
- [x] Local draft SQLite schema reopening fix uses explicit INSERT columns;
  this and additive versioning changes are **not yet committed**.
- [ ] W03.1 Review and land additive version storage without regressing old stores.
- [ ] W03.2 Test migrations, concurrent open, reopen, transaction rollback and
  uncertain commit behavior with revision-matched evidence.
- [ ] W03.3 Keep SQLite backend durability distinct from SQLite-hosting safety.

## W04 — PGlite

- [x] Land real socket-server integration and provider/factory coverage.
- [x] Fix transaction cleanup before releasing a connection slot (`29ffb3b`).
- [x] Add injected cleanup-failure regression (`cfce82a`): failed cleanup retains
  ownership and rejects unsafe reconnect rather than reusing the slot.
- [x] W04.1 Full `scripts/test-pglite.sh` run completed with exit 0. Passed stages
  include slot cleanup, injected failure, provider parity, reconnect, fencing,
  cancellation, disk restart, mixed stores, Node factories and userspace FUSE.
  Upstream suite passed with skips; all eight trace lanes passed five seeds of
  621 operations each. This local run does not replace hosted/live-R2 evidence.
- [x] Fresh current-tree rerun passed the same real-server lifecycle and then
  reported Rust SDK 4/4, Node SDK 5/5, CLI 6/6, upstream 1,194 passed/88
  skipped, and 40/40 PGlite-inclusive trace lanes. R2-only rows remained
  explicit skips without credentials.
- [x] The bounded test-server teardown race is covered by an exact PostgreSQL
  Terminate-frame cleanup path plus an I/O-turn barrier. The readiness slot test
  passed 10/10 and bounded close/reopen passed 5/5; the full PGlite and root
  gates then passed without the prior `Eio` reconnect failure.
- [ ] W04.2 Confirm hosted macOS/Linux reruns close the previous reconnect failure.
  The latest published-revision run [35587575994](https://github.com/andymac4182/mount-rs/actions/runs/35587575994)
  is running on `bdfcb11`: Linux Node job
  [106294483397](https://github.com/andymac4182/mount-rs/actions/runs/35587575994/job/106294483397),
  macOS-latest Node job
  [106294483486](https://github.com/andymac4182/mount-rs/actions/runs/35587575994/job/106294483486),
  and macOS-15-intel Node job
  [106294483435](https://github.com/andymac4182/mount-rs/actions/runs/35587575994/job/106294483435)
  have started. The historical run [35560240894](https://github.com/andymac4182/mount-rs/actions/runs/35560240894)
  is not closure evidence: macOS-latest passed its PGlite step, but
  macOS-15-intel failed earlier in `test-http-early-rejection.mjs` with
  `EPIPE`; the published `bdfcb11` fixture-shutdown fix is being requalified.
  Close W04.2 only after all three fresh Node jobs complete successfully and
  their exact `Verify PGlite integration and restart recovery` logs pass;
  queued, skipped, cancelled, partial, or pre-fix evidence does not count.
  Production rollout remains separately tracked in
  [`docs/w04-progress-ledger.md`](docs/w04-progress-ledger.md) and is NO-GO
  until its artifact, persistence/rollback, provider, and operational gates
  also close.
- [x] W04.3 Integrate versioning, mount-free VFS and native SQLite-hosting tests.
  The rebased packet (`43ded00`, `980cdd7`, `2d2ac5c`, `be2170b`, final
  rebased tip `7235fde`) adds durable PGlite version metadata, reconnect and
  version-history coverage, mount-free SQLite VFS tests and the PGlite gate;
  it was published sequentially through remote `90c33949`. Hosted
  macOS/Linux reconnect reruns remain the separate W04.2 gate.

## W05 — Cloudflare R2

- [x] Land object-store/R2 driver code and configurable endpoint support.
- [x] W05.1 Unblock D01 and run authenticated tests against actual Cloudflare R2.
  On 2026-09-20, main passed the explicit live filesystem contract test and
  SQLite-metadata/R2-chunk roundtrip/reopen test (two tests, no skips). The
  current CI credential path is the security-provisioned encrypted GitHub
  `r2-ci` environment, not a repository file or direct Keychain dependency;
  the final hosted acceptance is recorded in W05.7.
- [x] W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup and
  concurrent publication with independently selected metadata providers.
  The 2026-09-21 isolated rerun at base revision `6f1ab93` passed the local
  signed-HTTP R2 adapter contract (`10/10` unit tests and `1/1` HTTP test):
  immutable create, ranged reads, stale conditional read/write rejection,
  fresh-client reopen, eight concurrent publications, prefix isolation and
  exact cleanup. The provider matrix reported `5` local Rust SDK passes,
  `4` skips and `0` failures; its R2 and PGlite rows skipped because
  `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`,
  `R2_SECRET_ACCESS_KEY`, and `PGLITE_DATABASE_URL` were unset. This is
  local S3-compatible evidence, not live Cloudflare R2 evidence; no dedicated
  transient-failure retry injection was run, so that isolated rerun did not
  close W05.2.
  The current live rerun passed both ignored Cloudflare R2 filesystem/CAS
  tests (`2/2`), the independent SQLite-metadata/R2-block composition
  (`1/1`), and the independent PGlite-metadata/R2-block composition (`1/1`),
  all with unique fixtures and exact cleanup. The signed-HTTP adapter test now
  injects one transient read failure and passed retry plus exact prefix cleanup
  (`2/2` HTTP tests); this is deterministic S3-compatible retry evidence,
  while the provider-side live R2 tests cover the remaining durability and CAS
  paths.
- [x] W05.3 Run Node, CLI/native, parity and benchmark lanes on live R2.
  Main executed actual R2 differential traces with seeds 4182, 1, 42, 65535,
  and 4294967295: 621 operations each, all 3,105 matched the pinned TypeScript
  oracle, with per-run snapshot cleanup. On 2026-09-20, the live N-API R2
  factory passed exact-key DELETE/HEAD cleanup, the configuration-driven HTTP
  CLI passed both split-store drives and owned-prefix cleanup, and the
  ComputeSDK-aligned smoke benchmark passed a 1 MiB fixed-64 KiB chunked
  PGlite/R2 run (write 2,902.11 ms, read 1,657.28 ms, 5.06 MiB/s, delete
  8.51 ms; one iteration, zero timeouts/failures). Native/hosted lanes and the
  full benchmark matrix remain open. The full `scripts/test-all.sh` rerun at
  `73c33e0` also passed the live R2 lane end-to-end; this does not close the
  native/hosted portions of this task.
  The 2026-09-21 isolated rerun reported `4` Node SDK passes, `2` skips and
  `0` failures; `9` CLI passes, `2` skips and `0` failures; and the storage
  benchmark unit gate passed. The Node R2 factory and PGlite rows skipped for
  missing configuration. The live Cloudflare CLI and service-evidence scripts
  stopped at credential preflight with `R2_ENDPOINT` unset; no remote request,
  write or cleanup ran. The full N-API suite reached native server checks only
  after elevated host access, then stopped on a stale local native binding
  (`binding[nativeName] is not a function`) while R2 remained an explicit
  credential skip. That isolated rerun therefore did not close W05.3.
  The current live rerun passed the complete Node/N-API suite with the pinned
  oracle, live R2 and isolated PGlite; the explicit macOS native NFS lane
  passed mounted read/write and teardown; the five-seed live R2 trace passed
  all `3,105/3,105` operations; the configuration-driven live R2 CLI passed;
  and the full public-NAPI benchmark passed all `8/8` iterations across 1,
  4, 10 and 16 MiB with fixed 64 KiB chunks, zero timeouts/failures and
  verified cleanup. Hosted CI receives only the encrypted `r2-ci` environment
  secrets; its live-R2 evidence remains a separate hosted acceptance boundary
  from local and native evidence.
- [x] W05.4 Record service identity and revision without recording credentials.
  `integrations/mount-rs-r2` now exposes a redacted `R2ServiceIdentity` and
  `scripts/r2-service-evidence.sh` records only endpoint authority, bucket,
  revision and owned-prefix object counts; embedded endpoint credentials are
  rejected. The focused R2 unit/Clippy gates and shell syntax check passed.
- [x] W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare
  R2 endpoint with scoped S3 credentials. On 2026-09-20, the live gate passed
  both PGlite-metadata/R2-block and SQLite-metadata/R2-block drives, ranged
  reads, auth isolation, graceful reopen, object-presence checks and owned
  prefix cleanup. The runner now counts returned `Contents` because R2 may
  omit AWS's optional `KeyCount`; it does not weaken the cleanup gate.
- [x] W05.5 Fix live Node factory expected-byte assertion and guarantee unique
  cloud fixture keys with exact cleanup. Main reran the full PGlite/R2 script
  successfully: actual R2 Node factory and DELETE/HEAD cleanup, independent
  PGlite metadata + R2 blocks, provider lifecycle/restart/fencing, Node chunked
  factories and userspace FUSE. Upstream: 1,194 passed, 88 skipped; eight seeded
  lanes × five seeds × 621 operations passed. The object-store trace lanes are
  local, not live R2 traces. The subsequent full acceptance rerun passed with
  live R2 and PGlite enabled, while native privileged mounts and hosted CI remain
  separate evidence boundaries.
- [x] W05.7 Add usage-capped CI credentials, cost guard, and full hosted
  acceptance. Security-provisioned bucket-scoped Object Read & Write
  credentials are stored only as encrypted GitHub `r2-ci` environment secrets;
  the one-week token expires 2026-09-28 and no credential value is committed.
  The fail-closed budget job admits at most 20 accepted runs per UTC month at
  an assumed `$4` per run, for an `$80` envelope below the requested `$100`
  ceiling; the final run `35579757447` recorded `12/20` with eight slots
  remaining. A Cloudflare account-wide R2 alert is configured at `$80` as an
  early-warning notification. Hosted acceptance passed at `3db491e`: Rust SDK
  `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`,
  five seeds × eight local/PGlite backends × 621 operations, live R2 trace
  `621/621`, live CLI, hosted N-API/service evidence, and artifact
  `10630468958`. An earlier hosted `ESTALE` shutdown failure in run
  `35575940720` was fixed by `c71c8ee`, which refreshes an expired unfenced
  lease during shutdown and includes a regression test; runs `35577687152` and
  `35579757447` then passed. Remaining actions are secret rotation before
  expiry and separate Linux FUSE, Windows, and signed/activated FSKit gates.

## W06 — RustFS integration service

- [x] Land isolated real-service harness and Linux CI job (`f5759cc`).
- [x] Main verified actual pinned RustFS contract tests, concurrent CAS, ranges,
  restart/fresh reads and guarded container/data cleanup; baseline run passed.
- [x] W06.1 Land real split SQLite/PGlite metadata and Node-factory gates,
  bounded Docker/process-group cleanup, and isolated combo orchestration.
  Main reran both timeout regressions and the full real-service harness after
  the final script changes: exit 0, block/split-provider/Node/restart gates
  passed. CLI integration beyond existing configuration tests remains open.
- [x] W06.2 Verify the hosted RustFS CI job, not just its configuration.
  Hosted runs `35493800880` / `35493696795` passed block/restart tests but
  failed cleanup of container-owned `.rustfs.sys` bind-mount files with permission
  denied. Ownership-validated cleanup is implemented and locally passed;
  hosted Linux confirmation passed at `37e9ba1`, job `106049192404`.
- [ ] W06.3 Add fault and benchmark workloads with reproducible service settings.
- [ ] W06.4 Provide isolated RustFS service orchestration for W07.6 and W08.5;
  test actual composed filesystems rather than unrelated backend smoke tests.

## W07 — FoundationDB

- [x] W07.1 Review and register the FoundationDB integration crate and its
  provider/test harness as a target-gated root workspace member. The feature-off
  package remains portable for client-free workspace and Windows all-features
  checks; native FDB compilation remains opt-in on supported targets.
- [x] W07.2 Finish isolated real FoundationDB client/server harness. Main ran
  the real pinned 7.4.7 Linux ARM64 server/client in Docker; the provider
  contract passed. The container supplies `fdb_c`; no host install or mock.
- [ ] W07.3 Resolve production lease/time semantics: default unsupported clock
  behavior and a development clock do not establish safe distributed fencing.
  The safety slice adds `with_production_lease_oracle` plus the
  `LeaseAuthorityKind::SharedProvider` declaration gate: unverified,
  development and single-authority clocks fail closed. The provider now also
  exposes a FoundationDB-hosted write-side `FoundationDbLeaseAuthority` and a
  read-only `FoundationDbSharedLeaseOracle`; the real-cluster integration test
  exercises two independent readers, missing-authority fail-closed behavior,
  backward-sample clamping, forward recovery and stale-writer fencing. Hosted
  runtime evidence now also exercises the process-owned authority, reader and
  storage `connect` paths. The hosted native CLI lane now publishes a current
  authority sample in a separate process before running the consumer with the
  shared-provider/read-only configuration. Deployment-level authority
  credential/clock-skew controls are now documented as an explicit deployment
  contract, but their enforcement and hosted runtime evidence remain pending,
  so this item is not yet marked complete.
- [x] W07.4 Add conservative transaction/block limits, CAS, stale-writer and
  deterministic lease-fencing checks. Provider restart and hosted identity remain
  separate acceptance work.
- [ ] W07.5 Add Node, CLI, native-mount and macOS/Linux acceptance coverage. Rust
  SDK/CLI and the Node chunked factory now expose FoundationDB selection behind
  opt-in native features with explicit persisted single-authority/test and
  protected shared-provider modes. The latter requires an authority prefix and
  uses a read-only consumer API; deployment credentials must still enforce the
  provider-time publication boundary.
  The hosted FDB lane now builds the feature-enabled addon in the pinned
  libfdb_c client image and runs the live Node chunked factory against the real
  cluster, composing RustFS blocks when that lane is active. The same lane now
  has an explicit Linux FUSE prerequisite and runs the config-driven CLI native
  lifecycle/reopen test inside the client container with a separately
  published shared-provider authority when `/dev/fuse` is available; the script
  selects only that FoundationDB test so unrelated ignored FUSE cases cannot
  fail the lane. The live
  Node gate uses that same published authority prefix. The macOS native-NFS job
  now also compiles the FoundationDB-enabled CLI lifecycle test; live macOS
  service/cluster acceptance and the hosted result remain open.
- [ ] W07.6 **FoundationDB metadata + RustFS S3 chunks:** main passed the real-service
  composition and provider contract in the full RustFS harness (exit 0), with
  multi-chunk round trips, fresh-client reopen, CAS and expired-writer fencing.
  The surrounding RustFS/PGlite VFS restart checks also passed, but are not
  FoundationDB service-restart evidence. The hosted CI lane and Docker harness
  now run the consumer feature checks, shared-provider authority publication,
  owned FoundationDB service restart, a separate post-restart authority
  republish, and fresh-client RustFS reopen; hosted result remains pending until
  CI runs. No emulated acceptance.
- [x] W07.6a The bounded mixed-provider packet also verifies exact owned-prefix
  cleanup: every tracked block is absent after cleanup while sibling and parent
  sentinel objects remain untouched. This does not close the W07.6 service-
  restart or hosted-composition boundaries above. The
  published `629c2f6` packet adds an owned FoundationDB restart/readiness gate,
  fresh-client RustFS reopen/CAS/fencing checks and fail-closed external-FDB
  handling; its real runtime lane remains blocked by host `libfdb_c` and Docker.
- [ ] W07.7 **Production rollout readiness and go/no-go:** the demo and local
  Docker evidence are not production acceptance. Before enabling any production
  consumer, close every gate below with a linked revision, test/run result,
  environment identity and accountable owner:
  - [ ] **Identity and least privilege:** document and deploy one
    write-capable authority identity per authority prefix, read-only consumer
    identities, secret injection/rotation and no shared credentials. Prove
    with the actual production credential/tenant/ACL policy that a consumer
    cannot publish or overwrite the authority record.
  - [ ] **Time and authority failure policy:** enforce the authority-host
    clock-skew bound and alerting; publish more frequently than the shortest
    lease TTL; republish after authority restart before admitting readers; fail
    closed during authority loss; and test the reviewed failover and recovery
    procedure.
  - [ ] **FoundationDB durability and recovery:** prove the production
    cluster's replication/storage policy, backup/restore, service
    restart/failover, keyspace/version compatibility and a recovery drill.
    The single-node pinned Docker harness is not this evidence.
  - [ ] **Production-like workload and soak:** run multi-chunk reads/writes,
    partial writes, truncate/extend, concurrent publication, stale-writer
    fencing, maybe-committed reconciliation, lease renewal/expiry, reconnect
    and fresh-client reopen at production-like duration and load. Record
    latency, retry, capacity and error-budget results.
  - [ ] **Observability and operations:** expose and alert on cluster health,
    authority publication age/errors, reader failures, lease-fence/ESTALE,
    transaction retries/maybe-committed EIO and cleanup/space pressure.
    Publish the dashboards, on-call runbook, escalation thresholds and
    incident/recovery ownership.
  - [ ] **Rollout and rollback:** stage a canary with a holdback, define
    go/no-go and abort criteria, verify backward/forward compatibility of the
    keyspace and configuration, rehearse rollback/authority recovery and
    record owner sign-off.
  - [ ] **Hosted and platform evidence:** obtain green hosted
    FoundationDB/RustFS, Node, CLI/native Linux and macOS/Linux build/native
    acceptance runs. Record the actual runner, cluster/image, revision and
    result; failed, skipped, cancelled or unavailable evidence remains open.

  W07.7 remains open until every nested gate has concrete production-like
  evidence. No demo, local qualification, queued CI run or installation-only
  result may be promoted to a production PASS.
  The production gate ledger, deployment contract and rollout sequence are
  tracked in [`docs/foundationdb-production-rollout.md`](docs/foundationdb-production-rollout.md).

## W08 — TiDB

- [x] W08.1 Review and land the separate TiDB metadata/block implementation and
  dependencies (`ca57758`); four unit tests, format checks and test binaries
  passed.
- [x] W08.2 Complete the durable real TiDB/PD/TiKV Docker harness and restart
  results. Hosted CI run `35585066458` at source `9c098e5`, `tidb` job
  `106286459436`, passed the pinned v8.5.7 durable 3PD/3TiKV topology,
  frontend replacement, TiKV restart, PD restart, readiness checks and the
  post-restart persisted provider rerun. The acceptance marker was
  `evidence=durable-multinode-restart` on `linux/amd64` with 4 CPUs and
  16,766,414,848 bytes of Docker memory. The local host remains capacity-gated;
  hosted evidence is the authoritative service result.
- [x] W08.3 Verify provider time/fencing, ambiguous commits, concurrency and
  deployment durability assumptions. Liveness queries are not fsync evidence.
  The same hosted durable run passed provider-clock fencing/concurrency,
  ambiguous-commit no-replay, schema/identity, frontend/store/PD restart and
  persisted fresh-client checks. The ambiguous-commit injection is ordered
  after the durable restart gate so its intentionally unknown client outcome
  cannot contaminate restart readiness; it remains a distinct backend outcome.
- [x] W08.4 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
  Run `35585066458` passed the live Linux TiDB/RustFS consumer composition,
  ARM Node consumers, Linux FUSE, and both Ubuntu and macOS native-NFS jobs.
- [x] W08.4a The bounded Node/CLI consumer slice is wired through the public
  Rust SDK, N-API and Rust/Node CLI configuration: TiDB metadata can compose
  with RustFS/S3-compatible `r2` chunks, with explicit `durable` assertions.
  The provider matrix covers configuration, partial write, truncate, shutdown,
  reopen and owned RustFS-prefix cleanup. Local evidence: `cargo fmt --all -- --check`,
  focused Clippy, Rust SDK `2 passed`, CLI `40 library + 9 CLI
  tests passed`, N-API chunked/CLI/shutdown tests passed, Node matrix
  `pass=4 skip=3 fail=0`, and CLI matrix `pass=10 skip=3 fail=0`. The live
  TiDB+RustFS rows are explicit skips because `MOUNT_RS_TIDB_URL` and the
  loopback RustFS credential set are absent here. The consumer cleanup row
  removes only owned RustFS objects and verifies provider shutdown; exact TiDB
  metadata-row deletion remains the W08.5 service-harness boundary.
- [x] W08.4b Native-mount and live TiDB/RustFS consumer acceptance are
  complete for the defined hosted matrix. The `tidb-rustfs` job
  `106286459715` passed live TiDB/RustFS Node and CLI matrices, independent
  Rust/Node mounted I/O through Linux FUSE, clean unmount, fresh-provider
  readback and retained client bytes. The standalone `native-fuse` job
  `106286459998`, ARM Node job `106286459639`, Ubuntu native-NFS job
  `106286459483`, and macOS native-NFS job `106286459246` also passed. The
  macOS native-NFS row is a native-platform lifecycle gate, not a claim that
  TiDB/RustFS itself ran on macOS; the live provider row is the Linux FUSE
  composition job.
- [x] W08.5 **TiDB metadata + RustFS S3 chunks:** the real single-node v8.5.7
  TiDB service and pinned loopback RustFS endpoint passed the mixed-provider
  seed, partial-write, truncate, reopen, CAS/fencing and exact cleanup path;
  the hosted durable composition additionally passed the RustFS restart/reopen
  phases. Persisted fixtures require explicit volume/prefix/manifest scope and
  reject transient or symlink paths.

### W08 production rollout track — NO-GO (15% provisional)

The demo and W08 functional acceptance are not production approval. Track the
following gates separately from implementation, hosted provider, and native
platform evidence; the detailed ledger and evidence boundaries are in
`docs/W08-progress-ledger.md`. No production gate is checked until its exit
evidence is terminal, owned and reproducible in a production-like environment.

- [ ] **W08-P01 (20%) — deployment contract/topology:** choose and document the
  managed or self-hosted TiDB/PD/TiKV and object-storage architecture, HA,
  regions, TLS/network policy, resource limits, versions, tenancy and IaC;
  prove a staging deployment and smoke/restart gate. The hosted
  `tidb-tls-compile` job `106298487587` in run `35588858142` confirms that the
  public consumers can include the TLS client graph, but not a deployment or
  handshake. *(Implementation + hosted/provider; target platform not supplied.)*
- [ ] **W08-P02 (15%) — secrets/IAM/rotation:** bind production credentials
  through the approved secret manager; prove least privilege, rotation,
  revocation, audit and redaction without data loss. *(Implementation +
  provider; secret manager and IAM owner are external.)*
- [ ] **W08-P03 (10%) — backup/restore/DR:** define RPO/RTO and retention;
  configure backups/versioning and complete an isolated restore, corruption and
  region-loss/recovery drill with metadata/block consistency evidence. *(Hosted
  provider; backup environment and second region are external.)*
- [ ] **W08-P04 (10%) — upgrade/compatibility/rollback:** rehearse the
  supported TiDB/RustFS/client upgrade matrix, schema/config migration,
  interrupted upgrade recovery and package/provider rollback. *(Hosted
  provider + implementation; target versions and maintenance window are open.)*
- [ ] **W08-P05 (15%) — observability/SLO/alerting:** define SLOs and error
  budgets; configure metrics/logs/traces, health checks, dashboards, paging,
  retention and redaction; exercise an alert end to end. *(Implementation +
  hosted/provider; collector and on-call route are not configured.)*
- [ ] **W08-P06 (10%) — capacity/load/soak:** run representative baseline,
  peak, saturation, failover and multi-hour soak workloads; record latency,
  throughput, errors, headroom and scaling limits. *(Hosted/provider +
  implementation; workload and production-sized capacity are open.)*
- [ ] **W08-P07 (15%) — security/hardening:** enforce TLS/certificate
  rotation, network segmentation, authz/tenant isolation; complete dependency,
  image and SBOM scanning, threat-model review and security sign-off. *(Provider
  + implementation; hosted TLS compile and guard job `106301529923` in run
  `35589825356` passed, while the actual endpoint, certificates, security
  approval and network controls are external.
  `scripts/test-tidb-tls.sh` now provides the guarded credentialed provider
  gate and rejects missing TLS/CA/hostname verification before connecting.)*
- [ ] **W08-P08 (15%) — failure drills/runbooks/on-call:** exercise client and
  provider loss, stale leases, partitions, partial writes, rolling restart and
  restore; publish operator runbooks and complete an on-call tabletop/timed
  drill. *(Hosted/provider + operations; named operators and incident tooling
  are open.)*
- [ ] **W08-P09 (10%) — release/canary/go-no-go:** produce immutable signed
  artifacts and SBOM, verify target-platform packages, run a staged canary with
  live SLO telemetry, rehearse rollback and record explicit approval. *(Release
  implementation + hosted; registry, signing, deployment controller and
  approvers are external.)*

## W09 — napi-rs, Node API and packaging

- [x] Add public `createLoopback`/`resolveCapabilities` and associated types.
  Main passed pinned-oracle capability/binding/path/partial-I/O/error comparisons,
  TypeScript checks and package-content validation. This wrapper preserves caller
  driver/handle identity and does not own the caller's shutdown lifecycle.
- [x] Accept structural JavaScript drivers in mount/server factories and mixed
  S3 bucket maps, with owned-adapter cleanup and TypeScript declarations.
  Main independently passed the pinned-oracle structural factory suite and
  eight WebDAV DELETE status/survivor comparisons, plus 11 Rust WebDAV tests.
  Native structural mounts remain unverified; harness exports and remaining
  public API gaps are still open. These results do not qualify remote backends.
- [x] Migrate npm packages to `@mount-rs/core` and `@mount-rs/virtual-fs`,
  including loaders, dependencies, imports and distribution checks. No registry
  publication or namespace-ownership verification is implied.
- [x] Harden the scoped `@mount-rs/core` distribution metadata and generated
  platform-package loaders (`4fa908e`); aggregate, pack, loader and consumer
  checks pass. Publication and native artifact qualification remain open.
- [x] Land shared cancellation-safe shutdown, provider-reference release ordering,
  panic/error retry and Windows numeric flags. Main passed 12 native binding
  tests, strict Clippy, rebuilt-addon/oracle-enabled Node suite, consumer tests
  and TypeScript checks. R2/PGlite/native-mount opt-ins were skipped in this
  package run; hosted Windows verification remains open. Independent handles,
  servers and in-flight operations must finish before backing-file deletion.
- [x] Land JavaScript driver bridge, codec/server hooks and 9P ESM exports.
- [x] Fix shutdown test's early rejection handling (`8d1f4ad`); 25 strict repeats
  passed locally. This corrects the test race, not a proven runtime defect.
- [x] Add server-test phase/cleanup diagnostics (`2452463`); 20 strict main
  repeats passed. Earlier intermittent timeout cause is not established.
- [x] Full local Node suite passed after fixes, excluding opt-in service/native
  lanes; opt-in skips are not acceptance evidence.
- [x] `57632c6` + `e213856` add structural-driver adapter and opt-in native
  lifecycle acceptance; read/callback/unmount cleanup passed on macOS NFS.
  Structural native write support remains explicitly bounded by missing decoded
  `open_flags`.
- [x] `3042d09` exposes Rust-backed FUSE inode state through the N-API package;
  the Rust inode table, Node parity test, generated declarations and distribution
  checks passed locally.
- [ ] W09.1 Close remaining public exports, factories and declaration parity gaps.
- [ ] W09.2 Investigate any repeated server timeout using the new diagnostics.
- [ ] W09.3 Expose new providers, versioning, VFS and HTTP through tested APIs.
- [ ] W09.4 Verify platform artifacts, package install and native loading on both
  operating systems; keep package publication behind D06.

## W10 — Filesystem and protocol transports

- [x] Land FUSE, NFS, 9P, WebDAV and S3 transport implementations and tests.
- [x] `1af1986` + `8ac77f3` + `9215ba3` retain held NFSv3/v4 backend handles
  across unlink/rename; 266 pinned oracle cases passed through the supported
  Vitest runner, with the TypeScript control's stateless handle limitation kept
  explicit.
- [x] Land HTTP early-rejection regression (`aa44413`).
- [x] Land incremental S3 chunked codec helpers (`e218d18`); all 27 S3 tests and
  strict Clippy passed locally.
- [ ] W10.1 Finish per-transport backend/platform acceptance matrix, including
  native lifecycle, disconnect/error behavior and streaming/backpressure.
- [ ] W10.2 Verify transport auto-selection and explicit unsupported behavior.
- [ ] W10.3 Ensure all protocol paths operate on the same configured drive state.

## W11 — Config-driven CLI

- [x] Land strict configuration and CLI (`f20f1e3`).
- [x] Land actual macOS NFS host-backed mount/IO/unmount lifecycle and CI gate
  (`c4058f2`); main native test and strict Clippy passed.
- [x] W11.1 Review SQLite-backed native lifecycle and UID/GID config; main's
  33 unit + seven integration tests and formatting passed. Landing with this update.
- [x] W11.2 Fix review finding: explicit ownership mismatch on read-only config
  must not mutate persisted root ownership.
- [x] W11.3 Fix review finding: an existing `0:0` root is not proof of a new
  filesystem. Preserve existing ownership; avoid check-then-create races.
- [x] W11.4 Main ran actual macOS config→NFS mount→SQLite workload→unmount→
  fresh-process reopen: PASS, SQLite 3.51.0, DELETE journal, synchronous FULL.
  Process locking/recovery passed; mount-service crash was not tested here.
- [ ] W11.5 Extend configuration and lifecycle coverage to all new providers,
  version views, FSKit and multi-drive HTTP without silently falling back.
- [x] Main passed the checked-in HTTP config as a real local demo after
  `4af2a30`: memory and split SQLite drives served, bearer isolation returned
  401, and split SQLite data survived a service restart. This is local
  evidence only and does not qualify live R2 or native-mount acceptance.
- [x] `d526c18` extends the HTTP subprocess contract with streamed writes,
  ranges, truncate, concurrent writes, aborted-write recovery and listener
  cleanup; the focused CLI/HTTP tests and strict Clippy passed locally.
- [x] `0d7f1f4` adds a bounded native demo using the Rust CLI, independent Rust
  and Node clients, real macOS NFS, SIGINT unmount and durable backing-byte
  verification.
- [x] `0dca1d1` adds the Node SDK CLI example and default mount-free checks;
  the opt-in macOS NFS self-test passed against the rebuilt local N-API addon.
- [x] `29337f7` adds direct Node SDK read/write self-test coverage and routes the
  Rust CLI through `mount-rs-sdk`; both CLI entry points are now usable examples
  of the public SDKs and are included in the provider/consumer matrix.
- [x] `946a7cb` fixes the Windows Node CLI integration test to use the platform
  temp directory; the local check passes and the hosted Windows rerun is queued.
- [x] `21803fd` adds the Node CLI's versioned provider config, SDK-only reopen
  self-test and PGlite integration-matrix row. The PGlite config-to-SDK path
  passed through clean shutdown/reopen; memory is correctly rejected for
  `--reopen` because it is process-local.
- [x] `fd5eb04` makes the Node structural-driver lifecycle exercise real
  mounted writes and readback on the authorized macOS NFS path.
- [ ] W11.6 Run the same SDK-backed CLI flow against the configured metadata/
  block providers, including restart and cleanup, before treating the demo as a
  provider-integrated acceptance path. The local PGlite path is covered; live
  R2, native configured-provider mounts, and hosted platform runs remain open.

## W12 — Safely host SQLite database files

- [x] Record prior Linux FUSE DELETE/WAL and macOS NFS single-host DELETE
  evidence; those configurations alone do not establish universal safety.
- [ ] W12.1 Define and test supported journal/locking modes per transport.
  The local VFS checkpoint covers the required rollback matrix for
  DELETE/TRUNCATE/PERSIST × NORMAL/FULL/EXTRA and explicit HostLocal/ProcessLocal
  WAL cases; MEMORY/OFF, all transports and hosted platform coverage remain open.
- [x] W12.2 Prevent silent WAL fallback from being reported as WAL success.
  `5d9e513` asserts effective WAL for supported HostLocal scope and rejects
  unsupported WAL requests instead of silently falling back.
- [ ] W12.3 Exercise multiple connections/processes, readers/writers, locks,
  sync barriers, rename/unlink, disk-full errors and crash/restart integrity.
  The checkpoint covers process/child-process locking, reader/writer snapshots,
  sync-fault recovery, checkpoint/reopen and integrity; rename/unlink,
  disk-full and hosted crash coverage remain open.
- [ ] W12.4 Run integrity checks and acknowledged-commit recovery across every
  supported metadata/block combination and operating system.
- [x] W12.5 Explicitly reject unsupported safety modes and document constraints
  for WAL capability scope in `docs/sqlite-vfs-wal-plan.md` (`5d9e513`).
- [ ] W12.6 Run the new Linux CLI SQLite-backed FUSE SIGKILL/reopen test in
  hosted CI. Implementation and bounded cleanup are added; main independently
  passed both shared macOS NFS lifecycle regressions, formatting and Clippy.
  Linux execution is not yet verified on this macOS host.
- [x] `d6b80f4` adds local SQLite VFS fail-closed behavior after injected block or
  metadata-publication failures, plus the focused unit/engine/bridge gates. This
  is a local reliability boundary, not hosted cross-platform acceptance.

## W13 — FSKit

- [x] Land unsigned SDK compile target (`5993984`); main arm64 compile passed,
  worker reported x64 compile. Neither is a mounted-filesystem result.
- [x] W13.1 Implement FSVolume operations/read-write and tested Swift/Rust IPC
  (`95aca9c`); Rust bridge tests, Swift frame tests, XPC lifecycle tests, and
  unsigned arm64 Xcode targets passed locally.
- [ ] W13.2 Verify errors, handles, identity, concurrency and lifecycle at the seam.
  The FSKit path-resource bridge now maps macOS 26+ `FSPathURLResource` values
  to rooted `HostFs` workers and its direct host-path lifecycle/reopen test
  passes; installed-volume concurrency and cross-mount visibility remain open.
- [ ] W13.3 Complete packaging, entitlements and signing plan, then request D03.
  A minimal `MountRsHost` containing app now embeds the FSKit appex in
  `Contents/Extensions` and builds unsigned; Apple team/profile authorization
  and activation remain open.
- [x] `fdd7c81` adds the strict macOS activation gate and publishes it through `ea9ac68`: Rust/Swift/Xcode/embedded-bundle checks pass, while the real activation probe correctly reports `FSKIT_ACTIVATION=FAIL` because the host has zero valid Apple signing identities, only ad-hoc signing, and no installed/enabled `mount-rs` FSClient entry. This proves the blocker and does not close W13.4.
- [ ] W13.4 Activate and test real FSKit mounts, CLI integration, persistence and
  supported SQLite workloads. NFS/FUSE fallback does not satisfy this stream.

## W14 — Versioned filesystems

- [x] Land additive core versioning contract, separate coordinator crate and
  memory/SQLite history providers. Includes explicit publication identities,
  lost-ack fork reconciliation, pinned read-only views, restore, cross-store
  copying, schema/head validation and cancellation-safe pin accounting.
  Main tested the exact staged checkout: 6 memory, 10 SQLite and 20 versioning
  tests passed. This is not remote/Node/CLI/platform acceptance or physical COW;
  dropped views may retain pins until TTL unless explicitly closed.

- [x] Record versioning design (`dc6b0bb`).
- [x] Local uncommitted prototype passed four tests covering history, restore,
  fork, pins/retention, SQLite reopen and operation replay after head moves.
- [ ] W14.1 Review and land version IDs, publication, pinned views, history,
  restore/fork, retention and explicit provider capabilities.
- [ ] W14.2 Fix pin lifetime: an open historical handle must retain protection or
  be invalidated when its view closes; do not allow reads after protection lapses.
- [ ] W14.3 Validate expiry before/after reads and during slow in-flight reads;
  protect immutable public records and test close/read/delete races.
  Worker checkpoint reports pin-lifetime fixes, seven versioning tests and
  strict Clippy passing; main review and independent verification remain open.
- [ ] W14.4 Verify replay payload matching, CAS conflicts, cross-store forks,
  crash/reopen, block reachability and safe retention/GC boundaries.
- [ ] W14.5 Add real remote-provider, Node and CLI coverage; clearly label
  unsupported capabilities and database-quiescence requirements.
- [ ] W14.6 Keep full-copy snapshots distinct from future physical COW (W23).

## W15 — Mount-free SQLite VFS

- [x] Extend the same remote VFS fixture through a graceful PGlite process
  stop/start using its persisted data directory and a fresh listening port.
  Main's full local harness passed exact ledger and integrity checks after
  both RustFS and PGlite restarts, with fresh VFS test processes at each phase.
  Clean shutdown status is required. Abrupt process loss, power loss, WAL and
  hosted results remain separate unverified gates.
- [x] Actual RustFS service stop/start now has its own VFS fixture: separate
  prepare/reopen test processes share the persisted metadata/block prefix,
  verify two exact committed binary rows, exclusion of a rolled-back row,
  and `integrity_check = ok`. Full local RustFS harness passed both phases.
  PGlite itself was not restarted; this is not a power-loss or WAL result.
- [x] Live PGlite metadata + RustFS blocks VFS test passed locally: real SQLite
  transaction, exact binary bytes and integrity after reconnecting both clients.
  Added it to the standard RustFS harness instead of leaving it opt-in only.
  The surrounding RustFS suite also passed its service-restart check, but that
  fixture is separate: VFS-specific service restart and PGlite restart remain
  unverified. No remote power-loss durability claim is made.
- [x] Root-integrate the separate rollback-journal VFS and metadata/block
  storage bridge. Main passed 22 local tests and strict all-feature Clippy;
  the remote PGlite/RustFS test remains ignored in this run. Host-backed engine
  tests cover DELETE/TRUNCATE/PERSIST x NORMAL/FULL/EXTRA with exact binary
  ledgers and reopen checks. Memory/SQLite storage bridges have separate engine,
  fencing, failure and subprocess checks. The follow-up now adds all nine
  rollback journal/synchronous combinations to each memory and durable SQLite
  provider pair (18 cells), with exact committed/rolled-back bytes and reopen.
  WAL is explicitly rejected and remains unimplemented; Windows qualification,
  remote recovery, Node exposure, and broader crash/fault coverage stay open.
- [x] W15.1 Complete the separate draft crate and actual mount-rs storage bridge
  (`5d9e513`); host-file and StorageBackend implementations are both tested.
- [x] W15.2 Fix registration lifetime escapes through connection extraction or
  mutable access; test duplicate names and independently opened connections.
  Explicit quiescent close releases provider resources, rejects active callbacks
  and file handles, and retains backend-free callback tombstones. Main added
  extracted-connection and closed-wrapper/name-reuse regressions and passed
  28 local tests. Reentrant backend destruction runs outside the registry lock.
  Two remote tests remain opt-in; these local results are not WAL acceptance.
- [x] W15.3 Replace noop-waker/busy polling with a valid executor contract and
  test a real wake path (`5d9e513`); no hidden busy/noop-waker success path is
  used by the process-local bridge.
- [x] W15.4 Implement cross-connection SQLite locking or explicit safe
  serialization with fencing for the supported HostLocal/ProcessLocal scopes;
  child-process contention and stale-reader promotion tests pass in `5d9e513`.
- [ ] W15.5 Test stale-reader promotion, separate processes, lost updates,
  short reads, sync failures, journal recovery and database integrity. The
  checkpoint covers stale-reader promotion, separate processes, sync failures,
  journal recovery and integrity; lost-update/short-read and broader hosted
  crash coverage remain open.
- [ ] W15.6 Expose and integrate through Rust and Node across storage engines
  without requiring a native mount; document supported journal modes.

## W16 — Mount-independent consumer adapters

- [x] W16.1 Implement separate just-bash and Mastra adapters over mount-rs drives.
- [x] W16.2 Main independently passed actual just-bash 3.4.2 and Mastra 1.67.0
  tests, strict TypeScript checking and package dry-run (11 intended files).
  Memory and SQLite reopen/shared Node API visibility are covered. Wired into
  test-all and four-platform Node CI; hosted results remain pending.
- [ ] W16.3 Verify binary/path/error behavior, readonly/versioned views, shared
  namespace visibility, persistence and lifecycle in consumer integration tests.
- [ ] W16.4 Reuse the drive abstraction with W17 without requiring OS mounts.
- [x] W16.5 Fix review findings before landing: reject recursive copy into a
  descendant (including symlink aliases), preserve source on same-object copy,
  and handle partial/zero-progress writes. Add targeted regressions.
- [x] W16.6 Document that waiting for admitted operations does not establish
  bounded cleanup if a backend request never resolves.
- [ ] W16.7 Add explicit backend cancellation/timeout semantics and remote-store,
  versioned-view and actual native-mount shared-visibility acceptance. The new
  `tests/native_shared_visibility.rs` records the macOS capability boundary
  without claiming that acceptance: the current FUSE API is Linux-only and the
  FSKit target has no signed/activated mounted-volume host. The real three-way
  test remains open until all three independent mount authorities and their
  shared-provider locking contract exist.

## W17 — Multi-drive HTTP server

- [x] Root-integrate the separate HTTP crate and shared lockfile. Real loopback
  clients exercise isolated memory/SQLite drives, bounded requests, streaming
  disconnect cleanup, and shutdown retries. Main passed 7 unit and 6 integration
  tests locally. Listener completion is cached under one mutex, with a
  deterministic cancellation-after-join regression. Hosted, Node/CLI, remote
  backend, and distributed-cache acceptance remain open.
- [x] W17.1 Implement a separately packaged server and drive registry/config;
  expose several named drives, including unmounted drives.
  CLI `serve-http --config` reuses provider factories with per-drive env-token
  references. Main tested the exact staged snapshot: 38 unit, 7 CLI, 2 real
  HTTP subprocess and 1 native-artifact tests passed; 2 native-mount tests
  remained opt-in/ignored. Tests cover memory/SQLite/split-store isolation,
  forced-process SQLite reopen, volatile-store loss, and Unix SIGINT shutdown.
  Portable HTTP coverage now runs on Windows CI; no Windows runtime pass yet.
- [x] Add mandatory real CLI HTTP gate to the RustFS harness: PGlite metadata
  and SQLite metadata each use actual RustFS chunks. Main's full harness run
  exited 0 with binary multi-chunk writes, full/range reads, per-drive auth,
  graceful shutdown and fresh-process durable reopen with distinct default
  owners. Existing VFS/RustFS/PGlite restart checks also passed. The live
  Cloudflare R2 CLI gate now passes; Windows remote execution and abrupt CLI
  crash recovery remain open.
- [ ] W17.2 Define discovery, routing, filesystem operations, streaming/ranges,
  stable errors and lifecycle; share the actual native/API drive namespace.
- [ ] W17.3 Add per-drive authorization/isolation, limits and deployment/TLS guidance.
- [ ] W17.4 Test multiple drives and mixed stores through real HTTP clients,
  including concurrency, restart, failures and Node/CLI configuration. The
  local memory/split-store subprocess now covers concurrency, range reads,
  truncate, aborted writes and process restart (`d526c18`); remote, Node and
  crash-recovery coverage remain open.
- [ ] W17.5 Define a cache integration boundary, but do not implement/select the
  distributed cache until the W22 discussion and primary acceptance.
- [x] `4af2a30` records a runnable cross-platform local demo using independent
  SQLite metadata/blocks, live PUT/GET, authorization isolation and process-
  reopen persistence. Cloud/TLS/deployment acceptance remains open.

## W18 — Benchmarks, allocations and dependencies

- [x] Land canonical dispatch benchmark (`2aecccf`) and forwarding allocation
  cleanup (`a84faf7`); cleanup is not evidence of a measured performance win.
- [ ] W18.1 Align workloads with ComputeSDK storage benchmarks, including
  representative 1/4/10/16 MiB data sizes and small-file/metadata workloads.
- [ ] W18.2 Measure direct Rust, Node and native paths across required stores;
  report cold/warm behavior, latency, throughput, memory and service settings.
- [ ] W18.3 Produce controlled before/after dispatch/allocation results.
- [ ] W18.4 Add license-reviewed ArtifactFS comparisons and explain semantic
  differences rather than presenting incomparable throughput as equivalent.
- [ ] W18.5 Audit dependency count/features per crate; justify runtime/codec/SDK
  additions and keep integrations out of the core dependency surface.
- [ ] W18.6 Review the untracked duplicate dispatch draft against the canonical
  benchmark before any cleanup; preserve unrelated working-tree files.
- [x] Live Cloudflare R2 smoke benchmark passed on 2026-09-20 through the public
  N-API split-store path with PGlite metadata, R2 blocks, fixed 64 KiB chunks,
  verified full-byte readback, delete and remote-prefix cleanup. This is one
  macOS smoke measurement, not completion of the full matrix.

## W19 — Compression evaluation

- [x] Record [compression design review](docs/compression-design.md) (`642e81a`):
  initial recommendation is chunk first, then independently compress blocks
  through an optional wrapper. This is a proposal, not implemented compression.
- [ ] W19.1 Benchmark raw, zstd levels 1/3/6, LZ4 and justified alternatives over
  representative data and chunk sizes; compare pre/post-chunking layouts.
- [ ] W19.2 Measure ratio, CPU, memory, random access, rewrites, deduplication,
  network cost and effects on future chunkers/version diffs.
- [ ] W19.3 Specify codec/version metadata, dictionaries, corruption handling,
  decompression bounds and compatibility before choosing implementation scope.

## W20 — CI, packaging and final acceptance

- [ ] Windows pinned-oracle N-API parity and the portable HTTP tests are now in
  the `windows-node` job (`d526c18`); hosted execution and Windows Rust/host
  runtime qualification remain required.
- [ ] W20.1 Obtain revision-matched green required hosted macOS/Linux jobs.
  Latest jobs were queued/in progress at this update; earlier Node/PGlite
  failures are not closed by local fixes alone.
- [ ] W20.2 Verify new RustFS and CLI native gates actually execute and pass.
  Main reran all three macOS CLI native tests after collision-resistant paths
  and panic-safe cleanup: passed, including actual SQLite DELETE/FULL reopen.
  Linux crash harness now serializes native cases and tries regular unmount
  before lazy fallback; its previously failed hosted gate remains unverified.
- [ ] W20.3 Add real FoundationDB/TiDB, VFS, versioning, adapters and HTTP gates.
- [ ] W20.4 Run locked build/tests, formatting and strict Clippy on final changes;
  retain logs and clearly distinguish ignored, skipped and credential-gated lanes.
- [ ] W20.5 Validate install/package artifacts, license consistency and dependency
  inventory; document supported and explicitly unsupported configurations.
- [ ] W20.6 Audit every task and requirement against code plus integration
  evidence before marking the goal complete. Do not claim “100%” from test count.

## W21 — Reference review and learnings

Canonical source links and license considerations live in [REFERENCES.md](REFERENCES.md).
For each review, record applicable lessons, rejected ideas and resulting task IDs;
listing a source does not mean it has been reviewed or its code can be reused.

- [ ] W21.1 Close agentfs and Archil architecture/storage review.
- [ ] W21.2 Close Tensorlake repository/TLFS and changed-byte Firecracker snapshot
  review; feed version/diff implications into W14/W23.
- [ ] W21.3 Review relevant CrabBuild repositories for blob-backed versioning.
- [ ] W21.4 Review SlateDB for S3 storage, publication and recovery tradeoffs.
- [ ] W21.5 Review SQLite's VFS abstraction for logical/storage separation.
- [ ] W21.6 Review Cloudflare ArtifactFS for feature and benchmark comparisons.
- [ ] W21.7 Review Erlang gen_statem at the end of primary work for storage and
  communication lifecycle lessons; create follow-up tasks for applicable findings.
- [ ] W21.8 Verify license compatibility before incorporating any reference code.
- [ ] W21.9 Review Rivet Actors' FoundationDB/SQLite VFS implementation at a
  pinned revision. Record exact source paths, transaction/locking/durability
  assumptions, ambiguous-commit behavior and recovery tests; map adopt/adapt/
  reject decisions to W07/W12/W15/W28 and preserve applicable attribution.
  Source review recorded at [Rivet Actors review](docs/reviews/rivet-actors.md),
  pinned to `78336a1a0ee33bb45e6b15e89963cbe91713353b`: Apache-2.0, no code
  copied. Relevant implementation is UDB/Postgres/RocksDB rather than a direct
  FoundationDB client. Stable operation IDs, fencing and explicit sync/commit
  boundaries are applicable; single-actor no-op SQLite locks are not. Mapping
  all lessons into implemented acceptance tests remains open.

## W22 — Distributed cache: discuss after primary work

- [ ] W22.1 Hold D04 discussion before implementation: topology, service choice,
  consistency, invalidation, failure behavior, tenancy and cost expectations.
- [ ] W22.2 Implement the agreed cache across multiple HTTP server instances and
  drives; a single-process cache is not distributed-cache acceptance.
- [ ] W22.3 Test stale reads, concurrent writes, eviction, partitions, recovery,
  version-pinned reads and cross-drive isolation; benchmark against uncached IO.

## W23 — Future physical copy-on-write

- [ ] W23.1 Define immutable sharing, changed-byte/block granularity, reference
  lifetime, writable forks and reclamation using W14 and reviewed sources.
- [ ] W23.2 Agree implementation sequencing after primary requirements; then
  implement and test crash-safe sharing, isolation and changed-byte cost scaling.

## W24 — Domain and marketing site

- [x] W24.1 AWS MCP verified the Route 53 hosted zone for `mount-rs.com` and
  configured the apex and `www` A records to Vercel at `76.76.21.21`.
- [x] W24.2 Built and deployed the combined marketing/docs site with TanStack
  Start to the user's Vercel Hobby plan, explicitly authorized 2026-09-20. No
  paid plan upgrade or paid resources. Claims and support status are visible.
- [x] Site child task delivered Vercel prebuilt-output/header/route hardening in
  `c9088aa` and `8cf0c5d`; these commits are site-only and do not prove a live
  Vercel deployment.
- [x] W24.3 Public DNS, HTTPS and the actual deployment are verified for
  `mount-rs.com` and `www.mount-rs.com`. Route 53 change
  `C0259256PYIMTC38BKLA` reached `INSYNC`; both hosts returned HTTP 200 from
  Vercel with the expected homepage and security headers. Operational handoff
  is complete; this backlog entry did not authorize spending.

## W25 — Actual AWS S3 integration

- [x] W25.1 AWS MCP became available after the app restart. STS identity and
  account-owned bucket inventory verified; testing region is `ap-southeast-2`.
- [x] Create and read back private `myroot` bucket
  `mount-rs-integration-922978963556-ap-southeast-2`: all four public-access
  blocks enabled, bucket-owner-enforced ownership, AES256 server-side
  encryption, test-resource tags, seven-day expiry under `mount-rs-tests/`,
  and one-day incomplete multipart cleanup. The
  `mount-rs-aws-s3-integration-test` role is scoped to the harness prefix and
  trusted only by the authenticated `myroot` SSO role; no access keys were
  created. MCP provisioning is not a Rust integration test result.
- [x] AWS MCP OAuth was revalidated on 2026-09-21 for account `106427005394`: bucket location, all four public-access blocks, bucket-owner-enforced ownership, AES256 encryption, lifecycle and tags were read successfully; the root `mount-rs-tests/aws-s3/` inventory was empty before and after testing. A unique-prefix service-side probe passed create-only immutable publication, duplicate rejection, byte ranges, stale conditional read/CAS rejection, current ETag CAS, a 65,537-byte boundary read, four concurrent writers, scoped deletion, and post-delete empty-prefix verification. This is AWS API/SDK evidence only: the MCP caller was account root and the bucket has no bucket policy, so least-privilege authorization remains unverified. That historical MCP bucket is not the selected `myroot` target and remains untouched.
- [x] W25.2 Provision private test bucket, narrowly scoped access, and test-data
  cleanup/retention policy. The 2026-09-21 `myroot` provisioning created the
  dedicated `ap-southeast-2` bucket, Block Public Access, BucketOwnerEnforced,
  AES256, seven-day `mount-rs-tests/` retention, one-day multipart-abort
  cleanup, tags, and a one-hour role at
  `arn:aws:iam::922978963556:role/mount-rs/mount-rs-aws-s3-integration-test`.
  The role grants only prefix-scoped list/object access plus caller identity,
  and credentials remain outside chat and source control.
- [x] W25.3 Execute actual AWS S3 block and composed-filesystem integration
  tests with restart/reopen, ranges, conditional immutable writes and cleanup.
  The clean 2026-09-21 live run used `AWS_PROFILE=myroot` and the scoped role
  in `ap-southeast-2` and passed both ignored service tests in separate Cargo
  processes: `actual_aws_s3_block_and_composed_filesystem` (1 passed) and
  `actual_aws_s3_reopen_after_process_restart` (1 passed). The run covered
  immutable blocks, ranges, conditional/CAS behavior, multi-chunk writes and
  overwrite/truncate/extend/sparse-tail behavior, SQLite metadata composition,
  fresh-process reopen, and exact owner-verified prefix cleanup with zero
  remaining objects. The same run also passed the real Rust CLI
  `sdk-self-test --reopen` against a separate owned prefix, proving the
  configuration-driven consumer path. AWS S3 evidence does not replace
  Cloudflare R2 or RustFS acceptance.
- [x] W25.4 Expose and qualify the first-class AWS S3 provider through the
  public Rust SDK and versioned Rust CLI configuration. `kind: "aws-s3"`
  accepts only bucket, region, prefix, and durable fields, resolves signed
  workload credentials from the AWS environment/role chain, rejects custom
  endpoints, and remains block-only. The live gate now runs the CLI
  `sdk-self-test` plus the SDK composition against the same scoped role; the
  public AWS path is no longer only a direct integration-test construction.
- [x] The live AWS packet is present in the provider/test crates: immutable
  block/range/conditional/CAS, composed SQLite metadata, fresh-process reopen,
  the public SDK and CLI configuration paths, nonce-owned cleanup and
  credential-safe validation, and a non-mutating sibling-prefix authorization
  denial check. The live packet also includes a bounded repeated write/read
  workload sample; a production load/soak result remains a deployment gate.
  The harness emits the secret-free
  `AWS_S3_TEST_BLOCKED` result when local credentials are absent, while the
  renewed `myroot` run above provides the live Rust acceptance.
- [x] The harness accepts an optional `AWS_S3_TEST_ROLE_ARN`, assumes that
  caller-provided role with a one-hour session, and uses the resulting temporary
  credentials for all S3 requests and cleanup. It does not create IAM resources
  or access keys; W25.2 is provisioned in `myroot`, and the live local Rust
  run is now recorded above.
- [x] Repository qualification after the AWS packet passed on 2026-09-21 at
  local `4a72d85` (an ancestor of current `origin/main` `7488aa8`):
  `CARGO_NET_OFFLINE=true ./scripts/cargo-shared test --workspace
  --all-targets --locked --offline` and strict workspace Clippy with
  `--all-targets --locked --offline -- -D warnings` both exited 0. The
  all-features variant remains an explicit host prerequisite boundary because
  this macOS runner does not provide native `libfdb_c`; the site typecheck also
  needs a network-backed dependency install and is not claimed from the
  offline run.
- [ ] W25.5 Define and approve the production rollout contract: AWS account,
  region and bucket ownership; IaC or an equivalent reviewable change; bucket
  policy, Block Public Access, Object Ownership, encryption/KMS, versioning,
  retention/lifecycle, prefix ownership, runtime/maintenance roles, and no
  long-lived credentials. A read-only resource audit script now checks the
  qualification controls without mutation. It passed the `myroot` test bucket
  for all four public-access blocks, BucketOwnerEnforced ownership, AES256
  default encryption, seven-day `mount-rs-tests/` expiry, and one-day
  incomplete-multipart abort; the W25 bucket and role are test resources, so
  production resource review remains open.
- [ ] W25.6 Qualify the production metadata pairing. Select a remote durable
  metadata provider and pass multi-writer/fencing, restart, backup/restore,
  schema-migration, and failure-recovery tests with actual AWS S3 blocks.
  The current SQLite composition is single-host reopen evidence only.
- [ ] W25.7 Add deployment observability and operations: S3 latency/error and
  retry metrics, conditional-conflict and orphan/cleanup signals, credential
  expiry detection, capacity/cost alerts, SLOs, incident runbooks, and
  canary/rollback procedures.
- [ ] W25.8 Add hosted release evidence: locked build/artifact provenance,
  approved OIDC or equivalent short-lived role credentials, security scan,
  load/soak/fault/restore drills, staged canary, rollback, and post-deploy
  smoke. Do not place AWS secrets in the repository or CI logs.
- [ ] W25.9 Production sign-off: record the exact released commit/image,
  reviewed configuration, live smoke result, rollback owner, and evidence for
  every W25.5-W25.8 gate before calling the AWS workstream production-ready.

## W26 — Apache Ozone S3 backend

- [x] Land isolated, digest-pinned Apache Ozone 2.2.1 gateway harness and
  an Ubuntu CI gate. Main's real Linux-arm64 Docker run passed immutable
  blocks, conditional create/read/write and CAS, concurrent publication,
  service restart/reopen, and owned-resource cleanup. Hosted Linux-amd64
  run `35585066458` passed the Ozone gateway, mixed metadata-provider/Node/CLI,
  and Ozone-backed durable TiDB jobs. The
  all-in-one non-secure test deployment is loopback-only, not production auth
  or replicated-durability acceptance.
- [x] W26.1 Pin Apache Ozone 2.2.1 and architecture-specific container digests;
  the isolated macOS/Linux harness owns loopback readiness, SigV4 bucket
  bootstrap, bounded Docker/test actions, restart/failure windows and
  ownership-checked cleanup. Its single all-in-one service has anonymous
  volumes and no replication, Kerberos/TLS or power-loss durability claim.
- [x] W26.2 Run the immutable block contract through its actual S3 gateway:
  the 2026-09-21 arm64 run passed create-only publication and duplicate
  rejection, ETag stale-read/stale-write rejection and successful CAS,
  concurrent writers, full/range reads, missing objects, binary payloads,
  bounded stopped-gateway failure, service restart/reopen and cleanup via
  `./scripts/test-ozone.sh`. The hosted Linux result remains a separate,
  revision-specific evidence boundary.
- W26.3 partial evidence: SQLite and disk-backed PGlite compositions passed
  on 2026-09-21
  through the real Ozone gateway with seven-byte ChunkedFs chunks, partial
  writes, shrink/extend truncation, ranges, revision CAS, stale-writer
  fencing, fresh reopen and scoped block/metadata cleanup. A separate
  single-node TiDB/Ozone run also passed the direct TiDB contract, the
  ChunkedFs partial/truncate/CAS/stale-fencing/reopen path, ambiguous-commit
  handling and cleanup; it is explicitly not replicated-durability evidence.
- W26.3 FoundationDB durable evidence passed on 2026-09-21 arm64 after the
  harness recovered from the earlier Docker layer-registration failure. The
  run created three fixed-address FoundationDB 7.4.7 server containers with
  three coordinators, separate persistent Docker volumes and `double`/SSD
  configuration. The transactional readiness probe passed both before and
  after restarting replicated node 2; the first and fresh-client Ozone
  compositions emitted `FOUNDATIONDB_RUSTFS_CHUNKED_PASS` and
  `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS`, followed by
  `FOUNDATIONDB_TEST_PASS topology=durable` and owned Ozone cleanup. This is
  real multi-node restart evidence, but remains loopback/non-secure test
  deployment evidence rather than production auth, TLS or power-loss proof;
  hosted CI remains revision-specific and pending.
- Historical hosted W26 validation run `35581168122` on `63dbdbd` passed the actual
  Linux-amd64 Ozone gateway (`ozone`, job `106274147767`) and the mixed
  SQLite/PGlite Ozone composition (`ozone-compositions`, job
  `106274147763`). Its durable TiDB job (`tidb`, `106274147942`) passed the
  three direct TiDB provider tests and the ambiguous-commit test, then timed
  out for 300 seconds at the first `TiDB restart readiness` phase. The
  matching TiDB/RustFS job (`tidb-rustfs`, `106274147791`) passed its direct
  provider, `TIDB_CHUNKED_RUSTFS_SEED_PASS`, and its ambiguous-commit phase,
  then hit the same frontend restart timeout. The published 30-second
  graceful shutdown change (`63dbdbd`) did not resolve this boundary. W26.3
  therefore remained open. The bounded isolation patch was committed as
  `ecc1106`, moving the deliberate dropped-COMMIT failure injection after
  durable restart/reopen so the restart gate is not contaminated by that
  test's intentionally unknown client outcome. Its first rerun
  `35582936271` was canceled when concurrent main commit `210c9cd` landed;
  replacement run `35583109781` was superseded before terminal completion and
  no result from the canceled runs is treated as evidence. Final W26
  acceptance is recorded below.
- [x] W26.3 Extend the real Ozone ChunkedFs composition gate to independent
  TiDB and FoundationDB metadata, including partial writes/truncation,
  revision CAS and stale-writer fencing. SQLite, PGlite, single-node TiDB and
  durable three-node FoundationDB are covered above. Final run `35585066458`
  on `9c098e5` passed hosted `ozone-compositions` job `106286459622` and
  Ozone-backed durable `ozone-tidb` job `106286459540`; the latter emitted
  `TIDB_ACCEPTANCE evidence=durable-multinode-restart`, Ozone integration and
  cleanup markers. Generic durable TiDB job `106286459436` also passed.
- [x] W26.4 Cover Node factories and CLI configuration; add required CI gates
  and document verified versions, limitations and platform evidence. The
  2026-09-21 arm64 live Ozone run passed the Node provider matrix (including
  PGlite-to-S3 partial/truncate/reopen), Node CLI self-test/reopen, matching
  Rust CLI self-test/reopen and owned cleanup; the summary was `pass=7
  skip=1 fail=0`. The CI job now builds the NAPI addon and installs PGlite;
  hosted results remain revision-specific. Ozone remains loopback-only,
  non-secure and not production replicated-durability acceptance.

### W26 production-rollout readiness (post-demo; currently NO-GO)

W26 qualification is complete, but production rollout is a separate open
track. The detailed evidence ledger, provisional estimates and blockers are in
[`docs/w26-progress-ledger.md`](docs/w26-progress-ledger.md). Do not mark a
production gate complete from the demo or from the hosted qualification packet
alone.

| Gate | Status | Completion | Exit evidence / primary blocker |
| --- | --- | ---: | --- |
| P0 — scope, support matrix, SLO/RPO/RTO, ownership | Open | 10% | Approved production target and non-goals; product/operations decisions required |
| P1 — secure production Ozone topology and rehearsal | Not started | 0% | Multi-node persistent production-like deployment; Ozone/cluster infrastructure required |
| P2 — production metadata-provider matrix | Qualification only | 10% | Selected supported providers and secure staging matrix; managed-provider/version access required |
| P3 — authentication, TLS, secrets and redaction | Not started | 0% | Certificate/identity/secret rotation and negative tests; security/platform access required |
| P4 — replicated durability and storage failure protection | Not started | 0% | Storage/node/power-loss boundary and integrity recovery; production storage/fault controls required |
| P5 — fencing, ambiguous commit and failover recovery | Partial qualification | 25% | Secure multi-node failure/retry evidence; distributed fault tooling required |
| P6 — backup, restore and DR | Not started | 0% | Clean-environment restore with measured RPO/RTO; backup/KMS/second failure domain required |
| P7 — observability, alerting and runbooks | Not started | 0% | SLO telemetry, alerts and tested operator procedures; monitoring/on-call ownership required |
| P8 — load, capacity, soak and cost envelope | Not started | 0% | Production-shaped performance/soak evidence; dedicated capacity and budget required |
| P9 — upgrade, rollback and compatibility | Not started | 0% | Rehearsed migration and rollback on retained data; release/change-window approval required |
| P10 — security, privacy, tenancy and audit | Not started | 0% | Security review and closed findings/approved exceptions; security/compliance owner required |
| P11 — native client/mount/platform matrix | Not started | 0% | Every advertised native platform passes; native runners, facilities and signing required |
| P12 — signed release, promotion, canary and rollback automation | Qualification CI only | 10% | Production promotion controls and canary evidence; CI/CD/artifact/signing access required |
| P13 — incident, failover and recovery rehearsal | Not started | 0% | Timed operator exercise meets RTO and integrity criteria; on-call/incident participation required |
| P14 — final launch evidence audit and GO/NO-GO | Not started | 0% | One-revision evidence packet and release-owner decision; all upstream gates required |

The production track is **0/15 terminal gates accepted**. Its current
provisional planning range is **31–76 engineering days plus external waits**;
this is not a delivery commitment and must be refined after P0 fixes the
advertised provider/platform scope.

## W27 — Native Windows support and CI

- [ ] W27.1 Run native `windows-latest` Rust formatting, all-feature Clippy and
  workspace tests; repair platform compilation and behavior failures rather
  than adding continue-on-error. Windows CI is added, not yet verified green.
- [ ] W27.2 Build the Windows napi-rs addon and run native Node factories,
  chunked storage, just-bash/Mastra consumers and TypeScript checks in CI.
- [ ] W27.3 Expand to pinned mountx differential parity, real SQLite/PGlite,
  authenticated R2 and other required backend/service tests on Windows.
- [ ] W27.4 Qualify path/drive-letter handling, open-handle deletion, locks,
  process/service lifecycle, restart recovery and artifact installation.
- [ ] W27.5 Define and implement Windows mount support separately from Unix
  FUSE/NFS and macOS FSKit. Explicit unsupported operations are not proof of
  Windows mounting acceptance; retain capability and evidence matrices.
- [x] `d6b80f4` adds the HostFs Windows packet for unprivileged symlink flags,
  rooted absolute-target inference and metadata/lifecycle behavior. The local
  suite passed 14/14 where runnable; native `windows-latest` execution remains
  required.
- [x] `ccaf8f5` + `4cdeb58` correct the Windows WAL shared-memory mapping mask
  and add regression coverage; local focused tests pass, but `windows-latest`
  must rerun before W27.1/W27.2 can be checked.

## W28 — Deterministic fault injection

- [x] Land isolated storage-wrapper crate with explicit occurrence-based plans,
  redacted pending/completed/cancelled traces and optional delays. Main passed
  seven all-feature tests and strict Clippy locally. Seed labels evidence, not
  randomized scheduling. Dedicated Linux/macOS/Windows CI added; hosted results
  remain pending.
- [x] Register the crate in the root workspace and shared lockfile. Add four
  composed filesystem tests, each exercised with memory and SQLite metadata/block
  stores: pre-write ENOSPC, lost publish acknowledgement, failed metadata flush,
  and invalidated writer lease. Assert namespace/bytes on reopen and explicit
  temporary-directory cleanup. Local all-feature suite: 11 passed. These are
  in-process wrapper faults, not power-loss or remote-service qualification.
- [ ] W28.1 Add a separate minimal-dependency fault-injection crate with explicit
  opt-in plans, operation/occurrence selectors, seeded replay and event evidence.
  Wrap metadata and block stores without changing production defaults.
- [ ] W28.2 Cover before/after-operation failures, lost acknowledgments, IO/full/
  permission errors, delays/timeouts, missing/corrupt/torn blocks, lease expiry,
  stale fencing, CAS conflicts and failed sync barriers. Record unsupported
  hooks and add transport/process controls rather than pretending wrappers
  simulate kernel, network or power-loss behavior.
- [ ] W28.3 Wire plans through test CLI/Node/VFS/HTTP entry points; isolate test
  resources, redact data/credentials, bound execution and verify cleanup.
- [ ] W28.4 Sweep defined fault points in the SQLite reliability matrix, retain
  seed/plan and minimized failing trace, check integrity plus exact transaction
  history and acknowledged-commit durability for the selected configuration.
- [ ] W28.5 Require bounded PR fault suites and broader scheduled matrices on
  Linux/macOS/Windows; publish coverage and remaining gaps, not an unbounded
  claim that every possible fault has been tested.

## W29 — User-configurable lifecycle hooks (later)

- [ ] W29.1 Define an extensible event catalog for files/folders: creation,
  opening/closing, writes, truncation, metadata changes, rename/move and deletion;
  and drive/server/connection lifecycle: starting, started, stopping, stopped,
  connection opened, dropped, reconnecting, reconnected and failed. Distinguish
  requested operations, successful completion and failure events.
- [ ] W29.2 Design registration, filtering by drive/path/event, removal and
  event payloads for user-supplied after-event hooks. Review Rust, Node, CLI
  configuration and mount-free HTTP integration surfaces; keep integrations
  separate from core and dependency costs minimal.
- [ ] W29.3 Specify ordering, concurrency, delivery/retry/deduplication behavior,
  cancellation, bounded queues/backpressure and shutdown draining. Clearly
  define after-write versus after-durable-commit; do not imply exactly-once
  delivery or crash-surviving hooks without an implemented durable mechanism.
- [ ] W29.4 Define hook timeouts, error isolation, reentrancy/recursive-event
  prevention and permissions. Hooks must not silently corrupt file operations,
  SQLite durability, lease/fencing or transaction outcomes. Redact credentials
  and avoid exposing file contents by default.
- [ ] W29.5 Implement the agreed API later and test event payloads/order,
  registration/removal, hook failures, connection drops/reconnects, startup and
  shutdown, concurrent operations and process failures across supported entry
  points/backends/platforms. Integrate W28 fault injection and document gaps.

## W30 — OpenTelemetry observability (later)

- [ ] W30.1 Define trace spans, metric instruments and structured log events
  across filesystem operations, metadata/block providers, chunking/compression,
  SQLite VFS, mounts, Node, CLI and HTTP services. Include lifecycle, latency,
  errors, retries, lease/fencing, cache behavior and durability boundaries.
- [ ] W30.2 Implement optional instrumentation and a separate integration crate
  where practical; keep exporters/SDK dependencies out of the minimal core and
  preserve a low-overhead disabled mode. Applications own provider/exporter setup.
- [ ] W30.3 Propagate context across Rust async tasks, napi-rs/Node, HTTP and
  backend calls; correlate traces and logs. Define sampling, resource identity,
  versioned event/attribute conventions and bounded metric cardinality.
- [ ] W30.4 Provide configurable OTLP export for traces, metrics and logs with
  bounded queues, timeouts, flush/shutdown and exporter-failure isolation. Avoid
  secrets, file contents and unbounded/sensitive paths in telemetry by default.
- [ ] W30.5 Add collector-backed integration tests for all three signals,
  context propagation, redaction, disabled mode, dropped connections, exporter
  failures and shutdown. Benchmark overhead and qualify macOS/Linux/Windows;
  document setup and dashboards/examples without claiming unverified coverage.

Implementation note (2026-09-21): the separate `mount-rs-observability` crate,
feature-gated HTTP/SDK/Node/CLI integration, bounded redaction contract, W3C
carrier helpers, signal-specific OTLP/HTTP paths, blocking-client exporter
setup, and deterministic tests are in the current implementation. W30.5
evidence on macOS arm64 (macOS 26.5.1, Darwin 25.5.0, Rust/Cargo 1.95.0)
against the then-current `origin/main` base `af31ba7`:

- `cargo fmt --all -- --check`: PASS.
- `cargo check --workspace --locked --offline`: PASS.
- `cargo test -p mount-rs-observability --all-features --locked --offline`:
  5 unit tests, one loopback collector test, and one exporter-failure/shutdown
  test PASS. The collector received non-empty `/v1/traces`, `/v1/metrics`,
  and `/v1/logs` payloads and the test found no raw secret path bytes.
- `cargo test -p mount-rs-sdk --features observability --locked --offline`:
  3 PASS; `cargo test -p mount-rs-http --features observability-otlp
  --test http_integration --locked --offline`: 7 PASS with loopback access;
  `cargo test -p mount-rs-cli --features observability --locked --offline`:
  39 unit + 9 CLI + 2 HTTP subprocess + 1 native artifact test PASS, with 3
  documented native/remote tests ignored; N-API observability check PASS.
- `cargo clippy -p mount-rs-observability --all-features --all-targets
  --locked --offline -- -D warnings`: PASS. The optimized no-exporter facade
  benchmark ran 20,000 operations and recorded baseline 2 ns/op, disabled
  9 ns/op, enabled 59 ns/op on this host; these are comparative observations,
  not a cross-platform SLO.

The local collector and failure tests do not prove reachability of an external
collector. Linux and Windows remain explicitly unverified, and the platform
runbook is `docs/w30.5-platform-qualification.md`.

## W31 — Per-drive mounts from one backing datastore (future)

This is a future workstream for exposing multiple independently mounted
drives, each with its own drive identity and namespace, while sharing one
metadata/block backing datastore. It must not be implemented by treating a
shared provider handle as an implicit global filesystem or by weakening
cross-drive isolation.

- [ ] W31.1 Define drive identity, namespace roots, ownership, quotas and
  lifecycle when several drives share one metadata and/or block provider.
- [ ] W31.2 Specify metadata schema/indexing and block reachability so each
  drive can be opened, snapshotted, copied, retained and garbage-collected
  independently without deleting another drive's live blocks.
- [ ] W31.3 Define locking, leases, revision/CAS boundaries and crash/reopen
  behavior for concurrent writers on the same drive and on different drives.
- [ ] W31.4 Expose the drive registry through the CLI config, HTTP multi-drive
  API, Node/mount-free APIs, FUSE/NFS/9P/FSKit transports and future native
  mounts without silently collapsing all drives into one namespace.
- [ ] W31.5 Test per-drive authorization, path isolation, shared-store
  cleanup, quotas, provider failures, restart, versioned views and future
  distributed-cache/copy-on-write interactions across memory, SQLite, PGlite,
  R2/RustFS and the database metadata providers.
- [ ] W31.6 Benchmark shared-store efficiency against separate backing stores;
  document supported combinations, migration/format versioning and safe
  deletion rules before enabling automatic cleanup.

## Recent landed chunks

| Commit | Scope | Evidence boundary |
| --- | --- | --- |
| `a3795f0` (published as `a4fa70a`) | Public napi-rs FUSE `OPEN`/`OPENDIR` request codecs | Protocol 7.8/7.39/7.41 pinned differential, typed replies, malformed/truncated/trailing checks and full N-API/typecheck/Clippy gates passed; native FUSE session/device/mount remains open |
| `cc73ad5` (published as `0d8f3c3`) | Unstorage path, metadata and handle parity | 11 oracle rows passed with zero mismatches/skips; capability/edge/N-API/upstream gates passed; hardlinks, symlinks, statfs and mknod remain explicit limitations |
| `a31880d` (published as `1e45692`) | Seeded Rust SDK, Node SDK and CLI provider lifecycle matrix | Positional write, truncate, flush and reopen passed across 5 Rust SDK, 4 Node SDK and 9 CLI rows; PGlite/R2 remain explicit skips |
| `d6c8a29` (published as `89e4940`) | TiDB + RustFS durable fixture hardening | Real TiDB v8.5.7 and loopback RustFS composition passed seed, partial writes, truncate, reopen, CAS/fencing and exact block/metadata cleanup; replicated durable topology remains capacity-gated |
| `7da18fb` | FoundationDB + RustFS exact cleanup scope | Real FoundationDB/RustFS harness passed multi-chunk, partial update, truncate/extend, reopen, CAS, stale fencing and sibling/parent sentinel preservation; service restart/root/hosted composition remain open |
| `3b59dc8` | Public napi-rs FUSE `GETATTR`/`SETATTR` codecs | Protocol 7.41/7.8 pinned differential, typed replies, malformed/truncated/trailing checks, generated declarations and full N-API suite passed; native FUSE session/device/mount remains open |
| `8eac5cb` (published as `67f8b08`) | Public napi-rs FUSE `READ` request/raw-reply codecs | Protocol 7.41/7.8 pinned differential, malformed/truncated checks, declarations, distribution and full N-API suite passed; native FUSE session/device/mount remains open |
| `26427f8` (published as `2c141d5`) | Core handle and lifecycle parity packet | 72-step pinned-oracle trace passed with 63 successes, 9 expected errors, zero mismatches; cross-provider, transport and native lifecycle remain open |
| `750484d` (published as `e510bfb`) | Remaining Unstorage edge classifications | 11 edge rows passed with 11 explicit ENOSYS classifications and zero skips; hardlinks, symlinks, statfs and special nodes remain intentionally unsupported |
| `e64d161` | Public napi-rs FUSE `WRITE` request/reply body codecs | Rebuilt N-API package, malformed-input and pinned mountx differential tests passed; full oracle-enabled N-API suite and distribution checks passed. Existing unrelated strict-Clippy `js_driver.rs` type-complexity lint remains. |
| `8d52d3c` (published as `6ec7b4d`) | Unstorage special-node capability classification | FIFO, socket, character-device and block-device operations are explicitly classified as unsupported; 13-row capability parity passed with 12 oracle-classified unsupported rows and zero skips. |
| `6b9258e` (published as `5f1ec1a`) | Locked Cargo resolution for all N-API CI builds | YAML, shell, local locked release-build checks passed; hosted CI run `35528165152` was created but remains queued and is not acceptance evidence until it completes. |
| `dcc3aa4` | Rust CLI live PGlite SDK consumer reopen check | Real socket-backed Rust CLI write/shutdown/reopen/readback passed; R2 remains credential-gated |
| `10afea2` (published as `7ea3eb6`) | Fail-closed R2/S3 configuration validation | 8 provider tests plus signed HTTP reopen/CAS passed; no live Cloudflare credentials |
| `6855b2d` (published as `fad208e`) | Mountx-compatible 40-hop symlink resolution limit | 17 core behavior tests, 11 core unit tests and 100-step oracle trace passed |
| `2452463` | Server-test phase/cleanup diagnostics | Local repeats; timeout cause unresolved |
| `2085b19` | HTTP/cache requirements and SQLite VFS reference | Requirements, not server implementation |
| `cfce82a` | PGlite injected cleanup failure | Local regression passed |
| `8d1f4ad` | JS driver shutdown test ordering | 25 strict local repeats |
| `e218d18` | S3 incremental codec helpers | 27 tests and Clippy locally |
| `f5759cc` | Actual RustFS harness and CI wiring | Local baseline passed; hosted pending |
| `642e81a` | Compression review | Design, not codec implementation |
| `c4058f2` | CLI macOS NFS native lifecycle | Host-backed native lane passed |
| `29ffb3b` | PGlite cleanup/slot ordering | Local regressions; hosted rerun pending |
| `5993984` | FSKit SDK compile target | Unsigned compilation, not activation |
| `95aca9c` | FSKit Rust/Swift/XPC bridge checkpoint | Local tests and unsigned arm64 Xcode builds; signing/activation/mount pending |
| `ca57758` | TiDB metadata/block providers and pinned harness | Real single-node v8.5.7 ARM64 qualification passed; durable topology and RustFS composition remain open |
| `5d9e513` | SQLite VFS/WAL reliability checkpoint | 4 unit, 15 SQLite-engine and 15 storage-bridge tests plus strict Clippy; Windows/remote/Node acceptance remains open |
| `67498a2` | TiDB schema-test lint follow-up | Focused lint correction; no new service qualification |
| `d526c18` | CLI HTTP edge cases and Windows pinned-oracle CI | Local CLI/HTTP tests and Clippy passed; hosted Windows/oracle execution remains pending |
| `afc55cb` / `c9088aa` / `8cf0c5d` | TanStack Start site and Vercel output hardening | Production deployment and custom-domain DNS/HTTPS verified; broader project release readiness remains open |
| `a5d1dd2` | Windows HostFs and FUSE protocol parity | Focused macOS tests/Clippy; hosted Windows qualification pending |
| `7508a56` | Scoped Cloudflare R2 CLI gate and credential redaction | Runner added; object-count compatibility was fixed in `00e96ce` |
| `00e96ce` | Cloudflare R2 CLI object-count compatibility | Live bucket-scoped CLI, N-API factory, parity and smoke benchmark passed; cleanup readback passed |
| `ae7c4cb` | Isolate inherited PGlite URL from the preflight trace lane | Focused stale-URL regression passed; pushed to `origin/main` |
| `73c33e0` | Isolate PGlite lifecycle from all preflight suites | Full macOS acceptance with live PGlite/R2 exited 0; native/hosted gates remain open |
| `0de1832` + `6ba3d62` | W01 core in-memory parity harness | 101-step pinned-oracle trace passed with 79 successes, 22 expected errors and zero mismatches/skips; later/provider/concurrency behavior remains open |
| `7238c829` | W01 remaining Unstorage skip inventory | 14 rows classified: 4 PASS, 10 ENOSYS, 0 ENOTSUP, 0 skipped; capability-limited operations remain explicit |
| `5c16164` | Public napi-rs FUSE `CREATE` request/reply codecs | Protocol 7.41/7.39/7.12/7.8 differential, malformed/truncated/trailing checks, typecheck, Clippy and full N-API suite passed; native session/device/mount remains open |
| `ae2f12d` | W01 skip inventory and deterministic trace evidence | 88 skips classified; five memory seeds passed; full matrix remains open |
| `dd65770` | Rust-backed N-API FUSE codec subpath | Rebuilt-addon smoke, TypeScript declarations and codec test passed; native session remains open |
| `0d7f1f4` | Rust CLI native end-to-end demo | Actual macOS NFS plus Rust/Node mounted-path I/O and cleanup passed |
| `0dca1d1` | Node SDK CLI example and integration test | Argument checks plus opt-in actual macOS NFS SDK self-test passed |
| `b6800f7` | W01 concurrency, provider/consumer and SQLite acceptance packets | Full root gate exit 0; PGlite rows passed; R2/live native and hosted platform gates remain open |
| `29337f7` | Public Rust SDK facade, Rust CLI routing and Node SDK CLI self-test | SDK unit/example, Rust/Node/provider matrix and CLI self-tests passed locally; live remote/native/hosted lanes remain open |
| `655cf19` | SDK-backed Rust and Node CLI consumer packet | Actual Rust binary and Node CLI examples, SQLite reopen checks, provider matrix and FUSE READDIRPLUS differential pass locally; live PGlite/R2/native/hosted lanes remain open |
| `d6b80f4` | Chunked persistence, SQLite VFS failure handling and Windows HostFs acceptance packets | Focused local chunked, SQLite and HostFs gates passed; hosted Windows and remote-provider lanes remain open |
| `3042d09` | Rust-backed FUSE inode state in napi-rs | Rust/Node inode parity, generated package checks and complete local N-API suite passed; native mount remains open |
| `71e826f` | Pinned-oracle W01 parity audit and closure queue | Core/concurrency traces passed with zero mismatches; structural native, NFS handle, Unstorage and durability gaps remain explicit |
| `21803fd` (published as `9868e93`..`b43a4e9`) | Public-SDK provider matrices and Node CLI config/reopen | Rust/Node/PGlite and CLI local gates passed; memory durability guard added; R2 credentials and hosted CI remain pending |
| `fd5eb04` (published as `88f8f9b` / `f7e630e`) | Structural N-API open flags and native lifecycle | macOS NFS mounted write/readback passed; Linux, other transports, Windows hosted and full FSKit/FUSE acceptance remain open |
| `31d62df` (published as `968119a` / `3c98aa6` / `6b4e8b4`) | Linux structural-driver FUSE CI lifecycle | Prerequisite-gated CI wiring and local harness pass; hosted Linux result pending |
| `3355cc1` (published as `eaba478` / `6a4ec2a` / `3aac826`) | Unstorage timestamp metadata parity | 11/11 Rust tests plus direct and N-API oracle parity passed |
| `323231f` (published as `75d2cae` / `6db5dd9` / `890389f` / `34c6cb0`) | Public FUSE READDIR body codec | Pinned-oracle differential, typecheck and distribution/export checks passed |
