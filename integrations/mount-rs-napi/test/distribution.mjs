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

assert.equal(packageJson.license, "Apache-2.0");
for (const name of ["LICENSE", "THIRD_PARTY_NOTICES.md"]) {
  const canonical = await readFile(new URL(`../../${name}`, packageDirectory), "utf8");
  for (const directory of [packageDirectory, new URL("../mount-rs-virtual-fs/", packageDirectory)]) {
    assert.equal(await readFile(new URL(name, directory), "utf8"), canonical,
      `${directory.pathname}${name} must preserve the repository license/notice`);
  }
}

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
  "./nfs": { types: "./types/nfs-codec.d.ts", require: "./nfs.cjs", default: "./nfs.cjs" },
  "./9p": { types: "./types/p9-codec.d.ts", require: "./p9.cjs", default: "./p9.cjs" },
  ...Object.fromEntries(["./drivers/node-fs", "./drivers/unstorage", "./auto", "./s3", "./webdav"].map((path) => [path, {
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
for (const required of ["LICENSE", "THIRD_PARTY_NOTICES.md", "index.js", "index.d.ts", "package.json", "postlude.cjs", "postlude-utilities.cjs", "postlude-servers.cjs", "types/memory.d.ts"]) {
  assert.equal(files.has(required), true, `package is missing ${required}`);
}
for (const required of ["nfs.cjs", "p9.cjs", "postlude-nfs-codec.cjs", "postlude-p9-codec.cjs", "types/nfs-codec.d.ts", "types/p9-codec.d.ts"]) {
  assert.equal(files.has(required), true, `package is missing ${required}`);
}
assert.equal(
  [...files].some((path) => path.endsWith(".node")),
  true,
  "package must contain the host native addon after build",
);

const require = createRequire(import.meta.url);
const direct = await import("../index.js");
const self = await import("@mount-rs/core");
const selfCommonJs = require("@mount-rs/core");
assert.equal(typeof direct.Filesystem, "function");
assert.equal(self.Filesystem, direct.Filesystem);
assert.equal(selfCommonJs.Filesystem, direct.Filesystem);
assert.equal(require("@mount-rs/core/package.json").name, packageJson.name);
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
  const esm = await import(`@mount-rs/core/${path}`);
  const cjs = require(`@mount-rs/core/${path}`);
  assert.equal(typeof esm[exported], "function", `${path} ESM export`);
  assert.equal(esm[exported], cjs[exported], `${path} CJS/ESM identity`);
}

console.log("mount-rs N-API distribution/export coverage: PASS");
