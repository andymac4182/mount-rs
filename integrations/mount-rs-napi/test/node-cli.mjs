import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
  "crates/mount-rs-cli/examples/config-pglite-ozone.json",
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
