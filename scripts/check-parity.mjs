import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const expectedOracleRevision = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error("MOUNTX_SOURCE must point to a checkout of pithings/mountx");

const oracleRevision = execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (oracleRevision !== expectedOracleRevision) {
  throw new Error(
    `MOUNTX_SOURCE revision ${oracleRevision} is not the pinned oracle ${expectedOracleRevision}`,
  );
}

const env = { ...process.env, MOUNTX_SOURCE: source };
const rust = JSON.parse(execFileSync("cargo", ["run", "--quiet", "--locked", "--example", "parity"], { cwd: repo, env, encoding: "utf8" }));
const typescript = JSON.parse(execFileSync("node", ["scripts/mountx-oracle.mjs"], { cwd: repo, env, encoding: "utf8" }));
assert.deepStrictEqual(rust, typescript);
console.log(`mountx parity: PASS (oracle=${oracleRevision})`);
console.log(JSON.stringify(rust, null, 2));
