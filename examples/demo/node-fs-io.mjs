// Small process-level filesystem client used by scripts/demo-end-to-end.sh.
// The mount-rs CLI owns the native mount; this process proves that an
// independent Node.js process can use fs/promises on the mounted path.

import { open, readFile } from "node:fs/promises";
import path from "node:path";

const [operation, root, name, expectedText] = process.argv.slice(2);

function usage() {
  return "usage: node-fs-io.mjs write-read <root> <file-name> <expected-bytes>";
}

async function run() {
  if (operation !== "write-read" || !root || !name || expectedText === undefined) {
    throw new Error(usage());
  }
  if (path.basename(name) !== name) {
    throw new Error("file name must not contain a directory separator");
  }

  const filePath = path.join(root, name);
  const expected = Buffer.from(expectedText, "utf8");
  const handle = await open(filePath, "w");
  try {
    await handle.writeFile(expected);
    await handle.sync();
  } finally {
    await handle.close();
  }

  const actual = await readFile(filePath);
  if (!actual.equals(expected)) {
    throw new Error(
      `byte mismatch for ${name}: expected ${expected.length} bytes, read ${actual.length} bytes`,
    );
  }
  console.log(`node fs/promises: write-read ok (${name})`);
}

run().catch((error) => {
  console.error(`node-fs-io: ${error.message}`);
  process.exitCode = 1;
});
