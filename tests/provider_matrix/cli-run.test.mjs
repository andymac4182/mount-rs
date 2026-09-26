import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { run, waitForChild } from "./cli-run.mjs";

const noisyChild = `
  const { writeFileSync } = require("node:fs");
  const [mode, marker] = process.argv.slice(1);
  const block = Buffer.alloc(64 * 1024, 120);
  async function write(stream) {
    for (let i = 0; i < 64; i++) {
      await new Promise((resolve) => stream.write(block, resolve));
    }
  }
  (async () => {
    if (mode === "stdout" || mode === "both") await write(process.stdout);
    if (mode === "stderr" || mode === "both") await write(process.stderr);
    writeFileSync(marker, "completed");
  })().catch(() => process.exit(1));
`;

for (const mode of ["stdout", "stderr", "both"]) {
  test(`drains a noisy child on ${mode}`, async () => {
    const directory = await mkdtemp(join(tmpdir(), "mount-rs-cli-run-test-"));
    const marker = join(directory, "completed");
    try {
      assert.deepEqual(await run(process.execPath, ["-e", noisyChild, mode, marker], 1_500), {
        ok: true,
        reason: "exit-0",
      });
      assert.equal(await readFile(marker, "utf8"), "completed");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
}

test("does not forward child stdout or stderr", () => {
  const moduleUrl = new URL("./cli-run.mjs", import.meta.url).href;
  const wrapper = `
    import { run } from ${JSON.stringify(moduleUrl)};
    const result = await run(process.execPath, ["-e", "process.stdout.write('private-stdout'); process.stderr.write('private-stderr')"]);
    if (!result.ok) process.exit(1);
  `;
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", wrapper], {
    encoding: "utf8",
  });
  assert.equal(result.status, 0);
  assert.equal(result.stdout, "");
  assert.equal(result.stderr, "");
});

test("preserves nonzero exit and spawn failure results", async () => {
  assert.deepEqual(await run(process.execPath, ["-e", "process.exit(7)"]), {
    ok: false,
    reason: "exit-7",
  });
  assert.deepEqual(await run("/definitely-missing-mount-rs-cli-run-test", []), {
    ok: false,
    reason: "spawn",
  });
});

async function waitForReady(path) {
  const deadline = performance.now() + 10_000;
  while (performance.now() < deadline) {
    try {
      const value = await readFile(path, "utf8");
      if (value) return value;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    await delay(10);
  }
  throw new Error("child did not signal readiness");
}

function killTestGroup(child) {
  if (
    process.platform !== "win32" &&
    process.env.MOUNT_RS_TEST_POSIX_GROUP_SIGNALS === "1" &&
    child.pid
  ) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH" && error.code !== "EPERM") throw error;
      child.kill("SIGKILL");
    }
  } else {
    child.kill("SIGKILL");
  }
}

test("times out and kills a ready child that ignores SIGTERM", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-cli-timeout-test-"));
  const ready = join(directory, "ready");
  const child = spawn(
    process.execPath,
    [
      "-e",
      'const fs = require("node:fs"); process.on("SIGTERM", () => {}); fs.writeFileSync(process.argv[1], "ready"); setInterval(() => {}, 1000)',
      ready,
    ],
    { detached: process.platform !== "win32", stdio: ["ignore", "pipe", "pipe"] },
  );
  try {
    assert.equal(await waitForReady(ready), "ready");
    const signals = [];
    const signalProcess = (pid, signal) => {
      signals.push([pid, signal]);
      process.kill(Math.abs(pid), signal);
    };
    const useProcessGroup = process.platform !== "win32";
    const started = performance.now();
    assert.deepEqual(await waitForChild(child, 200, { useProcessGroup, signalProcess }), {
      ok: false,
      reason: "timeout",
    });
    const elapsed = performance.now() - started;
    assert.ok(elapsed >= 1_000 && elapsed < 2_000, `elapsed ${elapsed}ms`);
    assert.deepEqual(
      signals,
      useProcessGroup ? [[-child.pid, "SIGTERM"], [-child.pid, "SIGKILL"]] : [],
    );
  } finally {
    killTestGroup(child);
    await rm(directory, { recursive: true, force: true });
  }
});

test("timeout stops a ready descendant with closed inherited pipes", {
  skip: process.platform === "win32" || process.env.MOUNT_RS_TEST_POSIX_GROUP_SIGNALS !== "1",
}, async () => {
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-cli-descendant-test-"));
  const ready = join(directory, "ready");
  const grandReady = join(directory, "grand-ready");
  const heartbeat = join(directory, "heartbeat");
  const grandchildScript = `
    const fs = require("node:fs");
    process.on("SIGTERM", () => {});
    let count = 0;
    setInterval(() => fs.writeFileSync(process.argv[1], String(++count)), 25);
    fs.writeFileSync(process.argv[2], "ready");
  `;
  const childScript = `
    const fs = require("node:fs");
    const { spawn } = require("node:child_process");
    process.on("SIGTERM", () => {});
    const grandchild = spawn(
      process.execPath,
      ["-e", ${JSON.stringify(grandchildScript)}, process.argv[2], process.argv[3]],
      { stdio: "ignore" },
    );
    const poll = setInterval(() => {
      if (fs.existsSync(process.argv[3])) {
        fs.writeFileSync(process.argv[1], String(grandchild.pid));
        clearInterval(poll);
      }
    }, 10);
  `;
  const child = spawn(process.execPath, ["-e", childScript, ready, heartbeat, grandReady], {
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let grandchildPid;
  try {
    grandchildPid = Number(await waitForReady(ready));
    assert.ok(grandchildPid > 0);
    await waitForReady(heartbeat);
    assert.deepEqual(await waitForChild(child, 200, { useProcessGroup: true }), {
      ok: false,
      reason: "timeout",
    });
    const stoppedAt = await readFile(heartbeat, "utf8");
    await delay(200);
    assert.equal(
      await readFile(heartbeat, "utf8"),
      stoppedAt,
      `descendant ${grandchildPid} kept running`,
    );
  } finally {
    killTestGroup(child);
    if (grandchildPid) {
      try {
        process.kill(grandchildPid, "SIGKILL");
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    }
    await rm(directory, { recursive: true, force: true });
  }
});
