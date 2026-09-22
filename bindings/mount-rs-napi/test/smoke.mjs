import assert from "node:assert/strict";
import { Filesystem } from "../index.js";

const fs = Filesystem.memory();
assert.equal(typeof Filesystem.prototype.stat, "function");
assert.equal(fs instanceof Filesystem, true);
assert.equal(typeof fs.capabilities, "object");
assert.deepEqual(fs.getCapabilities(), fs.capabilities);
assert.throws(() => {
  fs.capabilities = {};
}, TypeError);
const directlyConstructed = new Filesystem();
assert.equal(directlyConstructed instanceof Filesystem, true);
assert.equal(typeof directlyConstructed.capabilities, "object");
await directlyConstructed.shutdown();
await fs.mkdir("/demo", true);
await fs.writeFile("/demo/hello", Buffer.from("hello"));
assert.equal((await fs.readFile("/demo/hello")).toString(), "hello");
assert.equal((await fs.stat("/demo/hello")).size, 5);
await fs.link("/demo/hello", "/demo/hard-link");
assert.equal((await fs.stat("/demo/hello")).nlink, 2);
await fs.chmod("/demo/hello", 0o640);
await fs.chown("/demo/hello", 123, 456);
await fs.utimes("/demo/hello", 1700000000, 1700000001);
assert.equal((await fs.stat("/demo/hello")).mtimeMs, 1700000001000);
await fs.truncate("/demo/hello", 3);
assert.equal((await fs.readFile("/demo/hello")).toString(), "hel");
const statfs = await fs.statfs("/");
assert.equal(statfs.blockSize, 4096);
await fs.mknod("/demo/fifo", 0o010000 | 0o644, 0);
assert.equal((await fs.lstat("/demo/fifo")).mode & 0o170000, 0o010000);

const sqlite = await Filesystem.sqlite(":memory:");
await sqlite.writeFile("/persisted", Buffer.from("sqlite"));
assert.equal((await sqlite.readFile("/persisted")).toString(), "sqlite");
console.log("mount-rs N-API smoke: PASS");
