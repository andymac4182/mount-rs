# Remote provider comparison evidence

See [the analysis](../../remote-provider-comparison.md) for workload, results, and limitations. `manifest.json` identifies the rebased source and SHA-256 fingerprints of the measured implementation and benchmark. The measurement windows are UTC.

`*-read.json` is the dedicated read sweep; `*-read-write.json` contains separate read and committed-write stages. `sqlite-extra-depths.json` is an additional successful invocation retained to show variability, rather than to select a more favorable write rate. `pglite-read.json` records the setup failure, with no measured stages and no fresh verification.

An artifact is successful only if every measured stage has zero failures, work/cleanup/verification errors are null, and a fresh coordinator verifies all 100 files. A failed result is not zero measured IOPS. This directory retains measurement JSON and diagnostic excerpts, without endpoints, credentials, or private FoundationDB cluster strings.

`foundationdb-read.json` passed fresh verification; `foundationdb-read-write.json` retains the Q100 CAS conflict failure and skipped verification. `foundationdb-cluster-identity.json` records the verified native SSD configuration. Docker statistics are one read-stage snapshot with cumulative counters. `sqlite-profile.json` and `sqlite-sample-*-excerpt.txt` are a separate CPU-sampled diagnostic, excluded from the matched throughput comparison.
