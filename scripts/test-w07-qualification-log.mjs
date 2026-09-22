#!/usr/bin/env node

// Exercise the W07 qualification-log verifier with synthetic, credential-free
// evidence. These cases validate the evidence-integrity boundary only; they
// do not contact FoundationDB, RustFS, or GitHub.

import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w07-qualification-log.mjs");

const validLog = [
  "W07_PRODUCTION_CONFIG_POLICY_PASS config_shape=splitstore metadata=foundationdb durable_metadata=true durable_blocks=true lease_authority=shared-provider lease_ttl=explicit-bounded blocks_tls=https secrets=external",
  "W07_PRODUCTION_CONFIG_POLICY_FAIL reason=config.driver.storage.blocks.secret_access_key-must-not-be-inline",
  "W07_PRODUCTION_CONFIG_POLICY_FAIL reason=storage.lease_ttl_ms-must-be-positive-safe-integer-at-most-24h",
  "FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000",
  "FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING container=mount-rs-foundationdb-authority-test interval_seconds=30 max_forward_jump_seconds=120 duration_seconds=1800",
  "FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3 publication_successes=3 publication_failures=0 reader_attempts=5 reader_successes=4 reader_failures=1 last_published_time_ms=2030001 last_observed_time_ms=2030001",
  "FOUNDATIONDB_RUSTFS_NETWORK_READY alias=mount-rs-rustfs container=mount-rs-rustfs-test",
  "FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403",
  "FOUNDATIONDB_RUSTFS_CHUNKED_PASS revision=10 volume_prefix=test cleanup_deferred=false",
  "FOUNDATIONDB_NAPI_PASS image=node:24-bookworm",
  "FOUNDATIONDB_SERVICE_RESTART_READY topology=durable server=foundationdb-test",
  "FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS volume_prefix=test",
  "FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse",
  "RUSTFS_COMBO_PASS name=foundationdb-production-qualification",
  "RUSTFS_INTEGRATION_PASS endpoint=http://127.0.0.1:32769 bucket=test prefix=test",
  "FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=29.13 output=/tmp/foundationdb-ozone-iops.json",
  "FOUNDATIONDB_SOAK_PASS rounds=5",
  "FOUNDATIONDB_TEST_PASS topology=durable manifests=tests/foundationdb/Cargo.toml+integrations/mount-rs-foundationdb/Cargo.toml platform=linux/amd64 service_restart=pass soak_rounds=5",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1000 p95_us=2000 p99_us=3000 total_ms=40 throughput_ops_per_sec=375",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1100 p95_us=2100 p99_us=3100 total_ms=41 throughput_ops_per_sec=365",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1200 p95_us=2200 p99_us=3200 total_ms=42 throughput_ops_per_sec=355",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1300 p95_us=2300 p99_us=3300 total_ms=43 throughput_ops_per_sec=345",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1400 p95_us=2400 p99_us=3400 total_ms=44 throughput_ops_per_sec=335",
  "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1500 p95_us=2500 p99_us=3500 total_ms=45 throughput_ops_per_sec=325",
].join("\n");

const validProvenance =
  "W07_QUALIFICATION_PROVENANCE repository=andymac4182/mount-rs workflow=W07 FoundationDB production qualification ref=refs/heads/main source_revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa run_id=35688516329 run_attempt=1 runner=GitHub Actions 1000026164";
const provenanceEnvironment = {
  W07_REQUIRE_PROVENANCE: "1",
  W07_QUALIFICATION_REPOSITORY: "andymac4182/mount-rs",
  W07_QUALIFICATION_WORKFLOW: "W07 FoundationDB production qualification",
  W07_QUALIFICATION_REF: "refs/heads/main",
  W07_QUALIFICATION_SOURCE_REVISION: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  W07_QUALIFICATION_RUN_ID: "35688516329",
  W07_QUALIFICATION_RUN_ATTEMPT: "1",
  W07_QUALIFICATION_RUNNER: "GitHub Actions 1000026164",
};

const cases = [
  {
    name: "valid-evidence",
    expectedStatus: 0,
    expectedOutput: "W07_PRODUCTION_QUALIFICATION_EVIDENCE_PASS",
    log: validLog,
  },
  {
    name: "valid-run-bound-provenance",
    expectedStatus: 0,
    expectedOutput: "W07_PRODUCTION_QUALIFICATION_PROVENANCE_PASS",
    log: `${validLog}\n${validProvenance}`,
    env: provenanceEnvironment,
  },
  {
    name: "missing-run-bound-provenance-marker",
    expectedStatus: 1,
    expectedOutput: "provenance-marker-missing",
    log: validLog,
    env: provenanceEnvironment,
  },
  {
    name: "mismatched-run-bound-provenance",
    expectedStatus: 1,
    expectedOutput: "provenance-mismatch-sourceRevision",
    log: `${validLog}\n${validProvenance.replace(
      "source_revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "source_revision=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )}`,
    env: provenanceEnvironment,
  },
  {
    name: "missing-unsafe-ttl-negative",
    expectedStatus: 1,
    expectedOutput: "missing=config-policy-negative-lease-ttl",
    log: validLog.replace(
      "W07_PRODUCTION_CONFIG_POLICY_FAIL reason=storage.lease_ttl_ms-must-be-positive-safe-integer-at-most-24h\n",
      "",
    ),
  },
  {
    name: "unsafe-publication-cadence",
    expectedStatus: 1,
    expectedOutput: "missing=lease-policy-values",
    log: validLog.replace(
      "publication_interval_ms=30000",
      "publication_interval_ms=120000",
    ),
  },
  {
    name: "unbounded-config-shape",
    expectedStatus: 1,
    expectedOutput: "missing=config-policy-pass",
    log: validLog.replace("lease_ttl=explicit-bounded", "lease_ttl=unbounded"),
  },
  {
    name: "missing-workload-qualification",
    expectedStatus: 1,
    expectedOutput: "missing=foundationdb-workload-profile",
    log: validLog.replace(
      "FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=29.13 output=/tmp/foundationdb-ozone-iops.json\n",
      "",
    ),
  },
  {
    name: "missing-authority-heartbeat",
    expectedStatus: 1,
    expectedOutput: "missing=authority-heartbeat,authority-heartbeat-values",
    log: validLog.replace(
      "FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING container=mount-rs-foundationdb-authority-test interval_seconds=30 max_forward_jump_seconds=120 duration_seconds=1800\n",
      "",
    ),
  },
  {
    name: "inconsistent-authority-stats",
    expectedStatus: 1,
    expectedOutput: "missing=authority-stats-values",
    log: validLog.replace(
      "publication_successes=3 publication_failures=0",
      "publication_successes=2 publication_failures=0",
    ),
  },
  {
    name: "missing-soak-latency-evidence",
    expectedStatus: 1,
    expectedOutput: "missing=latency-samples-6",
    log: validLog.replace(
      "FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1500 p95_us=2500 p99_us=3500 total_ms=45 throughput_ops_per_sec=325",
      "",
    ),
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w07-qualification-log-"));
try {
  for (const testCase of cases) {
    const logPath = join(sandbox, `${testCase.name}.log`);
    await writeFile(logPath, `${testCase.log}\n`);
    const result = spawnSync(process.execPath, [verifierPath, logPath, "5"], {
      cwd: repoRoot,
      encoding: "utf8",
      env: { ...process.env, ...(testCase.env ?? {}) },
    });
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W07_QUALIFICATION_LOG_TEST_FAIL case=${testCase.name} ` +
          `status=${result.status} output=${JSON.stringify(output.trim())}`,
      );
      process.exitCode = 1;
      break;
    }
  }
} finally {
  await rm(sandbox, { recursive: true, force: true });
}

if (process.exitCode !== 1) {
  console.log(`W07_QUALIFICATION_LOG_TEST_PASS cases=${cases.length}`);
}
