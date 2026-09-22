import assert from "node:assert/strict"

import { Filesystem, createS3Server } from "../index.js"

const requestIds = []
const clockCalls = []
const errors = []
const assertions = []
const fixedRequestId = "s3-fixed-request-id"
const fixedNow = 1_700_000_000_000

function header(response, name) {
  return response.headers.find(({ name: candidate }) => candidate.toLowerCase() === name)?.value
}

function emptyBody() {
  return new ReadableStream({
    start(controller) {
      controller.close()
    },
  })
}

const server = createS3Server(Filesystem.memory(), {
  bucket: "photos",
  debug: true,
  now() {
    clockCalls.push(Date.now())
    return fixedNow
  },
  requestId() {
    requestIds.push(fixedRequestId)
    return fixedRequestId
  },
  onError(error, head) {
    errors.push({ error, head })
  },
  onAssertion(message) {
    assertions.push(message)
  },
})

const session = server.session

try {
  const body = Buffer.from("S3 callback hooks")
  const put = await session.handleRequest(
    {
      method: "PUT",
      target: "/photos/hooks.txt",
      headers: [{ name: "content-length", value: String(body.length) }],
    },
    body,
  )
  assert.equal(put.status, 200)
  assert.equal(header(put, "x-amz-request-id"), fixedRequestId)

  const streamedBody = Buffer.from("streamed S3 callback hooks")
  const streamedPut = await session.handleRequestStream(
    {
      method: "PUT",
      target: "/photos/hooks-stream.txt",
      headers: [{ name: "content-length", value: String(streamedBody.length) }],
    },
    new ReadableStream({
      start(controller) {
        controller.enqueue(streamedBody)
        controller.close()
      },
    }),
  )
  assert.equal(streamedPut.status, 200)
  assert.equal(header(streamedPut, "x-amz-request-id"), fixedRequestId)
  assert.ok(streamedPut.body)
  for await (const _chunk of streamedPut.body) {}

  const initiated = await session.handleRequest(
    { method: "POST", target: "/photos/hooks-multipart.bin?uploads", headers: [] },
  )
  assert.equal(initiated.status, 200)

  const missing = await session.handleRequest(
    { method: "GET", target: "/photos/missing.txt", headers: [] },
  )
  assert.equal(missing.status, 404)
  assert.equal(header(missing, "x-amz-request-id"), fixedRequestId)

  // Request-level notifications are queued onto JavaScript's event loop.
  await new Promise((resolve) => setImmediate(resolve))
  assert.ok(clockCalls.length >= 1, "the injected clock is used by S3 session work")
  assert.equal(requestIds.length, 4, "buffered, streamed, multipart, and error requests use requestId")
  assert.equal(errors.length, 1)
  assert.ok(errors[0].error instanceof Error)
  assert.equal(errors[0].head.method, "GET")
  assert.equal(errors[0].head.target, "/photos/missing.txt")
  assert.deepEqual(assertions, [])

  // A throwing callback must not prevent the S3 session from returning its
  // single error response.
  const throwing = createS3Server(Filesystem.memory(), {
    requestId() {
      throw new Error("request id logger failed")
    },
    onError() {
      throw new Error("error logger failed")
    },
  })
  try {
    const response = await throwing.session.handleRequest(
      { method: "GET", target: "/mountx/missing.txt", headers: [] },
    )
    assert.equal(response.status, 404)
  } finally {
    await throwing.close()
  }

  const callsBeforeClose = requestIds.length
  await server.close()
  const afterClose = await session.handleRequest(
    { method: "GET", target: "/photos/missing-after-close.txt", headers: [] },
  )
  assert.notEqual(header(afterClose, "x-amz-request-id"), fixedRequestId)
  assert.equal(requestIds.length, callsBeforeClose, "close releases the requestId callback")
} finally {
  await server.close().catch(() => {})
}

console.log("mount-rs N-API S3 callback hooks: PASS (clock, request ID, error head, throwing callback, release)")
