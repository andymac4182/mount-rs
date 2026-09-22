# Compression placement review

Status: design recommendation, not implemented or benchmark-validated.

## Recommendation

Chunk logical, uncompressed bytes first, then compress each chunk independently.
Use a separate optional compression integration crate wrapping `BlockStore`.
Keep codecs out of core and out of individual storage providers. Start evaluation
with raw storage and zstd levels 1 and 3; compare LZ4 as a latency-oriented option.
Do not select a universal winning codec or chunk size without measurements.

Pipeline: logical bytes → chunk boundaries → optional logical content identity
→ independent compression frame → versioned envelope → underlying block store
→ block durability barrier → metadata publication. If encryption is added later,
compress before encryption. Compression does not strengthen durability.

## Why this placement

The current `BlockExtent` offsets and lengths describe logical file bytes.
`BlockStore::put/get` accept and return whole immutable blocks. Fixed-size writes
in `filesystems/mount-rs-chunked/src/lib.rs` reconstruct only touched chunks and
already omit all-zero chunks. A wrapper can preserve that contract: `get` returns
decoded bytes, while metadata keeps logical offsets. Sparse holes stay holes.

Whole-file compression before splitting creates dependencies between compressed
segments. A change can alter subsequent compressed bytes, weakening reuse across
versions and making logical seeks/updates expensive. Independent frames avoid
that dependency; content-defined chunking should also operate on plaintext if
introduced later. Fixed-size chunks remain the initial supported algorithm.

Larger independent chunks generally expose more redundancy, but require more
data to be fetched/decompressed/recompressed for small accesses. This is a
workload trade-off, especially for SQLite page-sized writes. Whole-file streams
remain a comparison baseline for immutable sequential data, not the filesystem
default. Packing independent frames into larger objects is a separate future
request-count optimization; it must not turn them into one dependent stream.

## Format and integration requirements

- Use an explicit versioned envelope with codec identifier, logical length,
  encoded length, integrity information and optional immutable dictionary ID.
  Unknown codecs/versions fail explicitly. Do not sniff magic bytes to distinguish
  legacy raw data: arbitrary user bytes may match. Store format identity in a
  namespace/config contract or an unambiguous block-reference scheme.
- Skip compression when its total envelope size does not save enough bytes;
  measure a savings threshold. Do not rely only on filename extensions.
- Underlying content-addressed IDs currently identify stored bytes. A transparent
  wrapper can retain those physical IDs, but codec/level changes can produce new
  IDs for identical logical content. Codec-independent deduplication requires a
  separate plaintext digest and atomic logical-to-physical representation mapping;
  do not silently change the existing immutable `BlockStore` identity contract.
- Read old and new codecs concurrently; policy changes affect new writes only.
  Historical versions must retain decoder/dictionary availability. Dictionary
  bytes need durable publication and retention as referenced objects before use.
  Include decoding format/namespace in `BlockStoreId` compatibility decisions.
- Bound decoded output, decoder window, encoded input and CPU concurrency; reject
  corrupt/truncated frames and length/digest mismatches. Execute CPU-heavy codec
  work off async I/O workers with bounded allocation and queueing.
- Respect provider limits using worst-case encoded size including headers; do not
  assume compression makes a too-large raw chunk fit. Test incompressible input.
- Metadata compression is a separate evaluation: do not compress away database
  keys or transactional fields needed for CAS, fencing and queries.

## Algorithms and dictionary evaluation

Zstd offers a broad speed/ratio range and trained dictionaries for correlated
small records. Dictionaries may improve small chunks without cross-chunk stream
dependencies, but add lifecycle obligations. Train only on representative training
data and evaluate on a separate corpus. No dictionaries in the initial baseline.
LZ4 is a speed-focused comparison. Brotli and DEFLATE are secondary benchmark
candidates for read-mostly/interoperability workloads, not default dependencies.
Audit each chosen implementation's license and macOS/Linux build requirements.

## Required measurements and tests

Compare raw, zstd 1/3/6, LZ4, and selected Brotli/DEFLATE settings across 4, 16,
64, 256 KiB and 1 MiB logical chunks, subject to provider limits. Include source
trees, JSON, SQLite databases/WAL, binaries, already-compressed media and seeded
random bytes. Test overwrite, append, insertion and version snapshots.

Record actual stored bytes including envelopes/indexes/dictionaries; compression
and decompression CPU, peak memory, throughput, p50/p95/p99 latency, backend
requests, fetched bytes, read/write amplification and unchanged-block reuse.
Separate cold/warm cache and direct/mounted/Node lanes. Run SQLite transactional
and recovery workloads rather than inferring safety from byte round trips.

Require mixed-codec reopen, old-version reads, sparse files, partial writes,
truncation, corruption, unknown codecs, missing dictionaries, bounded decode,
compression failure and durable publication ordering tests across backends.
Benchmark on macOS and Linux. Compression wins are unproven until these run.

## Primary sources

- [Zstd overview and dictionaries](https://facebook.github.io/zstd/)
- [Zstd independent seekable frames and size trade-offs](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/README.md)
- [Zstd frame format](https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md)
- [LZ4](https://github.com/lz4/lz4)
- [Brotli](https://github.com/google/brotli)

The placement and integration recommendations above are engineering conclusions
from these sources and the current mount-rs contracts, not measured results.
