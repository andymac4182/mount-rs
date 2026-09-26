import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// Read the real launch seam without importing the fixture or starting Cargo.
test("HTTP fixture uses the upstream shared target and locked default-profile launch", () => {
  const source = readFileSync(new URL("./check-http-parity.mjs", import.meta.url), "utf8");
  const startup = source.slice(source.indexOf("async function startRust()"), source.indexOf("async function stopRust("));
  const launch = /const child = spawn\(\s*([^\n]+)\n\s*(\[[^\n]+\]),/u.exec(startup);
  assert.ok(launch, "the real Cargo launch is present");
  assert.equal(launch[1].trim(), 'fileURLToPath(new URL("./cargo-shared", import.meta.url)),');
  assert.deepEqual(JSON.parse(launch[2]), ["run", "--locked", "--quiet", "--example", "http_oracle", "--"]);
  assert.ok(startup.includes("cwd: repo,"));
  assert.ok(startup.includes('env: { ...process.env, CARGO_TERM_COLOR: "never", RUST_BACKTRACE: "1" },'));
  assert.ok(startup.includes('stdio: ["pipe", "pipe", "pipe"],'));
  assert.ok(startup.includes("}, 120_000);"));
  assert.ok(startup.includes('const prefix = "MOUNT_RS_HTTP_ORACLE_READY ";'));
});
