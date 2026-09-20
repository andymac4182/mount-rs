import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { aggregateArtifacts, NATIVE_TARGETS } from "../scripts/aggregate-artifacts.mjs";

const inputDir = await mkdtemp(join(tmpdir(), "mount-rs-native-artifacts-"));
const before = await readFile(new URL("../package.json", import.meta.url), "utf8");

assert.deepEqual(
  NATIVE_TARGETS.find(({ platformArchABI }) => platformArchABI === "win32-x64-msvc"),
  {
    platformArchABI: "win32-x64-msvc",
    artifact: "mount-rs.win32-x64-msvc.node",
    packageName: "@mount-rs/core-win32-x64-msvc",
  },
  "Windows MSVC must remain part of the published native target matrix",
);

try {
  const existingDir = await mkdtemp(join(tmpdir(), "mount-rs-existing-stage-"));
  try {
    const sentinel = join(existingDir, "preserve.txt");
    await writeFile(sentinel, "existing user data");
    await assert.rejects(aggregateArtifacts({ inputDir, stagingDir: existingDir }), /already exists/);
    assert.equal(await readFile(sentinel, "utf8"), "existing user data");
  } finally {
    await rm(existingDir, { recursive: true, force: true });
  }
  // download-artifact may preserve one directory per matrix artifact. The
  // aggregator intentionally accepts either that layout or a flat merge.
  for (const target of NATIVE_TARGETS) {
    const directory = join(inputDir, `native-${target.platformArchABI}`);
    await mkdir(directory, { recursive: true });
    await writeFile(join(directory, target.artifact), Buffer.from(target.artifact));
  }

  const result = await aggregateArtifacts({ inputDir });
  assert.deepEqual(
    result.packageJson.optionalDependencies,
    Object.fromEntries(NATIVE_TARGETS.map(({ packageName }) => [packageName, "0.1.0"])),
  );
  for (const target of NATIVE_TARGETS) {
    assert(result.files.includes(`npm/${target.platformArchABI}/${target.artifact}`));
    assert.equal(result.stagedPackages[target.platformArchABI].name, target.packageName);
  }

  const missingDir = await mkdtemp(join(tmpdir(), "mount-rs-missing-artifact-"));
  try {
    await assert.rejects(
      aggregateArtifacts({ inputDir: missingDir }),
      /Missing native artifact mount-rs\.darwin-arm64\.node/,
    );
  } finally {
    await rm(missingDir, { recursive: true, force: true });
  }
} finally {
  await rm(inputDir, { recursive: true, force: true });
}

assert.equal(await readFile(new URL("../package.json", import.meta.url), "utf8"), before);
console.log("mount-rs N-API artifact aggregation: PASS");
