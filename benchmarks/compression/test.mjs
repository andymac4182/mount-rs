import assert from "node:assert/strict"

import {
  buildCases,
  makePayload,
  parseArgs,
  parseByteSize,
} from "./runner.mjs"

assert.equal(parseByteSize("64KiB"), 64 * 1024)
assert.equal(parseByteSize("1", "MiB"), 1024 * 1024)
assert.equal(parseByteSize("1MB"), 1_000_000)

const packetOptions = parseArgs(["--profile", "packet"])
const packetCases = buildCases(packetOptions)
assert.equal(packetCases.length, 34)
assert.deepEqual(packetCases[0], {
  workload: "structured",
  sizeBytes: 1024 * 1024,
  chunkSizeBytes: 64 * 1024,
})
assert.equal(packetCases.at(-1).workload, "small-files")
assert.equal(packetCases.at(-1).chunkSizeBytes, 1024 * 1024)
assert.deepEqual(
  [...new Set(packetCases.filter((item) => item.workload === "metadata").map((item) => item.chunkSizeBytes))],
  [4, 16, 64, 256, 1024].map((value) => value * 1024),
)

const smokeOptions = parseArgs(["--smoke"])
const smokeCases = buildCases(smokeOptions)
assert.equal(smokeCases.length, 4)
assert.deepEqual([...new Set(smokeCases.map((item) => item.sizeBytes))], [1024 * 1024])
assert.deepEqual([...new Set(smokeCases.map((item) => item.chunkSizeBytes))], [64 * 1024])

const structuredA = makePayload("structured", 64 * 1024, "test-seed")
const structuredB = makePayload("structured", 64 * 1024, "test-seed")
const random = makePayload("random", 64 * 1024, "test-seed")
assert.equal(structuredA.buffer.equals(structuredB.buffer), true)
assert.equal(structuredA.buffer.equals(random.buffer), false)
assert.equal(structuredA.details.generator, "source-config-records")
assert.equal(random.details.generator, "xorshift32")

assert.throws(() => parseArgs(["--sizes", "0"]), /positive safe integer/)
assert.throws(() => parseArgs(["--workloads", "not-a-workload"]), /unknown workload/)
assert.throws(() => buildCases(parseArgs(["--profile", "packet", "--max-cases", "1"])), /above --max-cases/)

console.log("compression benchmark unit tests: PASS")
