#!/usr/bin/env node

import crypto from "node:crypto";

const required = (name) => {
  const value = process.env[name];
  if (!value) throw new Error(name + " is required");
  return value;
};

const endpoint = required("R2_ENDPOINT").replace(/\/+$/, "");
const bucket = required("R2_BUCKET");
const accessKey = required("R2_ACCESS_KEY_ID");
const secretKey = required("R2_SECRET_ACCESS_KEY");
const region = process.env.OZONE_REGION || "us-east-1";
const url = new URL(endpoint + "/" + bucket);
const host = url.host;
const amzDate = new Date().toISOString().replace(/[-:]|\.\d{3}/g, "");
const date = amzDate.slice(0, 8);
const payloadHash = crypto.createHash("sha256").update("").digest("hex");
const signedHeaders = "host;x-amz-content-sha256;x-amz-date";
const canonicalHeaders =
  "host:" +
  host +
  "\n" +
  "x-amz-content-sha256:" +
  payloadHash +
  "\n" +
  "x-amz-date:" +
  amzDate +
  "\n";
const canonicalRequest = [
  "PUT",
  url.pathname,
  "",
  canonicalHeaders,
  signedHeaders,
  payloadHash,
].join("\n");
const scope = date + "/" + region + "/s3/aws4_request";
const hash = (value) => crypto.createHash("sha256").update(value).digest("hex");
const hmac = (key, value) => crypto.createHmac("sha256", key).update(value).digest();
const signingKey = hmac(
  hmac(hmac(hmac("AWS4" + secretKey, date), region), "s3"),
  "aws4_request",
);
const stringToSign =
  "AWS4-HMAC-SHA256\n" + amzDate + "\n" + scope + "\n" + hash(canonicalRequest);
const signature = crypto.createHmac("sha256", signingKey).update(stringToSign).digest("hex");
const authorization =
  "AWS4-HMAC-SHA256 Credential=" +
  accessKey +
  "/" +
  scope +
  ", SignedHeaders=" +
  signedHeaders +
  ", Signature=" +
  signature;

const response = await fetch(url, {
  method: "PUT",
  headers: {
    authorization,
    host,
    "x-amz-content-sha256": payloadHash,
    "x-amz-date": amzDate,
  },
  signal: AbortSignal.timeout(2000),
});
const body = await response.text();
if (response.status !== 200 && response.status !== 409) {
  throw new Error("Apache Ozone bucket bootstrap returned HTTP " + response.status + ": " + body);
}
console.log("OZONE_BUCKET_READY bucket=" + bucket + " status=" + response.status);
