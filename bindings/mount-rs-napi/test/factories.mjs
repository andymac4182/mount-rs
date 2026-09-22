import assert from "node:assert/strict";
import { constants } from "node:fs";
import { createHash, createHmac, randomUUID } from "node:crypto";
import { Filesystem } from "../index.js";

const R2_TEST_PREFIX = "mount-rs-napi-test/";

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function hmac(key, value) {
  return createHmac("sha256", key).update(value).digest();
}

function signedR2ObjectRequest(method, stateKey) {
  if (!stateKey.startsWith(R2_TEST_PREFIX) || stateKey.includes("..")) {
    throw new Error(`refusing R2 cleanup outside the test-owned prefix: ${stateKey}`);
  }
  const endpoint = process.env.R2_ENDPOINT.replace(/\/+$/, "");
  const bucket = process.env.R2_BUCKET;
  const secretAccessKey = process.env.R2_SECRET_ACCESS_KEY;
  const region = process.env.RUSTFS_REGION || "auto";
  const encodedKey = stateKey.split("/").map(encodeURIComponent).join("/");
  const url = new URL(`${endpoint}/${encodeURIComponent(bucket)}/${encodedKey}`);
  const amzDate = new Date().toISOString().replace(/[-:]|\.\d{3}/g, "");
  const date = amzDate.slice(0, 8);
  const payloadHash = sha256("");
  const signedHeaders = "host;x-amz-content-sha256;x-amz-date";
  const canonicalHeaders =
    `host:${url.host}\n` +
    `x-amz-content-sha256:${payloadHash}\n` +
    `x-amz-date:${amzDate}\n`;
  const canonicalRequest = [
    method,
    url.pathname,
    "",
    canonicalHeaders,
    signedHeaders,
    payloadHash,
  ].join("\n");
  const scope = `${date}/${region}/s3/aws4_request`;
  const signingKey = hmac(
    hmac(hmac(hmac(`AWS4${secretAccessKey}`, date), region), "s3"),
    "aws4_request",
  );
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    amzDate,
    scope,
    sha256(canonicalRequest),
  ].join("\n");
  const signature = createHmac("sha256", signingKey)
    .update(stringToSign)
    .digest("hex");
  const authorization =
    `AWS4-HMAC-SHA256 Credential=${process.env.R2_ACCESS_KEY_ID}/${scope}, ` +
    `SignedHeaders=${signedHeaders}, Signature=${signature}`;
  return fetch(url, {
    method,
    headers: {
      authorization,
      host: url.host,
      "x-amz-content-sha256": payloadHash,
      "x-amz-date": amzDate,
    },
    signal: AbortSignal.timeout(10_000),
  });
}

async function deleteR2Snapshot(stateKey) {
  const response = await signedR2ObjectRequest("DELETE", stateKey);
  const responseBody = await response.text();
  if (![200, 204, 404].includes(response.status)) {
    throw new Error(
      `R2 test snapshot deletion failed for ${stateKey}: HTTP ${response.status} ${responseBody}`,
    );
  }

  const verification = await signedR2ObjectRequest("HEAD", stateKey);
  const verificationBody = await verification.text();
  if (verification.status !== 404) {
    throw new Error(
      `R2 test snapshot still exists after exact-key cleanup for ${stateKey}: ` +
        `HTTP ${verification.status} ${verificationBody}`,
    );
  }
}

async function check(name, fs) {
  const path = `/factory-${name}`;
  await fs.writeFile(path, Buffer.from(`${name}:one`));
  assert.equal(Buffer.from(await fs.readFile(path)).toString(), `${name}:one`);
  let handle;
  try {
    handle = await fs.open(path, "r+");
    await handle.write(Buffer.from("two"), 0, 3, 0);
  } finally {
    await handle?.close();
  }
  assert.equal(
    Buffer.from(await fs.readFile(path)).toString(),
    `two${(`${name}:one`).slice(3)}`,
  );

  const numericPath = `${path}-numeric`;
  let numeric;
  try {
    numeric = await fs.open(
      numericPath,
      constants.O_RDWR | constants.O_CREAT,
      0o640,
    );
    await numeric.write(Buffer.from("n"), 0, 1, 0);
  } finally {
    await numeric?.close();
  }
  assert.equal(Buffer.from(await fs.readFile(numericPath)).toString(), "n");
  assert.equal((await fs.stat(numericPath)).mode & 0o777, 0o640);
}

async function runFactory(name, create, cleanup = async () => {}) {
  let fs;
  const failures = [];
  try {
    fs = await create();
    await check(name, fs);
  } catch (error) {
    failures.push(error);
  }
  try {
    await fs?.shutdown();
  } catch (error) {
    failures.push(error);
  }
  try {
    await cleanup();
  } catch (error) {
    failures.push(error);
  }
  if (failures.length === 1) throw failures[0];
  if (failures.length > 1) {
    throw new AggregateError(failures, `${name} factory test and cleanup failed`);
  }
}

await runFactory("memory", () => Filesystem.memory());
await runFactory("sqlite", () => Filesystem.sqlite(":memory:"));

if (process.env.PGLITE_DATABASE_URL) {
  await runFactory("pglite", () => Filesystem.pglite(process.env.PGLITE_DATABASE_URL));
} else {
  console.log("mount-rs N-API PGlite factory: SKIP (PGLITE_DATABASE_URL unset)");
}

const r2 = [
  "R2_ENDPOINT",
  "R2_BUCKET",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
].every((name) => process.env[name]);
if (r2) {
  const stateKey = `${R2_TEST_PREFIX}${process.pid}-${Date.now()}-${randomUUID()}.json`;
  await runFactory(
    "r2",
    () => Filesystem.r2({
      endpoint: process.env.R2_ENDPOINT,
      bucket: process.env.R2_BUCKET,
      accessKeyId: process.env.R2_ACCESS_KEY_ID,
      secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
      stateKey,
    }),
    () => deleteR2Snapshot(stateKey),
  );
} else {
  console.log("mount-rs N-API R2 factory: SKIP (R2 credentials unset)");
}

console.log("mount-rs N-API factory integration: PASS");
