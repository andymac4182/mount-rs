#!/usr/bin/env node

// Build the credential-free W08 release identity manifest for a real artifact.
// This computes artifact metadata only; it does not sign artifacts, create an
// SBOM, run a canary, perform rollback, or approve a release.

import { createHash } from "node:crypto";
import { lstat, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

function fail(reason) {
  console.error(`W08_RELEASE_MANIFEST_WRITE_FAIL reason=${reason}`);
  process.exit(1);
}

function requiredOption(options, name) {
  const value = options[name];
  if (typeof value !== "string" || value.length === 0) {
    fail(`${name}-required`);
  }
  return value;
}

function parsePositiveRunId(value) {
  const runId = Number(value);
  if (!Number.isSafeInteger(runId) || runId <= 0) {
    fail("workflow-run-id-must-be-positive-safe-integer");
  }
  return runId;
}

const supportedOptions = new Set([
  "artifact",
  "output",
  "repository",
  "source-commit",
  "version",
  "tag",
  "target",
  "workflow",
  "workflow-run-id",
  "signature-status",
  "sbom-status",
  "canary-status",
]);
const options = {};
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const argument = args[index];
  if (!argument.startsWith("--")) fail("unexpected-positional-argument");
  const name = argument.slice(2);
  if (!supportedOptions.has(name)) fail(`unknown-option-${name}`);
  const value = args[index + 1];
  if (!value || value.startsWith("--")) fail(`${name}-value-required`);
  options[name] = value;
  index += 1;
}

const artifactPath = requiredOption(options, "artifact");
const outputPath = requiredOption(options, "output");
const repository = options.repository ?? process.env.GITHUB_REPOSITORY;
if (repository !== "andymac4182/mount-rs") {
  fail("repository-must-be-andymac4182/mount-rs");
}
const sourceCommit = requiredOption(options, "source-commit");
if (!/^[0-9a-f]{40}$/u.test(sourceCommit)) {
  fail("source-commit-must-be-lowercase-40-hex-commit");
}
const version = requiredOption(options, "version");
const tag = requiredOption(options, "tag");
const target = requiredOption(options, "target");
const workflow = requiredOption(options, "workflow");
const workflowRunId = parsePositiveRunId(
  options["workflow-run-id"] ?? process.env.GITHUB_RUN_ID,
);
const signatureStatus = options["signature-status"] ?? "pending";
const sbomStatus = options["sbom-status"] ?? "pending";
const canaryStatus = options["canary-status"] ?? "not-run";

let artifactBytes;
try {
  const metadata = await lstat(artifactPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail("artifact-must-be-regular-file");
  }
  artifactBytes = await readFile(artifactPath);
} catch (error) {
  if (error?.message?.startsWith("W08_RELEASE_MANIFEST_WRITE_FAIL")) throw error;
  fail("artifact-unreadable");
}

const outputMetadata = await lstat(outputPath).catch(() => null);
if (outputMetadata?.isSymbolicLink() || outputMetadata?.isDirectory()) {
  fail("output-must-not-be-symbolic-link-or-directory");
}

const artifactName = path.basename(artifactPath);
if (!artifactName || artifactName === "." || artifactName === "..") {
  fail("artifact-name-must-be-basename");
}

const manifest = {
  schema: "mount-rs.release-manifest.v1",
  workstream: "W08",
  repository,
  source_commit: sourceCommit,
  version,
  tag,
  target,
  workflow,
  workflow_run_id: workflowRunId,
  artifact: {
    name: artifactName,
    sha256: createHash("sha256").update(artifactBytes).digest("hex"),
    size_bytes: artifactBytes.byteLength,
  },
  checksums: {
    algorithm: "sha256",
    file: "SHA256SUMS",
  },
  provenance: {
    builder: "github-actions",
    repository,
    source_commit: sourceCommit,
    workflow,
    run_id: workflowRunId,
  },
  release_controls: {
    signature_status: signatureStatus,
    sbom_status: sbomStatus,
    canary_status: canaryStatus,
  },
};

await writeFile(outputPath, `${JSON.stringify(manifest, null, 2)}\n`, {
  encoding: "utf8",
  mode: 0o644,
});

console.log(
  "W08_RELEASE_MANIFEST_WRITTEN " +
    `output=${outputPath} artifact=${artifactName} ` +
    `sha256=${manifest.artifact.sha256} size_bytes=${manifest.artifact.size_bytes}`,
);
