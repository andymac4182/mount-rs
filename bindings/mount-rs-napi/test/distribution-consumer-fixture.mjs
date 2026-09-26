import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";

const producerRequire = createRequire(import.meta.url);

export async function readProducerTypeVersions() {
  const nodePath = producerRequire.resolve("@types/node/package.json");
  const nodeTypes = JSON.parse(await readFile(nodePath, "utf8"));
  assert.equal(nodeTypes.name, "@types/node");
  assert.deepEqual(Object.keys(nodeTypes.dependencies ?? {}).sort(), ["undici-types"]);
  const nodeRequire = createRequire(nodePath);
  const undiciTypes = JSON.parse(
    await readFile(nodeRequire.resolve("undici-types/package.json"), "utf8"),
  );
  assert.equal(undiciTypes.name, "undici-types");
  assert.deepEqual(Object.keys(undiciTypes.dependencies ?? {}), []);
  return { node: nodeTypes.version, undici: undiciTypes.version };
}

export function makeConsumerFixture(coreSpecifier, nativeOverrides, typeVersions) {
  assert.match(typeVersions.node, /^\d+\.\d+\.\d+$/);
  assert.match(typeVersions.undici, /^\d+\.\d+\.\d+$/);
  const overrides = {
    ...nativeOverrides,
    "@types/node": typeVersions.node,
    "@types/node>undici-types": typeVersions.undici,
  };
  // Quoted YAML scalars preserve the existing relative file specifiers.
  const workspaceYaml = `overrides:\n${Object.entries(overrides)
    .map(([key, value]) => `  ${JSON.stringify(key)}: ${JSON.stringify(value)}`)
    .join("\n")}\n`;
  return {
    packageJson: {
      name: "mount-rs-consumer-smoke",
      private: true,
      dependencies: { "@mount-rs/core": coreSpecifier },
    },
    workspaceYaml,
  };
}
