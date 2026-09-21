#!/usr/bin/env node

// Verify the shape and artifact binding of the W08 CycloneDX SBOM.
// This validates an unsigned SBOM; it does not verify a signing service or
// approve the release.

import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";

const REPOSITORY = "andymac4182/mount-rs";
const SOURCE_COMMIT_PATTERN = /^[0-9a-f]{40}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const BOM_REF_PATTERN = /^urn:mount-rs:package:[0-9a-f]{32}$/;

function fail(message) {
  console.error(`W08 release SBOM verification failed: ${message}`);
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

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`cannot read JSON: ${error.message}`);
  }
}

function requireString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${label} must be a non-empty string`);
  }
  return value;
}

function requireProperty(properties, name) {
  const property = properties.find((candidate) => candidate?.name === name);
  if (!property) {
    fail(`missing metadata property ${name}`);
  }
  return requireString(property.value, `metadata property ${name}`);
}

function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

const options = parseArguments(process.argv.slice(2));
const sbomPath = requiredOption(options, "sbom");
const repository = requiredOption(options, "repository");
const expectedSourceCommit = requiredOption(options, "source-commit");
const expectedPackage = options.get("package") ?? "mount-rs-cli";
const expectedVersion = options.get("version");
const artifactPath = options.get("artifact");

if (repository !== REPOSITORY) {
  fail(`repository must be ${REPOSITORY}`);
}
if (!SOURCE_COMMIT_PATTERN.test(expectedSourceCommit)) {
  fail("expected source commit must be a lowercase 40-character commit SHA");
}
if (artifactPath && !statSync(artifactPath).isFile()) {
  fail(`artifact is not a regular file: ${artifactPath}`);
}

const document = readJson(sbomPath);
if (document.bomFormat !== "CycloneDX" || document.specVersion !== "1.5") {
  fail("document must be CycloneDX specVersion 1.5");
}
if (!Number.isInteger(document.version) || document.version !== 1) {
  fail("document.version must be 1");
}
if (!/^urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(document.serialNumber ?? "")) {
  fail("document.serialNumber must be a UUID URN");
}
if (!document.metadata || Number.isNaN(Date.parse(document.metadata.timestamp ?? ""))) {
  fail("metadata.timestamp must be an ISO-8601 date");
}
if (!Array.isArray(document.metadata.properties)) {
  fail("metadata.properties must be an array");
}

const repositoryProperty = requireProperty(document.metadata.properties, "mount-rs:repository");
const sourceCommitProperty = requireProperty(document.metadata.properties, "mount-rs:source-commit");
const artifactSha256 = requireProperty(document.metadata.properties, "mount-rs:artifact-sha256");
const artifactSize = requireProperty(document.metadata.properties, "mount-rs:artifact-size-bytes");
if (repositoryProperty !== repository) {
  fail("repository property does not match the expected repository");
}
if (sourceCommitProperty !== expectedSourceCommit) {
  fail("source commit property does not match the expected source commit");
}
if (!SHA256_PATTERN.test(artifactSha256) && artifactSha256 !== "unbound") {
  fail("artifact SHA-256 property must be a lowercase SHA-256 or unbound");
}
if (artifactSha256 !== "unbound" && (!/^\d+$/.test(artifactSize) || Number(artifactSize) <= 0)) {
  fail("artifact size property must be a positive integer when the artifact is bound");
}

const rootComponent = document.metadata.component;
if (!rootComponent || rootComponent.name !== expectedPackage) {
  fail(`metadata.component must be ${expectedPackage}`);
}
if (expectedVersion && rootComponent.version !== expectedVersion) {
  fail(`metadata.component version must be ${expectedVersion}`);
}
if (!BOM_REF_PATTERN.test(rootComponent["bom-ref"] ?? "")) {
  fail("metadata.component has an invalid bom-ref");
}

if (!Array.isArray(document.components) || document.components.length === 0) {
  fail("components must be a non-empty array");
}
const componentRefs = new Set();
for (const component of document.components) {
  requireString(component?.name, "component.name");
  requireString(component?.version, `component ${component?.name}.version`);
  if (!BOM_REF_PATTERN.test(component["bom-ref"] ?? "")) {
    fail(`component ${component.name} has an invalid bom-ref`);
  }
  if (componentRefs.has(component["bom-ref"])) {
    fail(`duplicate component bom-ref ${component["bom-ref"]}`);
  }
  componentRefs.add(component["bom-ref"]);
  if (typeof component.purl !== "string" || !component.purl.startsWith("pkg:cargo/")) {
    fail(`component ${component.name} must have a cargo purl`);
  }
  if (component.licenses !== undefined) {
    if (!Array.isArray(component.licenses) || component.licenses.length === 0) {
      fail(`component ${component.name} licenses must be a non-empty array`);
    }
    for (const licenseChoice of component.licenses) {
      const license = licenseChoice?.license;
      const expression = licenseChoice?.expression;
      const validLicenseObject =
        license &&
        ((typeof license.id === "string" && license.id.length > 0) ||
          (typeof license.name === "string" && license.name.length > 0));
      if (!validLicenseObject && !(typeof expression === "string" && expression.length > 0)) {
        fail(`component ${component.name} contains an invalid license choice`);
      }
    }
  }
}
if (!componentRefs.has(rootComponent["bom-ref"])) {
  fail("metadata.component is not present in components");
}

if (!Array.isArray(document.dependencies)) {
  fail("dependencies must be an array");
}
const dependencyRefs = new Set();
for (const dependency of document.dependencies) {
  if (!BOM_REF_PATTERN.test(dependency?.ref ?? "")) {
    fail("dependency has an invalid ref");
  }
  if (!componentRefs.has(dependency.ref)) {
    fail(`dependency ref is absent from components: ${dependency.ref}`);
  }
  if (dependencyRefs.has(dependency.ref)) {
    fail(`duplicate dependency ref ${dependency.ref}`);
  }
  dependencyRefs.add(dependency.ref);
  if (!Array.isArray(dependency.dependsOn)) {
    fail(`dependency ${dependency.ref} dependsOn must be an array`);
  }
  for (const dependencyRef of dependency.dependsOn) {
    if (!componentRefs.has(dependencyRef)) {
      fail(`dependency ${dependency.ref} points to an unknown component`);
    }
  }
}

if (artifactPath) {
  if (artifactSha256 === "unbound") {
    fail("a supplied artifact cannot be verified against an unbound SBOM");
  }
  const actualSha256 = hashFile(artifactPath);
  const actualSize = statSync(artifactPath).size;
  if (actualSha256 !== artifactSha256) {
    fail(`artifact SHA-256 mismatch: expected ${artifactSha256}, got ${actualSha256}`);
  }
  if (String(actualSize) !== artifactSize) {
    fail(`artifact size mismatch: expected ${artifactSize}, got ${actualSize}`);
  }
}

console.log(
  `W08_RELEASE_SBOM_PASS package=${rootComponent.name} version=${rootComponent.version} components=${document.components.length} artifact_sha256=${artifactSha256}`,
);
