import assert from "node:assert/strict";
import { Filesystem } from "../index.js";

const connectionString = process.env.PGLITE_DATABASE_URL;
if (!connectionString) {
  throw new Error("PGLITE_DATABASE_URL must be set");
}

const fs = await Filesystem.pglite(connectionString);
await fs.writeFile("/napi-pglite", Buffer.from("napi-pglite"));
assert.equal((await fs.readFile("/napi-pglite")).toString(), "napi-pglite");
console.log("mount-rs N-API PGlite integration: PASS");
