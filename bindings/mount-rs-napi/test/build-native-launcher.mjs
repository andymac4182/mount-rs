import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "win32") {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-napi-launcher-"));
  const executable = join(root, "pnpm-native");
  const javascriptCli = join(root, "pnpm-cli.js");
  const argumentsFile = join(root, "arguments.txt");
  const buildScript = resolve(dirname(fileURLToPath(import.meta.url)), "../scripts/build-native.mjs");
  try {
    await writeFile(
      executable,
      '#!/bin/sh\nprintf "%s\\n" "$@" > "$MOUNT_RS_FAKE_PNPM_ARGS"\n',
    );
    await chmod(executable, 0o755);
    const result = spawnSync(process.execPath, [buildScript, "--release"], {
      env: {
        ...process.env,
        npm_execpath: executable,
        MOUNT_RS_FAKE_PNPM_ARGS: argumentsFile,
        MOUNT_RS_NAPI_FEATURES: "foundationdb",
      },
      encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual((await readFile(argumentsFile, "utf8")).trim().split("\n"), [
      "exec",
      "napi",
      "build",
      "--platform",
      "--release",
      "--features",
      "foundationdb",
    ]);
    await writeFile(
      javascriptCli,
      'require("node:fs").writeFileSync(process.env.MOUNT_RS_FAKE_PNPM_ARGS, process.argv.slice(2).join("\\n"))\n',
    );
    const javascriptResult = spawnSync(process.execPath, [buildScript], {
      env: {
        ...process.env,
        npm_execpath: javascriptCli,
        MOUNT_RS_FAKE_PNPM_ARGS: argumentsFile,
        MOUNT_RS_NAPI_FEATURES: "rustls",
      },
      encoding: "utf8",
    });
    assert.equal(javascriptResult.status, 0, javascriptResult.stderr);
    assert.deepEqual((await readFile(argumentsFile, "utf8")).trim().split("\n"), [
      "exec",
      "napi",
      "build",
      "--platform",
      "--features",
      "rustls",
    ]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

console.log(
  process.platform === "win32"
    ? "mount-rs N-API native pnpm launcher: SKIP (POSIX executable fixture)"
    : "mount-rs N-API native pnpm launcher: PASS",
);
