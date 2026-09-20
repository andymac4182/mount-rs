#!/usr/bin/env node

// Public Node SDK consumer checks. The live rows use only already-exported
// factories and only read credential values into the SDK call; this file never
// logs them.

import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import { createChunkedDriver, Filesystem } from "../../integrations/mount-rs-napi/index.js";

const PAYLOAD = Buffer.from([
  109, 111, 117, 110, 116, 45, 114, 115, 0, 112, 114, 111, 118, 105, 100, 101, 114, 255,
]);
const R2_REQUIRED = [
  "R2_ENDPOINT",
  "R2_BUCKET",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
];

function safeRunId() {
  const configured = process.env.MOUNT_RS_PROVIDER_MATRIX_RUN_ID ?? "";
  return /^[A-Za-z0-9._-]+$/.test(configured)
    ? configured
    : String(process.pid);
}

function errorCode(error) {
  return error?.code ?? error?.name ?? "Error";
}

async function exercise(filesystem) {
  await filesystem.mkdir("/provider-matrix", { recursive: true, mode: 0o755 });
  await filesystem.writeFile("/provider-matrix/value", PAYLOAD);
  assert.deepEqual(Buffer.from(await filesystem.readFile("/provider-matrix/value")), PAYLOAD);
  assert.equal((await filesystem.stat("/provider-matrix/value")).size, PAYLOAD.length);
}

const failures = [];

async function runCase(label, factory, cleanup = async () => {}) {
  let filesystem;
  let failure;
  try {
    filesystem = await factory();
    await exercise(filesystem);
  } catch (error) {
    failure = error;
  }
  try {
    await filesystem?.shutdown();
  } catch (error) {
    failure ??= error;
  }
  try {
    await cleanup();
  } catch (error) {
    failure ??= error;
  }

  if (failure) {
    failures.push(label);
    console.log("FAIL node-sdk case=" + label + " reason=" + errorCode(failure));
  } else {
    console.log("PASS node-sdk case=" + label);
  }
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function hmac(key, value) {
  return createHmac("sha256", key).update(value).digest();
}

function signedR2Request(method, stateKey) {
  const endpoint = process.env.R2_ENDPOINT.replace(/\/+$/, "");
  const bucket = process.env.R2_BUCKET;
  const secretAccessKey = process.env.R2_SECRET_ACCESS_KEY;
  const accessKeyId = process.env.R2_ACCESS_KEY_ID;
  const region = process.env.RUSTFS_REGION || "auto";
  const encodedKey = stateKey.split("/").map(encodeURIComponent).join("/");
  const url = new URL(endpoint + "/" + encodeURIComponent(bucket) + "/" + encodedKey);
  const amzDate = new Date().toISOString().replace(/[-:]|\.\d{3}/g, "");
  const date = amzDate.slice(0, 8);
  const payloadHash = sha256("");
  const signedHeaders = "host;x-amz-content-sha256;x-amz-date";
  const canonicalHeaders =
    "host:" + url.host + "\n" +
    "x-amz-content-sha256:" + payloadHash + "\n" +
    "x-amz-date:" + amzDate + "\n";
  const canonicalRequest = [
    method,
    url.pathname,
    "",
    canonicalHeaders,
    signedHeaders,
    payloadHash,
  ].join("\n");
  const scope = date + "/" + region + "/s3/aws4_request";
  const signingKey = hmac(
    hmac(hmac(hmac("AWS4" + secretAccessKey, date), region), "s3"),
    "aws4_request",
  );
  const signature = createHmac("sha256", signingKey)
    .update([
      "AWS4-HMAC-SHA256",
      amzDate,
      scope,
      sha256(canonicalRequest),
    ].join("\n"))
    .digest("hex");
  const authorization =
    "AWS4-HMAC-SHA256 Credential=" + accessKeyId + "/" + scope +
    ", SignedHeaders=" + signedHeaders + ", Signature=" + signature;
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

async function cleanupR2Snapshot(stateKey) {
  if (!stateKey.startsWith("mount-rs-provider-matrix/") || stateKey.includes("..")) {
    const error = new Error("R2 cleanup key outside owned prefix");
    error.code = "R2_CLEANUP_SCOPE";
    throw error;
  }
  const deleted = await signedR2Request("DELETE", stateKey);
  await deleted.arrayBuffer();
  if (![200, 204, 404].includes(deleted.status)) {
    const error = new Error("R2 delete failed");
    error.code = "R2_CLEANUP_DELETE";
    throw error;
  }
  const verification = await signedR2Request("HEAD", stateKey);
  await verification.arrayBuffer();
  if (verification.status !== 404) {
    const error = new Error("R2 exact-key cleanup verification failed");
    error.code = "R2_CLEANUP_VERIFY";
    throw error;
  }
}

await runCase("memfs", () => Promise.resolve(Filesystem.memory()));
await runCase("sqlite", () => Filesystem.sqlite(":memory:"));
await runCase("chunked-memory/memory", () =>
  createChunkedDriver({
    metadata: { kind: "memory" },
    blocks: { kind: "memory" },
    chunkSize: 7,
    owner: "provider-matrix-node-memory",
  }),
);
await runCase("chunked-sqlite/sqlite", () =>
  createChunkedDriver({
    metadata: { kind: "sqlite", uri: ":memory:" },
    blocks: { kind: "sqlite", uri: ":memory:" },
    chunkSize: 7,
    owner: "provider-matrix-node-sqlite",
  }),
);

const pgliteUrl = process.env.PGLITE_DATABASE_URL;
if (pgliteUrl) {
  await runCase("chunked-pglite/pglite", () =>
    createChunkedDriver({
      metadata: {
        kind: "pglite",
        uri: pgliteUrl,
        key: "provider-matrix-node-pglite-metadata-" + safeRunId(),
        durable: false,
      },
      blocks: {
        kind: "pglite",
        uri: pgliteUrl,
        key: "provider-matrix-node-pglite-blocks-" + safeRunId(),
        durable: false,
      },
      chunkSize: 7,
      owner: "provider-matrix-node-pglite",
    }),
  );
} else {
  console.log("SKIP node-sdk case=chunked-pglite/pglite gate=PGLITE_DATABASE_URL");
}

const missingR2 = R2_REQUIRED.filter((name) => !process.env[name]);
if (missingR2.length === 0) {
  const stateKey = "mount-rs-provider-matrix/node-" + safeRunId() + ".json";
  await runCase(
    "r2-factory",
    () =>
      Filesystem.r2({
        endpoint: process.env.R2_ENDPOINT,
        bucket: process.env.R2_BUCKET,
        accessKeyId: process.env.R2_ACCESS_KEY_ID,
        secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
        stateKey,
      }),
    () => cleanupR2Snapshot(stateKey),
  );
} else {
  console.log("SKIP node-sdk case=r2-factory gate=" + missingR2.join("|"));
}

console.log(
  "SUMMARY node-sdk pass=" +
    (4 + (pgliteUrl ? 1 : 0) + (missingR2.length === 0 ? 1 : 0) - failures.length) +
    " skip=" +
    ((pgliteUrl ? 0 : 1) + (missingR2.length === 0 ? 0 : 1)) +
    " fail=" +
    failures.length,
);
if (failures.length > 0) process.exit(1);
