import assert from "node:assert/strict"

import root from "../index.js"
import p9 from "../p9.cjs"

const { Filesystem } = root
const {
  P9Session,
  P9_NOTAG,
  P9_RVERSION,
  P9_RLERROR,
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

  const structuralDriver = {
    stat: (...args) => filesystem.stat(...args),
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
  await structuralSession.destroy()
  structuralSession = undefined
  assert.equal((await filesystem.stat("/")).isDirectory(), true)
} finally {
  if (structuralSession) await structuralSession.destroy()
  await session.destroy()
  assert.equal(session.destroyed, true)
  await filesystem.shutdown()
}

console.log("mount-rs N-API direct P9 session: PASS")
