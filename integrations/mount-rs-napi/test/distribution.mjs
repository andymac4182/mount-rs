import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const packageDirectory = new URL("..", import.meta.url);
const packageJson = JSON.parse(
  await readFile(new URL("../package.json", import.meta.url), "utf8"),
);

assert.deepEqual(packageJson.exports, {
  ".": {
    types: "./index.d.ts",
    require: "./index.js",
    default: "./index.js",
  },
  "./drivers/memory": {
    types: "./types/memory.d.ts",
    require: "./index.js",
    default: "./index.js",
  },
  "./package.json": "./package.json",
  ...Object.fromEntries(["./drivers/node-fs", "./drivers/unstorage", "./auto", "./nfs", "./9p", "./s3", "./webdav"].map((path) => [path, {
    types: "./index.d.ts", require: "./index.js", default: "./index.js",
  }])),
});
assert.deepEqual(packageJson.napi.targets, [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-unknown-linux-gnu",
]);
assert.equal(
  packageJson.scripts.prepublishOnly,
  "napi create-npm-dirs && napi pre-publish --root-publisher pnpm",
);

const { stdout } = await execFileAsync(
  "pnpm",
  ["pack", "--dry-run", "--json"],
  { cwd: fileURLToPath(packageDirectory) },
);
const report = JSON.parse(stdout.trim());
const files = new Set(report.files.map(({ path }) => path));
for (const required of ["index.js", "index.d.ts", "package.json", "postlude.cjs", "types/memory.d.ts"]) {
  assert.equal(files.has(required), true, `package is missing ${required}`);
}
assert.equal(
  [...files].some((path) => path.endsWith(".node")),
  true,
  "package must contain the host native addon after build",
);

const require = createRequire(import.meta.url);
const direct = await import("../index.js");
const self = await import("@andymac4182/mount-rs");
const selfCommonJs = require("@andymac4182/mount-rs");
assert.equal(typeof direct.Filesystem, "function");
assert.equal(self.Filesystem, direct.Filesystem);
assert.equal(selfCommonJs.Filesystem, direct.Filesystem);
assert.equal(require("@andymac4182/mount-rs/package.json").name, packageJson.name);
for (const [path, exported] of [
  ["drivers/memory", "createMemoryDriver"],
  ["drivers/node-fs", "createNodeFsDriver"],
  ["drivers/unstorage", "createUnstorageDriver"],
  ["auto", "mount"],
  ["nfs", "createNfsServer"],
  ["9p", "createP9Server"],
  ["s3", "createS3Server"],
  ["webdav", "createWebdavServer"],
]) {
  const esm = await import(`@andymac4182/mount-rs/${path}`);
  const cjs = require(`@andymac4182/mount-rs/${path}`);
  assert.equal(typeof esm[exported], "function", `${path} ESM export`);
  assert.equal(esm[exported], cjs[exported], `${path} CJS/ESM identity`);
}

console.log("mount-rs N-API distribution/export coverage: PASS");
