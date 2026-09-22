import assert from "node:assert/strict"

import { Filesystem, createS3Server } from "../index.js"

const count = 64
const photos = Filesystem.memory()
const notes = Filesystem.memory()
const server = createS3Server(
  { buckets: { photos, notes } },
  { debug: true, maxBodyBytes: 1024 * 1024 },
)

const records = Array.from({ length: count }, (_, index) => {
  const bucket = index % 2 === 0 ? "photos" : "notes"
  const body = Buffer.from(`concurrent-s3-${index.toString().padStart(2, "0")}`)
  return { bucket, key: `concurrent/${index}.txt`, body }
})

function responseHeader(response, name) {
  return response.headers.find(({ name: candidate }) => candidate.toLowerCase() === name.toLowerCase())?.value
}

try {
  const putResponses = await Promise.all(
    records.map(({ bucket, key, body }) =>
      server.session.handleRequest(
        {
          method: "PUT",
          target: `/${bucket}/${key}`,
          headers: [{ name: "content-length", value: String(body.length) }],
        },
        body,
      ),
    ),
  )
  for (const response of putResponses) {
    assert.ok([200, 201, 204].includes(response.status), `PUT status ${response.status}`)
  }

  const getResponses = await Promise.all(
    records.map(({ bucket, key }) =>
      server.session.handleRequest({ method: "GET", target: `/${bucket}/${key}`, headers: [] }),
    ),
  )
  for (const [index, response] of getResponses.entries()) {
    assert.equal(response.status, 200, `GET status ${index}`)
    assert.deepEqual(response.body, records[index].body, `GET bytes ${index}`)
  }

  const conditionalTarget = "/photos/conditional/cas.txt"
  const seedBody = Buffer.from("conditional-seed")
  const seed = await server.session.handleRequest(
    {
      method: "PUT",
      target: conditionalTarget,
      headers: [{ name: "content-length", value: String(seedBody.length) }],
    },
    seedBody,
  )
  assert.equal(seed.status, 200, "conditional seed status")
  const seedEtag = responseHeader(seed, "etag")
  assert.ok(seedEtag, "conditional seed ETag")

  const conditionalBodies = [
    Buffer.from("conditional-first"),
    Buffer.from("conditional-second"),
  ]
  const conditionalResponses = await Promise.all(
    conditionalBodies.map((body) =>
      server.session.handleRequest(
        {
          method: "PUT",
          target: conditionalTarget,
          headers: [
            { name: "if-match", value: seedEtag },
            { name: "content-length", value: String(body.length) },
          ],
        },
        body,
      ),
    ),
  )
  assert.deepEqual(
    conditionalResponses.map(({ status }) => status).sort((left, right) => left - right),
    [200, 412],
    "same-key conditional PUTs must have one compare-and-swap winner",
  )
  const winnerEtag = responseHeader(
    conditionalResponses.find(({ status }) => status === 200),
    "etag",
  )
  assert.ok(winnerEtag, "conditional winner ETag")
  const streamedConditionalBody = Buffer.from("conditional-streamed-update")
  const streamedConditional = await server.session.handleRequestStream(
    {
      method: "PUT",
      target: conditionalTarget,
      headers: [
        { name: "if-match", value: winnerEtag },
        { name: "content-length", value: String(streamedConditionalBody.length) },
      ],
    },
    (async function* () {
      yield streamedConditionalBody
    })(),
  )
  assert.equal(streamedConditional.status, 200, "streamed conditional PUT status")
  if (streamedConditional.body) {
    for await (const _chunk of streamedConditional.body) {}
  }
  const conditionalGet = await server.session.handleRequest(
    { method: "GET", target: conditionalTarget, headers: [] },
  )
  assert.equal(conditionalGet.status, 200, "conditional final GET status")
  assert.deepEqual(conditionalGet.body, streamedConditionalBody, "conditional final bytes")

  const stats = await server.session.stats()
  assert.equal(stats.requests, count * 2 + 5, "request count")
  assert.equal(stats.replies, count * 2 + 5, "reply count")
  assert.equal(stats.errors, 1, "error count")
  assert.equal(stats.operations.PutObject, count + 4, "PUT operation count")
  assert.equal(stats.operations.GetObject, count + 1, "GET operation count")
  assert.deepEqual(server.session.assertions, [], "session assertions")
} finally {
  await server.close()
}

console.log("mount-rs N-API S3 direct-session concurrency: PASS (64 concurrent PUT/GETs plus same-key conditional CAS)")
