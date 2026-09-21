#!/usr/bin/env node

// Cleanup for the HTTP CLI Ozone lane. The caller supplies the unique Ozone
// run prefix and this script refuses to delete outside that exact run scope.

import assert from "node:assert/strict";

import { cleanupR2Prefix, r2ConfigFromEnv } from "../provider_matrix/r2-cleanup.mjs";

const ownerPrefix = process.env.OZONE_TEST_PREFIX;
const prefix = process.env.MOUNT_RS_CLI_REMOTE_PREFIX;
assert.match(
  ownerPrefix ?? "",
  /^mount-rs-ozone\/[A-Za-z0-9._-]+$/,
  "OZONE_TEST_PREFIX must identify one test-owned Ozone run",
);
assert.ok(prefix, "MOUNT_RS_CLI_REMOTE_PREFIX must be set for Ozone cleanup");
assert.ok(
  prefix.startsWith(`${ownerPrefix}/`),
  `Ozone cleanup prefix is outside the current run: ${prefix}`,
);
assert.ok(
  prefix.split("/").every((component) => /^[A-Za-z0-9._-]+$/.test(component)),
  `Ozone cleanup prefix contains an unsafe component: ${prefix}`,
);

const result = r2ConfigFromEnv();
assert.equal(result.missing.length, 0, `missing Ozone S3 configuration: ${result.missing.join(",")}`);
await cleanupR2Prefix(result.config, prefix);
console.log(`OZONE_REMOTE_PREFIX_CLEANUP_PASS prefix=${prefix}`);
