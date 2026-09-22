import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createNodeFsDriver as createRustDriver } from "../index.js";

const source = process.env.MOUNTX_SOURCE;
if (!source) {
  console.log("mount-rs host restart parity: SKIP (MOUNTX_SOURCE unset)");
  process.exit(0);
}

const expectedOracleRevision = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
assert.equal(
  execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  expectedOracleRevision,
);

const [{ createNodeFsDriver: createOracleDriver }, { createLoopback }] = await Promise.all([
  import(pathToFileURL(`${source}/src/drivers/node-fs.ts`).href),
  import(pathToFileURL(`${source}/src/harness.ts`).href),
]);

const readText = async (filesystem, path) => Buffer.from(await filesystem.readFile(path)).toString();

async function runScenario(name, createDriver) {
  const root = await mkdtemp(join(tmpdir(), `mount-rs-host-restart-${name}-`));
  let first;
  let second;
  try {
    first = createDriver(root);
    await first.mkdir("/state", { recursive: false, mode: 0o750 });
    const handle = await first.open("/state/payload", "w+");
    await handle.write(Buffer.from("restart-state"), 0, 13, 0);
    await handle.sync?.();
    await handle.close();
    await first.rename("/state/payload", "/state/committed");
    await first.shutdown?.();
    first = undefined;

    second = createDriver(root);
    const firstRestart = await readText(second, "/state/committed");
    const firstStat = await second.stat("/state/committed");
    const entries = (await second.readdir("/state", { withFileTypes: true }))
      .map((entry) => [entry.name, entry.isFile(), entry.isDirectory()])
      .sort((left, right) => left[0].localeCompare(right[0]));

    const reopened = await second.open("/state/committed", "r+");
    await reopened.write(Buffer.from("!"), 0, 1, 0);
    await reopened.sync?.();
    await reopened.close();
    await second.shutdown?.();
    second = undefined;

    const third = createDriver(root);
    try {
      return {
        firstRestart,
        secondRestart: await readText(third, "/state/committed"),
        stat: {
          size: firstStat.size,
          mode: firstStat.mode & 0o170000,
          file: firstStat.isFile(),
        },
        entries,
      };
    } finally {
      await third.shutdown?.();
    }
  } finally {
    await second?.shutdown?.();
    await first?.shutdown?.();
    await rm(root, { recursive: true, force: true });
  }
}

const rust = await runScenario("rust", (root) => createRustDriver(root));
const oracle = await runScenario("oracle", (root) =>
  createLoopback(createOracleDriver(root)),
);

assert.deepEqual(rust, oracle);
assert.deepEqual(rust, {
  firstRestart: "restart-state",
  secondRestart: "!estart-state",
  stat: { size: 13, mode: 0o100000, file: true },
  entries: [["committed", true, false]],
});
console.log(
  `mount-rs host restart parity: PASS (oracle=${expectedOracleRevision}, clean restarts=2)`,
);
