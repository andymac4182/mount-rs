#!/usr/bin/env node

// Validate the fixed W07 workload artifact without turning one benchmark
// machine's throughput into a production capacity claim. The benchmark must
// complete the exact workload shape with a non-zero completion floor; the
// measured rate is retained for later owner-approved SLO/capacity comparison.

import { lstat, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { validateArtifact, W26_IOPS_PROFILE } from "./verify-w26-ozone-iops-artifact.mjs";

export const W07_WORKLOAD_PROFILE = Object.freeze({
  name: "w07-bounded",
  provider: "mount-rs-split-foundationdb-r2",
  sizeMiB: 1,
  payloadBytes: W26_IOPS_PROFILE.payloadBytes,
  iterations: W26_IOPS_PROFILE.iterations,
  concurrency: W26_IOPS_PROFILE.concurrency,
  minimumIops: 1,
});

function failure(reason) {
  throw new Error(`W07_WORKLOAD_ARTIFACT_FAIL reason=${reason}`);
}

function requireEqual(value, expected, field) {
  if (value !== expected) failure(`${field}-must-equal-${expected}`);
}

function requireFinitePositive(value, field) {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    failure(`${field}-must-be-finite-positive-number`);
  }
}

export function validateW07WorkloadArtifact(document) {
  let result;
  try {
    result = validateArtifact(document, {
      providers: [W07_WORKLOAD_PROFILE.provider],
      minimumIops: W07_WORKLOAD_PROFILE.minimumIops,
      minimumIopsFloor: W07_WORKLOAD_PROFILE.minimumIops,
    });
  } catch (error) {
    const reason = error?.message?.replace(/^W26_OZONE_IOPS_ARTIFACT_FAIL reason=/u, "") ??
      "base-artifact-validation-failed";
    failure(reason);
  }

  const root = document;
  requireEqual(root.config.sizesMiB.length, 1, "config.sizesMiB.length");
  requireEqual(root.config.sizesMiB[0], W07_WORKLOAD_PROFILE.sizeMiB, "config.sizesMiB[0]");
  requireEqual(root.config.payloadSizesBytes.length, 1, "config.payloadSizesBytes.length");
  requireEqual(
    root.config.payloadSizesBytes[0],
    W07_WORKLOAD_PROFILE.payloadBytes,
    "config.payloadSizesBytes[0]",
  );

  const measuredIops = [];
  for (const provider of root.providers) {
    for (const size of provider.sizes) {
      requireEqual(size.sizeMiB, W07_WORKLOAD_PROFILE.sizeMiB, `${provider.provider}.sizeMiB`);
      requireEqual(
        size.iterationsRequested,
        W07_WORKLOAD_PROFILE.iterations,
        `${provider.provider}.iterationsRequested`,
      );
      requireEqual(
        size.concurrency,
        W07_WORKLOAD_PROFILE.concurrency,
        `${provider.provider}.concurrency`,
      );
      requireEqual(
        size.summary.iopsTarget,
        W07_WORKLOAD_PROFILE.minimumIops,
        `${provider.provider}.size.summary.iopsTarget`,
      );
      requireEqual(
        size.summary.iopsTargetMet,
        true,
        `${provider.provider}.size.summary.iopsTargetMet`,
      );
      requireFinitePositive(size.summary.iops, `${provider.provider}.size.summary.iops`);
      measuredIops.push(size.summary.iops);
    }
  }

  return {
    ...result,
    profile: { ...W07_WORKLOAD_PROFILE },
    measuredIops,
  };
}

function outputArgument(argv) {
  if (argv.length !== 2 || argv[0] !== "--output" || !argv[1]) {
    throw new Error("usage: verify-w07-workload-artifact.mjs --output artifact.json");
  }
  return argv[1];
}

async function readArtifact(output) {
  try {
    const metadata = await lstat(output);
    if (!metadata.isFile() || metadata.isSymbolicLink()) failure("output-must-be-regular-file");
    return JSON.parse(await readFile(output, "utf8"));
  } catch (error) {
    if (error?.message?.startsWith("W07_WORKLOAD_ARTIFACT_FAIL")) throw error;
    failure("output-unreadable-or-invalid-json");
  }
}

export async function main(argv = process.argv.slice(2)) {
  try {
    const output = outputArgument(argv);
    const artifact = await readArtifact(output);
    const result = validateW07WorkloadArtifact(artifact);
    const measuredIops = Math.min(...result.measuredIops).toFixed(2);
    console.log(
      `FOUNDATIONDB_W07_WORKLOAD_PASS profile=${W07_WORKLOAD_PROFILE.name} ` +
        `provider=${W07_WORKLOAD_PROFILE.provider} size_mib=${W07_WORKLOAD_PROFILE.sizeMiB} ` +
        `payload_bytes=${W07_WORKLOAD_PROFILE.payloadBytes} ` +
        `iterations=${W07_WORKLOAD_PROFILE.iterations} ` +
        `concurrency=${W07_WORKLOAD_PROFILE.concurrency} ` +
        `minimum_iops=${W07_WORKLOAD_PROFILE.minimumIops} ` +
        `measured_iops=${measuredIops} output=${resolve(output)}`,
    );
    return 0;
  } catch (error) {
    const message = error?.message || "validation-failed";
    console.error(
      message.startsWith("W07_WORKLOAD_ARTIFACT_FAIL")
        ? message
        : `W07_WORKLOAD_ARTIFACT_FAIL reason=${message}`,
    );
    return 1;
  }
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null;
if (invokedPath === resolve(fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code;
  });
}
