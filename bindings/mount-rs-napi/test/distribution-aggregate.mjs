import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, sep } from "node:path";
import { promisify } from "node:util";
import { aggregateArtifacts, NATIVE_TARGETS } from "../scripts/aggregate-artifacts.mjs";
import { makeConsumerFixture, readProducerTypeVersions } from "./distribution-consumer-fixture.mjs";

const inputDir = await mkdtemp(join(tmpdir(), "mount-rs-native-artifacts-"));
const before = await readFile(new URL("../package.json", import.meta.url), "utf8");
const lockBefore = await readFile(new URL("../pnpm-lock.yaml", import.meta.url), "utf8");

const execFileAsync = promisify(execFile);
const pnpmExecutable = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
const isolatedKeys = /^PNPM_CONFIG_(STORE_DIR|CACHE_DIR|STATE_DIR|UPDATE_NOTIFIER|ENABLE_GLOBAL_VIRTUAL_STORE|USE_RUNNING_STORE_SERVER)$/i;

function ownedPnpmOptions(cwd, ownedDir) {
  return {
    cwd,
    encoding: "utf8",
    timeout: 45_000,
    maxBuffer: 1024 * 1024,
    shell: process.platform === "win32",
    env: {
      ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !isolatedKeys.test(key))),
      PNPM_CONFIG_STORE_DIR: join(ownedDir, "store"),
      PNPM_CONFIG_CACHE_DIR: join(ownedDir, "cache"),
      PNPM_CONFIG_STATE_DIR: join(ownedDir, "state"),
      PNPM_CONFIG_UPDATE_NOTIFIER: "false",
      PNPM_CONFIG_ENABLE_GLOBAL_VIRTUAL_STORE: "false",
      PNPM_CONFIG_USE_RUNNING_STORE_SERVER: "false",
    },
  };
}

async function runPnpm(args, cwd, ownedDir) {
  try {
    const result = await execFileAsync(pnpmExecutable, args, ownedPnpmOptions(cwd, ownedDir));
    console.log(JSON.stringify({ probe: args, exitCode: 0, stdout: result.stdout, stderr: result.stderr }));
    return result;
  } catch (error) {
    console.log(JSON.stringify({ probe: args, exitCode: error.code, stdout: error.stdout, stderr: error.stderr }));
    throw error;
  }
}

async function readOverrides(cwd, ownedDir) {
  const { stdout } = await runPnpm(["config", "get", "--json", "overrides"], cwd, ownedDir);
  const value = stdout.trim();
  return value === "" || value === "undefined" ? undefined : JSON.parse(value);
}

function fileSpecifier(from, to) {
  return `file:${relative(from, to).split(sep).join("/")}`;
}

async function inspectTypeTree(directory, totals) {
  for (const name of await readdir(directory)) {
    assert.notEqual(name, "node_modules", "type snapshots must not include nested dependencies");
    const path = join(directory, name);
    const stat = await lstat(path);
    assert(!stat.isSymbolicLink(), "type snapshots must not follow symlinks");
    if (stat.isDirectory()) {
      await inspectTypeTree(path, totals);
    } else {
      assert(stat.isFile(), "type snapshots contain only regular files and directories");
      totals.files += 1;
      totals.bytes += stat.size;
      assert(totals.files <= 1000 && totals.bytes <= 16 * 1024 * 1024, "type snapshot control exceeds its bound");
    }
  }
}

async function runConsumerFixtureControls() {
  const ownedRoot = await mkdtemp(join(tmpdir(), "mount-rs consumer fixture-"));
  try {
    const configDir = join(ownedRoot, "config consumer");
    await mkdir(configDir);
    const nativeOverrides = Object.fromEntries(NATIVE_TARGETS.map(({ packageName }, index) => [
      packageName, `file:../packages/native target ${index}.tgz`,
    ]));
    assert.equal(new Set(Object.keys(nativeOverrides)).size, 5);
    const typeVersions = await readProducerTypeVersions();
    const fixture = makeConsumerFixture("file:../packages/core tarball.tgz", nativeOverrides, typeVersions);
    await writeFile(join(configDir, "package.json"), `${JSON.stringify(fixture.packageJson, null, 2)}\n`);
    if (fixture.workspaceYaml !== undefined) {
      await writeFile(join(configDir, "pnpm-workspace.yaml"), fixture.workspaceYaml);
    }
    const configState = join(ownedRoot, "config state");
    for (const [key, part] of [["store-dir", "store"], ["cache-dir", "cache"], ["state-dir", "state"]]) {
      const { stdout } = await runPnpm(["config", "get", "--json", key], configDir, configState);
      assert.equal(JSON.parse(stdout), join(configState, part), `pnpm must use owned ${key}`);
    }
    const expectedOverrides = {
      ...nativeOverrides,
      "@types/node": typeVersions.node,
      "@types/node>undici-types": typeVersions.undici,
    };
    assert.deepEqual(
      await readOverrides(configDir, configState), expectedOverrides,
      "pnpm must recognize all seven fixture overrides",
    );
    assert.equal(fixture.packageJson.pnpm, undefined, "obsolete package manifest configuration must be absent");
    assert.equal(fixture.packageJson.private, true);
    assert.equal(fixture.packageJson.dependencies["@mount-rs/core"], "file:../packages/core tarball.tgz");

    const require = createRequire(import.meta.url);
    const nodePath = require.resolve("@types/node/package.json");
    const undiciPath = createRequire(nodePath).resolve("undici-types/package.json");
    const snapshots = join(ownedRoot, "snapshots");
    await mkdir(snapshots);
    const totals = { files: 0, bytes: 0 };
    for (const [path, name, version] of [[nodePath, "node", typeVersions.node], [undiciPath, "undici", typeVersions.undici]]) {
      const directory = await realpath(dirname(path));
      const manifest = JSON.parse(await readFile(path, "utf8"));
      assert.equal(manifest.version, version);
      assert.deepEqual(Object.keys(manifest.scripts ?? {}), []);
      await inspectTypeTree(directory, totals);
      await cp(directory, join(snapshots, name), { recursive: true });
    }
    console.log(JSON.stringify({ typeSnapshot: { ...typeVersions, ...totals } }));
    const typeManifest = {
      name: "mount-rs-offline-type-resolution-control",
      private: true,
      dependencies: { "@types/node": JSON.parse(before).dependencies["@types/node"] },
    };
    const nodeLine = `  ${JSON.stringify("@types/node")}: ${JSON.stringify(typeVersions.node)}`;
    const undiciLine = `  ${JSON.stringify("@types/node>undici-types")}: ${JSON.stringify(typeVersions.undici)}`;
    assert(fixture.workspaceYaml.includes(`${nodeLine}\n`));
    assert(fixture.workspaceYaml.includes(`${undiciLine}\n`));
    for (const positive of [true, false]) {
      const typesDir = join(ownedRoot, positive ? "positive consumer" : "negative consumer");
      const stateDir = join(ownedRoot, positive ? "positive state" : "negative state");
      await mkdir(typesDir);
      const nodeFile = fileSpecifier(typesDir, join(snapshots, "node"));
      const undiciFile = fileSpecifier(typesDir, join(snapshots, "undici"));
      const parentLine = `  ${JSON.stringify("@types/node")}: ${JSON.stringify(nodeFile)}`;
      const childLine = `  ${JSON.stringify("@types/node>undici-types")}: ${JSON.stringify(undiciFile)}`;
      const workspace = fixture.workspaceYaml.replace(nodeLine, parentLine)
        .replace(`${undiciLine}\n`, positive ? `${childLine}\n` : "");
      await writeFile(join(typesDir, "package.json"), `${JSON.stringify(typeManifest, null, 2)}\n`);
      await writeFile(join(typesDir, "pnpm-workspace.yaml"), workspace);
      const controlOverrides = { ...nativeOverrides, "@types/node": nodeFile };
      if (positive) controlOverrides["@types/node>undici-types"] = undiciFile;
      assert.deepEqual(await readOverrides(typesDir, stateDir), controlOverrides);
      const install = () => runPnpm(
        ["install", "--offline", "--ignore-scripts", "--no-frozen-lockfile"], typesDir, stateDir,
      );
      if (positive) {
        await install();
        const installedRequire = createRequire(join(typesDir, "package.json"));
        const installedNodePath = installedRequire.resolve("@types/node/package.json");
        const installedNode = JSON.parse(await readFile(installedNodePath, "utf8"));
        const installedUndici = JSON.parse(await readFile(
          createRequire(installedNodePath).resolve("undici-types/package.json"), "utf8",
        ));
        assert.equal(installedNode.version, typeVersions.node);
        assert.equal(installedUndici.version, typeVersions.undici);
        console.log("mount-rs isolated offline type closure: PASS");
      } else {
        await assert.rejects(install, (error) => {
          assert.equal(error.code, 1, "negative control must fail through pnpm resolution");
          assert(!error.killed, "negative control must not time out");
          const detail = `${error.stdout}\n${error.stderr}`;
          assert.match(detail, /ERR_PNPM_NO_OFFLINE_(?:META|TARBALL)/);
          assert.match(detail, /undici-types/);
          return true;
        });
        console.log("mount-rs missing transitive offline rejection: PASS");
      }
    }
  } finally {
    await rm(ownedRoot, { recursive: true, force: true });
  }
}

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
  await runConsumerFixtureControls();
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
  assert.equal(result.packageJson.name, "@mount-rs/core");
  assert.equal(result.packageJson.napi.packageName, result.packageJson.name);
  assert.equal(result.packageJson.publishConfig.access, "public");
  assert.equal(result.packageJson.license, "Apache-2.0");
  for (const file of ["LICENSE", "THIRD_PARTY_NOTICES.md", "postlude-harness.cjs", "types/harness.d.ts"]) {
    assert(result.files.includes(file), `aggregate package is missing ${file}`);
  }
  assert.deepEqual(
    result.packageJson.optionalDependencies,
    Object.fromEntries(NATIVE_TARGETS.map(({ packageName }) => [packageName, "0.1.0"])),
  );
  for (const target of NATIVE_TARGETS) {
    assert(result.files.includes(`npm/${target.platformArchABI}/${target.artifact}`));
    assert.equal(result.stagedPackages[target.platformArchABI].name, target.packageName);
    assert.match(result.stagedPackages[target.platformArchABI].name, /^@mount-rs\//);
    assert.equal(result.stagedPackages[target.platformArchABI].license, "Apache-2.0");
    assert.equal(result.stagedPackages[target.platformArchABI].publishConfig.access, "public");
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
assert.equal(await readFile(new URL("../pnpm-lock.yaml", import.meta.url), "utf8"), lockBefore);
console.log("mount-rs N-API artifact aggregation: PASS");
