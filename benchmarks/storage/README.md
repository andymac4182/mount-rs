# Storage benchmark

This directory contains the dependency-light storage benchmark requested by
`REQUIREMENTS.md#storage-benchmark-acceptance`. It uses the public
`integrations/mount-rs-napi/index.js` loader for mount-rs and imports the actual
mountx TypeScript memory driver/loopback as the oracle. Node CI runs its unit
tests and smoke workload on macOS and Linux and uploads the raw smoke JSON.
The local `scripts/test-all.sh` gate also includes the smoke workload.

The workload mapping is pinned to ComputeSDK `benchmarks/storage` revision
`92fbbc9ba7739111899121195236acb4fc6a8bb5`:

| Reference phase | Runner phase | Timed? |
| --- | --- | --- |
| upload | `Filesystem.writeFile(path, payload)` | yes |
| full-byte download | `Filesystem.readFile(path)` | yes |
| delete | `Filesystem.unlink(path)` | yes, reported separately |
| payload allocation | deterministic payload generation | no |
| byte validation | compare every returned byte with the payload | no |

Initialization happens once per provider before any size's first timed write.
The same prepared payload is used for that size's iterations. The read timer
stops before byte validation. Every path is unique to this run, and failed or
timed-out lifecycles receive a bounded cleanup attempt; the final cleanup pass
only addresses paths created by this run.

Timeouts are bounds, not cancellation. A `Promise.race`-style timeout cannot
cancel an in-flight native write/read/delete. The runner observes late
settlement when possible, defers cleanup while a timed-out operation is still
pending, and reports `cleanupSucceeded: false`/a pending cleanup instead of
claiming that the path or provider shutdown is clean. A late write can still
finish after the timeout; such a result is a failure and must not be treated as
a successful cleanup or success sample.

## Commands

From the repository root:

```sh
node benchmarks/storage/test.mjs
MOUNT_RS_PGLITE_TEST_SCOPE=benchmark ./scripts/test-pglite.sh
node benchmarks/storage/runner.mjs --smoke --output artifacts/storage-smoke.json
node benchmarks/storage/runner.mjs \
  --sizes 1,4,10,16 --iterations 2 --concurrency 1 \
  --output artifacts/storage-full.json
```

The smoke default runs 1 MiB, one iteration, concurrency one, over
`mount-rs-memory`, `mount-rs-sqlite`, and `mountx-memory`. Full mode defaults to
all four required sizes, two iterations, concurrency one, and all provider
definitions. The runner writes the complete JSON to stdout and, when
`--output` is supplied, to that path as well. It returns a failing exit code
for failed operations, timeouts, or cleanup failures. Missing external
configuration is a machine-readable `skipped` result and is never counted as a
success.

Each result records `environment.sourceControl.mountRs` with `git rev-parse
HEAD`, a dirty flag, and a dirty-entry count (status contents are deliberately
omitted). The actual oracle checkout is recorded separately with the expected
mountx pin `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`; an existing source whose
revision cannot be verified or does not match that pin fails the oracle row
instead of being presented as a differential pass. The loaded NAPI artifact is
reported under `environment.nativeAddon` by byte size and SHA-256 digest;
source Git revisions are never presented as binary revisions.

Error records redact URL credentials, access keys, tokens, passwords, and
database-URL environment assignments. They retain only bounded diagnostic
fields needed to interpret failures.

Useful options include `--providers`, `--timeout-ms`,
`--cleanup-timeout-ms`, `--chunk-size-bytes`, `--payload-seed`, and
`--network-context`. A sequential run is explicit with `--concurrency 1`.

The PGlite script requires `pnpm --dir tests/pglite install --frozen-lockfile`
and a built native addon. It starts an isolated real PGlite socket server,
runs combined and split-provider smoke workloads, and stops the server. These
results are explicitly volatile, not durable-disk benchmark evidence. CI runs
this check on the Node platform matrix and uploads its JSON separately.

## Provider matrix and evidence boundaries

| Provider id | Implementation/binding | Metadata | Blocks | Topology | Configuration |
| --- | --- | --- | --- | --- | --- |
| `mount-rs-memory` | mount-rs public NAPI | memory | memory | combined | always available when the native package is loadable |
| `mount-rs-sqlite` | mount-rs public NAPI | SQLite | SQLite | combined | temporary SQLite file, removed after shutdown |
| `mount-rs-split-sqlite` | mount-rs public NAPI | SQLite | SQLite | split stores | temporary metadata and block databases |
| `mount-rs-pglite` | mount-rs public NAPI | PGlite | PGlite | combined | `MOUNT_RS_PGLITE_DATABASE_URL` or `PGLITE_DATABASE_URL` |
| `mount-rs-split-pglite` | mount-rs public NAPI | PGlite | PGlite | split stores | same PGlite URL variables |
| `mount-rs-split-pglite-r2` | mount-rs public NAPI | PGlite | Cloudflare R2 | split stores | PGlite URL plus R2 endpoint/bucket/key variables |
| `mountx-memory` | actual mountx TypeScript | memory | memory | combined | `MOUNTX_SOURCE`, or pinned checkout at repo-local `vendor/mountx` |

The actual TypeScript oracle is used only when `MOUNTX_SOURCE` points to the
pinned checkout (or the repository-local `vendor/mountx` checkout exists).
Otherwise `mountx-memory` is explicitly reported as `skipped`; the runner does
not infer a host-specific `/tmp` location.

The R2 provider uses `createChunkedDriver` with fixed-size chunking and a
PGlite metadata store, so its metadata and block labels are not collapsed into
one “R2” label. The default chunk size is 65,536 bytes and the selected value
is recorded in the JSON. Set `MOUNT_RS_R2_DURABLE=0` only when deliberately
measuring a volatile remote-block configuration; otherwise the requested R2
provider declares durable remote blocks. The runner records the configured
remote region if `MOUNT_RS_R2_REGION` or `R2_REGION` is set, but never records
credentials.

PGlite and R2 rows are skipped with their missing variable names when the
configuration is absent. A skip is not a live-credential result and is not a
claim that the remote integration was verified. The pinned mountx source has
the memory and host/unstorage drivers but no equivalent SQLite/PGlite/R2
filesystem provider in this runner; no substitute backend is invented for the
TypeScript oracle. The mountx memory row is the equivalent actual-TS oracle
comparison.

All current measurements are labelled `measurementSurface: "direct-api"`.
No privileged or OS-mounted path timings are reported; the JSON explicitly
marks `mountedPath` as `not-run`. Direct API and mounted-path results must not
be compared as if they were the same workload.

`durabilityClass`, per-store `metadataDurabilityClass` and
`blockDurabilityClass`, `cacheState`, `synchronizationPolicy`, metadata
provider, block provider, runtime versions, platform, architecture, payload
sizes, iterations, concurrency, and chunking are recorded in the machine
output.
Latency statistics are over successful operation samples and use the reference
five-percent trimming rule. Raw samples retain unrounded milliseconds and
throughput. Throughput is decimal Mbps:
`fileSizeBytes * 8 / (readMs / 1000) / 1_000_000`.

## Snapshot/fork and copy-on-write

The reference snapshot/fork benchmark is recorded in each result as
`snapshotFork.status: "deferred"`. This runner does not copy whole snapshots,
pretend that they are copy-on-write, or publish performance claims for the
future COW requirement.
