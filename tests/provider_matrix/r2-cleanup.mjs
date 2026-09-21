import { createHash, createHmac } from "node:crypto";

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function hmac(key, value) {
  return createHmac("sha256", key).update(value).digest();
}

function encode(value) {
  return encodeURIComponent(value).replace(/[!'()*]/g, (character) =>
    `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

function xmlDecode(value) {
  return value
    .replaceAll("&amp;", "&")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'");
}

function validatePrefix(prefix) {
  const ownedRoots = ["mount-rs-provider-matrix/", "mount-rs-ozone/"];
  if (!ownedRoots.some((root) => prefix.startsWith(root)) || prefix.includes("..")) {
    throw new Error(`R2 cleanup prefix is outside the test-owned scope: ${prefix}`);
  }
}

function requestUrl(config, key, query) {
  const endpoint = config.endpoint.replace(/\/+$/, "");
  const path = key
    ? `/${encode(config.bucket)}/${key.split("/").map(encode).join("/")}`
    : `/${encode(config.bucket)}/`;
  const url = new URL(endpoint + path);
  for (const [name, value] of Object.entries(query ?? {})) {
    if (value !== undefined) url.searchParams.set(name, value);
  }
  return url;
}

function canonicalQuery(url) {
  return [...url.searchParams.entries()]
    .sort(([leftName, leftValue], [rightName, rightValue]) =>
      encode(leftName).localeCompare(encode(rightName)) ||
      encode(leftValue).localeCompare(encode(rightValue)),
    )
    .map(([name, value]) => `${encode(name)}=${encode(value)}`)
    .join("&");
}

export function r2ConfigFromEnv(env = process.env) {
  const names = [
    "R2_ENDPOINT",
    "R2_BUCKET",
    "R2_ACCESS_KEY_ID",
    "R2_SECRET_ACCESS_KEY",
  ];
  const missing = names.filter((name) => !env[name]);
  if (missing.length > 0) return { missing };
  return {
    config: {
      endpoint: env.R2_ENDPOINT,
      bucket: env.R2_BUCKET,
      accessKeyId: env.R2_ACCESS_KEY_ID,
      secretAccessKey: env.R2_SECRET_ACCESS_KEY,
      region: env.RUSTFS_REGION || "auto",
    },
    missing: [],
  };
}

export function rustfsConfigFromEnv(env = process.env) {
  const result = r2ConfigFromEnv(env);
  if (result.missing.length > 0) return result;
  if (!/^http:\/\/(127\.0\.0\.1|localhost):\d+(?:\/|$)/.test(env.R2_ENDPOINT)) {
    return { missing: ["R2_ENDPOINT(loopback RustFS)"] };
  }
  return result;
}

async function signedRequest(config, method, key, query = {}) {
  const url = requestUrl(config, key, query);
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
    canonicalQuery(url),
    canonicalHeaders,
    signedHeaders,
    payloadHash,
  ].join("\n");
  const scope = `${date}/${config.region}/s3/aws4_request`;
  const signingKey = hmac(
    hmac(hmac(hmac(`AWS4${config.secretAccessKey}`, date), config.region), "s3"),
    "aws4_request",
  );
  const signature = createHmac("sha256", signingKey)
    .update(["AWS4-HMAC-SHA256", amzDate, scope, sha256(canonicalRequest)].join("\n"))
    .digest("hex");
  return fetch(url, {
    method,
    headers: {
      authorization:
        `AWS4-HMAC-SHA256 Credential=${config.accessKeyId}/${scope}, ` +
        `SignedHeaders=${signedHeaders}, Signature=${signature}`,
      host: url.host,
      "x-amz-content-sha256": payloadHash,
      "x-amz-date": amzDate,
    },
    signal: AbortSignal.timeout(10_000),
  });
}

export async function listR2Prefix(config, prefix) {
  validatePrefix(prefix);
  const keys = new Set();
  let continuationToken;
  do {
    const response = await signedRequest(config, "GET", "", {
      "list-type": "2",
      prefix: `${prefix}/`,
      "continuation-token": continuationToken,
    });
    const body = await response.text();
    if (response.status !== 200) {
      throw new Error(`S3-compatible prefix listing failed: HTTP ${response.status} ${body}`);
    }
    for (const match of body.matchAll(/<Key>([^<]*)<\/Key>/g)) {
      const key = xmlDecode(match[1]);
      if (!key.startsWith(`${prefix}/`)) {
        throw new Error(`S3-compatible listing escaped the owned prefix: ${key}`);
      }
      keys.add(key);
    }
    const next = body.match(/<NextContinuationToken>([^<]*)<\/NextContinuationToken>/);
    continuationToken = next ? xmlDecode(next[1]) : undefined;
  } while (continuationToken);
  return keys;
}

export async function cleanupR2Prefix(config, prefix, protectedKeys = new Set()) {
  const keys = await listR2Prefix(config, prefix);
  for (const key of keys) {
    if (protectedKeys.has(key)) continue;
    const response = await signedRequest(config, "DELETE", key);
    const body = await response.text();
    if (![200, 204, 404].includes(response.status)) {
      throw new Error(`S3-compatible object cleanup failed for ${key}: HTTP ${response.status} ${body}`);
    }
  }
  const remaining = await listR2Prefix(config, prefix);
  for (const key of remaining) {
    if (!protectedKeys.has(key)) {
      throw new Error(`RustFS prefix cleanup left object ${key}`);
    }
  }
}
