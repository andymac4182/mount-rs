// Round-trip cost, from JavaScript, of the calls that reach serialized native
// state: FUSE session `handle()` issued one at a time and sixteen at once,
// and response-body chunks pulled one at a time from the S3 and WebDAV
// sessions.
//
//   npx --yes pnpm@11 build        # a release build
//   node bench/serial-calls.mjs
//
// To compare two builds, keep each native module aside and alternate runs
// with NAPI_RS_NATIVE_LIBRARY_PATH=/path/to/mount-rs.<platform>.node, which
// the generated loader honours.
import { createRequire } from "node:module"

const require = createRequire(import.meta.url)
const binding = require("../index.js")
const fuse = require("../fuse.cjs")

const ROUNDS = Number(process.env.SERIAL_BENCH_ROUNDS ?? 7)
const CALLS = Number(process.env.SERIAL_BENCH_CALLS ?? 32_000)
const BODY_BYTES = 64 * 1024 * 1024
const CHUNK_BYTES = 64 * 1024

function frame(opcode, nodeid, body, unique) {
  const payload = Buffer.from(body)
  return Buffer.concat([
    fuse.encodeInHeader({
      len: 40 + payload.length,
      opcode,
      unique,
      nodeid,
      uid: 0,
      gid: 0,
      pid: 0,
      totalExtlen: 0,
    }),
    payload,
  ])
}

function summary(label, rounds) {
  const sorted = [...rounds].sort((a, b) => a.rate - b.rate)
  const best = sorted.at(-1)
  const median = sorted[Math.floor(sorted.length / 2)]
  const micros = (value) => `${value.toFixed(1)}us`
  console.log(
    `${label.padEnd(34)} ops/s best ${best.rate.toFixed(0).padStart(8)} median ${median.rate
      .toFixed(0)
      .padStart(8)} worst ${sorted[0].rate.toFixed(0).padStart(8)} | median round p50 ${micros(
      median.p50,
    )} p99 ${micros(median.p99)}`,
  )
}

function percentiles(latencies) {
  latencies.sort((a, b) => a - b)
  return {
    p50: latencies[Math.floor(latencies.length / 2)],
    p99: latencies[Math.floor(latencies.length * 0.99)],
  }
}

async function fuseRounds(width) {
  const filesystem = binding.Filesystem.memory()
  const session = new binding.FuseSession(filesystem)
  const init = Buffer.alloc(20)
  init.writeUInt32LE(7, 0)
  init.writeUInt32LE(41, 4)
  init.writeUInt32LE(65_536, 8)
  await session.handle(frame(26, 0n, init, 1n))
  const getattr = frame(3, 1n, Buffer.alloc(16), 2n)
  const rounds = []
  for (let round = 0; round <= ROUNDS; round += 1) {
    const latencies = []
    const started = process.hrtime.bigint()
    for (let done = 0; done < CALLS; done += width) {
      const at = process.hrtime.bigint()
      if (width === 1) {
        await session.handle(getattr)
        latencies.push(Number(process.hrtime.bigint() - at) / 1000)
      } else {
        await Promise.all(
          Array.from({ length: width }, () =>
            session.handle(getattr).then(() => {
              latencies.push(Number(process.hrtime.bigint() - at) / 1000)
            }),
          ),
        )
      }
    }
    const seconds = Number(process.hrtime.bigint() - started) / 1e9
    // The first round warms up.
    if (round > 0) rounds.push({ rate: latencies.length / seconds, ...percentiles(latencies) })
  }
  await session.destroy()
  await filesystem.shutdown()
  summary(`fuse handle, ${width} at once`, rounds)
}

async function bodyRounds(label, open) {
  const rounds = []
  for (let round = 0; round <= ROUNDS; round += 1) {
    const response = await open()
    const latencies = []
    let bytes = 0
    const started = process.hrtime.bigint()
    let at = started
    for await (const chunk of response.body) {
      const now = process.hrtime.bigint()
      latencies.push(Number(now - at) / 1000)
      at = now
      bytes += chunk.length
    }
    const seconds = Number(process.hrtime.bigint() - started) / 1e9
    if (bytes !== BODY_BYTES) throw new Error(`${label}: read ${bytes} of ${BODY_BYTES} bytes`)
    if (round > 0) rounds.push({ rate: latencies.length / seconds, ...percentiles(latencies) })
  }
  summary(`${label} body chunks, one at a time`, rounds)
}

await fuseRounds(1)
await fuseRounds(16)

const body = Buffer.alloc(BODY_BYTES, 0x5a)
{
  const filesystem = binding.Filesystem.memory()
  await filesystem.writeFile("/big.bin", body)
  const server = binding.createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    readChunkBytes: CHUNK_BYTES,
  })
  await bodyRounds("WebDAV", () =>
    server.session.handleRequestStream({ method: "GET", target: "/big.bin", headers: [] }, null),
  )
  await filesystem.shutdown()
}
{
  const bucket = binding.Filesystem.memory()
  await bucket.writeFile("/big.bin", body)
  const server = binding.createS3Server({ buckets: { bucket } }, { readChunkBytes: CHUNK_BYTES })
  await bodyRounds("S3", () =>
    server.session.handleRequestStream({ method: "GET", target: "/bucket/big.bin", headers: [] }, null),
  )
  await bucket.shutdown()
}
process.exit(0)
