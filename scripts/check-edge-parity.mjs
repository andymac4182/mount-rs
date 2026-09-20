import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error("MOUNTX_SOURCE must point to a checkout of pithings/mountx");
const env = { ...process.env, MOUNTX_SOURCE: source };
const rust = JSON.parse(execFileSync("cargo", ["run", "--quiet", "--example", "edge_parity"], { cwd: repo, env, encoding: "utf8" }));
const typescript = JSON.parse(execFileSync("node", ["scripts/mountx-edge-oracle.mjs"], { cwd: repo, env, encoding: "utf8" }));
assert.deepStrictEqual(rust, typescript);
console.log("mountx edge parity: PASS");
console.log(JSON.stringify(rust, null, 2));
