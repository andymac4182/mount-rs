#!/usr/bin/env node

// Credential-free policy gate for W08 release identity and provenance.
// This validates manifest shape and, when supplied, an artifact checksum. It
// does not sign artifacts, create an SBOM, run a canary, or approve a release.

import { createHash } from "node:crypto";
import { lstat, readFile } from "node:fs/promises";

const args = process.argv.slice(2);
const requireAcceptance = args.includes("--require-release-acceptance");
const positional = args.filter((value) => value !== "--require-release-acceptance");
const [manifestPath, artifactPath] = positional;

function fail(reason) {
  console.error(`W08_RELEASE_MANIFEST_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, path) {
  if (!isObject(value)) fail(`${path}-must-be-object`);
  return value;
}

function requireString(value, path) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${path}-must-be-non-empty-string`);
  }
  if (/[<>]/u.test(value)) fail(`${path}-contains-placeholder`);
  return value;
}

function requireMatch(value, path, pattern, reason) {
  requireString(value, path);
  if (!pattern.test(value)) fail(`${path}-${reason}`);
  return value;
}

function requirePositiveInteger(value, path) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    fail(`${path}-must-be-positive-safe-integer`);
  }
  return value;
}

async function readRegularJson(path, label) {
  if (!path) fail(`${label}-path-required`);
  try {
    const metadata = await lstat(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      fail(`${label}-must-be-regular-file`);
    }
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if (error?.message?.startsWith("W08_RELEASE_MANIFEST_POLICY_FAIL")) throw error;
    fail(`${label}-unreadable-or-invalid-json`);
  }
}

if (
  positional.length < 1 ||
  positional.length > 2 ||
  args.length !== positional.length + (requireAcceptance ? 1 : 0)
) {
  console.error(
    "usage: verify-w08-release-manifest.mjs <manifest.json> [artifact] " +
      "[--require-release-acceptance]",
  );
  process.exit(2);
}

const manifest = requireObject(
  await readRegularJson(manifestPath, "manifest"),
  "manifest",
);
if (manifest.schema !== "mount-rs.release-manifest.v1") {
  fail("manifest.schema-must-be-mount-rs.release-manifest.v1");
}
if (manifest.workstream !== "W08") fail("manifest.workstream-must-be-W08");
if (manifest.repository !== "andymac4182/mount-rs") {
  fail("manifest.repository-must-be-andymac4182/mount-rs");
}

const sourceCommit = requireMatch(
  manifest.source_commit,
  "manifest.source_commit",
  /^[0-9a-f]{40}$/u,
  "must-be-lowercase-40-hex-commit",
);
requireMatch(
  manifest.version,
  "manifest.version",
  /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u,
  "must-be-semver",
);
requireString(manifest.tag, "manifest.tag");
requireString(manifest.target, "manifest.target");
const workflow = requireString(manifest.workflow, "manifest.workflow");
const workflowRunId = requirePositiveInteger(
  manifest.workflow_run_id,
  "manifest.workflow_run_id",
);

const artifact = requireObject(manifest.artifact, "manifest.artifact");
const artifactName = requireString(artifact.name, "manifest.artifact.name");
if (artifactName.includes("/") || artifactName.includes("\\")) {
  fail("manifest.artifact.name-must-be-basename");
}
const artifactSha256 = requireMatch(
  artifact.sha256,
  "manifest.artifact.sha256",
  /^[0-9a-f]{64}$/u,
  "must-be-lowercase-64-hex-sha256",
);
const artifactSize = requirePositiveInteger(
  artifact.size_bytes,
  "manifest.artifact.size_bytes",
);

const checksums = requireObject(manifest.checksums, "manifest.checksums");
if (checksums.algorithm !== "sha256") {
  fail("manifest.checksums.algorithm-must-be-sha256");
}
if (checksums.file !== "SHA256SUMS") {
  fail("manifest.checksums.file-must-be-SHA256SUMS");
}

const provenance = requireObject(manifest.provenance, "manifest.provenance");
if (provenance.builder !== "github-actions") {
  fail("manifest.provenance.builder-must-be-github-actions");
}
if (provenance.repository !== manifest.repository) {
  fail("manifest.provenance.repository-must-match");
}
if (provenance.source_commit !== sourceCommit) {
  fail("manifest.provenance.source_commit-must-match");
}
if (provenance.workflow !== workflow) {
  fail("manifest.provenance.workflow-must-match");
}
if (provenance.run_id !== workflowRunId) {
  fail("manifest.provenance.run_id-must-match");
}

const controls = requireObject(manifest.release_controls, "manifest.release_controls");
const signatureStatus = requireString(
  controls.signature_status,
  "manifest.release_controls.signature_status",
);
const sbomStatus = requireString(
  controls.sbom_status,
  "manifest.release_controls.sbom_status",
);
const canaryStatus = requireString(
  controls.canary_status,
  "manifest.release_controls.canary_status",
);
if (!["pending", "verified"].includes(signatureStatus)) {
  fail("manifest.release_controls.signature_status-must-be-pending-or-verified");
}
if (!["pending", "verified"].includes(sbomStatus)) {
  fail("manifest.release_controls.sbom_status-must-be-pending-or-verified");
}
if (!["not-run", "passed"].includes(canaryStatus)) {
  fail("manifest.release_controls.canary_status-must-be-not-run-or-passed");
}
if (
  requireAcceptance &&
  (signatureStatus !== "verified" ||
    sbomStatus !== "verified" ||
    canaryStatus !== "passed")
) {
  fail("manifest.release_controls-production-acceptance-incomplete");
}

if (artifactPath) {
  let artifactBytes;
  try {
    const metadata = await lstat(artifactPath);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      fail("artifact-must-be-regular-file");
    }
    artifactBytes = await readFile(artifactPath);
  } catch (error) {
    if (error?.message?.startsWith("W08_RELEASE_MANIFEST_POLICY_FAIL")) throw error;
    fail("artifact-unreadable");
  }
  const actualSha256 = createHash("sha256").update(artifactBytes).digest("hex");
  if (actualSha256 !== artifactSha256) fail("artifact-sha256-mismatch");
  if (artifactBytes.byteLength !== artifactSize) fail("artifact-size-mismatch");
}

console.log(
  "W08_RELEASE_MANIFEST_POLICY_PASS " +
    `source_commit=${sourceCommit} artifact=${artifactName} ` +
    `signature=${signatureStatus} sbom=${sbomStatus} canary=${canaryStatus}`,
);
