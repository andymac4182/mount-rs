# Remote production qualification baselines

These retained compact artifacts precede the storage fixes. Source identity,
configuration, operation counts, exact-byte verification, cleanup, and measured
resource limits are recorded in the artifacts and
[qualification report](../../remote-production-qualification.md).

The TiDB comparison uses the preserved release saturation binary from merged
`cd92c7d4` plus planning-only `c74ca180`. Virgin ten-server startup failed before
I/O; the preprovisioned control passed. VM block counters are not physical Mac
SSD IOPS. Authentication is synthetic in that data-path fixture.

The local provider probe uses the preserved public NAPI addon from `180a4552`.
All four providers completed 400 create/read/unlink lifecycles and 400 exact
byte reads with zero failures. Durability and execution environments differ;
these short measurements identify profiling targets and are not a fair
production performance ranking.

Raw binaries, checksums, scripts, logs, and observer samples remain under the
owned local `/private/tmp/mount-rs-qualification-baseline` directory. Paired
after-fix measurements and final acceptance evidence are still required.
