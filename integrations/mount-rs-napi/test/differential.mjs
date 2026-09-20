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

assert.deepEqual(
  {
    data: native.data,
    recursiveMkdirResult: native.recursiveMkdirResult,
    existingRecursiveMkdirResult: native.existingRecursiveMkdirResult,
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
    recursiveMkdirResult: oracle.recursiveMkdirResult,
    existingRecursiveMkdirResult: oracle.existingRecursiveMkdirResult,
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
console.log("mount-rs N-API differential: PASS");
