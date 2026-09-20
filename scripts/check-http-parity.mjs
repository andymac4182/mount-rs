#!/usr/bin/env node

/**
 * Differential HTTP check for the pinned mountx S3 and WebDAV servers.
 *
 * The Rust example and the TypeScript oracle each get a fresh in-memory
 * driver. Every case below sends the same request to both loopback listeners,
 * then compares status, protocol headers, bytes, or XML structure. Date fields
 * and server-generated request ids are the only intentionally nondeterministic
 * values removed; ETags are compared, not discarded. A fragmented
 * ReadableStream body is used for writes so this exercises the HTTP streaming
 * boundary rather than only buffered `fetch` bodies.
 *
 * Usage:
 *
 *   MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX \
 *     node --experimental-strip-types scripts/check-http-parity.mjs
 *
 * No request leaves 127.0.0.1. The Rust fixture is a child process and is
 * stopped by closing stdin in the `finally` block.
 */

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { once } from "node:events";
import assert from "node:assert/strict";

const repo = fileURLToPath(new URL("..", import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to the pinned mountx checkout");
}

const sourceRoot = pathToFileURL(source.endsWith("/") ? source : `${source}/`);
const [s3Oracle, webdavOracle, signer] = await Promise.all([
  import(new URL("src/s3/server.ts", sourceRoot).href),
  import(new URL("src/webdav/server.ts", sourceRoot).href),
  import(new URL("src/s3/sigv4.ts", sourceRoot).href),
]);
const { createMemoryDriver: createS3MemoryDriver } = await import(
  new URL("src/drivers/memory.ts", sourceRoot).href,
);
const { createMemoryDriver: createWebdavMemoryDriver } = await import(
  new URL("src/drivers/memory.ts", sourceRoot).href,
);

const ACCESS_KEY = "AKIAMOUNTX7GATEWAY9";
const SECRET_KEY = "test-secret-key";
const REGION = "us-east-1";
const S3_BUCKET = "mountx";
const WEBDAV_USER = "ada";
const WEBDAV_PASSWORD = "a pass:word";
const FIXED_MTIME_SECONDS = "1600000000";
const FIXED_MTIME_MS = Number(FIXED_MTIME_SECONDS) * 1000;
const FIXED_MTIME_DATE = "Sun, 13 Sep 2020 12:26:40 GMT";
const CREDENTIALS = { accessKeyId: ACCESS_KEY, secretAccessKey: SECRET_KEY };

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function bytes(value) {
  return new TextEncoder().encode(value);
}

function seeded(size) {
  const value = new Uint8Array(size);
  for (let index = 0; index < size; index += 1) {
    value[index] = (index * 31 + (index >> 11)) & 0xff;
  }
  return value;
}

function basic(username, password) {
  return `Basic ${Buffer.from(`${username}:${password}`, "utf8").toString("base64")}`;
}

function fragmentedBody(value, chunkSizes = [7, 31, 113, 4096]) {
  const input = value instanceof Uint8Array ? value : bytes(value);
  let offset = 0;
  let pullCount = 0;
  return new ReadableStream({
    async pull(controller) {
      if (offset >= input.byteLength) {
        controller.close();
        return;
      }
      await sleep(1);
      const size = chunkSizes[pullCount % chunkSizes.length];
      const end = Math.min(input.byteLength, offset + size);
      controller.enqueue(input.slice(offset, end));
      offset = end;
      pullCount += 1;
    },
  });
}

function fixedTimeDriver(driver) {
  const fixedStats = async (method, path) => {
    const stats = await driver[method](path);
    return {
      ...stats,
      atimeMs: FIXED_MTIME_MS,
      mtimeMs: FIXED_MTIME_MS,
      ctimeMs: FIXED_MTIME_MS,
      birthtimeMs: FIXED_MTIME_MS,
    };
  };
  return new Proxy(driver, {
    get(target, property, receiver) {
      if (property === "stat" || property === "lstat") {
        return (path) => fixedStats(property, path);
      }
      const value = Reflect.get(target, property, receiver);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

function responseHeaders(response) {
  const ignored = new Set([
    "connection",
    "date",
    "keep-alive",
    "server",
    "transfer-encoding",
    "x-amz-id-2",
    "x-amz-request-id",
  ]);
  const values = {};
  for (const [name, value] of response.headers) {
    const lower = name.toLowerCase();
    if (ignored.has(lower)) continue;
    values[lower] = lower === "last-modified" ? "<http-date>" : value.trim();
  }
  // Node emits an explicit zero length for the no-body 204 response while
  // Hyper removes entity framing headers for that status on the wire. They
  // are the same HTTP representation; keep all binary, HEAD, range, and XML
  // lengths strict below.
  if (response.status === 204 && values["content-length"] === undefined) {
    values["content-length"] = "0";
  }
  return values;
}

function decodeXml(value) {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&")
    .replace(/&#(x[0-9a-f]+|\d+);/gi, (_, number) => {
      const radix = number[0].toLowerCase() === "x" ? 16 : 10;
      return String.fromCodePoint(Number.parseInt(number.slice(radix === 16 ? 1 : 0), radix));
    });
}

function xmlCanonical(value) {
  const source = value.replace(/^\s*<\?xml[^>]*\?>\s*/i, "");
  const namespaces = [{}];
  const elements = [];
  const output = [];
  const dynamic = new Set(["creationdate", "getlastmodified", "hostid", "lastmodified", "requestid"]);
  let position = 0;
  let textBuffer = "";

  const flushText = () => {
    if (textBuffer.length === 0) return;
    const text = decodeXml(textBuffer);
    // Whitespace-only nodes between elements are serializer formatting and do
    // not carry XML meaning. Text containing any non-whitespace is semantic;
    // retain it byte-for-byte so spaces in keys, hrefs, messages, and values
    // cannot be hidden by a permissive canonicalizer.
    if (text.trim() !== "") {
      const parent = elements.at(-1);
      output.push(
        dynamic.has(parent?.local?.toLowerCase()) ? "#<dynamic-value>" : `#${JSON.stringify(text)}`,
      );
    }
    textBuffer = "";
  };

  const resolveName = (name, scope) => {
    const colon = name.indexOf(":");
    const prefix = colon === -1 ? "" : name.slice(0, colon);
    const local = colon === -1 ? name : name.slice(colon + 1);
    const uri = scope[prefix] ?? "";
    return { local, uri };
  };

  const parseAttributes = (raw) => {
    const attributes = [];
    const pattern = /([^\s=]+)\s*=\s*("[^"]*"|'[^']*')/g;
    let match;
    while ((match = pattern.exec(raw)) !== null) {
      const name = match[1];
      const quoted = match[2];
      attributes.push([name, decodeXml(quoted.slice(1, -1))]);
    }
    return attributes;
  };

  while (position < source.length) {
    const opening = source.indexOf("<", position);
    if (opening === -1) {
      textBuffer += source.slice(position);
      break;
    }
    textBuffer += source.slice(position, opening);
    if (source.startsWith("<!--", opening)) {
      flushText();
      const end = source.indexOf("-->", opening + 4);
      if (end === -1) throw new Error("unterminated XML comment");
      position = end + 3;
      continue;
    }
    if (source.startsWith("<![CDATA[", opening)) {
      const end = source.indexOf("]]>", opening + 9);
      if (end === -1) throw new Error("unterminated XML CDATA");
      textBuffer += source.slice(opening + 9, end);
      position = end + 3;
      continue;
    }
    const closing = source.indexOf(">", opening + 1);
    if (closing === -1) throw new Error("unterminated XML tag");
    flushText();
    const raw = source.slice(opening + 1, closing).trim();
    if (raw.startsWith("?")) {
      position = closing + 1;
      continue;
    }
    if (raw.startsWith("/")) {
      const name = resolveName(raw.slice(1).trim(), namespaces.at(-1));
      output.push(`</{${name.uri}}${name.local}>`);
      namespaces.pop();
      elements.pop();
      position = closing + 1;
      continue;
    }
    const selfClosing = raw.endsWith("/");
    const withoutSlash = selfClosing ? raw.slice(0, -1).trim() : raw;
    const separator = withoutSlash.search(/\s/);
    const rawName = separator === -1 ? withoutSlash : withoutSlash.slice(0, separator);
    const attributes = parseAttributes(separator === -1 ? "" : withoutSlash.slice(separator));
    const scope = { ...namespaces.at(-1) };
    for (const [name, attributeValue] of attributes) {
      if (name === "xmlns") scope[""] = attributeValue;
      else if (name.startsWith("xmlns:")) scope[name.slice(6)] = attributeValue;
    }
    const name = resolveName(rawName, scope);
    const semanticAttributes = attributes
      .filter(([attribute]) => attribute !== "xmlns" && !attribute.startsWith("xmlns:"))
      .map(([attribute, attributeValue]) => {
        const resolved = resolveName(attribute, scope);
        return [`{${resolved.uri}}${resolved.local}`, attributeValue];
      })
      .sort(([left], [right]) => left.localeCompare(right));
    output.push(
      `<{${name.uri}}${name.local}${semanticAttributes
        .map(([attribute, attributeValue]) => ` ${attribute}=${JSON.stringify(attributeValue)}`)
        .join("")}${selfClosing ? "/>" : ">"}`,
    );
    if (!selfClosing) {
      namespaces.push(scope);
      elements.push({ local: name.local });
    }
    position = closing + 1;
  }
  flushText();
  return output.join("");
}

function bodyValue(bytesValue, contentType) {
  const text = new TextDecoder().decode(bytesValue);
  if (
    contentType?.toLowerCase().includes("xml") ||
    /^\s*(?:<\?xml|<[A-Za-z_][\w.-]*(?::[\w.-]+)?(?:\s|>))/s.test(text)
  ) {
    try {
      return { kind: "xml", value: xmlCanonical(text) };
    } catch {
      // A non-XML error body remains a byte comparison below.
    }
  }
  return { kind: "bytes", value: Buffer.from(bytesValue).toString("base64") };
}

async function readResponse(response, method) {
  const body = new Uint8Array(await response.arrayBuffer());
  const advertisedLength = response.headers.get("content-length");
  if (advertisedLength !== null && method !== "HEAD" && Number(advertisedLength) !== body.byteLength) {
    throw new Error(
      `${method} Content-Length ${advertisedLength} does not match its own ${body.byteLength}-byte body`,
    );
  }
  const semanticBody = bodyValue(body, response.headers.get("content-type"));
  const headers = responseHeaders(response);
  if (semanticBody.kind === "xml") {
    // XML whitespace, declarations, namespace prefixes, and dynamic request
    // ids are not wire-semantic. Compare the canonical representation's byte
    // length after validating each implementation's raw framing above.
    headers["content-length"] = String(Buffer.byteLength(semanticBody.value));
  }
  return {
    status: response.status,
    headers,
    body: semanticBody,
  };
}

function compare(label, left, right) {
  try {
    assert.deepStrictEqual(right, left);
  } catch (error) {
    throw new Error(
      `${label} mismatch\nTS: ${JSON.stringify(left, null, 2)}\nRust: ${JSON.stringify(right, null, 2)}`,
      { cause: error },
    );
  }
}

async function pair(label, tsRequest, rustRequest, { method } = {}) {
  const [typescriptResponse, rustResponse] = await Promise.all([tsRequest(), rustRequest()]);
  const [typescript, rust] = await Promise.all([
    readResponse(typescriptResponse, method),
    readResponse(rustResponse, method),
  ]);
  compare(label, typescript, rust);
  return { typescript, rust };
}

function urlFor(base, pathAndQuery) {
  return new URL(pathAndQuery, `${base.replace(/\/$/, "")}/`).href;
}

function queryEntries(pathAndQuery) {
  const url = new URL(pathAndQuery, "http://localhost");
  return [...url.searchParams].map(([name, value]) => ({ name, value }));
}

async function fetchWebdav(base, method, path, options = {}) {
  const headers = new Headers(options.headers ?? {});
  if (options.auth !== false) headers.set("authorization", basic(WEBDAV_USER, WEBDAV_PASSWORD));
  const body = options.fragmented ? fragmentedBody(options.body) : options.body;
  const request = {
    method,
    headers,
    body,
    ...(options.fragmented ? { duplex: "half" } : {}),
  };
  return fetch(urlFor(base, path), request);
}

function s3SignedHeaders(base, method, pathAndQuery, body, extra = {}, timestamp = Date.now(), payloadHash) {
  const url = new URL(pathAndQuery, `${base.replace(/\/$/, "")}/`);
  const hash = payloadHash ?? signer.sha256Hex(body ?? new Uint8Array());
  const headers = [
    { name: "host", value: url.host },
    { name: "x-amz-date", value: signer.formatAmzDate(timestamp) },
    { name: "x-amz-content-sha256", value: hash },
    ...Object.entries(extra).map(([name, value]) => ({ name: name.toLowerCase(), value: String(value) })),
  ];
  const signed = signer.signRequest({
    method,
    path: decodeURIComponent(url.pathname),
    query: queryEntries(pathAndQuery),
    headers,
    credentials: CREDENTIALS,
    region: REGION,
    timestamp,
    payloadHash: hash,
  });
  return {
    url: url.href,
    headers: Object.fromEntries([
      ...headers.map(({ name, value }) => [name, value]),
      ["authorization", signed.authorization],
    ]),
    hash,
  };
}

async function fetchS3(base, method, pathAndQuery, options = {}) {
  const body = options.body instanceof Uint8Array ? options.body : options.body ? bytes(options.body) : undefined;
  if (options.auth === false) {
    return fetch(urlFor(base, pathAndQuery), { method, headers: options.headers, body });
  }
  const signed = s3SignedHeaders(
    base,
    method,
    pathAndQuery,
    body,
    options.headers,
    options.timestamp,
    options.payloadHash,
  );
  if (options.badSignature) {
    signed.headers.authorization = signed.headers.authorization.replace(/.$/, (last) => (last === "0" ? "1" : "0"));
  }
  const request = {
    method,
    headers: signed.headers,
    body: options.fragmented ? fragmentedBody(body ?? new Uint8Array()) : body,
    ...(options.fragmented ? { duplex: "half" } : {}),
  };
  return fetch(signed.url, request);
}

async function startRust() {
  const child = spawn(
    process.env.CARGO ?? "cargo",
    ["run", "--quiet", "--example", "http_oracle", "--"],
    {
      cwd: repo,
      env: { ...process.env, CARGO_TERM_COLOR: "never", RUST_BACKTRACE: "1" },
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  const lines = createInterface({ input: child.stdout });
  const errors = [];
  child.stderr.on("data", (chunk) => errors.push(String(chunk)));
  const ready = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Rust fixture did not start\n${errors.join("")}`)), 120_000);
    const onExit = (code, signal) => {
      clearTimeout(timeout);
      reject(new Error(`Rust fixture exited ${code ?? signal}\n${errors.join("")}`));
    };
    child.once("exit", onExit);
    lines.on("line", (line) => {
      const prefix = "MOUNT_RS_HTTP_ORACLE_READY ";
      if (!line.startsWith(prefix)) return;
      clearTimeout(timeout);
      child.off("exit", onExit);
      resolve(JSON.parse(line.slice(prefix.length)));
    });
  });
  return { child, ready, errors, lines };
}

async function stopRust(fixture) {
  if (!fixture) return;
  fixture.lines.close();
  if (fixture.child.exitCode !== null) {
    if (fixture.child.exitCode !== 0) {
      throw new Error(`Rust fixture stopped with ${fixture.child.exitCode}\n${fixture.errors.join("")}`);
    }
    return;
  }
  fixture.child.stdin.end();
  const [code, signal] = await once(fixture.child, "exit");
  if (code !== 0) {
    throw new Error(`Rust fixture stopped with ${code ?? signal}\n${fixture.errors.join("")}`);
  }
}

async function runWebdavPair(tsBase, rustBase) {
  let tsEtag;
  let rustEtag;
  const run = async (label, method, path, options = {}) => {
    const result = await pair(
      `webdav/${label}`,
      () => fetchWebdav(tsBase, method, path, options),
      () => fetchWebdav(rustBase, method, path, options),
      { method },
    );
    const etag = result.typescript.headers.etag;
    if (etag) {
      tsEtag = etag;
      rustEtag = result.rust.headers.etag;
    }
    return result;
  };

  await run("unauthorized-options", "OPTIONS", "/", { auth: false });
  await run("wrong-password", "OPTIONS", "/", {
    auth: false,
    headers: { authorization: basic(WEBDAV_USER, "wrong") },
  });
  await run("options", "OPTIONS", "/");
  await run("mkcol", "MKCOL", "/docs");

  const payload = seeded(256 * 1024 + 17);
  await run("streaming-put", "PUT", "/docs/hello%20world.bin", {
    headers: {
      "content-length": String(payload.byteLength),
      "content-type": "application/octet-stream",
    },
    body: payload,
    fragmented: true,
  });
  await run("proppatch-fixed-time", "PROPPATCH", "/docs/hello%20world.bin", {
    headers: { "content-type": "application/xml; charset=utf-8" },
    body:
      `<?xml version="1.0" encoding="utf-8"?>` +
      `<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop>` +
      `<D:getlastmodified>${FIXED_MTIME_DATE}</D:getlastmodified>` +
      `</D:prop></D:set></D:propertyupdate>`,
  });
  const get = await run("get", "GET", "/docs/hello%20world.bin");
  tsEtag = get.typescript.headers.etag;
  rustEtag = get.rust.headers.etag;
  await run("head", "HEAD", "/docs/hello%20world.bin");
  await run("range", "GET", "/docs/hello%20world.bin", {
    headers: { range: "bytes=17-63" },
  });
  await run("propfind-links", "PROPFIND", "/docs", {
    headers: { depth: "1", "content-type": "application/xml" },
  });
  await pair(
    "webdav/if-none-match",
    () => fetchWebdav(tsBase, "GET", "/docs/hello%20world.bin", { headers: { "if-none-match": tsEtag } }),
    () => fetchWebdav(rustBase, "GET", "/docs/hello%20world.bin", { headers: { "if-none-match": rustEtag } }),
    { method: "GET" },
  );
  await pair(
    "webdav/copy",
    () => fetchWebdav(tsBase, "COPY", "/docs/hello%20world.bin", { headers: { destination: `${tsBase}/docs/copied.bin` } }),
    () => fetchWebdav(rustBase, "COPY", "/docs/hello%20world.bin", { headers: { destination: `${rustBase}/docs/copied.bin` } }),
    { method: "COPY" },
  );
  await pair(
    "webdav/move",
    () => fetchWebdav(tsBase, "MOVE", "/docs/copied.bin", { headers: { destination: `${tsBase}/docs/moved.bin` } }),
    () => fetchWebdav(rustBase, "MOVE", "/docs/copied.bin", { headers: { destination: `${rustBase}/docs/moved.bin` } }),
    { method: "MOVE" },
  );
  await run("unsupported-method", "PATCH", "/docs/moved.bin");
  await run("missing-get", "GET", "/docs/missing.bin");
  await run("delete", "DELETE", "/docs/moved.bin");
}

async function runS3Pair(tsBase, rustBase) {
  let tsEtag;
  let rustEtag;
  const run = async (label, method, path, options = {}) => {
    const result = await pair(
      `s3/${label}`,
      () => fetchS3(tsBase, method, path, options),
      () => fetchS3(rustBase, method, path, options),
      { method },
    );
    if (result.typescript.headers.etag) {
      tsEtag = result.typescript.headers.etag;
      rustEtag = result.rust.headers.etag;
    }
    return result;
  };

  await run("unauthorized-list", "GET", "/", { auth: false });
  await run("list-buckets", "GET", "/");
  await run("wrong-signature", "GET", `/${S3_BUCKET}/missing.txt`, {
    timestamp: Date.now(),
    badSignature: true,
  });

  const payload = seeded(256 * 1024 + 17);
  const put = await run("streaming-put", "PUT", `/${S3_BUCKET}/tree/hello.bin`, {
    headers: {
      "content-length": String(payload.byteLength),
      "x-amz-meta-mtime": FIXED_MTIME_SECONDS,
      "content-type": "application/octet-stream",
    },
    body: payload,
    fragmented: true,
  });
  tsEtag = put.typescript.headers.etag;
  rustEtag = put.rust.headers.etag;
  await run("head", "HEAD", `/${S3_BUCKET}/tree/hello.bin`);
  await run("get", "GET", `/${S3_BUCKET}/tree/hello.bin`);
  await run("range", "GET", `/${S3_BUCKET}/tree/hello.bin`, {
    headers: { range: "bytes=17-63" },
  });
  await pair(
    "s3/if-none-match",
    () => fetchS3(tsBase, "GET", `/${S3_BUCKET}/tree/hello.bin`, { headers: { "if-none-match": tsEtag } }),
    () => fetchS3(rustBase, "GET", `/${S3_BUCKET}/tree/hello.bin`, { headers: { "if-none-match": rustEtag } }),
    { method: "GET" },
  );
  await run("list-v2-delimiter", "GET", `/${S3_BUCKET}?list-type=2&prefix=tree%2F&delimiter=%2F`);
  await run("copy", "PUT", `/${S3_BUCKET}/tree/copied.bin`, {
    headers: { "x-amz-copy-source": `/${S3_BUCKET}/tree/hello.bin` },
  });
  await run("delete", "DELETE", `/${S3_BUCKET}/tree/copied.bin`);
  await run("missing-get", "GET", `/${S3_BUCKET}/missing.txt`);
  // The Rust gateway additionally rejects ordinary signed-body hash
  // mismatches; that safety invariant has a real-HTTP regression test in
  // transports/mount-rs-s3/tests/gateway.rs. This differential case keeps
  // the pinned oracle on the shared successful path with a matching digest.
  await run("signed-body-digest", "PUT", `/${S3_BUCKET}/body-digest.bin`, {
    body: bytes("body whose digest is intentionally wrong"),
    payloadHash: signer.sha256Hex(bytes("body whose digest is intentionally wrong")),
    headers: { "x-amz-meta-mtime": FIXED_MTIME_SECONDS },
  });
  await run("unsupported-method", "PATCH", `/${S3_BUCKET}/tree/hello.bin`);
}

let rust;
let tsS3;
let tsWebdav;
try {
  tsS3 = s3Oracle.createS3Server(createS3MemoryDriver(), {
    host: "127.0.0.1",
    credentials: CREDENTIALS,
    region: REGION,
  });
  tsWebdav = webdavOracle.createWebdavServer(fixedTimeDriver(createWebdavMemoryDriver()), {
    host: "127.0.0.1",
    credentials: { username: WEBDAV_USER, password: WEBDAV_PASSWORD },
  });
  await Promise.all([tsS3.listen(), tsWebdav.listen()]);
  rust = await startRust();
  await runS3Pair(tsS3.url, rust.ready.s3);
  await runWebdavPair(tsWebdav.url, rust.ready.webdav);
  console.log("mount-rs HTTP TypeScript/Rust differential: PASS (S3 + WebDAV, 30 paired cases)");
} finally {
  await tsS3?.close();
  await tsWebdav?.close();
  await stopRust(rust);
}
