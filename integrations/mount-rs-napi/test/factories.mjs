import assert from "node:assert/strict";
import { constants } from "node:fs";
import { Filesystem } from "../index.js";

async function check(name, fs) {
  const path = `/factory-${name}`;
  await fs.writeFile(path, Buffer.from(`${name}:one`));
  assert.equal(Buffer.from(await fs.readFile(path)).toString(), `${name}:one`);
  const handle = await fs.open(path, "r+");
  await handle.write(Buffer.from("two"), 0, 3, 0);
  await handle.close();
  assert.equal(Buffer.from(await fs.readFile(path)).toString(), `two${name.slice(3)}:one`);

  const numericPath = `${path}-numeric`;
  const numeric = await fs.open(
    numericPath,
    constants.O_RDWR | constants.O_CREAT,
    0o640,
  );
  await numeric.write(Buffer.from("n"), 0, 1, 0);
  await numeric.close();
  assert.equal(Buffer.from(await fs.readFile(numericPath)).toString(), "n");
  assert.equal((await fs.stat(numericPath)).mode & 0o777, 0o640);
}

await check("memory", Filesystem.memory());
await check("sqlite", await Filesystem.sqlite(":memory:"));

if (process.env.PGLITE_DATABASE_URL) {
  await check("pglite", await Filesystem.pglite(process.env.PGLITE_DATABASE_URL));
} else {
  console.log("mount-rs N-API PGlite factory: SKIP (PGLITE_DATABASE_URL unset)");
}

const r2 = [
  "R2_ENDPOINT",
  "R2_BUCKET",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
].every((name) => process.env[name]);
if (r2) {
  await check(
    "r2",
    await Filesystem.r2({
      endpoint: process.env.R2_ENDPOINT,
      bucket: process.env.R2_BUCKET,
      accessKeyId: process.env.R2_ACCESS_KEY_ID,
      secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
      stateKey: `mount-rs-napi-test/${process.pid}.json`,
    }),
  );
} else {
  console.log("mount-rs N-API R2 factory: SKIP (R2 credentials unset)");
}

console.log("mount-rs N-API factory integration: PASS");
