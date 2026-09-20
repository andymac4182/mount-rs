# Compression benchmark packet

Status: bounded W19/W18.1 measurement packet. This is an in-memory codec and
layout evaluation; it is not a storage-provider, mounted-filesystem, fsync, or
network benchmark.

The packet lives in disjoint files:

- `benchmarks/compression/runner.mjs` — deterministic runner and JSON schema;
- `benchmarks/compression/test.mjs` — unit checks for the plan and generators;
- `scripts/benchmark-compression.sh` — repository-root wrapper;
- this document — scope, interpretation, commands, and evidence boundaries.

It does not change `benchmarks/storage/**`, `WORK_TRACKER.md`, `apps/site`,
Cargo manifests, or active integrations.

## What is compared

For every available selected codec, the runner measures two physical layouts
against a raw baseline at the same fixed chunk size:

| Layout | Encoding representation | Random read implication | Small rewrite implication |
| --- | --- | --- | --- |
| `raw` | Logical bytes split into fixed chunks, without a codec frame | Fetch the touched chunk; a range crossing a boundary fetches each touched chunk | Replace only the touched raw chunk |
| `chunk-then-compress` | Split logical bytes first; compress each chunk as an independent frame | Fetch and decode only the touched frame; boundary-crossing ranges touch multiple frames | Recompress only the touched frame; unchanged frames remain reusable |
| `compress-then-chunk` | Compress the whole logical input as one frame, then split encoded bytes into physical chunks | This packet has no seekable frame index, so a small read requires all physical chunks and a whole-stream decode | Recompress the whole logical input and rewrite all physical chunks |

The third layout is a comparison baseline for dependency and rewrite costs, not
the proposed filesystem default. Chunk-then-compress corresponds to the
independent-frame recommendation in `docs/compression-design.md`.

`randomReadRewrite` is therefore a structural model, not a measured latency
claim. The benchmark validates every decoded byte and its SHA-256 digest, but
does not perform provider reads or rewrites.

## Bounded packet matrix

The default `packet` profile has 34 workload/chunk settings and records two
iterations after one warmup iteration for each available lane:

| Workloads | Logical sizes | Fixed chunks |
| --- | --- | --- |
| `structured`, `random` | 1, 4, 10, 16 MiB | 64 KiB, 256 KiB, 1 MiB |
| `metadata`, `small-files` | 1 MiB | 4 KiB, 16 KiB, 64 KiB, 256 KiB, 1 MiB |

The representative sizes align with W18.1. The metadata workload is
newline-delimited deterministic JSON records. The small-file workload is a
deterministic aggregate of file-like records ranging from 64 bytes to 16 KiB;
it is intentionally an aggregate codec workload, not a claim about a specific
metadata database implementation.

The fast `smoke` profile covers all four workloads at 1 MiB and 64 KiB with two
recorded iterations. The `matrix` profile is a bounded Cartesian matrix using
all four workloads, all four representative sizes, and all five chunk sizes;
use explicit options to reduce it when needed. `--max-cases` guards against an
accidental unbounded matrix.

Every payload is generated from the supplied seed and includes a SHA-256 in the
JSON report. Data generation is outside timed regions.

## Codecs and alternatives

The selected default set is:

| Codec | Setting | Why it is included | Current implementation surface |
| --- | --- | --- | --- |
| raw | none | No-compression storage baseline | Buffer slices |
| Zstandard | levels 1, 3, 6 | Broad speed/ratio range and the primary design candidate | Node built-in zstd when available, otherwise `zstd` CLI |
| LZ4 | default fast level | Latency-oriented comparison from the design review | Discovered `lz4` CLI; no Node built-in binding is assumed |
| Brotli | quality 4 | Interoperable alternative for read-mostly/text-like data | Node built-in Brotli when available, otherwise `brotli` CLI |
| gzip/DEFLATE | level 6 | Ubiquitous interoperability baseline | Node built-in gzip/DEFLATE |

The runner detects bzip2 and xz as additional host tools and records their
versions, but does not include them in the bounded packet: they are slower
archival/interoperability comparisons and do not add an independent random-read
frame format here. They can be added in a separate, explicitly scoped study.

No missing codec is replaced with another codec. If Node bindings and the
corresponding executable are absent, the report contains an
`unavailableCodecs` entry and the process continues with the lanes that can
actually run. A missing codec is never represented by fabricated measurements.

LZ4's external process startup is included in wall-clock encode/decode time.
The parent process's CPU and RSS fields do not include the child process and are
labelled accordingly. Node in-process codecs report parent process CPU and RSS;
their `maxRSS` value is a process-lifetime high-water delta, not an isolated
allocator peak.

## Recorded evidence

Each successful lane records:

- logical bytes, fixed logical chunk size, logical chunk count, encoded payload
  bytes, physical chunk count, and codec frame count;
- per-iteration encode, decode, transform, and validation wall time;
- encode/decode MiB/s, parent CPU user/system/total microseconds, and payload
  size ratio (`encoded/logical`), compression ratio (`logical/encoded`), and
  savings percentage;
- RSS before/after and process high-water information when safely available;
- codec implementation, codec/library version, layout, payload SHA-256, and
  byte-for-byte decode validation;
- explicit structural random-read/rewrite implications.

The primary size ratio is payload-only. The runner does not invent a final
BlockStore envelope, index, dictionary, provider request, or network byte
count. Small independent chunks will consequently show codec-frame overhead in
their encoded payload but not a hypothetical mount-rs envelope.

## Format and version recommendations

Before implementation, an optional wrapper should use a versioned envelope
with at least:

`format_version`, `codec_id`, `codec_level`, `logical_length`,
`encoded_length`, `logical_chunk_size`, `frame_count`, checksum algorithm and
value, and an optional immutable `dictionary_id`.

Unknown versions/codecs must fail explicitly. The format identity should be in
the namespace/configuration or block-reference contract rather than inferred by
sniffing arbitrary raw user bytes. Decoding must bound encoded input, decoded
length, codec window, allocation, and concurrent work. A plaintext digest may
be retained separately from physical-byte identity if codec changes need
logical deduplication. Dictionaries need durable publication before their
referencing blocks and a retention policy for historical reads.

These are recommendations recorded by the benchmark packet, not an implemented
compression envelope. No dictionary is used in this baseline.

## Reproducible commands

Run from the repository root. The wrapper resolves its own repository path, so
it is also safe to invoke by absolute path.

```sh
cd /Users/andrewmcclenaghan/github/andymac4182/mount-rs

# Unit checks; no benchmark matrix is run.
node benchmarks/compression/test.mjs

# Fast, real codec smoke result.
./scripts/benchmark-compression.sh --profile smoke \
  --output /tmp/mount-rs-compression-smoke.json

# The bounded W19/W18.1 packet.
./scripts/benchmark-compression.sh --profile packet \
  --output /tmp/mount-rs-compression-packet.json

# Example of a deliberately smaller custom comparison.
./scripts/benchmark-compression.sh \
  --sizes 1,4 --chunks 64KiB,256KiB --workloads structured,random \
  --codecs raw,zstd-1,zstd-3,zstd-6,lz4,brotli-4,gzip-6 \
  --iterations 3 --warmup 1 \
  --output /tmp/mount-rs-compression-custom.json
```

The output path contains the complete machine-readable report. When
`--output` is supplied, stdout is deliberately a concise status record. A
small Node-only summary can be produced without `jq`:

```sh
node -e '
const r = require("/tmp/mount-rs-compression-smoke.json");
console.log(JSON.stringify({
  status: r.status,
  environment: r.environment.codecInventory,
  unavailable: r.run.unavailableCodecs,
  lanes: r.results.filter(x => x.status === "ok").map(x => ({
    workload: x.workload,
    sizeMiB: x.workloadSizeBytes / 1048576,
    chunkKiB: x.chunkSizeBytes / 1024,
    codec: x.codec,
    layout: x.layout,
    sizeRatio: x.summary.sizeRatio.median,
    encodeMs: x.summary.encodeMs.median,
    decodeMs: x.summary.decodeMs.median,
    physicalChunks: x.physicalChunkCount
  }))
}, null, 2));
'
```

## Local checkpoint and interpretation

The runner records the source revision, dirty-state count, Node/library
versions, selected codec implementations, host platform, exact seed, plan, and
elapsed time in every report. A report from a dirty checkout remains valid as a
reproducible measurement of that checkout, but it must not be described as a
clean release baseline.

On the development host used for this packet on 2026-09-20, the discovered
surfaces were Node `v26.0.0` with built-in zstd `1.5.7`, Brotli `1.2.0`, and
zlib `1.2.12`; LZ4 CLI `1.9.4`; and available but unselected bzip2 `1.0.8` and
xz `5.4.6`. The exact smoke and packet measurements should be taken from the
JSON files produced by the commands above; codec results are not portable
across CPUs, library versions, or process surfaces.

The local verification checkpoint for this dirty checkout was:

| Check | Observed result |
| --- | --- |
| `node benchmarks/compression/test.mjs` | `compression benchmark unit tests: PASS` |
| `./scripts/benchmark-compression.sh --profile smoke --output /tmp/mount-rs-compression-smoke-final.json` | `completed`; 4 settings, 52 successful lanes, 0 failures, 0 unavailable codecs, 1,848.09 ms |
| `./scripts/benchmark-compression.sh --profile packet --output /tmp/mount-rs-compression-packet.json` | `completed`; 34 settings, 442 successful lanes, 0 failures, 0 unavailable codecs, 88,972.56 ms |
| `PATH=/usr/bin:/bin /opt/homebrew/bin/node benchmarks/compression/runner.mjs --sizes 1 --chunks 64 --workloads random --codecs raw,lz4,zstd-1 --iterations 1 --warmup 0 --output /tmp/mount-rs-compression-no-lz4.json` | `completed-with-unavailable-codecs`; 3 lanes, LZ4 explicitly recorded unavailable, no substitution |

The packet JSON recorded source revision
`7c558b1a625693e1dd31a3298bf8854b08eade98` and a dirty-entry count of 12 at
that checkpoint; the final smoke rerun recorded `0a168bccd9908c57ae2aba73e7f329ba15184557`
with the same dirty-entry count. The checkout changed externally between those
two local reads, so the JSON provenance is authoritative for each result and no
baseline cleanliness is inferred. Representative smoke medians included structured 1 MiB at
64 KiB with zstd-3 size ratios of `0.002337` for chunk-then-compress and
`0.000296` for compress-then-chunk; random data stayed approximately `1.000214`
and `1.000035`, respectively. Those ratios are payload-only and do not justify
choosing whole-file compression because the random-read/rewrite implications
above remain materially different.

## Remaining measurement gaps

This packet closes only the bounded codec/layout evidence requested here. It
does not close the following:

- direct Rust, Node/native, mounted-path, or actual storage-provider latency and
  throughput; cold/warm cache, fsync, service configuration, request count, and
  network cost;
- measured random reads, append/overwrite/insertion rewrites, version
  snapshots, unchanged-chunk reuse, or plaintext/physical deduplication;
- final envelope/index/dictionary bytes, mixed-codec reopen, old-version reads,
  corruption/truncation handling, bounded decoder rejection, or durable
  dictionary publication;
- isolated child-process memory/CPU for the LZ4 CLI, and a per-lane allocator
  peak for the in-process codecs;
- trained dictionaries and a separate training/evaluation corpus;
- bzip2/xz or other alternatives beyond the justified Brotli and gzip baselines;
- macOS/Linux/Windows cross-host comparison and the SQLite transactional,
  sparse-file, and recovery workloads called for before compression scope is
  selected.
