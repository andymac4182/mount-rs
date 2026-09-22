import { existsSync } from "node:fs"
import { mkdtemp, rm } from "node:fs/promises"
import { execFileSync } from "node:child_process"
import { tmpdir } from "node:os"
import { createRequire } from "node:module"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

export const DEFAULT_CHUNK_SIZE_BYTES = 64 * 1024
export const MOUNTX_PINNED_REVISION = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8"

const storageDirectory = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(storageDirectory, "../..")
const nativePackagePath = join(repoRoot, "bindings/mount-rs-napi/index.js")
// Keep the oracle reproducible across hosts: callers should prefer an explicit
// MOUNTX_SOURCE, while a checkout at this documented repository-local location
// is also accepted. Never fall back to a host-specific temporary directory.
const repoLocalMountxSource = join(repoRoot, "vendor/mountx")
const requireFromBenchmark = createRequire(import.meta.url)

let nativeModule
const mountxModuleCache = new Map()

function loadNapi() {
  nativeModule ??= requireFromBenchmark(nativePackagePath)
  return nativeModule
}

function firstEnvironmentValue(environment, names) {
  for (const name of names) {
    const value = environment[name]
    if (typeof value === "string" && value.length > 0) return value
  }
  return undefined
}

function readPgliteConfig(environment) {
  const uri = firstEnvironmentValue(environment, [
    "MOUNT_RS_PGLITE_DATABASE_URL",
    "PGLITE_DATABASE_URL",
  ])
  return {
    configured: Boolean(uri),
    uri,
    missing: uri ? [] : ["MOUNT_RS_PGLITE_DATABASE_URL (or PGLITE_DATABASE_URL)"],
  }
}

function readTidbConfig(environment) {
  const uri = firstEnvironmentValue(environment, ["MOUNT_RS_TIDB_URL", "TIDB_URL"])
  return {
    configured: Boolean(uri),
    uri,
    durable: environment.MOUNT_RS_TIDB_DURABLE !== "0",
    missing: uri ? [] : ["MOUNT_RS_TIDB_URL (or TIDB_URL)"],
  }
}

function readFoundationDbConfig(environment) {
  const clusterFile = firstEnvironmentValue(environment, [
    "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE",
    "FOUNDATIONDB_CLUSTER_FILE",
  ])
  const sharedProvider = environment.MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER === "1"
  const authorityPrefix = firstEnvironmentValue(environment, [
    "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX",
  ])
  const missing = []
  if (environment.MOUNT_RS_NAPI_FOUNDATIONDB !== "1") {
    missing.push("MOUNT_RS_NAPI_FOUNDATIONDB=1")
  }
  if (!clusterFile) {
    missing.push(
      "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE (or FOUNDATIONDB_CLUSTER_FILE)",
    )
  }
  if (sharedProvider && !authorityPrefix) {
    missing.push("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX")
  }
  return {
    configured: missing.length === 0,
    clusterFile,
    sharedProvider,
    authorityPrefix,
    leaseAuthority: sharedProvider ? "shared-provider" : "persisted-single-authority",
    missing,
  }
}

function readR2Config(environment) {
  const endpoint = firstEnvironmentValue(environment, ["MOUNT_RS_R2_ENDPOINT", "R2_ENDPOINT"])
  const bucket = firstEnvironmentValue(environment, ["MOUNT_RS_R2_BUCKET", "R2_BUCKET"])
  const accessKeyId = firstEnvironmentValue(environment, [
    "MOUNT_RS_R2_ACCESS_KEY_ID",
    "R2_ACCESS_KEY_ID",
  ])
  const secretAccessKey = firstEnvironmentValue(environment, [
    "MOUNT_RS_R2_SECRET_ACCESS_KEY",
    "R2_SECRET_ACCESS_KEY",
  ])
  const missing = []
  if (!endpoint) missing.push("MOUNT_RS_R2_ENDPOINT (or R2_ENDPOINT)")
  if (!bucket) missing.push("MOUNT_RS_R2_BUCKET (or R2_BUCKET)")
  if (!accessKeyId) missing.push("MOUNT_RS_R2_ACCESS_KEY_ID (or R2_ACCESS_KEY_ID)")
  if (!secretAccessKey) {
    missing.push("MOUNT_RS_R2_SECRET_ACCESS_KEY (or R2_SECRET_ACCESS_KEY)")
  }
  return {
    configured: missing.length === 0,
    endpoint,
    bucket,
    accessKeyId,
    secretAccessKey,
    region: firstEnvironmentValue(environment, ["MOUNT_RS_R2_REGION", "R2_REGION"]),
    durable: environment.MOUNT_RS_R2_DURABLE !== "0",
    missing,
  }
}

function mountxSource(environment) {
  return environment.MOUNTX_SOURCE || repoLocalMountxSource
}

export function mountxSourcePath(environment = process.env) {
  return mountxSource(environment)
}

function gitRevision(source) {
  try {
    return execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: source,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim() || null
  } catch {
    return null
  }
}

function mountxMemoryAvailability(environment) {
  const source = mountxSource(environment)
  const memoryPath = join(source, "src/drivers/memory.ts")
  const harnessPath = join(source, "src/harness.ts")
  if (!existsSync(memoryPath) || !existsSync(harnessPath)) {
    return {
      configured: false,
      missing: environment.MOUNTX_SOURCE
        ? [environment.MOUNTX_SOURCE]
        : ["MOUNTX_SOURCE", "repo-local vendor/mountx checkout"],
      source,
      reason: `actual mountx TypeScript oracle not found at ${source}`,
    }
  }
  const sourceRevision = gitRevision(source)
  const revisionMatchesPinned = sourceRevision === MOUNTX_PINNED_REVISION
  return {
    configured: true,
    missing: [],
    source,
    sourceRevision,
    sourceRevisionVerified: sourceRevision !== null,
    revisionMatchesPinned,
    revisionMismatch: !revisionMatchesPinned,
    reason: sourceRevision
      ? revisionMatchesPinned
        ? undefined
        : `mountx source revision ${sourceRevision} does not match pinned ${MOUNTX_PINNED_REVISION}`
      : `mountx source revision could not be read; expected pinned ${MOUNTX_PINNED_REVISION}`,
  }
}

async function openNapiMemory() {
  const { Filesystem } = loadNapi()
  return { filesystem: Filesystem.memory(), cleanup: async () => {} }
}

async function openNapiSqlite() {
  const { Filesystem } = loadNapi()
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-storage-sqlite-"))
  try {
    const filesystem = await Filesystem.sqlite(join(directory, "filesystem.sqlite"))
    return {
      filesystem,
      cleanup: onceCleanup(filesystem, directory),
    }
  } catch (error) {
    await rm(directory, { recursive: true, force: true })
    throw error
  }
}

async function openNapiPglite(context) {
  const { Filesystem } = loadNapi()
  const { uri } = readPgliteConfig(context.environment)
  const filesystem = await Filesystem.pglite(uri)
  return {
    filesystem,
    cleanup: () => onceCleanupFromFilesystem(filesystem),
  }
}

async function openNapiSplitSqlite(context) {
  const { createChunkedDriver } = loadNapi()
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-storage-split-sqlite-"))
  try {
    const filesystem = await createChunkedDriver({
      metadata: { kind: "sqlite", uri: join(directory, "metadata.sqlite") },
      blocks: { kind: "sqlite", uri: join(directory, "blocks.sqlite") },
      chunkSize: context.chunkSizeBytes,
      owner: `storage-benchmark-${context.runId}`,
    })
    return {
      filesystem,
      cleanup: onceCleanup(filesystem, directory),
    }
  } catch (error) {
    await rm(directory, { recursive: true, force: true })
    throw error
  }
}

async function openNapiSplitPglite(context) {
  const { createChunkedDriver } = loadNapi()
  const { uri } = readPgliteConfig(context.environment)
  const filesystem = await createChunkedDriver({
    metadata: {
      kind: "pglite",
      uri,
      key: `storage-benchmark/${context.runId}/metadata`,
      durable: context.environment.MOUNT_RS_PGLITE_DURABLE === "1",
    },
    blocks: {
      kind: "pglite",
      uri,
      key: `storage-benchmark/${context.runId}/blocks`,
      durable: context.environment.MOUNT_RS_PGLITE_DURABLE === "1",
    },
    chunkSize: context.chunkSizeBytes,
    owner: `storage-benchmark-${context.runId}`,
  })
  return {
    filesystem,
    cleanup: () => onceCleanupFromFilesystem(filesystem),
  }
}

async function openNapiSplitPgliteR2(context) {
  const { createChunkedDriver } = loadNapi()
  const pglite = readPgliteConfig(context.environment)
  const r2 = readR2Config(context.environment)
  const filesystem = await createChunkedDriver({
    metadata: {
      kind: "pglite",
      uri: pglite.uri,
      key: `storage-benchmark/${context.runId}/metadata`,
      durable: context.environment.MOUNT_RS_PGLITE_DURABLE === "1",
    },
    blocks: {
      kind: "r2",
      key: `storage-benchmark/${context.runId}/blocks`,
      endpoint: r2.endpoint,
      bucket: r2.bucket,
      accessKeyId: r2.accessKeyId,
      secretAccessKey: r2.secretAccessKey,
      durable: r2.durable,
    },
    chunkSize: context.chunkSizeBytes,
    owner: `storage-benchmark-${context.runId}`,
  })
  return {
    filesystem,
    cleanup: () => onceCleanupFromFilesystem(filesystem),
  }
}

async function openNapiSplitSqliteR2(context) {
  const { createChunkedDriver } = loadNapi()
  const r2 = readR2Config(context.environment)
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-storage-split-sqlite-r2-"))
  try {
    const filesystem = await createChunkedDriver({
      metadata: {
        kind: "sqlite",
        uri: join(directory, "metadata.sqlite"),
      },
      blocks: {
        kind: "r2",
        key: `storage-benchmark/${context.runId}/sqlite-r2/blocks`,
        endpoint: r2.endpoint,
        bucket: r2.bucket,
        accessKeyId: r2.accessKeyId,
        secretAccessKey: r2.secretAccessKey,
        durable: r2.durable,
      },
      chunkSize: context.chunkSizeBytes,
      owner: `storage-benchmark-${context.runId}`,
    })
    return {
      filesystem,
      cleanup: onceCleanup(filesystem, directory),
    }
  } catch (error) {
    await rm(directory, { recursive: true, force: true })
    throw error
  }
}

async function openNapiSplitTidbR2(context) {
  const { createChunkedDriver } = loadNapi()
  const tidb = readTidbConfig(context.environment)
  const r2 = readR2Config(context.environment)
  const filesystem = await createChunkedDriver({
    metadata: {
      kind: "tidb",
      uri: tidb.uri,
      key: `storage-benchmark/${context.runId}/tidb-r2/metadata`,
      durable: tidb.durable,
    },
    blocks: {
      kind: "r2",
      key: `storage-benchmark/${context.runId}/tidb-r2/blocks`,
      endpoint: r2.endpoint,
      bucket: r2.bucket,
      accessKeyId: r2.accessKeyId,
      secretAccessKey: r2.secretAccessKey,
      durable: r2.durable,
    },
    chunkSize: context.chunkSizeBytes,
    owner: `storage-benchmark-${context.runId}`,
  })
  return {
    filesystem,
    cleanup: () => onceCleanupFromFilesystem(filesystem),
  }
}

async function openNapiSplitFoundationDbR2(context) {
  const { createChunkedDriver } = loadNapi()
  const foundationDb = readFoundationDbConfig(context.environment)
  const r2 = readR2Config(context.environment)
  const metadata = {
    kind: "foundationdb",
    uri: foundationDb.clusterFile,
    key: `storage-benchmark/${context.runId}/foundationdb-r2/metadata`,
    durable: true,
    leaseAuthority: foundationDb.leaseAuthority,
  }
  if (foundationDb.sharedProvider) metadata.authorityPrefix = foundationDb.authorityPrefix
  const filesystem = await createChunkedDriver({
    metadata,
    blocks: {
      kind: "r2",
      key: `storage-benchmark/${context.runId}/foundationdb-r2/blocks`,
      endpoint: r2.endpoint,
      bucket: r2.bucket,
      accessKeyId: r2.accessKeyId,
      secretAccessKey: r2.secretAccessKey,
      durable: r2.durable,
    },
    chunkSize: context.chunkSizeBytes,
    owner: `storage-benchmark-${context.runId}`,
  })
  return {
    filesystem,
    cleanup: () => onceCleanupFromFilesystem(filesystem),
  }
}

async function openMountxMemory(environment) {
  const availability = mountxMemoryAvailability(environment)
  if (!availability.configured) {
    const error = new Error(availability.reason)
    error.code = "ORACLE_UNAVAILABLE"
    throw error
  }
  const source = availability.source
  let modules = mountxModuleCache.get(source)
  if (!modules) {
    modules = {
      memory: await import(pathToFileURL(join(source, "src/drivers/memory.ts")).href),
      harness: await import(pathToFileURL(join(source, "src/harness.ts")).href),
    }
    mountxModuleCache.set(source, modules)
  }
  return {
    filesystem: modules.harness.createLoopback(modules.memory.createMemoryDriver()),
    cleanup: async () => {},
  }
}

async function onceCleanupFromFilesystem(filesystem) {
  if (filesystem && typeof filesystem.shutdown === "function") {
    await filesystem.shutdown()
  }
}

function onceCleanup(filesystem, directory) {
  let complete = false
  return async () => {
    if (complete) return
    complete = true
    let firstError
    try {
      await onceCleanupFromFilesystem(filesystem)
    } catch (error) {
      firstError = error
    }
    try {
      await rm(directory, { recursive: true, force: true })
    } catch (error) {
      firstError ??= error
    }
    if (firstError) throw firstError
  }
}

function provider(specification) {
  return {
    measurementSurface: "direct-api",
    executionSurface: {
      callerRuntime: "node",
      implementationLanguage: specification.implementation === "mount-rs" ? "rust" : "typescript",
      apiBinding: specification.binding,
      nativeAddon: specification.implementation === "mount-rs",
      directRust: "not-run",
    },
    cacheState: "fresh-provider-instance; OS/remote caches uncontrolled",
    ...specification,
  }
}

export function providerDefinitions(environment = process.env) {
  const pglite = readPgliteConfig(environment)
  const tidb = readTidbConfig(environment)
  const foundationDb = readFoundationDbConfig(environment)
  const r2 = readR2Config(environment)
  const oracle = mountxMemoryAvailability(environment)

  return [
    provider({
      id: "mount-rs-memory",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "memfs",
      topology: "combined-filesystem",
      metadataProvider: "memory",
      blockProvider: "memory",
      durabilityClass: "volatile",
      metadataDurabilityClass: "volatile",
      blockDurabilityClass: "volatile",
      synchronizationPolicy: "writeFile resolves after the in-memory handle closes",
      chunking: { algorithm: "none", version: null, chunkSizeBytes: null },
      availability: () => ({ configured: true, missing: [] }),
      create: openNapiMemory,
    }),
    provider({
      id: "mount-rs-sqlite",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "sqlite",
      topology: "combined-filesystem",
      metadataProvider: "sqlite",
      blockProvider: "sqlite",
      durabilityClass: "durable-local-file",
      metadataDurabilityClass: "durable-local-file",
      blockDurabilityClass: "durable-local-file",
      synchronizationPolicy: "writeFile resolves after the SQLite-backed handle closes",
      chunking: { algorithm: "none", version: null, chunkSizeBytes: null },
      availability: () => ({ configured: true, missing: [] }),
      create: openNapiSqlite,
    }),
    provider({
      id: "mount-rs-split-sqlite",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "sqlite",
      blockProvider: "sqlite",
      durabilityClass: "durable-local-file",
      metadataDurabilityClass: "durable-local-file",
      blockDurabilityClass: "durable-local-file",
      synchronizationPolicy: "fixed chunk publication and SQLite provider close",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      availability: () => ({ configured: true, missing: [] }),
      create: openNapiSplitSqlite,
    }),
    provider({
      id: "mount-rs-pglite",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "pglite",
      topology: "combined-filesystem",
      metadataProvider: "pglite",
      blockProvider: "pglite",
      durabilityClass: environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      metadataDurabilityClass:
        environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      blockDurabilityClass:
        environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      synchronizationPolicy: "Filesystem.pglite writeFile and explicit shutdown",
      chunking: { algorithm: "none", version: null, chunkSizeBytes: null },
      requiredEnvVars: pglite.missing,
      availability: () => ({
        configured: pglite.configured,
        missing: pglite.missing,
        reason: pglite.configured
          ? undefined
          : "PGlite URL is absent; no live PGlite result is claimed",
      }),
      create: openNapiPglite,
    }),
    provider({
      id: "mount-rs-split-pglite",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "pglite",
      blockProvider: "pglite",
      durabilityClass: environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      metadataDurabilityClass:
        environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      blockDurabilityClass:
        environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      synchronizationPolicy: "fixed chunk publication and explicit PGlite shutdown",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      requiredEnvVars: pglite.missing,
      availability: () => ({
        configured: pglite.configured,
        missing: pglite.missing,
        reason: pglite.configured
          ? undefined
          : "PGlite URL is absent; no live PGlite result is claimed",
      }),
      create: openNapiSplitPglite,
    }),
    provider({
      id: "mount-rs-split-pglite-r2",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "pglite",
      blockProvider: "cloudflare-r2",
      durabilityClass: "mixed-configured",
      metadataDurabilityClass:
        environment.MOUNT_RS_PGLITE_DURABLE === "1" ? "configured-durable" : "configured-volatile",
      blockDurabilityClass: r2.durable
        ? "configured-durable-remote"
        : "configured-volatile-remote",
      synchronizationPolicy: "block upload barrier before metadata publication; explicit provider shutdown",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      requiredEnvVars: [...pglite.missing, ...r2.missing],
      remoteRegion: r2.region || null,
      availability: () => {
        const missing = [...pglite.missing, ...r2.missing]
        return {
          configured: missing.length === 0,
          missing,
          reason:
            missing.length === 0
              ? undefined
              : "PGlite/R2 configuration is absent; no live remote result is claimed",
        }
      },
      create: openNapiSplitPgliteR2,
    }),
    provider({
      id: "mount-rs-split-sqlite-r2",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "sqlite",
      blockProvider: "cloudflare-r2",
      durabilityClass: "mixed-configured",
      metadataDurabilityClass: "durable-local-file",
      blockDurabilityClass: r2.durable
        ? "configured-durable-remote"
        : "configured-volatile-remote",
      synchronizationPolicy: "SQLite metadata publication after confirmed remote block upload",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      requiredEnvVars: r2.missing,
      remoteRegion: r2.region || null,
      availability: () => ({
        configured: r2.configured,
        missing: r2.missing,
        reason: r2.configured
          ? undefined
          : "R2 configuration is absent; no live remote result is claimed",
      }),
      create: openNapiSplitSqliteR2,
    }),
    provider({
      id: "mount-rs-split-tidb-r2",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "tidb",
      blockProvider: "cloudflare-r2",
      durabilityClass: "mixed-configured",
      metadataDurabilityClass: tidb.durable
        ? "configured-durable-remote"
        : "configured-volatile-remote",
      blockDurabilityClass: r2.durable
        ? "configured-durable-remote"
        : "configured-volatile-remote",
      synchronizationPolicy: "TiDB metadata publication after confirmed remote block upload",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      requiredEnvVars: [...tidb.missing, ...r2.missing],
      remoteRegion: r2.region || null,
      availability: () => {
        const missing = [...tidb.missing, ...r2.missing]
        return {
          configured: missing.length === 0,
          missing,
          reason:
            missing.length === 0
              ? undefined
              : "TiDB/R2 configuration is absent; no live remote result is claimed",
        }
      },
      create: openNapiSplitTidbR2,
    }),
    provider({
      id: "mount-rs-split-foundationdb-r2",
      implementation: "mount-rs",
      binding: "public-napi",
      backend: "fixed-chunked",
      topology: "split-stores",
      metadataProvider: "foundationdb",
      blockProvider: "cloudflare-r2",
      durabilityClass: "mixed-configured",
      metadataDurabilityClass: "configured-durable-provider",
      blockDurabilityClass: r2.durable
        ? "configured-durable-remote"
        : "configured-volatile-remote",
      synchronizationPolicy:
        "FoundationDB metadata publication after confirmed remote block upload and lease authority",
      chunking: {
        algorithm: "fixed-size",
        version: "1",
        chunkSizeBytes: DEFAULT_CHUNK_SIZE_BYTES,
      },
      requiredEnvVars: [...foundationDb.missing, ...r2.missing],
      remoteRegion: r2.region || null,
      availability: () => {
        const missing = [...foundationDb.missing, ...r2.missing]
        return {
          configured: missing.length === 0,
          missing,
          reason:
            missing.length === 0
              ? undefined
              : "FoundationDB/R2 configuration is absent; no live remote result is claimed",
        }
      },
      create: openNapiSplitFoundationDbR2,
    }),
    provider({
      id: "mountx-memory",
      implementation: "mountx",
      binding: "actual-typescript-oracle",
      backend: "memfs",
      topology: "combined-filesystem",
      metadataProvider: "memory",
      blockProvider: "memory",
      durabilityClass: "volatile",
      metadataDurabilityClass: "volatile",
      blockDurabilityClass: "volatile",
      synchronizationPolicy: "mountx loopback writeFile resolves after the in-memory handle closes",
      chunking: { algorithm: "none", version: null, chunkSizeBytes: null },
      requiredEnvVars: ["MOUNTX_SOURCE"],
      availability: () => ({ ...oracle }),
      create: (context) => openMountxMemory(context.environment),
    }),
  ]
}

export function providerById(environment = process.env) {
  return new Map(providerDefinitions(environment).map((definition) => [definition.id, definition]))
}

/** Return loaded native addon paths without exposing native loader internals. */
export function loadedNativeAddonPaths() {
  loadNapi()
  return Object.keys(requireFromBenchmark.cache).filter((path) => path.endsWith(".node"))
}

export function providerSummary(definition, chunkSizeBytes) {
  const chunking =
    definition.chunking.algorithm === "fixed-size" && Number.isInteger(chunkSizeBytes)
      ? { ...definition.chunking, chunkSizeBytes }
      : definition.chunking
  return {
    provider: definition.id,
    implementation: definition.implementation,
    binding: definition.binding,
    backend: definition.backend,
    topology: definition.topology,
    metadataProvider: definition.metadataProvider,
    blockProvider: definition.blockProvider,
    durabilityClass: definition.durabilityClass,
    metadataDurabilityClass: definition.metadataDurabilityClass,
    blockDurabilityClass: definition.blockDurabilityClass,
    synchronizationPolicy: definition.synchronizationPolicy,
    measurementSurface: definition.measurementSurface,
    executionSurface: definition.executionSurface,
    cacheState: definition.cacheState,
    chunking,
    ...(definition.remoteRegion ? { remoteRegion: definition.remoteRegion } : {}),
  }
}
