#!/usr/bin/env node

/**
 * Regression for an early HTTP response while an AWS-chunked request is still
 * being written. The request deliberately has a bad first chunk signature.
 * This uses node:http so the writer can stop at the response boundary; fetch's
 * ReadableStream writer is intentionally not part of this negative-path test.
 *
 * Usage:
 *
 *   MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX \
 *     node scripts/test-http-early-rejection.mjs
 */

import http from "node:http";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import assert from "node:assert/strict";

const repo = new URL("..", import.meta.url);
const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to the pinned mountx checkout");
}

const sourceRoot = pathToFileURL(source.endsWith("/") ? source : `${source}/`);
const [signer, chunked] = await Promise.all([
  import(new URL("src/s3/sigv4.ts", sourceRoot).href),
  import(new URL("src/s3/chunked.ts", sourceRoot).href),
]);

const ACCESS_KEY = "AKIAMOUNTX7GATEWAY9";
const SECRET_KEY = "test-secret-key";
const REGION = "us-east-1";
const BUCKET = "mountx";
const CREDENTIALS = { accessKeyId: ACCESS_KEY, secretAccessKey: SECRET_KEY };
const PAYLOAD = Uint8Array.from({ length: 256 * 1024 + 17 }, (_, index) =>
  (index * 31 + (index >> 11)) & 0xff,
);
const FIXTURE_START_TIMEOUT_MS = 120_000;
const HTTP_TIMEOUT_MS = 10_000;
const FIXTURE_STOP_TIMEOUT_MS = 10_000;

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function bytes(value) {
  return new TextEncoder().encode(value);
}

function queryEntries(pathAndQuery) {
  const url = new URL(pathAndQuery, "http://localhost");
  return [...url.searchParams].map(([name, value]) => ({ name, value }));
}

function s3SignedHeaders(
  base,
  method,
  pathAndQuery,
  body,
  extra = {},
  timestamp = Date.now(),
  payloadHash = signer.sha256Hex(body ?? new Uint8Array()),
) {
  const url = new URL(pathAndQuery, `${base.replace(/\/$/, "")}/`);
  const extraHeaders = Object.entries(extra).map(([name, value]) => ({
    name: name.toLowerCase(),
    value: String(value),
  }));
  const host = extraHeaders.find(({ name }) => name === "host")?.value ?? url.host;
  const headers = [
    { name: "host", value: host },
    { name: "x-amz-date", value: signer.formatAmzDate(timestamp) },
    { name: "x-amz-content-sha256", value: payloadHash },
    ...extraHeaders.filter(({ name }) => name !== "host"),
  ];
  const signed = signer.signRequest({
    method,
    path: decodeURIComponent(url.pathname),
    query: queryEntries(pathAndQuery),
    headers,
    credentials: CREDENTIALS,
    region: REGION,
    timestamp,
    payloadHash,
  });
  return {
    url: url.href,
    headers: Object.fromEntries([
      ...headers.map(({ name, value }) => [name, value]),
      ["authorization", signed.authorization],
    ]),
    amzDate: signed.amzDate,
    scope: signed.scope,
    signature: signed.signature,
  };
}

function concatBytes(parts) {
  const total = parts.reduce((sum, part) => sum + part.byteLength, 0);
  const output = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.byteLength;
  }
  return output;
}

function awsChunkedBytes(payload, signature, chunkSize = 64 * 1024) {
  const parts = [];
  let previous = signature.seed;
  for (let offset = 0; offset < payload.byteLength; offset += chunkSize) {
    const chunk = payload.slice(offset, Math.min(payload.byteLength, offset + chunkSize));
    const current = chunked.signChunk(signature, previous, signer.sha256Hex(chunk));
    parts.push(
      bytes(`${chunk.byteLength.toString(16)};chunk-signature=${current}\r\n`),
      chunk,
      bytes("\r\n"),
    );
    previous = current;
  }
  const terminal = chunked.signChunk(signature, previous, signer.sha256Hex(new Uint8Array()));
  parts.push(bytes(`0;chunk-signature=${terminal}\r\n\r\n`));
  return concatBytes(parts);
}

function tamperFirstChunkSignature(body) {
  const marker = bytes("chunk-signature=");
  const output = body.slice();
  for (let offset = 0; offset + marker.byteLength + 64 < output.byteLength; offset += 1) {
    if (marker.every((value, index) => output[offset + index] === value)) {
      const signatureOffset = offset + marker.byteLength + 63;
      output[signatureOffset] = output[signatureOffset] === 48 ? 49 : 48;
      return output;
    }
  }
  throw new Error("AWS chunked body has no signature to tamper");
}

function expectedShutdownError(error) {
  return error && ["EPIPE", "ECONNRESET", "ERR_STREAM_DESTROYED"].includes(error.code);
}

function bufferedRequest(request, body = new Uint8Array()) {
  const target = new URL(request.url);
  return new Promise((resolve, reject) => {
    let settled = false;
    let responseEnded = false;
    let timeout;
    const client = http.request(target, { method: "POST", headers: request.headers });
    const fail = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      client.destroy();
      reject(error);
    };
    const succeed = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolve(value);
    };
    client.once("error", fail);
    client.once("response", (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.once("error", fail);
      response.once("aborted", () => fail(new Error("buffered HTTP response was aborted")));
      response.once("close", () => {
        if (!responseEnded) fail(new Error("buffered HTTP response closed before end"));
      });
      response.once("end", () => {
        responseEnded = true;
        succeed({
          status: response.statusCode,
          body: Buffer.concat(chunks).toString("utf8"),
        });
      });
    });
    timeout = setTimeout(
      () => fail(new Error(`buffered HTTP request timed out after ${HTTP_TIMEOUT_MS}ms`)),
      HTTP_TIMEOUT_MS,
    );
    try {
      client.end(Buffer.from(body));
    } catch (error) {
      fail(error);
    }
  });
}

async function writeUntilEarlyResponse(request) {
  const target = new URL(request.url);
  const chunkSizes = [1, 3, 7, 31, 113, 4096];
  const state = {
    stopped: false,
    responseStarted: false,
    responseEnded: false,
    requestClosed: false,
    responseStatus: undefined,
    responseBody: "",
    sent: 0,
    requestError: undefined,
  };

  return await new Promise((resolve, reject) => {
    let settled = false;
    const client = http.request(target, {
      method: "PUT",
      headers: {
        ...request.headers,
        // The server is allowed to stop consuming this deliberately invalid
        // body as soon as the first bad signature is decoded.
        connection: "close",
      },
    });

    const fail = (error) => {
      if (settled) return;
      settled = true;
      state.stopped = true;
      client.destroy();
      reject(error);
    };

    const finish = () => {
      if (settled || !state.responseEnded || !state.requestClosed) return;
      if (state.sent >= request.body.byteLength) {
        fail(
          new Error(
            `early-rejection response arrived after the full ${request.body.byteLength}-byte body`,
          ),
        );
        return;
      }
      settled = true;
      resolve(state);
    };

    client.on("error", (error) => {
      if (state.responseStarted && state.stopped && expectedShutdownError(error)) {
        state.requestError = error;
        return;
      }
      fail(error);
    });

    client.once("close", () => {
      state.requestClosed = true;
      if (!state.responseStarted) {
        fail(new Error("HTTP request closed before the early rejection response"));
        return;
      }
      finish();
    });

    client.on("response", (response) => {
      state.responseStarted = true;
      state.stopped = true;
      state.responseStatus = response.statusCode;
      response.setEncoding("utf8");
      response.on("data", (chunk) => {
        state.responseBody += chunk;
      });
      response.once("error", fail);
      response.once("aborted", () => fail(new Error("early rejection response was aborted")));
      response.once("close", () => {
        if (!state.responseEnded) fail(new Error("early rejection response closed before end"));
      });
      response.once("end", () => {
        state.responseEnded = true;
        client.destroy();
        finish();
      });
    });

    const write = async () => {
      let offset = 0;
      let pull = 0;
      while (offset < request.body.byteLength && !state.stopped) {
        await sleep(2);
        if (state.stopped) break;
        const size = chunkSizes[pull % chunkSizes.length];
        const end = Math.min(request.body.byteLength, offset + size);
        client.write(request.body.subarray(offset, end));
        offset = end;
        state.sent = offset;
        pull += 1;
        while (client.writableNeedDrain && !state.stopped) await sleep(1);
      }
      if (!state.stopped) client.end();
    };

    client.setTimeout(HTTP_TIMEOUT_MS, () =>
      fail(new Error(`timed out waiting for early rejection after ${HTTP_TIMEOUT_MS}ms`)),
    );
    write().catch(fail);
  });
}

function childHasExited(child) {
  return (
    (child.exitCode !== null && child.exitCode !== undefined) ||
    (child.signalCode !== null && child.signalCode !== undefined)
  );
}

function waitForChildExit(child, timeoutMs) {
  if (childHasExited(child)) return Promise.resolve(true);
  return new Promise((resolve) => {
    let settled = false;
    let timeout;
    const cleanup = () => {
      clearTimeout(timeout);
      child.off("exit", onExit);
    };
    const finish = (exited) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(exited);
    };
    const onExit = () => finish(true);
    child.once("exit", onExit);
    timeout = setTimeout(() => finish(false), timeoutMs);
    if (childHasExited(child)) finish(true);
  });
}

async function startRust() {
  let child;
  try {
    child = spawn(
      process.env.CARGO ?? "cargo",
      ["run", "--locked", "--quiet", "--example", "http_oracle", "--"],
      {
        cwd: fileURLToPath(repo),
        env: { ...process.env, CARGO_TERM_COLOR: "never", RUST_BACKTRACE: "1" },
        stdio: ["pipe", "pipe", "pipe"],
      },
    );
  } catch (error) {
    throw new Error("could not spawn the Rust HTTP fixture", { cause: error });
  }

  const lines = createInterface({ input: child.stdout });
  const errors = [];
  const fixture = { child, errors, lines };
  const recordError = (error) => {
    errors.push(`${error?.stack ?? error}\n`);
  };
  child.stderr.on("data", (chunk) => errors.push(String(chunk)));
  child.stderr.on("error", recordError);
  child.stdout.on("error", recordError);

  // Keep an error listener for the whole fixture lifetime. In particular, an
  // asynchronous spawn failure must never become an unhandled event after the
  // readiness promise has already been created.
  child.on("error", recordError);

  try {
    fixture.ready = await new Promise((resolve, reject) => {
      let settled = false;
      const timeout = setTimeout(
        () => finishReject(new Error(`Rust fixture did not start\n${errors.join("")}`)),
        FIXTURE_START_TIMEOUT_MS,
      );
      const cleanup = () => {
        clearTimeout(timeout);
        child.off("error", onChildError);
        child.stdout.off("error", onOutputError);
        child.off("exit", onExit);
        lines.off("line", onLine);
      };
      const finishReject = (error) => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(error);
      };
      const finishResolve = (value) => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve(value);
      };
      const onExit = (code, signal) => {
        finishReject(new Error(`Rust fixture exited ${code ?? signal}\n${errors.join("")}`));
      };
      const onLine = (line) => {
        const prefix = "MOUNT_RS_HTTP_ORACLE_READY ";
        if (!line.startsWith(prefix)) return;

        let value;
        try {
          value = JSON.parse(line.slice(prefix.length));
        } catch (error) {
          finishReject(new Error("Rust fixture readiness JSON is malformed", { cause: error }));
          return;
        }
        if (!value || typeof value !== "object") {
          finishReject(new Error("Rust fixture readiness JSON is not an object"));
          return;
        }
        for (const field of ["s3", "webdav"]) {
          if (typeof value[field] !== "string") {
            finishReject(new Error(`Rust fixture readiness JSON has no ${field} URL`));
            return;
          }
          try {
            new URL(value[field]);
          } catch (error) {
            finishReject(new Error(`Rust fixture readiness JSON has an invalid ${field} URL`, { cause: error }));
            return;
          }
        }
        finishResolve(value);
      };
      const onChildError = (error) => {
        finishReject(new Error("Rust fixture failed while starting", { cause: error }));
      };
      const onOutputError = (error) => {
        finishReject(new Error("Rust fixture output failed while starting", { cause: error }));
      };
      child.once("error", onChildError);
      child.stdout.once("error", onOutputError);
      child.once("exit", onExit);
      lines.on("line", onLine);
    });
    return fixture;
  } catch (error) {
    try {
      await stopRust(fixture);
    } catch (cleanupError) {
      throw new AggregateError(
        [error, cleanupError],
        "Rust fixture startup failed and cleanup also failed",
      );
    }
    throw error;
  }
}

async function stopRust(fixture) {
  if (!fixture) return;
  fixture.lines?.close();
  const child = fixture.child;
  if (!child) return;

  const stdinErrors = [];
  const onStdinError = (error) => stdinErrors.push(error);
  let forced = false;
  let killError;
  if (!childHasExited(child)) {
    child.stdin?.once("error", onStdinError);
    try {
      if (child.stdin && !child.stdin.destroyed) child.stdin.end();
    } catch (error) {
      stdinErrors.push(error);
    }

    let exited = await waitForChildExit(child, FIXTURE_STOP_TIMEOUT_MS);
    const kill = (signal) => {
      try {
        child.kill(signal);
      } catch (error) {
        killError = error;
      }
    };
    if (!exited) {
      forced = true;
      kill("SIGTERM");
      exited = await waitForChildExit(child, FIXTURE_STOP_TIMEOUT_MS);
    }
    if (!exited) {
      kill("SIGKILL");
      exited = await waitForChildExit(child, FIXTURE_STOP_TIMEOUT_MS);
    }
    child.stdin?.off("error", onStdinError);
    if (!exited) {
      throw new Error(
        `Rust fixture did not stop within ${FIXTURE_STOP_TIMEOUT_MS * 3}ms${
          killError ? `: ${killError.message}` : ""
        }`,
      );
    }
  }

  if (stdinErrors.length) {
    throw new Error(`Rust fixture stdin shutdown failed\n${stdinErrors.join("\n")}`);
  }
  if (forced) {
    throw new Error(`Rust fixture required forced termination\n${fixture.errors.join("")}`);
  }
  if (child.exitCode !== 0) {
    throw new Error(`Rust fixture stopped with ${child.exitCode ?? child.signalCode}\n${fixture.errors.join("")}`);
  }
}

let fixture;
try {
  fixture = await startRust();
  const key = "early-rejection.bin";
  const createPath = `/${BUCKET}/${key}?uploads`;
  const create = s3SignedHeaders(fixture.ready.s3, "POST", createPath, new Uint8Array(), {
    "content-length": 0,
  });
  const created = await bufferedRequest(create);
  assert.equal(created.status, 200);
  const uploadId = created.body.match(/<UploadId>([^<]+)<\/UploadId>/)?.[1];
  assert(uploadId, `multipart initiation did not return an UploadId: ${created.body}`);

  const path = `/${BUCKET}/${key}?uploadId=${encodeURIComponent(uploadId)}&partNumber=2`;
  const timestamp = Date.now();
  const amzDate = signer.formatAmzDate(timestamp);
  const draft = awsChunkedBytes(
    PAYLOAD,
    {
      seed: "0".repeat(64),
      amzDate,
      scope: { date: amzDate.slice(0, 8), region: REGION, service: "s3" },
      secretAccessKey: SECRET_KEY,
    },
  );
  const request = s3SignedHeaders(fixture.ready.s3, "PUT", path, PAYLOAD, {
    "content-encoding": "aws-chunked",
    "x-amz-decoded-content-length": PAYLOAD.byteLength,
    "content-length": draft.byteLength,
  }, timestamp, "STREAMING-AWS4-HMAC-SHA256-PAYLOAD");
  const framed = awsChunkedBytes(PAYLOAD, {
    seed: request.signature,
    amzDate: request.amzDate,
    scope: request.scope,
    secretAccessKey: SECRET_KEY,
  });
  assert.equal(framed.byteLength, draft.byteLength);
  request.body = tamperFirstChunkSignature(framed);
  // The request signature covers the streaming sentinel, not the per-chunk
  // signatures; changing the first chunk signature therefore reaches the
  // streaming decoder and should produce SignatureDoesNotMatch.
  const result = await writeUntilEarlyResponse(request);
  assert.equal(result.responseStatus, 403);
  assert.match(result.responseBody, /SignatureDoesNotMatch/);
  assert(result.sent > 0 && result.sent < request.body.byteLength);
  console.log(
    `mount-rs HTTP early-rejection regression: PASS (response at ${result.sent}/${request.body.byteLength} body bytes)`,
  );
} finally {
  await stopRust(fixture);
}
