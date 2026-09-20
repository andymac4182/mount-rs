import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { Filesystem } from "../index.js";
import { exercise } from "./contract.mjs";

const source = process.env.MOUNTX_SOURCE;
if (!source) {
  console.log("mount-rs N-API differential: SKIP (MOUNTX_SOURCE unset)");
  process.exit(0);
}

const [{ createMemoryDriver }, { createLoopback }] = await Promise.all([
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
  import(pathToFileURL(`${source}/src/harness.ts`).href),
]);

// The common scenario intentionally avoids timestamps and inode numbers in the
// comparison. It exercises the same handle/range/flag/metadata semantics in
// the native binding and the upstream TypeScript oracle.
const native = await exercise(Filesystem.memory());
const oracle = await exercise(createLoopback(createMemoryDriver()));

// The oracle returns the first directory it created for recursive mkdir. The
// Rust core currently returns only success, so the binding must remain
// `undefined` rather than guessing from a racy preflight/stat pass.
assert.equal(oracle.recursiveMkdirResult, "/tree");
assert.equal(native.recursiveMkdirResult, undefined);

assert.deepEqual(
  {
    data: native.data,
    // Intentionally omitted: recursive mkdir's first-created return is a
    // documented shared-core gap, not a value the facade can derive safely.
    entries: native.entries,
    numericMode: native.numericMode,
    owner: native.owner,
    statfs: {
      blockSize: native.statfs.blockSize,
      blocks: native.statfs.blocks,
      blocksFree: native.statfs.blocksFree,
    },
  },
  {
    data: oracle.data,
    entries: oracle.entries,
    numericMode: oracle.numericMode,
    owner: oracle.owner,
    statfs: {
      blockSize: oracle.statfs.bsize,
      blocks: oracle.statfs.blocks,
      blocksFree: oracle.statfs.bfree,
    },
  },
);
console.log("mount-rs N-API differential: PASS (mkdir return gap recorded)");
