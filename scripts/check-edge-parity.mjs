import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error("MOUNTX_SOURCE must point to a checkout of pithings/mountx");
const env = { ...process.env, MOUNTX_SOURCE: source };
const typescript = JSON.parse(execFileSync("node", ["scripts/mountx-edge-oracle.mjs"], { cwd: repo, env, encoding: "utf8" }));
for (const backend of ['memory', 'chunked-memory', 'chunked-sqlite']) {
  const rust = JSON.parse(execFileSync("cargo", ["run", "--locked", "--quiet", "--example", "edge_parity", "--", backend], { cwd: repo, env, encoding: "utf8" }));
  assert.deepStrictEqual(rust, typescript, backend);
  console.log(`mountx edge parity (${backend}): PASS`);
}
