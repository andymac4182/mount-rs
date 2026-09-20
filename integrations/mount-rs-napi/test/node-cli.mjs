import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(testDirectory, "../../..");
const cliPath = resolve(repositoryRoot, "examples/node-cli/index.mjs");

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
  "--transport", "remote", "--mountpoint", "/tmp/mount-rs-node-cli-invalid",
]);
assert.equal(invalidTransport.code, 2, output(invalidTransport));
assert.match(invalidTransport.stderr, /auto, fuse, 9p, or nfs/);
assert.doesNotMatch(output(invalidTransport), /^mounted\s+/im);

const invalidRoot = await runCli([
  "--driver", "memory", "--root", "/tmp/should-not-be-used",
  "--mountpoint", "/tmp/mount-rs-node-cli-invalid-root", "--check",
]);
assert.equal(invalidRoot.code, 2, output(invalidRoot));
assert.match(invalidRoot.stderr, /--root is only valid/);

const checkMountpoint = await fs.mkdtemp(resolve("/tmp", "mount-rs-node-cli-check-"));
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
  const nativeMountpoint = await fs.mkdtemp(resolve("/tmp", "mount-rs-node-cli-native-"));
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
