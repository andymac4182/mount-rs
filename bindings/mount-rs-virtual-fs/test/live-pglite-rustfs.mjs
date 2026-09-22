import assert from "node:assert/strict";

import { createChunkedDriver } from "@mount-rs/core";
import { Workspace } from "@mastra/core/workspace";
import {
  createBash,
} from "@mount-rs/virtual-fs/just-bash";
import { createMastraFilesystem } from "@mount-rs/virtual-fs/mastra";

const OPT_IN = "MOUNT_RS_VIRTUAL_FS_LIVE";
const args = process.argv.slice(2);
const liveArgs = args.filter((argument) => argument === "--live");
const invalidArgs = args.filter((argument) => argument !== "--live");
if (invalidArgs.length > 0 || liveArgs.length > 1) {
  throw new Error(
    "usage: node test/live-pglite-rustfs.mjs [--live]",
  );
}

const envOptIn = process.env[OPT_IN];
if (envOptIn !== undefined && envOptIn !== "1") {
  throw new Error(OPT_IN + " must be exactly 1 when enabling the live lane");
}

if (liveArgs.length === 0 && envOptIn !== "1") {
  console.log(
    "virtual-fs PGlite/RustFS live integration: SKIP (pass --live or set " +
      OPT_IN +
      "=1)",
  );
  process.exit(0);
}

const required = (name) => {
  const value = process.env[name];
  if (!value || value.trim() === "") {
    throw new Error(`${name} must be set for the opt-in PGlite/RustFS adapter test`);
  }
  return value;
};

const pgliteUrl = required("PGLITE_DATABASE_URL");
const endpoint = required("R2_ENDPOINT").replace(/\/+$/, "");
const rustfsEndpoint = required("RUSTFS_ENDPOINT").replace(/\/+$/, "");
const bucket = required("R2_BUCKET");
const accessKeyId = required("R2_ACCESS_KEY_ID");
const secretAccessKey = required("R2_SECRET_ACCESS_KEY");
const harnessPrefix = required("RUSTFS_TEST_PREFIX").replace(/\/+$/, "");

if (endpoint !== rustfsEndpoint) {
  throw new Error(
    `R2_ENDPOINT must match RUSTFS_ENDPOINT for the RustFS-only lane: ${endpoint} !== ${rustfsEndpoint}`,
  );
}
const parsedEndpoint = new URL(endpoint);
if (
  parsedEndpoint.protocol !== "http:" ||
  !["127.0.0.1", "localhost"].includes(parsedEndpoint.hostname)
) {
  throw new Error(
    `the live adapter lane requires the harness loopback RustFS endpoint, got ${endpoint}`,
  );
}

const runId = `${process.pid}-${Date.now()}`;
const namespace = `${harnessPrefix}/virtual-fs/${runId}`;
let ownerNumber = 0;

function driverOptions(label) {
  const key = `${namespace}/${label}`;
  return {
    metadata: {
      kind: "pglite",
      uri: pgliteUrl,
      key: `${key}/metadata`,
      durable: false,
    },
    blocks: {
      kind: "r2",
      key: `${key}/blocks`,
      endpoint,
      bucket,
      accessKeyId,
      secretAccessKey,
    },
    chunkSize: 7,
    owner: `virtual-fs-${label}-${runId}-${++ownerNumber}`,
  };
}

async function closeBash(bash, driver) {
  try {
    await bash?.fs.close();
  } finally {
    await driver?.shutdown();
  }
}

async function testBashAdapter() {
  const label = "just-bash";
  const payload = Uint8Array.from({ length: 97 }, (_, index) => (index * 73 + 11) & 0xff);
  let driver;
  let bash;
  try {
    driver = await createChunkedDriver(driverOptions(label));
    bash = await createBash(driver, { cwd: "/" });
    const result = await bash.exec(
      "mkdir -p /live && printf 'pglite-rustfs-bash\\n' > /live/message.txt && cat /live/message.txt",
    );
    assert.equal(result.exitCode, 0, result.stderr);
    assert.equal(result.stdout, "pglite-rustfs-bash\n");
    await bash.fs.writeFile("/live/payload.bin", payload);
    assert.deepEqual([...await bash.fs.readFileBuffer("/live/payload.bin")], [...payload]);

    await closeBash(bash, driver);
    bash = undefined;
    driver = undefined;

    driver = await createChunkedDriver(driverOptions(label));
    bash = await createBash(driver, { cwd: "/" });
    const reopened = await bash.exec("cat /live/message.txt");
    assert.equal(reopened.exitCode, 0, reopened.stderr);
    assert.equal(reopened.stdout, "pglite-rustfs-bash\n");
    assert.deepEqual([...await bash.fs.readFileBuffer("/live/payload.bin")], [...payload]);
  } finally {
    await closeBash(bash, driver);
  }
}

async function testMastraAdapter() {
  const label = "mastra";
  const payload = Uint8Array.from({ length: 113 }, (_, index) => (index * 41 + 5) & 0xff);
  let driver;
  let workspace;
  try {
    driver = await createChunkedDriver(driverOptions(label));
    const filesystem = createMastraFilesystem(driver, {
      id: `live-${label}`,
      closeOnDestroy: true,
    });
    workspace = new Workspace({ filesystem });
    await workspace.init();
    await workspace.filesystem.mkdir("/live");
    await workspace.filesystem.writeFile("/live/message.txt", "pglite-rustfs-mastra");
    await workspace.filesystem.writeFile("/live/payload.bin", payload);
    assert.deepEqual(
      [...await workspace.filesystem.readFile("/live/payload.bin")],
      [...payload],
    );

    await workspace.destroy();
    workspace = undefined;
    driver = undefined;

    driver = await createChunkedDriver(driverOptions(label));
    const reopenedFilesystem = createMastraFilesystem(driver, {
      id: `live-${label}-reopen`,
      closeOnDestroy: true,
    });
    workspace = new Workspace({ filesystem: reopenedFilesystem });
    await workspace.init();
    assert.equal(
      await workspace.filesystem.readFile("/live/message.txt", { encoding: "utf8" }),
      "pglite-rustfs-mastra",
    );
    assert.deepEqual(
      [...await workspace.filesystem.readFile("/live/payload.bin")],
      [...payload],
    );
  } finally {
    try {
      await workspace?.destroy();
    } finally {
      await driver?.shutdown();
    }
  }
}

await testBashAdapter();
await testMastraAdapter();

console.log("virtual-fs PGlite/RustFS live integration: PASS");
