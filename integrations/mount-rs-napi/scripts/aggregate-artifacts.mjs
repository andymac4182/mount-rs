import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { cp, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const napiCommand = fileURLToPath(new URL("../node_modules/.bin/napi", import.meta.url));

/**
 * The four native artifacts uploaded by the CI node matrix. Keep this list in
 * the same target order as package.json so the validation also checks that an
 * aggregate job cannot silently omit one architecture.
 */
export const NATIVE_TARGETS = Object.freeze([
  Object.freeze({
    platformArchABI: "darwin-arm64",
    artifact: "mount-rs.darwin-arm64.node",
    packageName: "@andymac4182/mount-rs-darwin-arm64",
  }),
  Object.freeze({
    platformArchABI: "darwin-x64",
    artifact: "mount-rs.darwin-x64.node",
    packageName: "@andymac4182/mount-rs-darwin-x64",
  }),
  Object.freeze({
    platformArchABI: "linux-arm64-gnu",
    artifact: "mount-rs.linux-arm64-gnu.node",
    packageName: "@andymac4182/mount-rs-linux-arm64-gnu",
  }),
  Object.freeze({
    platformArchABI: "linux-x64-gnu",
    artifact: "mount-rs.linux-x64-gnu.node",
    packageName: "@andymac4182/mount-rs-linux-x64-gnu",
  }),
]);

const ROOT_FILES = ["index.js", "index.d.ts", "postlude.cjs", "package.json"];

async function walkFiles(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...await walkFiles(path));
    } else if (entry.isFile()) {
      files.push(path);
    }
  }
  return files;
}

async function copyNativeArtifacts(inputDir, outputDir) {
  const expected = new Map(NATIVE_TARGETS.map(({ artifact }) => [artifact, null]));
  for (const source of await walkFiles(inputDir)) {
    const name = basename(source);
    if (!name.endsWith(".node")) continue;
    if (!expected.has(name)) {
      throw new Error(`Unexpected native artifact ${relative(inputDir, source)}`);
    }
    if (expected.get(name) !== null) {
      throw new Error(`Duplicate native artifact ${name}`);
    }
    expected.set(name, source);
  }

  for (const target of NATIVE_TARGETS) {
    const source = expected.get(target.artifact);
    assert(source !== null, `Missing native artifact ${target.artifact}`);
    await cp(source, join(outputDir, target.artifact));
  }
}

async function run(command, args, cwd) {
  try {
    return await execFileAsync(command, args, {
      cwd,
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    });
  } catch (error) {
    const stdout = typeof error.stdout === "string" ? error.stdout.trim() : "";
    const stderr = typeof error.stderr === "string" ? error.stderr.trim() : "";
    const detail = [stdout, stderr].filter(Boolean).join("\n");
    throw new Error(`${basename(command)} ${args.join(" ")} failed${detail ? `:\n${detail}` : ""}`, {
      cause: error,
    });
  }
}

async function copyRootPackage(stagingDir) {
  for (const file of ROOT_FILES) {
    await cp(join(packageRoot, file), join(stagingDir, file));
  }
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

function packageFiles(report) {
  return new Set(report.files.map(({ path }) => path));
}

function assertPackageManifest(packageJson) {
  assert.equal(packageJson.name, "@andymac4182/mount-rs");
  assert.deepEqual(
    packageJson.optionalDependencies,
    Object.fromEntries(NATIVE_TARGETS.map(({ packageName }) => [packageName, packageJson.version])),
  );
}

async function packDryRun(directory) {
  const { stdout } = await run("pnpm", ["pack", "--dry-run", "--json"], directory);
  return JSON.parse(stdout.trim());
}

async function validateStagedPackages(stagingDir) {
  const packageJson = await readJson(join(stagingDir, "package.json"));
  assertPackageManifest(packageJson);

  const stagedPackages = {};
  for (const target of NATIVE_TARGETS) {
    const targetDir = join(stagingDir, "npm", target.platformArchABI);
    const targetPackage = await readJson(join(targetDir, "package.json"));
    assert.equal(targetPackage.name, target.packageName);
    assert.equal(targetPackage.main, target.artifact);
    assert.deepEqual(targetPackage.files, [target.artifact]);
    await stat(join(targetDir, target.artifact));
    const targetReport = await packDryRun(targetDir);
    const targetFiles = packageFiles(targetReport);
    assert(targetFiles.has("package.json"), `${target.packageName} pack is missing package.json`);
    assert(targetFiles.has(target.artifact), `${target.packageName} pack is missing ${target.artifact}`);
    stagedPackages[target.platformArchABI] = targetPackage;
  }

  const report = await packDryRun(stagingDir);
  const files = packageFiles(report);
  for (const required of ROOT_FILES) {
    assert(files.has(required), `staged package is missing ${required}`);
  }
  for (const target of NATIVE_TARGETS) {
    const prefix = `npm/${target.platformArchABI}/`;
    for (const file of ["package.json", "README.md", target.artifact]) {
      assert(files.has(`${prefix}${file}`), `staged package is missing ${prefix}${file}`);
    }
  }

  return {
    packageJson,
    files: [...files].sort(),
    stagedPackages,
    tarball: report.filename,
  };
}

/**
 * Aggregate CI-downloaded native addons in an isolated staging package.
 *
 * The source checkout is never changed. The staging copy is reconciled by
 * NAPI-RS, which creates `npm/<platform-arch-abi>` packages, copies each
 * binary into the matching package, and is then checked with a pack dry-run.
 * Root optionalDependencies are written locally; no publication is attempted.
 */
export async function aggregateArtifacts({ inputDir, stagingDir, keepStaging = false }) {
  const resolvedInputDir = resolve(inputDir);
  await stat(resolvedInputDir);

  const ownsStagingDir = stagingDir === undefined;
  const resolvedStagingDir = ownsStagingDir
    ? await mkdtemp(join(dirname(packageRoot), ".mount-rs-napi-aggregate-"))
    : resolve(stagingDir);
  let createdStaging = ownsStagingDir;

  try {
    if (!ownsStagingDir) {
      await stat(resolvedStagingDir).then(
        () => {
          throw new Error(`Staging directory already exists: ${resolvedStagingDir}`);
        },
        (error) => {
          if (error.code !== "ENOENT") throw error;
        },
      );
      await mkdir(resolvedStagingDir);
      createdStaging = true;
    }
    await copyRootPackage(resolvedStagingDir);
    const stagedArtifacts = join(resolvedStagingDir, "artifacts");
    await mkdir(stagedArtifacts, { recursive: true });
    await copyNativeArtifacts(resolvedInputDir, stagedArtifacts);

    await run(napiCommand, ["create-npm-dirs"], resolvedStagingDir);
    await run(
      napiCommand,
      ["artifacts", "--output-dir", "artifacts", "--npm-dir", "npm"],
      resolvedStagingDir,
    );
    // Pure local staging: never call a publishing or release command, even
    // with suppression flags. Validation does not require registry writes.
    const manifestPath = join(resolvedStagingDir, "package.json");
    const manifest = await readJson(manifestPath);
    manifest.optionalDependencies = Object.fromEntries(
      NATIVE_TARGETS.map(({ packageName }) => [packageName, manifest.version]),
    );
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

    const result = await validateStagedPackages(resolvedStagingDir);
    return {
      ...result,
      stagingDir: resolvedStagingDir,
      kept: keepStaging,
    };
  } finally {
    if (createdStaging && !keepStaging) {
      await rm(resolvedStagingDir, { recursive: true, force: true });
    }
  }
}

function parseArgs(args) {
  const options = {
    inputDir: process.env.MOUNT_RS_NATIVE_ARTIFACTS ?? "artifacts",
    stagingDir: undefined,
    keepStaging: false,
  };
  for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === "--") {
      continue;
    } else if (argument === "--input-dir") {
      options.inputDir = args[++index];
    } else if (argument === "--staging-dir") {
      options.stagingDir = args[++index];
    } else if (argument === "--keep-staging") {
      options.keepStaging = true;
    } else if (argument === "--help" || argument === "-h") {
      console.log("Usage: node scripts/aggregate-artifacts.mjs --input-dir <downloaded-artifacts> [--staging-dir <path>] [--keep-staging]");
      process.exit(0);
    } else {
      throw new Error(`Unknown argument ${argument}`);
    }
    if (argument === "--input-dir" || argument === "--staging-dir") {
      if (!options.inputDir && argument === "--input-dir") throw new Error("--input-dir requires a path");
      if (!options.stagingDir && argument === "--staging-dir") throw new Error("--staging-dir requires a path");
    }
  }
  return options;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const result = await aggregateArtifacts(parseArgs(process.argv.slice(2)));
  console.log(JSON.stringify({
    package: result.packageJson.name,
    version: result.packageJson.version,
    optionalDependencies: result.packageJson.optionalDependencies,
    tarball: result.tarball,
    files: result.files,
  }, null, 2));
}
