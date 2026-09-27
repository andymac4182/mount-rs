import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// Read the real launch seams without importing either fixture or starting Cargo.
const launchers = [
  {
    script: "check-http-parity.mjs",
    command: 'fileURLToPath(new URL("./cargo-shared", import.meta.url)),',
    cwd: "cwd: repo,",
    watchdog: "}, 120_000);",
  },
  {
    script: "test-http-early-rejection.mjs",
    command: 'process.env.CARGO ?? fileURLToPath(new URL("./cargo-shared", import.meta.url)),',
    cwd: "cwd: fileURLToPath(repo),",
    watchdog: "FIXTURE_START_TIMEOUT_MS,",
  },
];

for (const launcher of launchers) {
  test(`${launcher.script} uses the shared target and locked default-profile launch`, () => {
    const source = readFileSync(new URL(`./${launcher.script}`, import.meta.url), "utf8");
    const startup = source.slice(source.indexOf("async function startRust()"), source.indexOf("async function stopRust("));
    const launch = /(?:const )?child = spawn\(\s*([^\n]+)\n\s*(\[[^\n]+\]),/u.exec(startup);
    assert.ok(launch, "the real Cargo launch is present");
    assert.equal(launch[1].trim(), launcher.command);
    assert.deepEqual(JSON.parse(launch[2]), ["run", "--locked", "--quiet", "--example", "http_oracle", "--"]);
    assert.ok(startup.includes(launcher.cwd));
    assert.ok(startup.includes('env: { ...process.env, CARGO_TERM_COLOR: "never", RUST_BACKTRACE: "1" },'));
    assert.ok(startup.includes('stdio: ["pipe", "pipe", "pipe"],'));
    assert.ok(startup.includes(launcher.watchdog));
    assert.ok(startup.includes('const prefix = "MOUNT_RS_HTTP_ORACLE_READY ";'));
    if (launcher.script === "test-http-early-rejection.mjs") {
      assert.ok(source.includes("const FIXTURE_START_TIMEOUT_MS = 120_000;"));
    }
  });
}
