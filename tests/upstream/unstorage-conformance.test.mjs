import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);
const binding = require("../../bindings/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;

if (!source) {
  throw new Error("MOUNTX_SOURCE must identify the pinned upstream checkout");
}

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));

// Resolve these from mountx's pinned dependency graph. The test package itself
// intentionally has no second, independently-versioned copy of unstorage.
const unstorage = await import(pathToFileURL(upstreamRequire.resolve("unstorage")).href);
const memory = await import(
  pathToFileURL(upstreamRequire.resolve("unstorage/drivers/memory")).href,
);
const [{ conformance }, { createUnstorageDriver }, { createLoopback, resolveCapabilities }] =
  await Promise.all([
    upstream("test/conformance.ts"),
    upstream("src/drivers/unstorage.ts"),
    upstream("src/harness.ts"),
  ]);

const { createStorage } = unstorage;
const { default: memoryStorageDriver } = memory;

const createMemoryStorage = () => createStorage({ driver: memoryStorageDriver() });

const rustCapabilities = await (async () => {
  const fs = binding.createUnstorageDriver(createMemoryStorage());
  try {
    return fs.capabilities;
  } finally {
    // The native adapter owns strong callback references. Release the probe
    // before registering the suites so a failed collection cannot keep Node
    // alive indefinitely.
    await fs.shutdown();
  }
})();

conformance({
  name: "Unstorage memory (TypeScript control)",
  capabilities: resolveCapabilities(createUnstorageDriver(createMemoryStorage())),
  setup: async () => ({
    fs: createLoopback(createUnstorageDriver(createMemoryStorage())),
  }),
});

conformance({
  name: "Unstorage memory (Rust NAPI bridge)",
  capabilities: rustCapabilities,
  setup: async () => {
    const fs = binding.createUnstorageDriver(createMemoryStorage());
    return {
      fs,
      cleanup: async () => {
        await fs.shutdown();
      },
    };
  },
});
