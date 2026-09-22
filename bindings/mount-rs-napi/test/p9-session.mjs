import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"

import root from "../index.js"
import p9 from "../p9.cjs"

const { Filesystem } = root
const {
  P9Session,
  P9_GETATTR_BASIC,
  P9_NOTAG,
  P9_RATTACH,
  P9_RFLUSH,
  P9_RGETATTR,
  P9_RVERSION,
  P9_RLERROR,
  P9_TATTACH,
  P9_TFLUSH,
  P9_TGETATTR,
  P9_TVERSION,
  encodeMessage,
} = p9

function waitUntil(predicate, label) {
  const deadline = Date.now() + 5_000
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (predicate()) {
        resolve()
        return
      }
      if (Date.now() >= deadline) {
        reject(new Error(`${label} timed out`))
        return
      }
      setImmediate(poll)
    }
    poll()
  })
}

function messageHeader(bytes) {
  return p9.readHeader(new p9.P9Reader(bytes))
}

function rlerrorCode(bytes) {
  return p9.decodeMessageAs(bytes, p9.readRlerror).value.ecode
}

function publicSessionMembers(value) {
  const members = new Set()
  for (let current = value; current && current !== Object.prototype; current = Object.getPrototypeOf(current)) {
    for (const name of Object.getOwnPropertyNames(current)) {
      if (name !== "constructor") members.add(name)
    }
  }
  return [...members].sort()
}

const expectedSessionMembers = [
  "assertions",
  "destroy",
  "destroyed",
  "driver",
  "fids",
  "generation",
  "handleCall",
  "inflight",
  "locks",
  "msize",
  "options",
  "stats",
  "userFor",
  "version",
]

const filesystem = Filesystem.memory()
const reports = []
const assertions = []
const session = new P9Session(filesystem, {
  msize: 32 * 1024,
  debug: true,
  onError(error, header) {
    reports.push({ error, header })
  },
  onAssertion(message) {
    assertions.push(message)
  },
})
let structuralSession
let releaseBlockedStat = () => {}

try {
  assert.ok(session.driver instanceof Filesystem)
  assert.equal(session.options.msize, 32 * 1024)
  assert.equal(session.options.useDriverIno, true)
  assert.equal(session.options.readOnly, false)
  assert.equal(session.options.claimOwnership, true)
  assert.equal(session.options.debug, true)
  assert.equal(session.options.onError, undefined)
  assert.equal(session.options.onAssertion, undefined)
  assert.equal(session.msize, undefined)
  assert.equal(session.version, undefined)
  assert.equal(session.destroyed, false)
  assert.deepEqual(publicSessionMembers(session), expectedSessionMembers)
  assert.ok(session.locks)
  assert.ok(session.fids)
  assert.ok(session.stats)
  assert.deepEqual(session.assertions, [])

  if (process.env.MOUNTX_SOURCE) {
    const oraclePath = pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/9p/session.ts`).href
    const { P9Session: OracleP9Session } = await import(oraclePath)
    const { createMemoryDriver } = await import(
      pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/drivers/memory.ts`).href,
    )
    const oracleSession = new OracleP9Session(createMemoryDriver())
    try {
      assert.deepEqual(publicSessionMembers(oracleSession), expectedSessionMembers)
    } finally {
      await oracleSession.destroy()
    }
  }

  assert.equal(await session.handleCall(Buffer.from([1, 2, 3])), null)
  await waitUntil(() => reports.length === 1, "direct 9P malformed-frame callback")
  assert.equal(reports[0].error.code, "EPROTO")
  assert.equal(reports[0].header, undefined)

  const version = await session.handleCall(
    encodeMessage(P9_TVERSION, P9_NOTAG, (writer) => {
      writer.u32(32 * 1024)
      writer.string("9P2000.L")
    }),
  )
  assert.ok(Buffer.isBuffer(version))
  assert.equal(version[4], P9_RVERSION)
  assert.equal(version.readUInt32LE(7), 32 * 1024)
  assert.equal(session.msize, 32 * 1024)
  assert.equal(session.version, "9P2000.L")

  const unsupported = await session.handleCall(encodeMessage(250, 17, () => {}))
  assert.ok(Buffer.isBuffer(unsupported))
  assert.equal(unsupported[4], P9_RLERROR)
  await waitUntil(() => reports.length === 2, "direct 9P protocol-error callback")
  assert.equal(reports[1].error.code, "ENOTSUP")
  assert.deepEqual(reports[1].header, { size: 7, type: 250, tag: 17 })
  assert.deepEqual(assertions, [])

  let blockNextStat = false
  let statStarted = false
  let statGate = Promise.resolve()
  const holdNextStat = () => {
    blockNextStat = true
    statStarted = false
    statGate = new Promise((resolve) => {
      releaseBlockedStat = () => {
        resolve()
        releaseBlockedStat = () => {}
      }
    })
  }
  const structuralDriver = {
    stat: async (...args) => {
      if (blockNextStat) {
        blockNextStat = false
        statStarted = true
        await statGate
      }
      return filesystem.stat(...args)
    },
    readdir: (...args) => filesystem.readdir(...args),
    open: (...args) => filesystem.open(...args),
  }
  structuralSession = new P9Session(structuralDriver, { msize: 16 * 1024 })
  assert.ok(structuralSession instanceof P9Session)
  assert.ok(structuralSession.driver instanceof Filesystem)
  const structuralVersion = await structuralSession.handleCall(
    encodeMessage(P9_TVERSION, P9_NOTAG, (writer) => {
      writer.u32(16 * 1024)
      writer.string("9P2000.L")
    }),
  )
  assert.ok(Buffer.isBuffer(structuralVersion))
  assert.equal(structuralVersion[4], P9_RVERSION)

  const attached = await structuralSession.handleCall(
    encodeMessage(P9_TATTACH, 1, (writer) => {
      writer.writeTattach({
        fid: 1,
        afid: 0xffff_ffff,
        uname: "node",
        aname: "",
        nUname: 0xffff_ffff,
      })
    }),
  )
  assert.equal(messageHeader(attached).type, P9_RATTACH)
  assert.equal(structuralSession.fids.size, 1)

  holdNextStat()
  let getattrSettled = false
  const pendingGetattr = structuralSession.handleCall(
    encodeMessage(P9_TGETATTR, 7, (writer) => {
      writer.writeTgetattr({ fid: 1, requestMask: P9_GETATTR_BASIC })
    }),
  ).then((reply) => {
    getattrSettled = true
    return reply
  })
  await waitUntil(
    () => statStarted && structuralSession.inflight === 1,
    "direct 9P Tgetattr in-flight state",
  )
  let flushSettled = false
  const pendingFlush = structuralSession.handleCall(
    encodeMessage(P9_TFLUSH, 8, (writer) => {
      writer.writeTflush({ oldtag: 7 })
    }),
  ).then((reply) => {
    flushSettled = true
    return reply
  })
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(getattrSettled, false)
  assert.equal(flushSettled, false)
  releaseBlockedStat()
  const [getattrReply, flushReply] = await Promise.all([pendingGetattr, pendingFlush])
  assert.equal(messageHeader(getattrReply).type, P9_RGETATTR)
  assert.equal(messageHeader(flushReply).type, P9_RFLUSH)
  assert.equal(structuralSession.stats.flushed, 1)
  assert.equal(structuralSession.inflight, 0)

  const generationBeforeReset = structuralSession.generation
  holdNextStat()
  const pendingReset = structuralSession.handleCall(
    encodeMessage(P9_TGETATTR, 9, (writer) => {
      writer.writeTgetattr({ fid: 1, requestMask: P9_GETATTR_BASIC })
    }),
  )
  await waitUntil(
    () => statStarted && structuralSession.inflight === 1,
    "direct 9P reset in-flight state",
  )
  const resetReply = await structuralSession.handleCall(
    encodeMessage(P9_TVERSION, P9_NOTAG, (writer) => {
      writer.writeTversion({ msize: 8 * 1024, version: "9P2000.L" })
    }),
  )
  assert.equal(messageHeader(resetReply).type, P9_RVERSION)
  assert.equal(structuralSession.generation, generationBeforeReset + 1)
  assert.equal(structuralSession.msize, 8 * 1024)
  assert.equal(structuralSession.fids.size, 0)
  assert.equal(structuralSession.userFor(1), undefined)
  releaseBlockedStat()
  const resetStaleReply = await pendingReset
  assert.equal(messageHeader(resetStaleReply).type, P9_RLERROR)
  assert.equal(rlerrorCode(resetStaleReply), 5)
  assert.equal(structuralSession.inflight, 0)

  const reattached = await structuralSession.handleCall(
    encodeMessage(P9_TATTACH, 1, (writer) => {
      writer.writeTattach({
        fid: 1,
        afid: 0xffff_ffff,
        uname: "node",
        aname: "",
        nUname: 0xffff_ffff,
      })
    }),
  )
  assert.equal(messageHeader(reattached).type, P9_RATTACH)
  const generationBeforeDestroy = structuralSession.generation
  holdNextStat()
  const pendingDestroy = structuralSession.handleCall(
    encodeMessage(P9_TGETATTR, 10, (writer) => {
      writer.writeTgetattr({ fid: 1, requestMask: P9_GETATTR_BASIC })
    }),
  )
  await waitUntil(
    () => statStarted && structuralSession.inflight === 1,
    "direct 9P destroy in-flight state",
  )
  let destroySettled = false
  const destroyPromise = structuralSession.destroy().then(() => {
    destroySettled = true
  })
  await waitUntil(() => destroySettled, "direct 9P destroy cancellation")
  assert.equal(structuralSession.destroyed, true)
  assert.equal(structuralSession.msize, undefined)
  assert.equal(structuralSession.fids.size, 0)
  assert.equal(structuralSession.generation, generationBeforeDestroy + 1)
  assert.equal(structuralSession.inflight, 0)
  releaseBlockedStat()
  const destroyStaleReply = await pendingDestroy
  await destroyPromise
  assert.equal(messageHeader(destroyStaleReply).type, P9_RLERROR)
  assert.equal(rlerrorCode(destroyStaleReply), 19)
  assert.equal((await filesystem.stat("/")).isDirectory(), true)
} finally {
  if (releaseBlockedStat) releaseBlockedStat()
  if (structuralSession) await structuralSession.destroy()
  await session.destroy()
  assert.equal(session.destroyed, true)
  await filesystem.shutdown()
}

console.log("mount-rs N-API direct P9 session: PASS")
