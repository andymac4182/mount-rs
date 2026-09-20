#!/usr/bin/env node

/**
 * Dependency-light compression placement benchmark.
 *
 * This intentionally measures codec transforms in memory. It does not claim
 * mounted-path, provider, fsync, network, or database performance. The three
 * representations are:
 *
 *   raw                 logical fixed-size chunks, no codec frame
 *   chunk-then-compress one independent codec frame per logical chunk
 *   compress-then-chunk one codec frame, then fixed-size physical chunks
 *
 * Node's built-in zlib bindings are preferred. LZ4 uses the discovered lz4
 * command because Node has no built-in LZ4 binding. The chosen implementation
 * and version are recorded in every report so those surfaces are not silently
 * presented as equivalent.
 */

import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { writeFileSync } from "node:fs"
import { release as osRelease } from "node:os"
import { performance } from "node:perf_hooks"
import { fileURLToPath } from "node:url"
import { dirname, resolve } from "node:path"
import * as zlib from "node:zlib"

const KIB = 1024
const MIB = 1024 * 1024
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const DEFAULT_SEED = "mount-rs-compression-benchmark-v1"
const DEFAULT_CHUNKS = [64 * KIB, 256 * KIB, MIB]
const ALL_CHUNKS = [4 * KIB, 16 * KIB, 64 * KIB, 256 * KIB, MIB]
const REPRESENTATIVE_SIZES = [MIB, 4 * MIB, 10 * MIB, 16 * MIB]
const WORKLOAD_NAMES = ["structured", "random", "metadata", "small-files"]
const CODEC_NAMES = [
  "raw",
  "zstd-1",
  "zstd-3",
  "zstd-6",
  "lz4",
  "brotli-4",
  "gzip-6",
]
const LAYOUT_NAMES = ["raw", "chunk-then-compress", "compress-then-chunk"]
const MAX_EXTERNAL_BUFFER = 128 * MIB
const EXTERNAL_TIMEOUT_MS = 120_000

const WORKLOADS = Object.freeze({
  structured: {
    description:
      "Deterministic source/config-like records with repeated syntax and changing identifiers.",
    sizePolicy: "representative",
  },
  random: {
    description: "Deterministic xorshift bytes representing already-incompressible data.",
    sizePolicy: "representative",
  },
  metadata: {
    description:
      "Deterministic newline-delimited metadata records with paths, sizes, modes, versions, and digests.",
    sizePolicy: "one-mib-chunk-sweep",
  },
  "small-files": {
    description:
      "Deterministic framed corpus of many 64 B to 16 KiB file-like objects; aggregate bytes are 1 MiB.",
    sizePolicy: "one-mib-chunk-sweep",
  },
})

const FORMAT_RECOMMENDATIONS = Object.freeze({
  envelopeVersion: "mount-rs.compression-envelope.v1 (recommendation; not implemented here)",
  requiredFields: [
    "format_version",
    "codec_id",
    "codec_level",
    "logical_length",
    "encoded_length",
    "logical_chunk_size",
    "frame_count",
    "checksum_algorithm",
    "checksum",
    "dictionary_id (null for this packet)",
  ],
  compatibility: [
    "Reject unknown format versions and codec identifiers explicitly.",
    "Retain decoder and immutable dictionary availability for historical blocks.",
    "Do not sniff raw bytes to distinguish legacy raw blocks; use namespace/config or block-reference identity.",
    "Bound decoded length, codec window, input bytes, and concurrent codec work before allocation.",
  ],
  identity: [
    "Keep physical-byte identity separate from optional plaintext identity when codec changes must deduplicate logically identical data.",
    "Publish dictionary objects before any block that references them and retain referenced dictionaries.",
  ],
})

function usageError(message) {
  const error = new Error(message)
  error.code = "BENCHMARK_USAGE"
  return error
}

export function parseByteSize(value, defaultUnit = "B") {
  const text = String(value).trim()
  const match = /^(\d+(?:\.\d+)?)(b|kib|mib|kb|mb)?$/i.exec(text)
  if (!match) throw usageError(`invalid byte size: ${value}`)
  const amount = Number(match[1])
  const unit = (match[2] ?? defaultUnit).toLowerCase()
  const factor = {
    b: 1,
    kib: KIB,
    mib: MIB,
    kb: 1000,
    mb: 1000 * 1000,
  }[unit]
  if (!factor || !Number.isSafeInteger(amount * factor) || amount <= 0) {
    throw usageError(`byte size must be a positive safe integer: ${value}`)
  }
  return amount * factor
}

function parseList(value, parser) {
  const values = String(value)
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean)
    .map(parser)
  if (values.length === 0) throw usageError("list must contain at least one value")
  return [...new Set(values)]
}

function parseSizeList(value) {
  return parseList(value, (item) => parseByteSize(item, "MiB"))
}

function parseChunkList(value) {
  return parseList(value, (item) => parseByteSize(item, "KiB"))
}

function parsePositiveInteger(value, name) {
  if (!/^\d+$/.test(String(value))) throw usageError(`${name} must be a positive integer`)
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw usageError(`${name} must be a positive integer`)
  }
  return parsed
}

function parseNonNegativeInteger(value, name) {
  if (!/^\d+$/.test(String(value))) throw usageError(`${name} must be a non-negative integer`)
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed)) throw usageError(`${name} must be a non-negative integer`)
  return parsed
}

export function parseArgs(argv) {
  let profile = "packet"
  let output
  let seed = DEFAULT_SEED
  let sizes
  let chunks
  let workloads
  let codecs
  let iterations
  let warmup
  let maxCases
  let custom = false

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    const next = () => {
      if (index + 1 >= argv.length) throw usageError(`${argument} requires a value`)
      index += 1
      return argv[index]
    }

    if (argument === "--help" || argument === "-h") return { help: true }
    if (argument === "--smoke") {
      profile = "smoke"
      continue
    }
    if (argument === "--profile") {
      profile = next()
      if (!["smoke", "packet", "matrix"].includes(profile)) {
        throw usageError(`unknown profile: ${profile}`)
      }
      continue
    }
    if (argument === "--sizes") {
      sizes = parseSizeList(next())
      custom = true
      continue
    }
    if (argument === "--chunks") {
      chunks = parseChunkList(next())
      custom = true
      continue
    }
    if (argument === "--workloads") {
      workloads = parseList(next(), (item) => {
        if (!WORKLOAD_NAMES.includes(item)) throw usageError(`unknown workload: ${item}`)
        return item
      })
      custom = true
      continue
    }
    if (argument === "--codecs") {
      codecs = parseList(next(), (item) => {
        if (!CODEC_NAMES.includes(item)) throw usageError(`unknown codec: ${item}`)
        return item
      })
      custom = true
      continue
    }
    if (argument === "--iterations") {
      iterations = parsePositiveInteger(next(), "--iterations")
      continue
    }
    if (argument === "--warmup") {
      warmup = parseNonNegativeInteger(next(), "--warmup")
      continue
    }
    if (argument === "--max-cases") {
      maxCases = parsePositiveInteger(next(), "--max-cases")
      continue
    }
    if (argument === "--seed") {
      seed = next()
      if (seed.length === 0 || seed.length > 128) throw usageError("--seed must be 1-128 characters")
      continue
    }
    if (argument === "--output") {
      output = next()
      if (output.length === 0) throw usageError("--output must not be empty")
      continue
    }
    throw usageError(`unknown argument: ${argument}`)
  }

  const defaults = {
    smoke: { iterations: 2, warmup: 1, maxCases: 64 },
    packet: { iterations: 2, warmup: 1, maxCases: 128 },
    matrix: { iterations: 1, warmup: 0, maxCases: 512 },
  }[profile]

  return {
    help: false,
    profile,
    custom,
    sizes,
    chunks,
    workloads,
    codecs: codecs ?? [...CODEC_NAMES],
    iterations: iterations ?? defaults.iterations,
    warmup: warmup ?? defaults.warmup,
    maxCases: maxCases ?? defaults.maxCases,
    seed,
    output,
  }
}

export function helpText() {
  return `Compression benchmark (dependency-light, in-memory)

Usage:
  ./scripts/benchmark-compression.sh [options]

Profiles:
  --profile smoke       1 MiB, all workloads, 64 KiB chunks (fast validation)
  --profile packet      bounded W19/W18.1 packet (default)
  --profile matrix      Cartesian matrix; use --sizes/--chunks/--workloads to bound it

Options:
  --sizes LIST          MiB by default, e.g. 1,4,10,16 or 1MiB,4MiB
  --chunks LIST         KiB by default, e.g. 64,256,1024 or 4KiB,1MiB
  --workloads LIST      structured,random,metadata,small-files
  --codecs LIST         raw,zstd-1,zstd-3,zstd-6,lz4,brotli-4,gzip-6
  --iterations N        recorded iterations per lane
  --warmup N            unrecorded iterations per lane
  --max-cases N         maximum logical workload/chunk settings
  --seed TEXT           deterministic payload seed
  --output PATH         write full JSON report to PATH; stdout stays concise
  --help

The runner reports unavailable codecs and never substitutes another codec for a
requested one. External LZ4 process startup is included in wall time; child
process CPU/RSS is explicitly excluded from the parent-process memory fields.
`
}

export function buildCases(options) {
  const hasExplicitMatrix = options.custom
  const cases = []

  if (options.profile === "packet" && !hasExplicitMatrix) {
    for (const workload of ["structured", "random"]) {
      for (const sizeBytes of REPRESENTATIVE_SIZES) {
        for (const chunkSizeBytes of DEFAULT_CHUNKS) {
          cases.push({ workload, sizeBytes, chunkSizeBytes })
        }
      }
    }
    for (const workload of ["metadata", "small-files"]) {
      for (const chunkSizeBytes of ALL_CHUNKS) {
        cases.push({ workload, sizeBytes: MIB, chunkSizeBytes })
      }
    }
  } else {
    const sizes = options.sizes ?? (options.profile === "smoke" ? [MIB] : REPRESENTATIVE_SIZES)
    const chunks = options.chunks ?? (options.profile === "smoke" ? [64 * KIB] : ALL_CHUNKS)
    const workloads = options.workloads ?? (options.profile === "smoke" ? WORKLOAD_NAMES : WORKLOAD_NAMES)
    for (const workload of workloads) {
      for (const sizeBytes of sizes) {
        for (const chunkSizeBytes of chunks) {
          cases.push({ workload, sizeBytes, chunkSizeBytes })
        }
      }
    }
  }

  if (cases.length > options.maxCases) {
    throw usageError(
      `case plan has ${cases.length} settings, above --max-cases ${options.maxCases}; narrow the matrix explicitly`,
    )
  }
  return cases
}

function commandPath(command) {
  const result = spawnSync("/bin/sh", ["-c", `command -v ${command}`], {
    encoding: "utf8",
    timeout: 5_000,
  })
  if (result.status !== 0) return null
  const path = result.stdout.trim().split(/\s+/)[0]
  return path || null
}

function commandVersion(path, args = ["--version"]) {
  const result = spawnSync(path, args, {
    encoding: "utf8",
    timeout: 5_000,
    maxBuffer: 16 * KIB,
  })
  // bzip2 writes binary probe bytes to stdout for -V and the human version to
  // stderr; prefer stderr so optional-tool inventory never records that probe.
  const output = `${result.stderr ?? ""}\n${result.stdout ?? ""}`
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find(Boolean)
  return output ? output.slice(0, 240) : "version command returned no text"
}

function unavailableCodec(id, reason) {
  return {
    id,
    status: "unavailable",
    reason,
    implementation: null,
    version: null,
    format: null,
  }
}

function externalCodec(id, command, version, format, compressArgs, decompressArgs) {
  const run = (args, input, expectedBytes) => {
    const result = spawnSync(command, args, {
      input,
      encoding: null,
      timeout: EXTERNAL_TIMEOUT_MS,
      maxBuffer: Math.min(
        MAX_EXTERNAL_BUFFER,
        Math.max(4 * MIB, input.length * 4 + MIB, expectedBytes + MIB),
      ),
    })
    if (result.error) throw result.error
    if (result.status !== 0) {
      const stderr = Buffer.isBuffer(result.stderr)
        ? result.stderr.toString("utf8")
        : String(result.stderr ?? "")
      throw new Error(`${id} command failed (${result.status}): ${stderr.trim().slice(0, 300)}`)
    }
    return result.stdout
  }

  return {
    id,
    status: "available",
    implementation: `external:${command}`,
    version,
    format,
    external: true,
    compress: (input) => run(compressArgs, input, input.length),
    decompress: (input, expectedBytes) => run(decompressArgs, input, expectedBytes),
  }
}

function nodeCodec(id, version, format, compress, decompress) {
  return {
    id,
    status: "available",
    implementation: "node:zlib",
    version,
    format,
    external: false,
    compress,
    decompress,
  }
}

export function detectCodecs() {
  const zstdCommand = commandPath("zstd")
  const lz4Command = commandPath("lz4")
  const brotliCommand = commandPath("brotli")
  const bzip2Command = commandPath("bzip2")
  const xzCommand = commandPath("xz")
  const zstdLevel = zlib.constants.ZSTD_c_compressionLevel
  const zstdChecksum = zlib.constants.ZSTD_c_checksumFlag
  const brotliQuality = zlib.constants.BROTLI_PARAM_QUALITY
  const codecs = new Map()

  codecs.set("raw", {
    id: "raw",
    status: "available",
    implementation: "buffer-slices",
    version: process.version,
    format: "raw bytes",
    external: false,
  })

  for (const level of [1, 3, 6]) {
    const id = `zstd-${level}`
    if (typeof zlib.zstdCompressSync === "function" && typeof zlib.zstdDecompressSync === "function") {
      codecs.set(
        id,
        nodeCodec(
          id,
          process.versions.zstd ?? "unknown",
          "Zstandard frame",
          (input) =>
            zlib.zstdCompressSync(input, {
              params: {
                [zstdLevel]: level,
                [zstdChecksum]: 1,
              },
            }),
          (input, expectedBytes) =>
            zlib.zstdDecompressSync(input, { maxOutputLength: expectedBytes }),
        ),
      )
    } else if (zstdCommand) {
      codecs.set(
        id,
        externalCodec(
          id,
          zstdCommand,
          commandVersion(zstdCommand, ["--version"]),
          "Zstandard frame",
          ["-q", `-${level}`, "-c", "-"],
          ["-q", "-d", "-c", "-"],
        ),
      )
    } else {
      codecs.set(id, unavailableCodec(id, "Node zstd bindings and the zstd executable are unavailable"))
    }
  }

  if (lz4Command) {
    codecs.set(
      "lz4",
      externalCodec(
        "lz4",
        lz4Command,
        commandVersion(lz4Command, ["--version"]),
        "LZ4 frame",
        ["-q", "-z", "-c", "-"],
        ["-q", "-d", "-c", "-"],
      ),
    )
  } else {
    codecs.set("lz4", unavailableCodec("lz4", "the lz4 executable is unavailable and Node has no built-in LZ4 binding"))
  }

  if (typeof zlib.brotliCompressSync === "function" && typeof zlib.brotliDecompressSync === "function") {
    codecs.set(
      "brotli-4",
      nodeCodec(
        "brotli-4",
        process.versions.brotli ?? "unknown",
        "Brotli RFC 7932 stream",
        (input) =>
          zlib.brotliCompressSync(input, {
            params: { [brotliQuality]: 4 },
          }),
        (input, expectedBytes) =>
          zlib.brotliDecompressSync(input, { maxOutputLength: expectedBytes }),
      ),
    )
  } else if (brotliCommand) {
    codecs.set(
      "brotli-4",
      externalCodec(
        "brotli-4",
        brotliCommand,
        commandVersion(brotliCommand, ["--version"]),
        "Brotli RFC 7932 stream",
        ["-q", "4", "-c", "-"],
        ["-d", "-c", "-"],
      ),
    )
  } else {
    codecs.set("brotli-4", unavailableCodec("brotli-4", "Node Brotli bindings and the brotli executable are unavailable"))
  }

  if (typeof zlib.gzipSync === "function" && typeof zlib.gunzipSync === "function") {
    codecs.set(
      "gzip-6",
      nodeCodec(
        "gzip-6",
        process.versions.zlib ?? "unknown",
        "gzip container with DEFLATE level 6",
        (input) => zlib.gzipSync(input, { level: 6 }),
        (input, expectedBytes) => zlib.gunzipSync(input, { maxOutputLength: expectedBytes }),
      ),
    )
  } else {
    codecs.set("gzip-6", unavailableCodec("gzip-6", "Node gzip bindings are unavailable"))
  }

  const optionalAlternatives = {
    bzip2: bzip2Command
      ? {
          status: "available-but-not-selected",
          command: bzip2Command,
          version: commandVersion(bzip2Command, ["-V"]),
          reason: "Excluded from the bounded packet because it adds a slower, heavyweight stream without an independent-frame random-read advantage.",
        }
      : { status: "unavailable", reason: "bzip2 executable not found" },
    xz: xzCommand
      ? {
          status: "available-but-not-selected",
          command: xzCommand,
          version: commandVersion(xzCommand, ["--version"]),
          reason: "Excluded from the bounded packet because it is a slower archival/interoperability comparison and does not provide independent random-read frames here.",
        }
      : { status: "unavailable", reason: "xz executable not found" },
  }

  return { codecs, optionalAlternatives, discoveredExecutables: { zstdCommand, lz4Command, brotliCommand, bzip2Command, xzCommand } }
}

function fnv1a(seed) {
  let hash = 0x811c9dc5
  for (const byte of Buffer.from(seed, "utf8")) {
    hash ^= byte
    hash = Math.imul(hash, 0x01000193)
  }
  return hash >>> 0 || 0x9e3779b9
}

function appendBuffer(target, offset, source) {
  const length = Math.min(source.length, target.length - offset)
  if (length > 0) source.copy(target, offset, 0, length)
  return offset + Math.max(length, 0)
}

function makeRandomPayload(sizeBytes, seed) {
  const output = Buffer.allocUnsafe(sizeBytes)
  let state = fnv1a(seed)
  for (let offset = 0; offset < output.length; offset += 4) {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    state >>>= 0
    if (output.length - offset >= 4) {
      output.writeUInt32LE(state, offset)
    } else {
      for (let byte = 0; byte < output.length - offset; byte += 1) {
        output[offset + byte] = (state >>> (byte * 8)) & 0xff
      }
    }
  }
  return { buffer: output, details: { generator: "xorshift32", seed } }
}

function makeStructuredPayload(sizeBytes, seed) {
  const output = Buffer.alloc(sizeBytes)
  const repeated = Buffer.from(
    "const result = await driver.read({ path, offset, length });\n" +
      "if (result.status === \"ok\") return result.bytes;\n" +
      "throw new Error(\"backend read failed\");\n",
    "utf8",
  )
  let offset = 0
  let record = 0
  while (offset < output.length) {
    const header = Buffer.from(
      `// seed=${seed}; module=mount-rs; record=${String(record).padStart(8, "0")}; mode=read-write\n`,
      "utf8",
    )
    offset = appendBuffer(output, offset, header)
    while (offset < output.length && (offset % 4096 !== 0 || offset === 0)) {
      offset = appendBuffer(output, offset, repeated)
    }
    record += 1
  }
  return { buffer: output, details: { generator: "source-config-records", records: record, seed } }
}

function makeMetadataPayload(sizeBytes, seed) {
  const output = Buffer.alloc(sizeBytes)
  let offset = 0
  let record = 0
  while (offset < output.length) {
    const row = Buffer.from(
      `${JSON.stringify({
        path: `/tenant-${record % 37}/objects/${String(record).padStart(10, "0")}.bin`,
        size: (record * 4099) % (16 * MIB),
        mode: record % 9 === 0 ? 420 : 33188,
        version: `v${(record % 19) + 1}`,
        etag: `${fnv1a(`${seed}:${record}`).toString(16).padStart(8, "0")}${fnv1a(`${record}:${seed}`).toString(16).padStart(8, "0")}`,
      })}\n`,
      "utf8",
    )
    offset = appendBuffer(output, offset, row)
    record += 1
  }
  return { buffer: output, details: { generator: "newline-delimited-json", records: record, seed } }
}

function makeSmallFilesPayload(sizeBytes, seed) {
  const output = Buffer.alloc(sizeBytes)
  const fileSizes = [64, 128, 256, 1024, 4096, 16 * KIB]
  let offset = 0
  let file = 0
  while (offset < output.length) {
    const fileSize = fileSizes[file % fileSizes.length]
    const header = Buffer.from(
      `path=/small/${String(file).padStart(8, "0")}.dat;length=${fileSize};seed=${seed}\n`,
      "utf8",
    )
    offset = appendBuffer(output, offset, header)
    const body = Buffer.alloc(Math.min(fileSize, output.length - offset))
    body.fill((file * 31 + seed.length) & 0xff)
    offset = appendBuffer(output, offset, body)
    file += 1
  }
  return {
    buffer: output,
    details: { generator: "framed-small-file-corpus", fileCount: file, fileSizes, seed },
  }
}

export function makePayload(workload, sizeBytes, seed = DEFAULT_SEED) {
  if (!WORKLOAD_NAMES.includes(workload)) throw usageError(`unknown workload: ${workload}`)
  if (!Number.isSafeInteger(sizeBytes) || sizeBytes <= 0) throw usageError("payload size must be positive")
  const scopedSeed = `${seed}:${workload}:${sizeBytes}`
  if (workload === "random") return makeRandomPayload(sizeBytes, scopedSeed)
  if (workload === "structured") return makeStructuredPayload(sizeBytes, scopedSeed)
  if (workload === "metadata") return makeMetadataPayload(sizeBytes, scopedSeed)
  return makeSmallFilesPayload(sizeBytes, scopedSeed)
}

function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex")
}

function splitBuffer(buffer, chunkSizeBytes) {
  const chunks = []
  for (let offset = 0; offset < buffer.length; offset += chunkSizeBytes) {
    chunks.push(buffer.subarray(offset, Math.min(offset + chunkSizeBytes, buffer.length)))
  }
  return chunks
}

function numericStats(values) {
  const finiteValues = values.filter((value) => Number.isFinite(value))
  if (finiteValues.length === 0) return { count: 0, median: null, p95: null, p99: null }
  const sorted = [...finiteValues].sort((left, right) => left - right)
  const percentile = (percent) => sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((percent / 100) * sorted.length) - 1))]
  const middle = Math.floor(sorted.length / 2)
  const median = sorted.length % 2 === 0 ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle]
  return { count: finiteValues.length, median, p95: percentile(95), p99: percentile(99) }
}

function memorySnapshot() {
  const rssBytes = Number(process.memoryUsage().rss)
  const usage = typeof process.resourceUsage === "function" ? process.resourceUsage() : null
  const maxRssRaw = usage && Number.isFinite(usage.maxRSS) ? Number(usage.maxRSS) : null
  const maxRssBytes = maxRssRaw === null ? null : process.platform === "win32" ? maxRssRaw : maxRssRaw * 1024
  return { rssBytes, maxRssBytes }
}

function cpuSnapshot() {
  return process.cpuUsage()
}

function cpuDelta(before) {
  const delta = process.cpuUsage(before)
  return {
    userMicros: delta.user,
    systemMicros: delta.system,
    totalMicros: delta.user + delta.system,
  }
}

function verifyParts(parts, expected, expectedDigest) {
  const digest = createHash("sha256")
  let offset = 0
  let bytes = 0
  for (const part of parts) {
    const expectedPart = expected.subarray(offset, offset + part.length)
    if (part.length !== expectedPart.length || !part.equals(expectedPart)) {
      throw new Error(`decoded bytes differ at offset ${offset}`)
    }
    digest.update(part)
    offset += part.length
    bytes += part.length
  }
  const actualDigest = digest.digest("hex")
  if (bytes !== expected.length || actualDigest !== expectedDigest) {
    throw new Error(`decoded length/digest mismatch: ${bytes}/${expected.length} ${actualDigest}/${expectedDigest}`)
  }
  return { bytes, digest: actualDigest }
}

function implication(layout, logicalBytes, chunkSizeBytes, logicalChunkCount, physicalChunkCount) {
  if (layout === "compress-then-chunk") {
    return {
      randomRead: {
        request: "one byte or small range within the logical file",
        logicalBytesDecoded: logicalBytes,
        physicalChunksFetched: physicalChunkCount,
        note: "The packet uses one non-seekable whole-file frame; a random read requires the complete compressed stream and decode.",
        measured: false,
      },
      rewrite: {
        logicalBytesReencoded: logicalBytes,
        physicalChunksRewritten: physicalChunkCount,
        note: "Any logical rewrite invalidates the whole compressed stream and its physical chunks.",
        measured: false,
      },
    }
  }
  return {
    randomRead: {
      request: "one byte or small range within one logical chunk",
      logicalBytesDecoded: chunkSizeBytes,
      physicalChunksFetched: 1,
      note:
        layout === "raw"
          ? "Raw fixed chunks fetch the touched logical chunk with no codec decode. A range crossing a boundary needs two chunks."
          : "Independent frames fetch and decode only the touched logical chunk. A range crossing a boundary needs one frame per touched chunk.",
      measured: false,
    },
    rewrite: {
      logicalBytesReencoded: layout === "raw" ? 0 : chunkSizeBytes,
      physicalChunksRewritten: 1,
      note:
        layout === "raw"
          ? "A rewrite replaces only the touched raw chunk."
          : "A rewrite recompresses only the touched independent frame; unchanged chunks remain reusable.",
      measured: false,
    },
    logicalChunkCount,
  }
}

function runOneIteration(codec, layout, payload, payloadDigest, chunkSizeBytes) {
  const logicalChunkCount = Math.ceil(payload.length / chunkSizeBytes)
  const memoryBefore = memorySnapshot()
  const cpuBefore = cpuSnapshot()
  let encodedBytes
  let physicalChunks
  let frameCount
  let encodeMs
  let decodeMs
  let verificationMs

  const encodeStart = performance.now()
  if (layout === "raw") {
    physicalChunks = splitBuffer(payload, chunkSizeBytes)
    encodedBytes = payload.length
    frameCount = 0
  } else if (layout === "chunk-then-compress") {
    const logicalChunks = splitBuffer(payload, chunkSizeBytes)
    physicalChunks = logicalChunks.map((chunk) => codec.compress(chunk))
    encodedBytes = physicalChunks.reduce((total, chunk) => total + chunk.length, 0)
    frameCount = physicalChunks.length
  } else if (layout === "compress-then-chunk") {
    const frame = codec.compress(payload)
    encodedBytes = frame.length
    physicalChunks = splitBuffer(frame, chunkSizeBytes)
    frameCount = 1
  } else {
    throw new Error(`unknown layout: ${layout}`)
  }
  encodeMs = performance.now() - encodeStart

  const decodeStart = performance.now()
  let decodedParts
  if (layout === "raw") {
    decodedParts = physicalChunks
  } else if (layout === "chunk-then-compress") {
    decodedParts = physicalChunks.map((chunk, index) => {
      const expectedLength = Math.min(chunkSizeBytes, payload.length - index * chunkSizeBytes)
      return codec.decompress(chunk, expectedLength)
    })
  } else {
    decodedParts = [codec.decompress(Buffer.concat(physicalChunks), payload.length)]
  }
  decodeMs = performance.now() - decodeStart

  const verifyStart = performance.now()
  const verification = verifyParts(decodedParts, payload, payloadDigest)
  verificationMs = performance.now() - verifyStart
  const cpu = cpuDelta(cpuBefore)
  const memoryAfter = memorySnapshot()
  const sizeRatio = encodedBytes / payload.length
  const compressionRatio = payload.length / encodedBytes

  return {
    encodeMs,
    decodeMs,
    transformMs: encodeMs + decodeMs,
    verificationMs,
    encodedBytes,
    storedBytesPayloadOnly: encodedBytes,
    sizeRatio,
    compressionRatio,
    savingsPercent: (1 - sizeRatio) * 100,
    encodeMiBPerSecond: encodeMs > 0 ? payload.length / MIB / (encodeMs / 1000) : null,
    decodeMiBPerSecond: decodeMs > 0 ? payload.length / MIB / (decodeMs / 1000) : null,
    cpu,
    memory: {
      rssBeforeBytes: memoryBefore.rssBytes,
      rssAfterBytes: memoryAfter.rssBytes,
      rssDeltaBytes: memoryAfter.rssBytes - memoryBefore.rssBytes,
      maxRssBeforeBytes: memoryBefore.maxRssBytes,
      maxRssAfterBytes: memoryAfter.maxRssBytes,
      maxRssDeltaBytes:
        memoryBefore.maxRssBytes === null || memoryAfter.maxRssBytes === null
          ? null
          : Math.max(0, memoryAfter.maxRssBytes - memoryBefore.maxRssBytes),
      note:
        codec.external
          ? "Parent-process RSS/high-water only; external codec child memory is not included."
          : "Parent process RSS and process-lifetime high-water delta; not an isolated allocator peak for this lane.",
    },
    verification,
  }
}

function laneSummary(iterations) {
  const field = (name) => numericStats(iterations.map((item) => item[name]))
  const memoryField = (name) => numericStats(iterations.map((item) => item.memory[name]).filter((value) => value !== null))
  return {
    encodeMs: field("encodeMs"),
    decodeMs: field("decodeMs"),
    transformMs: field("transformMs"),
    verificationMs: field("verificationMs"),
    encodeMiBPerSecond: field("encodeMiBPerSecond"),
    decodeMiBPerSecond: field("decodeMiBPerSecond"),
    cpuUserMicros: numericStats(iterations.map((item) => item.cpu.userMicros)),
    cpuSystemMicros: numericStats(iterations.map((item) => item.cpu.systemMicros)),
    cpuTotalMicros: numericStats(iterations.map((item) => item.cpu.totalMicros)),
    encodedBytes: field("encodedBytes"),
    sizeRatio: field("sizeRatio"),
    compressionRatio: field("compressionRatio"),
    savingsPercent: field("savingsPercent"),
    rssDeltaBytes: memoryField("rssDeltaBytes"),
    maxRssDeltaBytes: memoryField("maxRssDeltaBytes"),
  }
}

function errorRecord(error) {
  return {
    name: error?.name ?? "Error",
    message: String(error?.message ?? error).slice(0, 500),
    code: error?.code ?? null,
  }
}

function runLane({ codec, codecId, layout, payload, payloadDigest, workload, sizeBytes, chunkSizeBytes, payloadDetails, options }) {
  const logicalChunkCount = Math.ceil(sizeBytes / chunkSizeBytes)
  const physicalChunkCountEstimate = layout === "compress-then-chunk" ? null : logicalChunkCount
  const iterations = []
  for (let warmup = 0; warmup < options.warmup; warmup += 1) {
    runOneIteration(codec, layout, payload, payloadDigest, chunkSizeBytes)
  }
  for (let iteration = 0; iteration < options.iterations; iteration += 1) {
    iterations.push(runOneIteration(codec, layout, payload, payloadDigest, chunkSizeBytes))
  }

  const first = iterations[0]
  const physicalChunkCount =
    layout === "compress-then-chunk"
      ? Math.ceil(first.encodedBytes / chunkSizeBytes)
      : physicalChunkCountEstimate
  const frameCount = layout === "raw" ? 0 : layout === "chunk-then-compress" ? logicalChunkCount : 1
  const memoryStatus = codec.external ? "parent-process-only" : "parent-process"

  return {
    status: "ok",
    workload,
    workloadDescription: WORKLOADS[workload].description,
    workloadSizeBytes: sizeBytes,
    workloadDetails: payloadDetails,
    payloadSha256: payloadDigest,
    chunkSizeBytes,
    logicalChunkCount,
    physicalChunkCount,
    frameCount,
    codec: codecId,
    codecVersion: codec.version,
    codecImplementation: codec.implementation,
    codecFormat: codec.format,
    layout,
    randomReadRewrite: implication(layout, sizeBytes, chunkSizeBytes, logicalChunkCount, physicalChunkCount),
    memoryMeasurement: memoryStatus,
    iterations,
    summary: laneSummary(iterations),
  }
}

function rawCodec() {
  return {
    id: "raw",
    status: "available",
    version: process.version,
    implementation: "buffer-slices",
    format: "raw bytes",
    external: false,
  }
}

function selectedLayouts(codecId) {
  return codecId === "raw" ? ["raw"] : ["chunk-then-compress", "compress-then-chunk"]
}

function gitMetadata() {
  const run = (args) => {
    const result = spawnSync("git", ["-C", ROOT, ...args], { encoding: "utf8", timeout: 5_000 })
    return result.status === 0 ? result.stdout.trim() : null
  }
  const head = run(["rev-parse", "HEAD"])
  const status = run(["status", "--porcelain=v1"])
  const entries = status ? status.split(/\r?\n/).filter(Boolean).length : status === "" ? 0 : null
  return { head, dirty: entries === null ? null : entries > 0, dirtyEntryCount: entries }
}

function buildReport(options, cases, discovery, results, payloadGroups, startedAt, finishedAt) {
  const unavailable = [...discovery.codecs.values()]
    .filter((codec) => options.codecs.includes(codec.id) && codec.status !== "available")
    .map(({ id, status, reason }) => ({ id, status, reason }))
  const errors = results.filter((result) => result.status === "error")
  const status = errors.length > 0 ? "completed-with-errors" : unavailable.length > 0 ? "completed-with-unavailable-codecs" : "completed"
  const selectedCodecInventory = Object.fromEntries(
    options.codecs.map((id) => {
      const codec = discovery.codecs.get(id)
      return [
        id,
        codec
          ? {
              id: codec.id,
              status: codec.status,
              reason: codec.reason ?? null,
              implementation: codec.implementation ?? null,
              version: codec.version ?? null,
              format: codec.format ?? null,
            }
          : { id, status: "unknown", reason: "not detected" },
      ]
    }),
  )
  return {
    schemaVersion: "mount-rs.compression-benchmark.v1",
    status,
    generatedAt: new Date().toISOString(),
    run: {
      profile: options.profile,
      customMatrix: options.custom,
      seed: options.seed,
      iterations: options.iterations,
      warmup: options.warmup,
      maxCases: options.maxCases,
      caseSettings: cases.length,
      payloadGroups,
      laneResults: results.length,
      successfulLanes: results.filter((result) => result.status === "ok").length,
      failedLanes: errors.length,
      unavailableCodecs: unavailable,
      elapsedMs: finishedAt - startedAt,
    },
    environment: {
      repositoryRoot: ROOT,
      sourceControl: gitMetadata(),
      node: process.version,
      nodeVersions: {
        zstd: process.versions.zstd ?? null,
        brotli: process.versions.brotli ?? null,
        zlib: process.versions.zlib ?? null,
        v8: process.versions.v8 ?? null,
      },
      platform: process.platform,
      arch: process.arch,
      osRelease: osRelease(),
      codecInventory: selectedCodecInventory,
      discoveredExecutables: discovery.discoveredExecutables,
      optionalAlternatives: discovery.optionalAlternatives,
    },
    measurement: {
      timer: "performance.now() wall-clock milliseconds",
      cpu: "process.cpuUsage() around encode/decode; external codec child CPU is excluded",
      memory: "process.memoryUsage().rss plus process.resourceUsage().maxRSS high-water delta when available; not an isolated peak",
      validation: "every decoded lane is byte- and SHA-256-validated before recording success",
      dataGeneration: "outside timed regions",
      storageAccounting: "encoded codec payload bytes only; no unimplemented envelope/index/network/fsync bytes are invented",
    },
    layouts: {
      raw: "fixed logical chunks with no codec frame",
      "chunk-then-compress": "fixed logical chunks, one independent codec frame per chunk",
      "compress-then-chunk": "one whole-file codec frame split into fixed physical chunks; frame is not seekable in this packet",
    },
    formatRecommendations: FORMAT_RECOMMENDATIONS,
    limitations: [
      "No mounted-path, storage-provider, fsync, network, encryption, or database transaction measurement.",
      "Random-read and rewrite fields are explicit structural implications, not latency measurements.",
      "No dictionaries, trained corpora, deduplication experiment, corruption/reopen test, or mixed-version decoder test.",
      "LZ4 runs through a separate external process per frame; wall time includes process startup, while parent CPU/RSS fields exclude child resources.",
      "Brotli quality 4 and gzip/DEFLATE level 6 are selected alternatives; detected bzip2/xz are recorded but intentionally excluded from this bounded packet.",
      "The packet reports payload compression bytes, not a final BlockStore envelope or provider index size.",
    ],
    results,
  }
}

export async function runBenchmark(options) {
  if (options.help) return { help: true, text: helpText() }
  const cases = buildCases(options)
  const discovery = detectCodecs()
  const startedAt = performance.now()
  const results = []
  let payloadGroups = 0
  let currentGroupKey = null
  let currentPayloadInfo = null
  let currentPayloadDigest = null

  for (const setting of cases) {
    const groupKey = `${setting.workload}:${setting.sizeBytes}`
    if (groupKey !== currentGroupKey) {
      currentGroupKey = groupKey
      payloadGroups += 1
      currentPayloadInfo = makePayload(setting.workload, setting.sizeBytes, options.seed)
      currentPayloadDigest = sha256(currentPayloadInfo.buffer)
    }
    for (const codecId of options.codecs) {
      const codec = codecId === "raw" ? rawCodec() : discovery.codecs.get(codecId)
      if (!codec || codec.status !== "available") continue
      for (const layout of selectedLayouts(codecId)) {
        try {
          results.push(
            runLane({
              codec,
              codecId,
              layout,
              payload: currentPayloadInfo.buffer,
              payloadDigest: currentPayloadDigest,
              workload: setting.workload,
              sizeBytes: setting.sizeBytes,
              chunkSizeBytes: setting.chunkSizeBytes,
              payloadDetails: currentPayloadInfo.details,
              options,
            }),
          )
        } catch (error) {
          results.push({
            status: "error",
            workload: setting.workload,
            workloadSizeBytes: setting.sizeBytes,
            chunkSizeBytes: setting.chunkSizeBytes,
            codec: codecId,
            codecVersion: codec.version,
            codecImplementation: codec.implementation,
            layout,
            error: errorRecord(error),
          })
        }
      }
    }
  }

  const finishedAt = performance.now()
  return buildReport(options, cases, discovery, results, payloadGroups, startedAt, finishedAt)
}

function isMainModule() {
  return process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)
}

async function main() {
  try {
    const options = parseArgs(process.argv.slice(2))
    if (options.help) {
      process.stdout.write(helpText())
      return
    }
    const report = await runBenchmark(options)
    if (options.output) {
      writeFileSync(options.output, `${JSON.stringify(report, null, 2)}\n`, "utf8")
      process.stdout.write(
        `${JSON.stringify({ status: report.status, output: options.output, laneResults: report.run.laneResults, elapsedMs: report.run.elapsedMs })}\n`,
      )
    } else {
      process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)
    }
    if (report.status === "completed-with-errors") process.exitCode = 1
  } catch (error) {
    if (error?.code === "BENCHMARK_USAGE") {
      process.stderr.write(`benchmark usage error: ${error.message}\n`)
      process.stderr.write(`Run with --help for usage.\n`)
      process.exitCode = 2
      return
    }
    process.stderr.write(`compression benchmark failed: ${error?.stack ?? error}\n`)
    process.exitCode = 1
  }
}

if (isMainModule()) await main()
