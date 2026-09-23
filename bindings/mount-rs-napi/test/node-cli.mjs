import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(testDirectory, "../../..");
const cliPath = resolve(repositoryRoot, "examples/node-cli/index.mjs");
const temporaryRoot = tmpdir();

function runCli(args, extraEnv = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
      cwd: repositoryRoot,
      env: { ...process.env, ...extraEnv },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", rejectPromise);
    child.once("close", (code, signal) => resolvePromise({ code, signal, stdout, stderr }));
  });
}

function output(result) {
  return `${result.stdout}\n${result.stderr}`;
}

const help = await runCli(["--help"]);
assert.equal(help.code, 0, output(help));
assert.match(help.stdout, /@mount-rs\/core/);
assert.doesNotMatch(output(help), /^mounted\s+/im);

const invalidTransport = await runCli([
  "--transport", "remote", "--mountpoint", join(temporaryRoot, "mount-rs-node-cli-invalid"),
]);
assert.equal(invalidTransport.code, 2, output(invalidTransport));
assert.match(invalidTransport.stderr, /auto, fuse, 9p, or nfs/);
assert.doesNotMatch(output(invalidTransport), /^mounted\s+/im);

const invalidRoot = await runCli([
  "--driver", "memory", "--root", join(temporaryRoot, "should-not-be-used"),
  "--mountpoint", join(temporaryRoot, "mount-rs-node-cli-invalid-root"), "--check",
]);
assert.equal(invalidRoot.code, 2, output(invalidRoot));
assert.match(invalidRoot.stderr, /--root is only valid/);

const ozoneConfig = resolve(
  repositoryRoot,
  "apps/mount-rs-cli/examples/config-pglite-ozone.json",
);
const ozoneFixture = JSON.parse(await fs.readFile(ozoneConfig, "utf8"));
assert.equal(ozoneFixture.version, 1);
assert.equal(ozoneFixture.driver?.kind, "splitstore");
assert.equal(ozoneFixture.driver?.storage?.metadata?.kind, "pglite");
assert.equal(ozoneFixture.driver?.storage?.blocks?.kind, "r2");
assert.equal(ozoneFixture.driver?.storage?.blocks?.endpoint, "http://127.0.0.1:9878");
assert.equal(ozoneFixture.driver?.storage?.blocks?.bucket, "mount-rs-ozone-test");
assert.deepEqual(ozoneFixture.driver?.storage?.blocks?.access_key_id, {
  env: "R2_ACCESS_KEY_ID",
});
assert.deepEqual(ozoneFixture.driver?.storage?.blocks?.secret_access_key, {
  env: "R2_SECRET_ACCESS_KEY",
});
const ozoneCheck = await runCli(
  ["--config", ozoneConfig, "--check"],
  { MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-check" },
);
assert.equal(ozoneCheck.code, 0, output(ozoneCheck));
assert.match(ozoneCheck.stdout, /driver=splitstore/);
assert.match(ozoneCheck.stdout, /no SDK loaded; no mount attempted/);
assert.doesNotMatch(output(ozoneCheck), /^mounted\s+/im);

const foundationdbConfig = resolve(
  repositoryRoot,
  "apps/mount-rs-cli/examples/config-foundationdb-rustfs.json",
);
const foundationdbFixture = JSON.parse(await fs.readFile(foundationdbConfig, "utf8"));
assert.equal(foundationdbFixture.driver?.storage?.blocks?.kind, "rustfs");
assert.equal(foundationdbFixture.driver?.storage?.blocks?.region, "us-east-1");
assert.deepEqual(foundationdbFixture.driver?.storage?.blocks?.secret_access_key, {
  env: "RUSTFS_SECRET_ACCESS_KEY",
});
const foundationdbCheck = await runCli(
  ["--config", foundationdbConfig, "--mountpoint", join(temporaryRoot, "mount-rs-node-cli-fdb-check"), "--check"],
  { MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-check" },
);
assert.equal(foundationdbCheck.code, 0, output(foundationdbCheck));
assert.match(foundationdbCheck.stdout, /driver=splitstore/);
assert.match(foundationdbCheck.stdout, /no SDK loaded; no mount attempted/);
assert.doesNotMatch(output(foundationdbCheck), /^mounted\s+/im);

const concurrentTidbRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-tidb-concurrent-check-"));
try {
  const configPath = join(concurrentTidbRoot, "config.json");
  const mountpoint = join(concurrentTidbRoot, "mnt");
  const metadata = {
    kind: "tidb", connection: { env: "MOUNT_RS_NODE_CLI_TIDB_CONFIG_URL" },
    volume_key: "shared-tidb-metadata", durable: true,
  };
  const storage = { concurrent_writes: true, metadata };
  const unreachableSdk = {
    MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-tidb-config",
    MOUNT_RS_NODE_CLI_TIDB_CONFIG_URL: "mysql://root@127.0.0.1:1/test",
  };
  const check = async (blocks, configurationOnly = true) => {
    storage.blocks = blocks;
    await fs.writeFile(configPath, JSON.stringify({
      version: 1, driver: { kind: "splitstore", storage },
    }));
    return runCli([
      "--config", configPath, "--mountpoint", mountpoint,
      ...(configurationOnly ? ["--check"] : []),
    ], unreachableSdk);
  };
  for (const blocks of [
    { ...metadata, volume_key: "shared-tidb-blocks" },
    { kind: "rustfs", endpoint: "http://127.0.0.1:1", bucket: "owned-test",
      region: "us-east-1", prefix: "shared-tidb-blocks",
      access_key_id: { env: "RUSTFS_ACCESS_KEY_ID" },
      secret_access_key: { env: "RUSTFS_SECRET_ACCESS_KEY" }, durable: true },
  ]) {
    const accepted = await check(blocks);
    assert.equal(accepted.code, 0, output(accepted));
    assert.match(accepted.stdout, /driver=splitstore/);
    assert.match(accepted.stdout, /no SDK loaded; no mount attempted/);
    assert.doesNotMatch(output(accepted), /^mounted\s+/im);
  }
  for (const blocks of [
    { kind: "memory" }, { kind: "sqlite", path: "local-blocks.sqlite" },
  ]) {
    // Exercise both --check and the mount entry point. Each must reject the
    // pairing before importing the SDK or connecting to the TiDB endpoint.
    for (const configurationOnly of [true, false]) {
      const rejected = await check(blocks, configurationOnly);
      assert.equal(rejected.code, 1, output(rejected));
      assert.match(rejected.stderr, /blocks visible to every writer/);
      assert.doesNotMatch(output(rejected), /this-package-must-not-be-loaded|ECONNREFUSED|^mounted\s+/im);
    }
  }
  assert.equal((await fs.readdir(concurrentTidbRoot)).includes("mnt"), false);
  assert.equal((await fs.readdir(concurrentTidbRoot)).includes("local-blocks.sqlite"), false);
  console.log("NODE_CLI_TIDB_CONCURRENT_CONFIG_PASS shared_pairings=2 rejected_local_pairings=2");
} finally {
  await fs.rm(concurrentTidbRoot, { recursive: true, force: true });
}

const rustfsDefaultRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-rustfs-default-"));
try {
  const configPath = join(rustfsDefaultRoot, "config.json");
  const mockSdkPath = join(rustfsDefaultRoot, "sdk-capture.mjs");
  const localSdk = pathToFileURL(resolve(repositoryRoot, "bindings/mount-rs-napi/index.js")).href;
  await fs.writeFile(mockSdkPath, `
import sdk from ${JSON.stringify(localSdk)};
export function mount() { throw new Error("mount should not run during sdk-self-test"); }
export function createChunkedDriver(options) {
  console.log("RUSTFS_DURABLE=" + options.blocks.durable);
  return sdk.createMemoryDriver();
}
`);
  const storage = {
    metadata: { kind: "memory" },
    blocks: {
      kind: "rustfs",
      endpoint: "http://127.0.0.1:9878",
      bucket: "mount-rs-test",
      region: "us-east-1",
      prefix: "mount-rs/test",
      access_key_id: { env: "RUSTFS_ACCESS_KEY_ID" },
      secret_access_key: { env: "RUSTFS_SECRET_ACCESS_KEY" },
    },
  };
  const run = async () => {
    await fs.writeFile(configPath, JSON.stringify({
      version: 1,
      driver: { kind: "splitstore", storage },
    }));
    return runCli(["--config", configPath, "--sdk-self-test"], {
      MOUNT_RS_NAPI_PACKAGE: pathToFileURL(mockSdkPath).href,
      RUSTFS_ACCESS_KEY_ID: "test-key",
      RUSTFS_SECRET_ACCESS_KEY: "test-secret",
    });
  };
  const omitted = await run();
  assert.equal(omitted.code, 0, output(omitted));
  assert.match(omitted.stdout, /RUSTFS_DURABLE=false/, output(omitted));
  storage.blocks.durable = true;
  const asserted = await run();
  assert.equal(asserted.code, 0, output(asserted));
  assert.match(asserted.stdout, /RUSTFS_DURABLE=true/, output(asserted));
} finally {
  await fs.rm(rustfsDefaultRoot, { recursive: true, force: true });
}

const concurrentConfigRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-concurrent-check-"));
try {
  const concurrentConfig = join(concurrentConfigRoot, "config.json");
  const storage = {
    concurrent_writes: true,
    metadata: { kind: "sqlite", path: ":memory:" },
    blocks: { kind: "sqlite", path: "blocks.sqlite" },
  };
  await fs.writeFile(concurrentConfig, JSON.stringify({ version: 1, driver: { kind: "splitstore", storage } }));
  const volatile = await runCli([
    "--config", concurrentConfig, "--mountpoint", join(concurrentConfigRoot, "mnt"), "--check",
  ]);
  assert.equal(volatile.code, 1, output(volatile));
  assert.match(volatile.stderr, /durable local database file/);

  storage.concurrent_writes = false;
  await fs.writeFile(concurrentConfig, JSON.stringify({ version: 1, driver: { kind: "splitstore", storage } }));
  const legacy = await runCli([
    "--config", concurrentConfig, "--mountpoint", join(concurrentConfigRoot, "mnt"), "--check",
  ]);
  assert.equal(legacy.code, 0, output(legacy));
} finally {
  await fs.rm(concurrentConfigRoot, { recursive: true, force: true });
}

const concurrentMountRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-concurrent-mount-"));
try {
  const configPath = join(concurrentMountRoot, "config.json");
  const mountpoint = join(concurrentMountRoot, "mnt");
  await fs.mkdir(mountpoint);
  const storage = {
    concurrent_writes: true,
    metadata: { kind: "sqlite", path: "metadata.sqlite" },
    blocks: { kind: "sqlite", path: "blocks.sqlite" },
  };
  const checkLayout = async () => {
    await fs.writeFile(configPath, JSON.stringify({
      version: 1,
      driver: { kind: "splitstore", storage },
    }));
    return runCli(["--config", configPath, "--mountpoint", mountpoint, "--check"]);
  };
  const assertRejected = (result) => {
    assert.equal(result.code, 1, output(result));
    assert.match(result.stderr, /outside the native .*mountpoint/i, output(result));
    assert.doesNotMatch(output(result), /metadata\.sqlite|blocks\.sqlite/);
  };

  storage.metadata.path = "mnt/metadata.sqlite";
  assertRejected(await checkLayout());
  await fs.writeFile(configPath, JSON.stringify({
    version: 1,
    mountpoint: "mnt",
    driver: { kind: "splitstore", storage },
  }));
  const configuredMount = await runCli(["--config", configPath, "--check"]);
  assertRejected(configuredMount);
  const attemptedMount = await runCli(["--config", configPath], {
    MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-mount",
  });
  assertRejected(attemptedMount);
  assert.doesNotMatch(output(attemptedMount), /this-package-must-not-be-loaded-for-mount/);
  storage.metadata.path = "metadata.sqlite";
  storage.blocks.path = "mnt/blocks.sqlite";
  assertRejected(await checkLayout());

  storage.blocks.path = "blocks.sqlite";
  storage.metadata.path = "missing/../mnt/metadata.sqlite";
  assertRejected(await checkLayout());

  if (process.platform !== "win32") {
    const alias = join(concurrentMountRoot, "alias");
    await fs.symlink(mountpoint, alias, "dir");
    storage.metadata.path = "alias/metadata.sqlite";
    assertRejected(await checkLayout());
    storage.metadata.path = "mnt/metadata.sqlite";
    await fs.writeFile(configPath, JSON.stringify({
      version: 1,
      driver: { kind: "splitstore", storage },
    }));
    const mountAlias = await runCli([
      "--config", configPath, "--mountpoint", alias, "--check",
    ]);
    assertRejected(mountAlias);
  }

  storage.metadata.path = "mnt-neighbor/metadata.sqlite";
  storage.blocks.path = "blocks.sqlite";
  const safe = await checkLayout();
  assert.equal(safe.code, 0, output(safe));
  storage.metadata.path = "mnt/metadata.sqlite";
  storage.blocks.path = "mnt/blocks.sqlite";
  storage.concurrent_writes = false;
  const exclusive = await checkLayout();
  assert.equal(exclusive.code, 0, output(exclusive));
} finally {
  await fs.rm(concurrentMountRoot, { recursive: true, force: true });
}

if (process.platform === "darwin") {
  const caseAliasRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-case-alias-"));
  try {
    const configPath = join(caseAliasRoot, "config.json");
    const mountpoint = join(caseAliasRoot, "mnt");
    const storage = {
      concurrent_writes: true,
      metadata: { kind: "sqlite", path: "MNT/metadata.sqlite" },
      blocks: { kind: "sqlite", path: "blocks.sqlite" },
    };
    const checkLayout = async (view = mountpoint, check = true) => {
      await fs.writeFile(configPath, JSON.stringify({
        version: 1,
        driver: { kind: "splitstore", storage },
      }));
      return runCli(["--config", configPath, "--mountpoint", view, ...(check ? ["--check"] : [])], {
        MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-case-check",
      });
    };
    const assertRejected = (result) => {
      assert.equal(result.code, 1, output(result));
      assert.match(result.stderr, /outside the native mountpoint/i, output(result));
      assert.doesNotMatch(output(result), /metadata\.sqlite|blocks\.sqlite|this-package-must-not-be-loaded/);
    };

    // Neither case variant exists yet. A case-insensitive volume maps both to
    // one directory as soon as the native mountpoint is created.
    assertRejected(await checkLayout());
    storage.metadata.path = "metadata.sqlite";
    storage.blocks.path = "MNT/blocks.sqlite";
    assertRejected(await checkLayout());
    assertRejected(await checkLayout(mountpoint, false));
    const entriesAfterReject = await fs.readdir(caseAliasRoot);
    assert.equal(entriesAfterReject.includes("mnt"), false, "the guard must not create a mountpoint");
    assert.equal(entriesAfterReject.includes("MNT"), false, "the guard must not open SQLite backing");

    storage.blocks.path = "blocks.sqlite";
    storage.metadata.path = `${"café".normalize("NFD")}/metadata.sqlite`;
    assertRejected(await checkLayout(join(caseAliasRoot, "café".normalize("NFC"))));
    storage.metadata.path = "ss/metadata.sqlite";
    assertRejected(await checkLayout(join(caseAliasRoot, "ß")));
    storage.metadata.path = "k/metadata.sqlite";
    assertRejected(await checkLayout(join(caseAliasRoot, "K")));
    storage.metadata.path = "i\u0307/metadata.sqlite";
    assertRejected(await checkLayout(join(caseAliasRoot, "İ")));
    storage.metadata.path = "s/metadata.sqlite";
    assertRejected(await checkLayout(join(caseAliasRoot, "ſ")));
    assertRejected(await checkLayout(join(caseAliasRoot, "ſ"), false));
    storage.metadata.path = "ἀι/metadata.sqlite";
    assertRejected(await checkLayout(join(caseAliasRoot, "ᾀ")));
    assertRejected(await checkLayout(join(caseAliasRoot, "ᾀ"), false));
    const afterUnicodeReject = await fs.readdir(caseAliasRoot);
    for (const name of ["s", "ἀι", "ſ", "ᾀ"]) {
      assert.equal(afterUnicodeReject.includes(name), false, "the guard must not leave backing or mountpoint directories");
    }

    storage.metadata.path = "MNT-neighbor/metadata.sqlite";
    const neighbor = await checkLayout();
    assert.equal(neighbor.code, 0, output(neighbor));
    storage.metadata.path = "β/metadata.sqlite";
    const unrelatedUnicode = await checkLayout(join(caseAliasRoot, "α"));
    assert.equal(unrelatedUnicode.code, 0, output(unrelatedUnicode));
    storage.metadata.path = "café/metadata.sqlite";
    const accentSibling = await checkLayout(join(caseAliasRoot, "cafe"));
    assert.equal(accentSibling.code, 0, output(accentSibling));
    assert.equal((await fs.readdir(caseAliasRoot)).includes("cafe"), false, "--check must remove its empty probe directory");

    const danglingLink = join(caseAliasRoot, "sqlite-dangling-link");
    await fs.symlink(join(mountpoint, "metadata.sqlite"), danglingLink);
    storage.metadata.path = "sqlite-dangling-link";
    assertRejected(await checkLayout());
    assertRejected(await checkLayout(mountpoint, false));
    await fs.rm(danglingLink);
    const safeRoot = join(caseAliasRoot, "safe-backing");
    await fs.mkdir(safeRoot);
    const safeLink = join(caseAliasRoot, "sqlite-safe-link");
    await fs.symlink(join(safeRoot, "metadata.sqlite"), safeLink);
    storage.metadata.path = "sqlite-safe-link";
    const safeSymlink = await checkLayout();
    assert.equal(safeSymlink.code, 0, output(safeSymlink));
    await fs.rm(safeLink);

    await fs.mkdir(mountpoint);
    storage.metadata.path = "MNT/metadata.sqlite";
    let upperDirectory;
    try {
      upperDirectory = await fs.stat(join(caseAliasRoot, "MNT"));
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    if (upperDirectory) {
      const lowerDirectory = await fs.stat(mountpoint);
      assert.equal(upperDirectory.ino, lowerDirectory.ino, "case variants refer to one directory");
      assertRejected(await checkLayout());
    } else {
      await fs.mkdir(join(caseAliasRoot, "MNT"));
      const distinctExisting = await checkLayout();
      assert.equal(distinctExisting.code, 0, output(distinctExisting));
    }

    if (process.platform !== "win32") {
      const actual = join(caseAliasRoot, "actual");
      const alias = join(caseAliasRoot, "alias");
      await fs.mkdir(actual);
      await fs.symlink(actual, alias, "dir");
      storage.metadata.path = "actual/MNT/metadata.sqlite";
      assertRejected(await checkLayout(join(alias, "mnt")));
    }

    storage.metadata.path = "MNT/metadata.sqlite";
    storage.concurrent_writes = false;
    const exclusive = await checkLayout();
    assert.equal(exclusive.code, 0, output(exclusive));
  } finally {
    await fs.rm(caseAliasRoot, { recursive: true, force: true });
  }
}

if (process.platform === "linux" || process.platform === "win32") {
  const nonMacCaseRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-nonmac-case-"));
  try {
    const configPath = join(nonMacCaseRoot, "config.json");
    await fs.writeFile(configPath, JSON.stringify({
      version: 1,
      driver: {
        kind: "splitstore",
        storage: {
          concurrent_writes: true,
          metadata: { kind: "sqlite", path: "MNT/metadata.sqlite" },
          blocks: { kind: "sqlite", path: "blocks.sqlite" },
        },
      },
    }));
    const layout = await runCli([
      "--config", configPath, "--mountpoint", join(nonMacCaseRoot, "mnt"), "--check",
    ]);
    if (process.platform === "win32") {
      assert.equal(layout.code, 1, output(layout));
      assert.match(layout.stderr, /outside the native mountpoint/i, output(layout));
    } else {
      assert.equal(layout.code, 0, output(layout));
    }
  } finally {
    await fs.rm(nonMacCaseRoot, { recursive: true, force: true });
  }
}

const sdkSelfTest = await runCli([
  "--driver", "memory", "--sdk-self-test",
]);
assert.equal(sdkSelfTest.code, 0, output(sdkSelfTest));
assert.match(sdkSelfTest.stdout, /sdk self-test passed: Node SDK wrote and read/);
assert.doesNotMatch(output(sdkSelfTest), /^mounted\s+/im);

const durableRoot = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-sdk-"));
const durableConfig = join(durableRoot, "config.json");
try {
  await fs.writeFile(
    durableConfig,
    `${JSON.stringify({
      version: 1,
      driver: {
        kind: "splitstore",
        storage: {
          metadata: { kind: "sqlite", path: "./metadata.sqlite" },
          blocks: { kind: "sqlite", path: "./blocks.sqlite" },
          chunk_size_bytes: 7,
          lease_ttl_ms: 120000,
          owner: `node-cli-sdk-${process.pid}`,
        },
      },
    }, null, 2)}\n`,
    { mode: 0o600 },
  );
  const durable = await runCli([
    "--config", durableConfig, "--sdk-self-test", "--reopen",
  ]);
  assert.equal(durable.code, 0, output(durable));
  assert.match(
    durable.stdout,
    /sdk self-test passed: Node SDK wrote, shut down, reopened, and read/,
  );
  assert.doesNotMatch(output(durable), /^mounted\s+/im);

  const invalidTtlConfig = join(durableRoot, "invalid-ttl.json");
  await fs.writeFile(
    invalidTtlConfig,
    `${JSON.stringify({
      version: 1,
      driver: {
        kind: "splitstore",
        storage: {
          metadata: { kind: "sqlite", path: "./metadata.sqlite" },
          blocks: { kind: "sqlite", path: "./blocks.sqlite" },
          lease_ttl_ms: 0,
        },
      },
    }, null, 2)}\n`,
    { mode: 0o600 },
  );
  const invalidTtl = await runCli(["--config", invalidTtlConfig, "--check"]);
  assert.equal(invalidTtl.code, 1, output(invalidTtl));
  assert.match(invalidTtl.stderr, /lease_ttl_ms must be a positive integer/);

  if (process.env.MOUNT_RS_NAPI_FOUNDATIONDB === "1") {
    const clusterFile = process.env.MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE;
    assert.ok(clusterFile, "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE is required for the Node CLI FoundationDB gate");
    const prefix = `mount-rs-napi/node-cli/${process.pid}-${Date.now()}`;
    const foundationdbRuntimeConfig = join(durableRoot, "foundationdb.json");
    const store = (volume) => ({
      kind: "foundationdb",
      cluster_file: clusterFile,
      volume_key: `${prefix}/${volume}`,
      durable: true,
      lease_authority: "persisted-single-authority",
    });
    await fs.writeFile(
      foundationdbRuntimeConfig,
      `${JSON.stringify({
        version: 1,
        driver: {
          kind: "splitstore",
          storage: {
            metadata: store("metadata"),
            blocks: store("blocks"),
            chunk_size_bytes: 4096,
            owner: `node-cli-foundationdb-${process.pid}`,
          },
        },
      }, null, 2)}\n`,
      { mode: 0o600 },
    );
    const foundationdbRuntime = await runCli([
      "--config", foundationdbRuntimeConfig, "--sdk-self-test", "--reopen",
    ]);
    assert.equal(foundationdbRuntime.code, 0, output(foundationdbRuntime));
    assert.equal(foundationdbRuntime.signal, null, output(foundationdbRuntime));
    assert.match(
      foundationdbRuntime.stdout,
      /sdk self-test passed: Node SDK wrote, shut down, reopened, and read/,
    );
    console.log("native FoundationDB Node CLI SDK self-test: PASS");
  }
} finally {
  await fs.rm(durableRoot, { recursive: true, force: true });
}

const volatileReopen = await runCli([
  "--driver", "memory", "--sdk-self-test", "--reopen",
]);
assert.equal(volatileReopen.code, 2, output(volatileReopen));
assert.match(volatileReopen.stderr, /requires a durable driver/);

const checkMountpoint = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-check-"));
try {
  for (const driver of ["memory", "host"]) {
    const args = ["--driver", driver, "--transport", "nfs", "--mountpoint", checkMountpoint, "--check"];
    if (driver === "host") args.push("--root", repositoryRoot);
    const check = await runCli(args, {
      MOUNT_RS_NAPI_PACKAGE: "@mount-rs/this-package-must-not-be-loaded-for-check",
    });
    assert.equal(check.code, 0, output(check));
    assert.match(check.stdout, new RegExp(`driver=${driver}`));
    assert.match(check.stdout, /no SDK loaded; no mount attempted/);
    assert.doesNotMatch(output(check), /^mounted\s+/im);
  }
} finally {
  await fs.rm(checkMountpoint, { recursive: true, force: true });
}

if (process.env.MOUNT_RS_RUN_NATIVE_MOUNT === "1") {
  const nativeMountpoint = await fs.mkdtemp(join(temporaryRoot, "mount-rs-node-cli-native-"));
  try {
    const native = await runCli([
      "--driver", process.env.MOUNT_RS_TEST_DRIVER ?? "memory",
      "--transport", process.env.MOUNT_RS_TEST_TRANSPORT ?? "nfs",
      "--mountpoint", nativeMountpoint, "--self-test",
    ]);
    assert.equal(native.code, 0, output(native));
    assert.match(native.stdout, /self-test passed/);
  } finally {
    await fs.rm(nativeMountpoint, { recursive: true, force: true });
  }
  console.log("native Node SDK-backed mount self-test passed");
} else {
  console.log("native Node SDK-backed mount self-test skipped (set MOUNT_RS_RUN_NATIVE_MOUNT=1 to opt in)");
}

console.log("node SDK CLI argument/configuration checks passed");
