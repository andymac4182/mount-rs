import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

const repo = new URL("..", import.meta.url);
const root = fileURLToPath(repo);
const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to the pinned mountx checkout");
}
if (!existsSync(`${source}/src/auto.ts`)) {
  throw new Error(`MOUNTX_SOURCE does not contain src/auto.ts: ${source}`);
}

const rust = JSON.parse(
  execFileSync("cargo", ["run", "--locked", "--quiet", "--example", "auto_oracle"], {
    cwd: root,
    env: process.env,
    encoding: "utf8",
  }),
);
const auto = await import(
  pathToFileURL(`${source}/src/auto.ts`).href
);
const { liveMounts, p9ModuleRefusal, probeTransports } = auto;

const platforms = ["linux", "darwin", "win32"];
const transportNames = ["fuse", "9p", "nfs"];

function typescriptProbe(platform) {
  return probeTransports(platform).then((probe) => ({
    platform: probe.platform,
    preference: [...probe.preference],
    chosen: probe.chosen ?? null,
    fuse: { usable: probe.fuse.usable, reason: probe.fuse.reason ?? null },
    "9p": { usable: probe["9p"].usable, reason: probe["9p"].reason ?? null },
    nfs: { usable: probe.nfs.usable, reason: probe.nfs.reason ?? null },
    reason: probe.reason ?? null,
  }));
}

const typescript = {
  platforms: await Promise.all(platforms.map(typescriptProbe)),
  p9_refusal: {
    loaded: p9ModuleRefusal({
      usable: true,
      platform: "linux",
      kernel: true,
      transport: true,
      modules: true,
      root: true,
      reason: undefined,
    }) ?? null,
    loadable: p9ModuleRefusal({
      usable: true,
      platform: "linux",
      kernel: true,
      transport: false,
      modules: true,
      root: true,
      reason: undefined,
    }) ?? null,
    refused: p9ModuleRefusal({
      usable: true,
      platform: "linux",
      kernel: true,
      transport: false,
      modules: false,
      root: true,
      reason: undefined,
    }) ?? null,
  },
  registry: {
    // The pinned source currently exports liveMounts(), but not the internal
    // `loaded` set. If a later oracle exports loadedTransports(), consume it
    // here rather than silently replacing it with an expected literal.
    loaded:
      typeof auto.loadedTransports === "function"
        ? [...(await auto.loadedTransports())].map((entry) =>
            typeof entry === "string" ? entry : entry.transport,
          )
        : null,
    live: (await liveMounts()).length,
  },
};

assert.deepStrictEqual(
  rust.platforms.map(({ platform, preference, chosen }) => ({ platform, preference, chosen })),
  typescript.platforms.map(({ platform, preference, chosen }) => ({ platform, preference, chosen })),
  "automatic preference and choice diverged",
);

function classifyLinuxFuseReason(reason) {
  if (/^no \/dev\/fuse —/.test(reason)) return "missing-fuse-device";
  if (/^mounting without root needs the fusermount3 helper and mountx's native addon,/.test(reason)) {
    return "rootless-helper";
  }
  if (/^rootless FUSE mounting requires fusermount3 or fusermount on PATH$/.test(reason)) {
    return "rootless-helper";
  }
  return null;
}

function compareReason(platform, transport, expected, actual) {
  assert.equal(actual.usable, expected.usable, `${platform}/${transport}: usability diverged`);
  assert.equal(
    typeof actual.reason === "string",
    !actual.usable,
    `${platform}/${transport}: Rust usability and reason presence disagree`,
  );
  if (expected.usable) {
    assert.equal(actual.reason, null, `${platform}/${transport}: usable probe has a reason`);
    return "exact-usable";
  }

  assert.equal(typeof expected.reason, "string", `${platform}/${transport}: TS refusal has no reason`);
  assert.notEqual(expected.reason, "", `${platform}/${transport}: TS refusal reason is empty`);
  if (actual.reason === expected.reason) return "exact-reason";

  // The only intentionally tolerated wording difference is the Linux
  // rootless-FUSE diagnostic: the Rust crate cannot load mountx's native addon
  // probe. It must still identify the same concrete prerequisite, not merely
  // return some non-empty refusal.
  assert.equal(`${platform}/${transport}`, "linux/fuse", "unexpected Rust probe wording drift");
  const expectedClass = classifyLinuxFuseReason(expected.reason);
  const actualClass = classifyLinuxFuseReason(actual.reason);
  assert.equal(expectedClass, "rootless-helper", "unmapped TS Linux/FUSE reason");
  assert.equal(actualClass, expectedClass, "Rust Linux/FUSE reason names a different cause");
  return `mapped:${expectedClass}`;
}

const reasonMappings = [];
for (const [index, platform] of platforms.entries()) {
  const expected = typescript.platforms[index];
  const actual = rust.platforms[index];
  for (const transport of transportNames) {
    const mapping = compareReason(platform, transport, expected[transport], actual[transport]);
    if (mapping.startsWith("mapped:")) reasonMappings.push(`${platform}/${transport}:${mapping}`);
  }
  if (expected.reason === null) {
    assert.equal(actual.reason, null, `${platform}: Rust added a top-level refusal reason`);
  } else {
    assert.equal(
      actual.reason,
      `no transport can mount on this host — FUSE: ${actual.fuse.reason}; 9P: ${actual["9p"].reason}; NFS: ${actual.nfs.reason}`,
      `${platform}: Rust aggregate refusal does not contain its exact child diagnostics`,
    );
    if (reasonMappings.every((mapping) => !mapping.startsWith(`${platform}/`))) {
      assert.equal(actual.reason, expected.reason, `${platform}: aggregate refusal wording diverged`);
    }
  }
}

assert.equal(rust.p9_refusal.loaded, typescript.p9_refusal.loaded, "loaded 9P refusal diverged");
assert.equal(rust.p9_refusal.loadable, typescript.p9_refusal.loadable, "loadable 9P refusal diverged");
assert.equal(rust.p9_refusal.refused, typescript.p9_refusal.refused, "automatic 9P refusal diverged");
assert.equal(rust.registry.live, typescript.registry.live, "fresh live-mount registry diverged");
if (typescript.registry.loaded === null) {
  console.log("mount-rs auto registry: pinned TS source has no loadedTransports export; liveMounts was queried");
} else {
  assert.deepStrictEqual(rust.registry.loaded, typescript.registry.loaded, "loaded transport registry diverged");
}

const wordingDifferences = [];
for (const [index, platform] of platforms.entries()) {
  for (const transport of transportNames) {
    const expected = typescript.platforms[index][transport].reason;
    const actual = rust.platforms[index][transport].reason;
    if (expected !== actual) wordingDifferences.push(`${platform}/${transport}`);
  }
}
if (wordingDifferences.length > 0) {
  console.log(
    `mount-rs auto semantic parity: PASS (reason wording differs only at ${wordingDifferences.join(", ")}; explicit mappings: ${reasonMappings.join(", ") || "none"})`,
  );
} else {
  console.log("mount-rs auto semantic parity: PASS (probe reasons byte-identical)");
}
console.log(JSON.stringify({ rust, typescript }, null, 2));
