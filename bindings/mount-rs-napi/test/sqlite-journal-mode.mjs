import assert from "node:assert/strict"
import { access, mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { test } from "node:test"

import { __napiBindingTarget, createChunkedDriver } from "../index.js"
import { validateCompactLayoutReceipt } from "../../../benchmarks/storage/compact-layout.mjs"

test("native SQLite journal selection preserves bytes and compact authority after reopen", { timeout: 30_000 }, async (context) => {
  assert.equal(__napiBindingTarget, "native", "this gate requires the actual native addon")
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-sqlite-journal-"))
  const drivers = new Set()
  const files = new Set()
  const databases = new Set()
  const observations = []
  let retainFixture = false
  const retainOnAbort = () => {
    retainFixture = true
    console.error(`SQLite journal fixture retained after test cancellation: ${directory}`)
  }
  context.signal.addEventListener("abort", retainOnAbort, { once: true })

  const open = async (options) => {
    const filesystem = await createChunkedDriver(options)
    drivers.add(filesystem)
    return filesystem
  }
  const close = async (filesystem) => {
    try {
      await filesystem.shutdown()
      drivers.delete(filesystem)
    } catch (error) {
      retainFixture = true
      throw error
    }
  }
  const closeFile = async (handle) => {
    try {
      await handle.close()
      files.delete(handle)
    } catch (error) {
      retainFixture = true
      throw error
    }
  }
  const stores = (name, sameFile = false, modes = {}) => {
    const metadata = join(directory, `${name}-metadata.sqlite`)
    const blocks = sameFile ? metadata : join(directory, `${name}-blocks.sqlite`)
    return {
      metadata: { kind: "sqlite", uri: metadata, ...(modes.metadata === undefined ? {} : { journalMode: modes.metadata }) },
      blocks: { kind: "sqlite", uri: blocks, ...(modes.blocks === undefined ? {} : { journalMode: modes.blocks }) },
      chunkSize: 4096,
      owner: `sqlite-journal-${name}`,
    }
  }
  const assertMode = async (options, modes) => {
    for (const role of ["metadata", "blocks"]) {
      const path = options[role].uri
      const expected = modes[role]
      const header = await readFile(path)
      assert.equal(header.subarray(0, 16).toString("latin1"), "SQLite format 3\0", `${role} SQLite header`)
      assert.deepEqual([...header.subarray(18, 20)], expected === "wal" ? [2, 2] : [1, 1], `${role} persisted ${expected} header`)
      // Query through an independent read-only Node 24 connection; no PRAGMA
      // assignment can accidentally enable WAL while observing the result.
      const database = new DatabaseSync(path, { readOnly: true })
      databases.add(database)
      try {
        assert.equal(database.prepare("PRAGMA journal_mode").get().journal_mode, expected, `${role} effective journal mode`)
      } finally {
        try {
          database.close()
          databases.delete(database)
        } catch (error) {
          retainFixture = true
          throw error
        }
      }
    }
  }
  const payload = Buffer.alloc(4096 * 3 + 173)
  for (let index = 0; index < payload.length; index++) payload[index] = (index * 17 + 29) % 251
  const assertPayload = async (filesystem) => {
    assert.deepEqual(Buffer.from(await filesystem.readFile("/oracle")), payload, "full persisted payload")
    assert.equal((await filesystem.stat("/oracle")).size, payload.length, "persisted stat size")
    const handle = await filesystem.open("/oracle", "r")
    files.add(handle)
    try {
      const target = Buffer.alloc(payload.length + 1, 0xff)
      const result = await handle.read(target, 0, target.length, 0)
      assert.equal(result.bytesRead, payload.length, "fresh handle exact byte count")
      assert.deepEqual(target.subarray(0, result.bytesRead), payload, "fresh handle full payload")
      assert.equal(target[payload.length], 0xff, "read stops at exact EOF")
      assert.equal((await handle.read(target, 0, 1, payload.length)).bytesRead, 0, "fresh handle EOF")
    } finally {
      await closeFile(handle)
    }
  }
  const exercise = async (name, sameFile, modes) => {
    // Separate files qualify the existing compact MRC5 path. Same-file cases
    // exercise journal conversion through the ordinary chunked filesystem.
    const options = { ...stores(name, sameFile, modes), ...(sameFile ? {} : { concurrentWrites: true, inodeUpdates: true, compactInodeUpdates: true }) }
    const effective = Object.fromEntries(Object.entries(modes).map(([role, mode]) => [role, mode === "wal" ? "wal" : "delete"]))
    const first = await open(options)
    await first.writeFile("/oracle", payload)
    await assertPayload(first)
    const receipt = sameFile ? null : validateCompactLayoutReceipt(await first.inspectCompactLayout())
    await close(first)
    await assertMode(options, effective)

    for (const selection of ["omitted", "preserve"]) {
      const reopenedOptions = {
        ...options,
        metadata: { kind: "sqlite", uri: options.metadata.uri, ...(selection === "omitted" ? {} : { journalMode: "preserve" }) },
        blocks: { kind: "sqlite", uri: options.blocks.uri, ...(selection === "omitted" ? {} : { journalMode: "preserve" }) },
      }
      const reopened = await open(reopenedOptions)
      await assertPayload(reopened)
      if (receipt !== null) {
        assert.deepEqual(validateCompactLayoutReceipt(await reopened.inspectCompactLayout()), receipt, `${selection} reopen retains exact MRC5 authority and generation`)
      }
      await close(reopened)
      await assertMode(options, effective)
    }
    observations.push({ name, sameFile, effective, receipt })
  }

  let failure
  const cleanupErrors = []
  try {
    if (process.platform === "win32") {
      // Windows has no qualified local-file guard for explicit provider WAL.
      // Validate that boundary instead of reporting persistence acceptance.
      for (const role of ["metadata", "blocks"]) {
        const modes = { metadata: "preserve", blocks: "preserve", [role]: "wal" }
        await assert.rejects(() => open(stores(`windows-${role}`, false, modes)), { code: "ENOTSUP" })
      }
    } else {
      // Keep WAL first: the old native addon ignores journalMode and leaves
      // these files in DELETE mode, producing a semantic header failure.
      await exercise("wal-separate", false, { metadata: "wal", blocks: "wal" })
      await exercise("wal-same", true, { metadata: "wal", blocks: "wal" })
      await exercise("preserve-separate", false, { metadata: "preserve", blocks: "preserve" })
      await exercise("preserve-same", true, { metadata: "preserve", blocks: "preserve" })
      await exercise("metadata-wal", false, { metadata: "wal", blocks: "preserve" })
      await exercise("blocks-wal", false, { metadata: "preserve", blocks: "wal" })
    }

    let rejection = 0
    const rejectBeforeOpen = async (role, invalidStore, actualTargets = []) => {
      const options = stores(`invalid-${rejection++}`, false, { metadata: "preserve", blocks: "preserve" })
      const counterpart = options[role === "metadata" ? "blocks" : "metadata"].uri
      const invalidPath = options[role].uri
      options[role] = { ...options[role], ...invalidStore }
      await assert.rejects(() => open(options), (error) => {
        assert.equal(error.code, "EINVAL", `${role} invalid journal configuration`)
        assert.equal(error.syscall, "createChunkedDriver")
        return true
      })
      for (const path of [counterpart, invalidPath, ...actualTargets]) {
        await assert.rejects(() => access(path), { code: "ENOENT" }, "validation precedes either backing file creation")
      }
    }
    for (const role of ["metadata", "blocks"]) {
      for (const journalMode of ["delete", "WAL", "", "wal "]) {
        await rejectBeforeOpen(role, { journalMode })
      }
      for (const kind of ["memory", "pglite", "tidb", "foundationdb", "r2", "rustfs"]) {
        for (const journalMode of ["preserve", "wal"]) {
          await rejectBeforeOpen(role, { kind, journalMode })
        }
      }
      for (const uri of ["", ":memory:", "file::memory:?cache=shared"]) {
        await rejectBeforeOpen(role, { uri, journalMode: "wal" })
      }
      const uriTarget = join(directory, `${role}-invalid-file-uri.sqlite`)
      await rejectBeforeOpen(role, { uri: `file:${uriTarget}`, journalMode: "wal" }, [uriTarget])
    }
  } catch (error) {
    failure = error
  } finally {
    for (const handle of files) {
      try { await closeFile(handle) } catch (error) { cleanupErrors.push(error) }
    }
    for (const filesystem of drivers) {
      try { await close(filesystem) } catch (error) { cleanupErrors.push(error) }
    }
    for (const database of databases) {
      try { database.close(); databases.delete(database) } catch (error) {
        retainFixture = true
        cleanupErrors.push(error)
      }
    }
    try {
      if (retainFixture || context.signal.aborted || drivers.size || files.size || databases.size) {
        console.error(`SQLite journal fixture retained because close or shutdown is unresolved: ${directory}`)
      } else {
        await rm(directory, { recursive: true, force: true })
      }
    } finally {
      // The runner also aborts this signal after ordinary test completion.
      context.signal.removeEventListener("abort", retainOnAbort)
    }
  }
  if (failure) throw failure
  if (cleanupErrors.length) throw new AggregateError(cleanupErrors, "SQLite journal cleanup failed")
  console.log(JSON.stringify({
    schema: "mount-rs.sqlite-journal-native-test.v1",
    status: "passed",
    platform: process.platform,
    scope: process.platform === "win32" ? "qualified-local-file-guard-unsupported-and-config-rejections" : "native-journal-selection-and-clean-reopen",
    observations,
    crashDurabilityProof: false,
  }))
})
