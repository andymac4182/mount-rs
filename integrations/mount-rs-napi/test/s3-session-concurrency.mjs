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

  const stats = await server.session.stats()
  assert.equal(stats.requests, count * 2, "request count")
  assert.equal(stats.replies, count * 2, "reply count")
  assert.equal(stats.errors, 0, "error count")
  assert.equal(stats.operations.PutObject, count, "PUT operation count")
  assert.equal(stats.operations.GetObject, count, "GET operation count")
  assert.deepEqual(server.session.assertions, [], "session assertions")
} finally {
  await server.close()
}

console.log("mount-rs N-API S3 direct-session concurrency: PASS (64 concurrent PUT/GETs across two buckets)")
