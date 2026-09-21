#!/usr/bin/env node

// Generate a credential-free CycloneDX SBOM from locked cargo metadata.
// The document is bound to the release artifact and source commit, but this
// script does not sign the SBOM or approve a release.

import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";

const REPOSITORY = "andymac4182/mount-rs";
const SOURCE_COMMIT_PATTERN = /^[0-9a-f]{40}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;

function fail(message) {
  console.error(`W08 release SBOM generation failed: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const options = new Map();
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (!token.startsWith("--")) {
      fail(`unexpected argument ${token}`);
    }
    const name = token.slice(2);
    if (!name || index + 1 >= argv.length || argv[index + 1].startsWith("--")) {
      fail(`missing value for --${name}`);
    }
    if (options.has(name)) {
      fail(`duplicate option --${name}`);
    }
    options.set(name, argv[index + 1]);
    index += 1;
  }
  return options;
}

function requiredOption(options, name) {
  const value = options.get(name);
  if (!value) {
    fail(`missing --${name}`);
  }
  return value;
}

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`cannot read ${label}: ${error.message}`);
  }
}

function requireString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${label} must be a non-empty string`);
  }
  return value;
}

function hashText(value) {
  return createHash("sha256").update(value).digest("hex");
}

function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function encodePurl(value) {
  return encodeURIComponent(value).replace(/%2F/gi, "/");
}

function packageRef(packageId) {
  return `urn:mount-rs:package:${hashText(packageId).slice(0, 32)}`;
}

function packageSource(packageInfo) {
  if (typeof packageInfo.source === "string" && packageInfo.source.startsWith("registry+")) {
    return "registry";
  }
  if (typeof packageInfo.source === "string") {
    return "external";
  }
  return "workspace";
}

function packageComponent(packageInfo, rootPackageId) {
  const component = {
    type: packageInfo.id === rootPackageId ? "application" : "library",
    "bom-ref": packageRef(packageInfo.id),
    name: requireString(packageInfo.name, "package.name"),
    version: requireString(packageInfo.version, `package ${packageInfo.name}.version`),
    purl: `pkg:cargo/${encodePurl(packageInfo.name)}@${encodePurl(packageInfo.version)}`,
    properties: [
      { name: "mount-rs:source-kind", value: packageSource(packageInfo) },
    ],
  };

  if (typeof packageInfo.license === "string" && packageInfo.license.trim()) {
    const license = packageInfo.license.trim();
    component.licenses = [
      license.includes("/")
        ? { license: { name: license } }
        : /^[A-Za-z0-9][A-Za-z0-9.+-]*$/.test(license)
          ? { license: { id: license } }
          : { expression: license },
    ];
  }

  return component;
}

function metadataProperty(name, value) {
  return { name, value: String(value) };
}

const options = parseArguments(process.argv.slice(2));
const metadataPath = requiredOption(options, "metadata");
const outputPath = requiredOption(options, "output");
const repository = requiredOption(options, "repository");
const sourceCommit = requiredOption(options, "source-commit");
const artifactPath = options.get("artifact");
const requestedPackage = options.get("package") ?? "mount-rs-cli";

if (repository !== REPOSITORY) {
  fail(`repository must be ${REPOSITORY}`);
}
if (!SOURCE_COMMIT_PATTERN.test(sourceCommit)) {
  fail("source commit must be a lowercase 40-character commit SHA");
}
if (artifactPath && !statSync(artifactPath).isFile()) {
  fail(`artifact is not a regular file: ${artifactPath}`);
}

const cargoMetadata = readJson(metadataPath, "cargo metadata");
if (!Array.isArray(cargoMetadata.packages) || !cargoMetadata.resolve) {
  fail("cargo metadata must include packages and resolve data; do not use --no-deps");
}

const packageById = new Map();
for (const packageInfo of cargoMetadata.packages) {
  if (!packageInfo || typeof packageInfo.id !== "string") {
    fail("cargo metadata contains a package without an id");
  }
  if (packageById.has(packageInfo.id)) {
    fail(`duplicate cargo package id ${packageInfo.id}`);
  }
  packageById.set(packageInfo.id, packageInfo);
}

const matchingPackages = [...packageById.values()].filter(
  (packageInfo) => packageInfo.name === requestedPackage,
);
if (matchingPackages.length !== 1) {
  fail(`cargo metadata must contain exactly one package named ${requestedPackage}`);
}
const rootPackage = matchingPackages[0];
const rootPackageId = rootPackage.id;

const requestedVersion = options.get("version") ?? rootPackage.version;
if (requestedVersion !== rootPackage.version) {
  fail(`requested version ${requestedVersion} does not match ${rootPackage.version}`);
}

let artifactSha256 = "unbound";
let artifactSizeBytes = "unbound";
if (artifactPath) {
  artifactSha256 = hashFile(artifactPath);
  artifactSizeBytes = statSync(artifactPath).size;
}

const nodeById = new Map();
for (const node of cargoMetadata.resolve.nodes ?? []) {
  if (!node || typeof node.id !== "string") {
    fail("cargo resolve contains a node without an id");
  }
  if (!packageById.has(node.id)) {
    fail(`cargo resolve node is absent from packages: ${node.id}`);
  }
  nodeById.set(node.id, node);
}
if (!nodeById.has(rootPackageId)) {
  fail(`cargo resolve does not contain package ${requestedPackage}`);
}

const reachableIds = new Set();
const pendingIds = [rootPackageId];
while (pendingIds.length > 0) {
  const packageId = pendingIds.pop();
  if (reachableIds.has(packageId)) {
    continue;
  }
  const node = nodeById.get(packageId);
  if (!node) {
    fail(`reachable package is absent from cargo resolve: ${packageId}`);
  }
  reachableIds.add(packageId);
  for (const dependency of node.deps ?? []) {
    pendingIds.push(dependency.pkg);
  }
}

const components = [...reachableIds]
  .map((packageId) => packageById.get(packageId))
  .map((packageInfo) => packageComponent(packageInfo, rootPackageId))
  .sort((left, right) => left["bom-ref"].localeCompare(right["bom-ref"]));

const componentRefs = new Set(components.map((component) => component["bom-ref"]));
const dependencies = [...reachableIds]
  .map((packageId) => nodeById.get(packageId))
  .map((node) => {
    const dependsOn = [...new Set((node.deps ?? []).map((dependency) => dependency.pkg))]
      .map((dependencyId) => {
        if (!nodeById.has(dependencyId)) {
          fail(`dependency node is absent from cargo resolve: ${dependencyId}`);
        }
        return packageRef(dependencyId);
      })
      .sort();
    return {
      ref: packageRef(node.id),
      dependsOn,
    };
  })
  .sort((left, right) => left.ref.localeCompare(right.ref));

const timestamp = options.get("timestamp") ?? new Date().toISOString();
if (Number.isNaN(Date.parse(timestamp))) {
  fail("timestamp must be an ISO-8601 date");
}

const serialSource = `${repository}|${sourceCommit}|${requestedPackage}|${requestedVersion}|${artifactSha256}`;
const serialHex = hashText(serialSource).slice(0, 32);
const serialNumber = `urn:uuid:${serialHex.slice(0, 8)}-${serialHex.slice(8, 12)}-${serialHex.slice(12, 16)}-${serialHex.slice(16, 20)}-${serialHex.slice(20)}`;
const rootComponent = packageComponent(rootPackage, rootPackageId);

const document = {
  bomFormat: "CycloneDX",
  specVersion: "1.5",
  serialNumber,
  version: 1,
  metadata: {
    timestamp: new Date(timestamp).toISOString(),
    tools: [
      {
        vendor: "mount-rs",
        name: "write-w08-release-sbom.mjs",
        version: "1",
      },
    ],
    component: rootComponent,
    properties: [
      metadataProperty("mount-rs:repository", repository),
      metadataProperty("mount-rs:source-commit", sourceCommit),
      metadataProperty("mount-rs:artifact-sha256", artifactSha256),
      metadataProperty("mount-rs:artifact-size-bytes", artifactSizeBytes),
      metadataProperty("mount-rs:metadata-package-count", components.length),
    ],
  },
  components,
  dependencies,
};

writeFileSync(outputPath, `${JSON.stringify(document, null, 2)}\n`, "utf8");
console.log(
  `W08_RELEASE_SBOM_GENERATED package=${requestedPackage} version=${requestedVersion} components=${components.length} artifact_sha256=${artifactSha256}`,
);
