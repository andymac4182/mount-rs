import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

import { NATIVE_TARGETS } from "../scripts/aggregate-artifacts.mjs";

const execFileAsync = promisify(execFile);

function packageSpecifier(fromDirectory, packageFile) {
  const relativePath = relative(fromDirectory, packageFile).split(sep).join("/");
  return `file:${relativePath}`;
}

async function run(command, args, cwd) {
  const executable = process.platform === "win32" ? `${command}.cmd` : command;
  try {
    return await execFileAsync(executable, args, {
      cwd,
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
      shell: process.platform === "win32",
    });
  } catch (error) {
    const stdout = typeof error.stdout === "string" ? error.stdout.trim() : "";
    const stderr = typeof error.stderr === "string" ? error.stderr.trim() : "";
    const detail = [stdout, stderr].filter(Boolean).join("\n");
    throw new Error(`${command} ${args.join(" ")} failed${detail ? `:\n${detail}` : ""}`, {
      cause: error,
    });
  }
}

function parseArgs(args) {
  const options = { packageDir: undefined };
  for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === "--") {
      continue;
    } else if (argument === "--package-dir") {
      options.packageDir = args[++index];
      if (!options.packageDir) throw new Error("--package-dir requires a path");
    } else if (argument === "--help" || argument === "-h") {
      console.log("Usage: node test/distribution-consumer.mjs --package-dir <staged-package>");
      process.exit(0);
    } else {
      throw new Error(`Unknown argument ${argument}`);
    }
  }
  if (!options.packageDir) throw new Error("--package-dir is required");
  return options;
}

async function pack(directory, destination) {
  const { stdout } = await run("pnpm", ["pack", "--pack-destination", destination, "--json"], directory);
  const parsed = JSON.parse(stdout.trim());
  const report = Array.isArray(parsed) ? parsed[0] : parsed;
  assert.equal(typeof report.filename, "string", `pnpm pack returned no filename for ${directory}`);
  return resolve(report.filename);
}

const { packageDir } = parseArgs(process.argv.slice(2));
const resolvedPackageDir = resolve(packageDir);
const workDir = await mkdtemp(join(tmpdir(), "mount-rs-consumer-smoke-"));
const packageFilesDir = join(workDir, "packages");
const consumerDir = join(workDir, "consumer");
await mkdir(packageFilesDir);
await mkdir(consumerDir);

try {
  const rootTarball = await pack(resolvedPackageDir, packageFilesDir);
  const overrides = {};
  for (const target of NATIVE_TARGETS) {
    const targetTarball = await pack(join(resolvedPackageDir, "npm", target.platformArchABI), packageFilesDir);
    overrides[target.packageName] = packageSpecifier(consumerDir, targetTarball);
  }

  await writeFile(
    join(consumerDir, "package.json"),
    `${JSON.stringify(
      {
        name: "mount-rs-consumer-smoke",
        private: true,
        dependencies: {
          "@mount-rs/core": packageSpecifier(consumerDir, rootTarball),
        },
        pnpm: { overrides },
      },
      null,
    )}\n`,
  );
  await writeFile(
    join(consumerDir, "smoke.mjs"),
    `import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const packageJson = require("@mount-rs/core/package.json");
assert.equal(packageJson.name, "@mount-rs/core");
const { Filesystem } = require("@mount-rs/core");
const filesystem = Filesystem.memory();
try {
  await filesystem.writeFile("/consumer-smoke", Buffer.from("hosted package"));
  assert.equal((await filesystem.readFile("/consumer-smoke")).toString(), "hosted package");
} finally {
  await filesystem.shutdown();
}
console.log("mount-rs clean consumer install/smoke: PASS");
`,
  );

  await run("pnpm", ["install", "--offline", "--ignore-scripts", "--no-frozen-lockfile"], consumerDir);
  const smoke = await run("node", ["smoke.mjs"], consumerDir);
  process.stdout.write(smoke.stdout);
} finally {
  await rm(workDir, { recursive: true, force: true });
}
