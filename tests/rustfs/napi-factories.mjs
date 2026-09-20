import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { createChunkedDriver, Filesystem } from "../../integrations/mount-rs-napi/index.js";

const required = (name) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
};

const endpoint = required("R2_ENDPOINT");
const bucket = required("R2_BUCKET");
const accessKeyId = required("R2_ACCESS_KEY_ID");
const secretAccessKey = required("R2_SECRET_ACCESS_KEY");
const prefix = required("RUSTFS_TEST_PREFIX");
const runDir = required("RUSTFS_RUN_DIR");
const pgliteUrl = required("PGLITE_DATABASE_URL");
const suffix = `${process.pid}-${Date.now()}`;

await mkdir(runDir, { recursive: true });

const r2 = {
  endpoint,
  bucket,
  accessKeyId,
  secretAccessKey,
};

async function roundTrip(label, open, path, value) {
  const first = await open(`rustfs-napi-${label}-first-${suffix}`);
  try {
    await first.writeFile(path, Buffer.from(value));
    assert.equal(Buffer.from(await first.readFile(path)).toString(), value);
  } finally {
    await first.shutdown();
  }

  const reopened = await open(`rustfs-napi-${label}-reopen-${suffix}`);
  try {
    assert.equal(Buffer.from(await reopened.readFile(path)).toString(), value);
  } finally {
    await reopened.shutdown();
  }
}

await roundTrip(
  "legacy-r2",
  () =>
    Filesystem.r2({
      ...r2,
      stateKey: `${prefix}/napi-legacy-${suffix}.json`,
    }),
  `/napi-legacy-${suffix}`,
  "real RustFS legacy factory",
);

const sqliteMetadata = join(runDir, `napi-sqlite-metadata-${suffix}.sqlite`);
await roundTrip(
  "chunked-sqlite-r2",
  (owner) =>
    createChunkedDriver({
      metadata: { kind: "sqlite", uri: sqliteMetadata },
      blocks: {
        kind: "r2",
        key: `${prefix}/napi-chunked-sqlite-${suffix}`,
        ...r2,
      },
      chunkSize: 7,
      owner,
    }),
  `/napi-chunked-sqlite-${suffix}`,
  "real RustFS blocks with SQLite metadata",
);

const pgliteKey = `${prefix}/napi-pglite-${suffix}`;
await roundTrip(
  "chunked-pglite-r2",
  (owner) =>
    createChunkedDriver({
      metadata: {
        kind: "pglite",
        uri: pgliteUrl,
        key: pgliteKey,
      },
      blocks: {
        kind: "r2",
        key: `${prefix}/napi-chunked-pglite-${suffix}`,
        ...r2,
      },
      chunkSize: 7,
      owner,
    }),
  `/napi-chunked-pglite-${suffix}`,
  "real RustFS blocks with PGlite metadata",
);

console.log("RUSTFS_NAPI_FACTORIES_PASS");
