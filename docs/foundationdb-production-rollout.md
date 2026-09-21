# W07 FoundationDB production rollout

This ledger tracks the production qualification of the W07 FoundationDB
workstream after the demo. It is deliberately separate from the provider and
local/hosted qualification checks in `WORK_TRACKER.md`: a green demo, a local
Docker cluster, or a hosted fixture does not authorize a production rollout.

## Current decision

| Field | Status |
| --- | --- |
| Workstream | W07 — FoundationDB metadata with RustFS S3 chunks |
| Qualification baseline | W07.1, W07.2, W07.4, W07.6 and W07.6a are checked; W07.3, W07.5 and W07.7 remain open in `WORK_TRACKER.md` |
| Production rollout | **NO-GO** |
| Primary reason | Hosted Linux qualification is green, but no production topology, identity/ACL proof, clock/failover rehearsal, backup/restore packet, operational telemetry, production load/capacity result, complete native support decision, or release-owner sign-off is recorded |
| Evidence rule | Every result must identify the tested revision, image/provider versions, topology, test/run ID, terminal status, owner, and cleanup/rollback outcome |

The detailed tracker is the source of truth for implementation and acceptance
status. This ledger is the source of truth for the separate production track.
Do not check a production gate from demo behavior, a queued/skipped/cancelled
CI job, installation-only evidence, or a local qualification report.

## Latest qualification evidence

| Run | Result | Boundary |
| --- | --- | --- |
| `foundationdb-soak-durable`, 2026-09-21, arm64, tested tree `3b64a98` (published as `39a20b6`) | **PASS** — three pinned FoundationDB 7.4.7 servers with `double`/SSD configuration; one bounded composition soak round; replicated-node restart; authority republish; fresh-client RustFS reopen; owned cleanup | Disposable loopback/non-secure Docker qualification only. It does not prove production identity/ACL/TLS, power-loss or backup recovery, capacity/cost, multi-day soak, hosted CI, native platform support or operator readiness. |
| `foundationdb-network-cleanup`, 2026-09-21, arm64, tested tree `65b521c` (published as `6d2a5d4`) | **PASS** — shared RustFS/FDB client-network endpoint, endpoint reachability probe, durable composition, FoundationDB node restart/reopen and owned network disconnect/cleanup | Local disposable Docker qualification only; it does not prove hosted CI, production identity/ACL/TLS, capacity, backup/restore or operator readiness. |
| [Hosted run `35598049389`](https://github.com/andymac4182/mount-rs/actions/runs/35598049389), job `106327338587`, revision `65c52b9`, Ubuntu 24.04 | **PASS — hosted qualification** — pinned RustFS and FoundationDB image matches; durable three-server `double`/SSD topology; shared-network endpoint reachability (`403`); multi-chunk composition; one soak round; live Node/N-API; Linux FUSE/native CLI gate; service-node restart; authority republish; fresh-client reopen; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and final RustFS integration pass | Terminal hosted Linux evidence for this revision only. It is not production identity/ACL/TLS, power-loss or backup/restore, capacity/cost, multi-day soak, macOS, or release approval. |
| [Hosted run `35601357569`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35601357569), job `106337982523`, revision `622d0d1`, Ubuntu 24.04 | **PASS — hosted qualification with latency evidence** — production-config positive/negative policy fixtures passed and failed closed; durable three-server composition; bounded workload marker `FOUNDATIONDB_LATENCY_PASS workload=composition operations=11 p50_us=9893 p95_us=28865 p99_us=28865 total_ms=124 throughput_ops_per_sec=88.62`; one soak round; live Node/N-API; Linux FUSE/native CLI; service-node restart; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and RustFS integration pass | Terminal hosted Linux evidence for this revision only. The latency result is a bounded one-round measurement, not production load/capacity evidence; it does not prove production identity/ACL/TLS, backup/restore, multi-day soak, macOS, or release approval. |
| [Hosted attempt `35606084750`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606084750), job `106353392536`, revision `2d4ca9f`, Ubuntu 24.04 | **NO HOSTED PASS** — setup, config policy, prerequisites and N-API build passed; the RustFS harness stopped before provider execution because `tests/rustfs/Cargo.lock` did not include the `futures-util` dependency declared by `mount-rs-r2` | Reproducibility/lockfile failure, not provider acceptance. The lockfile correction is published in the follow-up revision; this run contributes no FoundationDB or RustFS runtime evidence. |
| [Hosted run `35606741719`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606741719), job `106355562011`, revision `4aadbb1`, Ubuntu 24.04 | **PASS — guarded-authority hosted qualification** — policy positive/negative fixtures passed and failed closed; shared RustFS network and endpoint reachability (`403`); bounded workload marker `FOUNDATIONDB_LATENCY_PASS workload=composition operations=11 p50_us=11702 p95_us=26193 p99_us=26193 total_ms=120 throughput_ops_per_sec=91.24`; chunked composition; one soak round; live Node/N-API; Linux FUSE/native CLI; service-node restart; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and RustFS integration pass | Terminal hosted Linux evidence for this revision only. This validates the bounded publisher guard and qualification path, not production identity/ACL/TLS, backup/restore, production load/capacity, multi-day soak, macOS, or release approval. |
| [Hosted run `35608336345`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35608336345), job `106360895920`, revision `976725e`, Ubuntu 24.04 | **PASS — five-round guarded-authority hosted qualification** — policy positive/negative fixtures passed and failed closed; shared RustFS network and endpoint reachability (`403`); bounded workload marker `FOUNDATIONDB_LATENCY_PASS workload=composition operations=11 p50_us=9990 p95_us=85061 p99_us=85061 total_ms=167 throughput_ops_per_sec=65.76`; five isolated soak rounds; live Node/N-API; Linux FUSE/native CLI; service-node restart; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and RustFS integration pass | Terminal hosted Linux evidence for this revision only. The five rounds strengthen bounded repeatability and cleanup evidence, but do not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35620006731`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35620006731), job `106402140768`, revision `aa3dae3`, Ubuntu 24.04 | **PASS — corrected five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable three-server FoundationDB `double`/SSD topology; RustFS endpoint reachability (`403`); chunked composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9593 p95_us=48162 p99_us=48162 total_ms=171 throughput_ops_per_sec=87.35`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and RustFS integration pass. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for `aa3dae3` only. This confirms the corrected privileged `mount(2)` path in the production workflow, but does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35623491280`](https://github.com/andymac4182/mount-rs/actions/runs/35623491280), job `106415143857`, revision `336d9a3`, Ubuntu 24.04 | **PASS — current-main five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=13140 p95_us=61081 p99_us=61081 total_ms=233 throughput_ops_per_sec=64.28`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; `FOUNDATIONDB_TEST_PASS`, `RUSTFS_COMBO_PASS` and RustFS integration pass. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for `336d9a3` only. This is a newer bounded current-main checkpoint; it does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35627206302`](https://github.com/andymac4182/mount-rs/actions/runs/35627206302), job `106424385715`, revision `87db9fd`, Ubuntu 24.04 | **PASS — provenance-validated current-main five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7981 p95_us=23550 p99_us=23550 total_ms=124 throughput_ops_per_sec=120.85`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 summary and workflow provenance validation passed. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for `87db9fd` only. Artifact `foundationdb-production-qualification-35627206302-1` has SHA-256 `d2e6743dedf5d1099e383cfc41fe00cfb2e22a05068289084c2db75641cbf7e3`; this remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted attempt `35632139449`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632139449), job `106440649294`, revision `3109a2e`, Ubuntu 24.04 | **NO HOSTED PASS** — policy fixtures, prerequisites and dependency installation passed, but the default N-API build stopped before provider execution because `FuseSession::interrupt_target` called the expanded body validator without its protocol-context argument (`E0061` at `transports/mount-rs-fuse/src/session.rs:696`) | Compile/reproducibility blocker, not FoundationDB, RustFS or native acceptance. The correction was already published upstream as `f16eec2` and was requalified below; this attempt contributes no runtime evidence. |
| [Hosted run `35632935680`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632935680), job `106443276982`, revision `0bb628b`, Ubuntu 24.04 | **PASS — corrected current-main provenance-validated five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9577 p95_us=41547 p99_us=41547 total_ms=168 throughput_ops_per_sec=89.02`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 summary and workflow provenance/evidence validation passed. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for the tested revision `0bb628b` only, completed in 11m41s. Artifact `foundationdb-production-qualification-35632935680-1` has SHA-256 `f22252db5e4efcbb3cb4d8f5d0bcf955bd96981c71ed314cdb3aa555add222f4`; its provenance records source revision `0bb628b0323901a1632b05dd8321c32fcabca7d8`, run `35632935680`, attempt `1`, and runner `GitHub Actions 1000020575`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35634895382`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35634895382), job `106449795704`, revision `0e0454da`, Ubuntu 24.04 | **PASS — newer provenance-validated five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9462 p95_us=398345 p99_us=398345 total_ms=803 throughput_ops_per_sec=18.68`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 summary and workflow provenance/evidence validation passed. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for the tested revision `0e0454da` only, completed in 10m10s. Artifact `foundationdb-production-qualification-35634895382-1` has SHA-256 `c542e9538bc29708fa187ecf78281075060f981a3b06b4d30e686c79a6a33bf7`; its provenance records source revision `0e0454da7973c08a56c8634435909da037d21fe7`, run `35634895382`, attempt `1`, and runner `GitHub Actions 1000020805`. The bounded latency outlier is qualification telemetry, not production capacity evidence; production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval remain open. |
| [Hosted run `35636591071`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35636591071), job `106455406713`, revision `97b63aed`, Ubuntu 24.04 | **PASS — post-FUSE-abort-fix provenance-validated five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9640 p95_us=27896 p99_us=27896 total_ms=157 throughput_ops_per_sec=95.52`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 summary and workflow provenance/evidence validation passed. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for the tested revision `97b63aed` only, completed in 11m46s. Artifact `foundationdb-production-qualification-35636591071-1` has SHA-256 `123c2c5ece757fda141342d2b8e415e34269fff4246f5c3a8c147902415c137d`; its provenance records source revision `97b63aed2236696a8d397054b3c04c824e89c229`, run `35636591071`, attempt `1`, and runner `GitHub Actions 1000020918`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35638932960`](https://github.com/andymac4182/mount-rs/actions/runs/35638932960), job `106463213777`, revision `3817efc9`, Ubuntu 24.04 | **PASS — exact-current-main provenance-validated five-round hosted qualification** — policy fixtures passed with the expected positive/negative outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9751 p95_us=28824 p99_us=28824 total_ms=151 throughput_ops_per_sec=99.31`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 summary and workflow provenance/evidence validation passed. The retained artifact summary reports `qualification-pass`. | Terminal hosted Linux qualification for the tested revision `3817efc9` only, completed in 11m33s. Artifact `foundationdb-production-qualification-35638932960-1` has SHA-256 `c474b5ef9275ef88daf73e7fe90e36bd25eba8d149ac6849edfc672217ef4bd0`; its provenance records source revision `3817efc9999cc9d77e2c597873193d56206adb08`, run `35638932960`, attempt `1`, and runner `GitHub Actions 1000021113`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35641662353`](https://github.com/andymac4182/mount-rs/actions/runs/35641662353), job `106472188828`, revision `31e122b`, Ubuntu 24.04 | **PASS — latest mainline provenance-validated five-round hosted qualification** — the rollout-ledger guard passed with the repository still **NO-GO** and W07.7 open; production-config positive/negative fixtures passed with the expected outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9861 p95_us=29367 p99_us=29367 total_ms=163 throughput_ops_per_sec=91.81`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `31e122bc2e5790bb3568c01aaea4b236d89dce96` only, completed in 11m37s. Artifact `foundationdb-production-qualification-35641662353-1` has SHA-256 `5f744788bd82fd52fd7e59f1201885dc79f80af2b0558eef917187abce222b50`; its provenance records source revision `31e122bc2e5790bb3568c01aaea4b236d89dce96`, run `35641662353`, attempt `1`, and runner `GitHub Actions 1000021349`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35645459649`](https://github.com/andymac4182/mount-rs/actions/runs/35645459649), job `106484724044`, revision `b3a0a92`, Ubuntu 24.04 | **PASS — latest mainline qualification with rollout-ledger regression coverage** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; production-config positive/negative fixtures passed with the expected outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9350 p95_us=49303 p99_us=49303 total_ms=182 throughput_ops_per_sec=82.07`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `b3a0a92d617e66ad58460f345c428e197f8c9e2d` only, completed in 11m28s. Artifact `foundationdb-production-qualification-35645459649-1` has SHA-256 `0bab89cbff4d36b68351d84978ad56991e2ac16621084dd987f8717d4b07e63e`; its provenance records source revision `b3a0a92d617e66ad58460f345c428e197f8c9e2d`, run `35645459649`, attempt `1`, and runner `GitHub Actions 1000021544`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35648296386`](https://github.com/andymac4182/mount-rs/actions/runs/35648296386), job `106494088981`, revision `20fb445a`, Ubuntu 24.04 | **PASS — latest mainline qualification with rollout-ledger regression coverage** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; production-config positive/negative fixtures passed with the expected outcomes; durable FoundationDB/RustFS composition; `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7654 p95_us=36558 p99_us=36558 total_ms=146 throughput_ops_per_sec=102.61`; five isolated soak rounds; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `20fb445a02b16e3dcb62525fcbd84ff14d7c8dea` only, completed in 11m16s. Artifact `foundationdb-production-qualification-35648296386-1` has SHA-256 `8c93fc161217e7c032be17c6892791bb764a081ddad678ad601a0d6775a5962b`; its provenance records source revision `20fb445a02b16e3dcb62525fcbd84ff14d7c8dea`, run `35648296386`, attempt `1`, and runner `GitHub Actions 1000021765`. This remains bounded qualification and does not prove production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval. |
| [Hosted run `35650634174`](https://github.com/andymac4182/mount-rs/actions/runs/35650634174), job `106501907695`, revision `8f3a19a`, Ubuntu 24.04 | **PASS — previous mainline qualification with rollout-ledger regression coverage** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; production-config positive/negative fixtures passed with the expected outcomes; durable FoundationDB/RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=12440 p95_us=117600 p99_us=117600 total_ms=385 throughput_ops_per_sec=38.88`; five isolated soak rounds with p95/p99 ranging from 31,686µs to 346,494µs and throughput from 23.53 to 97.18 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `8f3a19a891b8d432ff551c04789921575bb12f4f` only, completed in 10m19s. Artifact `foundationdb-production-qualification-35650634174-1` has SHA-256 `6a99d3778504fa2ab123c7256d168b4b52a424c2aea594f7dc03ce7d076a16f8`; its provenance records source revision `8f3a19a891b8d432ff551c04789921575bb12f4f`, run `35650634174`, attempt `1`, and runner `GitHub Actions 1000021966`. This remains bounded qualification; the latency variability is not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval remain open. |
| [Hosted run `35652638242`](https://github.com/andymac4182/mount-rs/actions/runs/35652638242), job `106508480404`, revision `c6f0039`, Ubuntu 24.04 | **PASS — latest mainline qualification with explicit lease-TTL policy coverage** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; the positive production-config fixture passed with `lease_ttl=explicit-bounded`, while inline-secret and unsafe-TTL fixtures failed closed; durable three-server FoundationDB/RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7712 p95_us=228723 p99_us=228723 total_ms=353 throughput_ops_per_sec=42.41`; five isolated soak rounds with p95/p99 from 27,598µs to 35,139µs and throughput from 77.45 to 95.05 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `c6f0039471ebdd441a21648a1fb923abc38c9a6b` only, completed in 11m58s. Artifact `foundationdb-production-qualification-35652638242-1` has SHA-256 `366cb78d19bc5182a438a8459ebfe54efc510232e9dab667a815aba5eacfbee7`; its provenance records source revision `c6f0039471ebdd441a21648a1fb923abc38c9a6b`, run `35652638242`, attempt `1`, and runner `GitHub Actions 1000022176`. This remains bounded qualification; the base latency outlier is qualification telemetry, not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval remain open. |
| Local source gate, 2026-09-22, revision `2641962a` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `./scripts/cargo-shared test --workspace --all-targets --locked` all passed on the current shared source tip; current FUSE sync-barrier/session, NFS, transport, SDK, CLI and provider unit coverage passed in the all-target run | Source qualification only. Provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `e8f3715` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `./scripts/cargo-shared test --workspace --all-targets --locked` all passed on the latest shared tip, including the current WebDAV close-timeout tests and core/transport/SDK/CLI coverage | Source qualification only. FoundationDB provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `4ea3268` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `./scripts/cargo-shared test --workspace --all-targets --locked` all passed on the published shared tip; current 9P/FUSE, NFS, transport, SDK, CLI and provider unit coverage passed in the all-target run | Source qualification only. Provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `3cd4377` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `CARGO_TARGET_DIR=/private/tmp/mount-rs-w07-main-gate-3cd43779-test ./scripts/cargo-shared test --workspace --all-targets --locked` all passed; current FUSE sync-barrier/session and NFS coverage passed in the all-target run | Current-main source qualification only. Provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `9d3a6e5` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `CARGO_TARGET_DIR=/private/tmp/mount-rs-w07-main-gate-9d3a6e50-test ./scripts/cargo-shared test --workspace --all-targets --locked` all passed; the FUSE sync-barrier/session coverage passed in the all-target run | Current-main source qualification only. Provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `29365e9` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, and `CARGO_TARGET_DIR=/private/tmp/mount-rs-w07-main-gate-29365e9 ./scripts/cargo-shared test --workspace --all-targets --locked` all passed | Current-main source qualification only. Provider, native-mount and external-service rows remained explicitly ignored where their required harnesses were unavailable; this does not close the hosted platform or production deployment gates. |
| Local source gate, 2026-09-22, revision `717a0ab` | **PASS** — `cargo fmt --all -- --check`, strict workspace Clippy with `-D warnings`, and the locked all-target workspace test suite passed with loopback networking enabled for the TCP integration test | Source qualification only. Provider, native-mount and external-service rows were explicitly ignored without their required harnesses; this is not hosted or production acceptance. |
| Hosted attempt `35591729423`, revision `1ea183e`, Linux `foundationdb-rustfs` | **NO HOSTED PASS** — FoundationDB configured/readiness/image-match markers passed, then the client failed to reach the published RustFS endpoint at `host.docker.internal:32768`; the workflow was later superseded | Historical blocker that motivated the shared-network fix in `6d2a5d4`; the terminal rerun is recorded above. |

The latest hosted pass above is retained as qualification evidence for the
restart/fencing and harness gates, not as production acceptance. Its markers
included
`FOUNDATIONDB_RUSTFS_CHUNKED_PASS`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
`FOUNDATIONDB_NAPI_PASS`, `FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS` and
`FOUNDATIONDB_TEST_PASS topology=durable`.

The previous corrected terminal hosted evidence is run
[`35620006731`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35620006731)
(job `106402140768`, revision `aa3dae3`). Its retained artifact summary is
`qualification-pass` and records `FOUNDATIONDB_CLI_PASS
mode=foundationdb-rustfs-fuse`, `FOUNDATIONDB_SOAK_PASS rounds=5`,
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9593
p95_us=48162 p99_us=48162 total_ms=171 throughput_ops_per_sec=87.35`,
`FOUNDATIONDB_TEST_PASS ... platform=linux/amd64 service_restart=pass
soak_rounds=5`, `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. This closes
the corrected hosted Linux qualification checkpoint for the tested revision; it
does not close the separate production gates.

The previous current-main hosted run
[`35623491280`](https://github.com/andymac4182/mount-rs/actions/runs/35623491280)
(job `106415143857`, revision `336d9a3`) also retained a
`qualification-pass` summary with `FOUNDATIONDB_CLI_PASS`, five soak rounds,
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=13140
p95_us=61081 p99_us=61081 total_ms=233 throughput_ops_per_sec=64.28`,
`FOUNDATIONDB_TEST_PASS ... platform=linux/amd64 service_restart=pass
soak_rounds=5`, `RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS`. It is the
earlier hosted qualification checkpoint. Its retained artifact
`foundationdb-production-qualification-35623491280-1` has SHA-256
`b353ac2744dae9c469f7807533f8eec0d52e49120a101552c428585e0559741d`; the
schema-versioned summary records the workflow provenance for that prior
checkpoint. It is still qualification evidence, not production acceptance.

The previous current-main hosted run
[`35627206302`](https://github.com/andymac4182/mount-rs/actions/runs/35627206302)
(job `106424385715`, revision `87db9fd`) completed green on Ubuntu 24.04 in
10m47s. Its retained artifact
`foundationdb-production-qualification-35627206302-1` has SHA-256
`d2e6743dedf5d1099e383cfc41fe00cfb2e22a05068289084c2db75641cbf7e3`; the
schema-2 summary records `sourceRevision=87db9fdad8b85d8f11ad1176ef97ec5e5b380435`,
`runId=35627206302`, `runAttempt=1`, and the GitHub Actions workflow/runner.
It also records five soak rounds, `FOUNDATIONDB_CLI_PASS`, the native restart
path and the expected positive/negative policy markers. It is still bounded
qualification evidence, not production acceptance.

The previous corrected current-main hosted run
[`35632935680`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632935680)
(job `106443276982`, revision `0bb628b`) completed green on Ubuntu 24.04 in
11m41s after the preceding N-API compile blocker was corrected upstream. Its
retained artifact `foundationdb-production-qualification-35632935680-1` has
SHA-256 `f22252db5e4efcbb3cb4d8f5d0bcf955bd96981c71ed314cdb3aa555add222f4`.
The schema-2 summary records
`sourceRevision=0bb628b0323901a1632b05dd8321c32fcabca7d8`,
`runId=35632935680`, `runAttempt=1`, runner `GitHub Actions 1000020575`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9577
p95_us=41547 p99_us=41547 total_ms=168 throughput_ops_per_sec=89.02`.
It is the latest bounded hosted qualification evidence, not production
acceptance.

The previous current-main hosted run
[`35634895382`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35634895382)
(job `106449795704`, revision `0e0454da`) completed green on Ubuntu 24.04 in
10m10s. Its retained artifact
`foundationdb-production-qualification-35634895382-1` has SHA-256
`c542e9538bc29708fa187ecf78281075060f981a3b06b4d30e686c79a6a33bf7`.
The schema-2 summary records
`sourceRevision=0e0454da7973c08a56c8634435909da037d21fe7`,
`runId=35634895382`, `runAttempt=1`, runner `GitHub Actions 1000020805`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9462
p95_us=398345 p99_us=398345 total_ms=803 throughput_ops_per_sec=18.68`.
The p95/p99 outlier remains a bounded qualification observation, not a
production capacity result; this run is not production acceptance.

The previous post-FUSE-abort-fix hosted run
[`35636591071`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35636591071)
(job `106455406713`, revision `97b63aed`) completed green on Ubuntu 24.04 in
11m46s. Its retained artifact
`foundationdb-production-qualification-35636591071-1` has SHA-256
`123c2c5ece757fda141342d2b8e415e34269fff4246f5c3a8c147902415c137d`.
The schema-2 summary records
`sourceRevision=97b63aed2236696a8d397054b3c04c824e89c229`,
`runId=35636591071`, `runAttempt=1`, runner `GitHub Actions 1000020918`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9640
p95_us=27896 p99_us=27896 total_ms=157 throughput_ops_per_sec=95.52`.
It is prior bounded hosted qualification evidence, not production
acceptance.

An earlier hosted mainline run
[`35638932960`](https://github.com/andymac4182/mount-rs/actions/runs/35638932960)
(job `106463213777`, revision `3817efc9`) completed green on Ubuntu 24.04 in
11m33s after exercising the current FUSE sync-barrier and blocked-read changes.
Its retained artifact
`foundationdb-production-qualification-35638932960-1` has SHA-256
`c474b5ef9275ef88daf73e7fe90e36bd25eba8d149ac6849edfc672217ef4bd0`.
The schema-2 summary records
`sourceRevision=3817efc9999cc9d77e2c597873193d56206adb08`,
`runId=35638932960`, `runAttempt=1`, runner `GitHub Actions 1000021113`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9751
p95_us=28824 p99_us=28824 total_ms=151 throughput_ops_per_sec=99.31`.
It is prior bounded hosted qualification evidence, not production acceptance.

The previous hosted mainline run
[`35641662353`](https://github.com/andymac4182/mount-rs/actions/runs/35641662353)
(job `106472188828`, revision `31e122bc`) completed green on Ubuntu 24.04 in
11m37s. Its retained artifact
`foundationdb-production-qualification-35641662353-1` has SHA-256
`5f744788bd82fd52fd7e59f1201885dc79f80af2b0558eef917187abce222b50`.
The schema-2 summary records
`sourceRevision=31e122bc2e5790bb3568c01aaea4b236d89dce96`,
`runId=35641662353`, `runAttempt=1`, runner `GitHub Actions 1000021349`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9861
p95_us=29367 p99_us=29367 total_ms=163 throughput_ops_per_sec=91.81`.
The same run passed `W07_ROLLOUT_LEDGER_POLICY_PASS` while preserving the
NO-GO boundary. It is prior bounded hosted qualification evidence, not
production acceptance.

The previous hosted mainline run
[`35645459649`](https://github.com/andymac4182/mount-rs/actions/runs/35645459649)
(job `106484724044`, revision `b3a0a92`) completed green on Ubuntu 24.04 in
11m28s. Its retained artifact
`foundationdb-production-qualification-35645459649-1` has SHA-256
`0bab89cbff4d36b68351d84978ad56991e2ac16621084dd987f8717d4b07e63e`.
The schema-2 summary records
`sourceRevision=b3a0a92d617e66ad58460f345c428e197f8c9e2d`,
`runId=35645459649`, `runAttempt=1`, runner `GitHub Actions 1000021544`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9350
p95_us=49303 p99_us=49303 total_ms=182 throughput_ops_per_sec=82.07`.
It also passed `W07_ROLLOUT_LEDGER_POLICY_PASS` and
`W07_ROLLOUT_LEDGER_TEST_PASS cases=6` while preserving the NO-GO boundary.
It is the latest bounded hosted qualification evidence, not production
acceptance.

The previous hosted mainline run
[`35648296386`](https://github.com/andymac4182/mount-rs/actions/runs/35648296386)
(job `106494088981`, revision `20fb445a`) completed green on Ubuntu 24.04 in
11m16s. Its retained artifact
`foundationdb-production-qualification-35648296386-1` has SHA-256
`8c93fc161217e7c032be17c6892791bb764a081ddad678ad601a0d6775a5962b`.
The schema-2 summary records
`sourceRevision=20fb445a02b16e3dcb62525fcbd84ff14d7c8dea`,
`runId=35648296386`, `runAttempt=1`, runner `GitHub Actions 1000021765`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7654
p95_us=36558 p99_us=36558 total_ms=146 throughput_ops_per_sec=102.61`.
It also passed `W07_ROLLOUT_LEDGER_POLICY_PASS`,
`W07_ROLLOUT_LEDGER_TEST_PASS cases=6`, the credential-free production
configuration policy fixtures, `RUSTFS_COMBO_PASS` and
`RUSTFS_INTEGRATION_PASS` while preserving the NO-GO boundary. It is the
prior bounded hosted qualification evidence, not production acceptance.

The latest hosted mainline run
[`35650634174`](https://github.com/andymac4182/mount-rs/actions/runs/35650634174)
(job `106501907695`, revision `8f3a19a`) completed green on Ubuntu 24.04 in
10m19s. Its retained artifact
`foundationdb-production-qualification-35650634174-1` has SHA-256
`6a99d3778504fa2ab123c7256d168b4b52a424c2aea594f7dc03ce7d076a16f8`.
The schema-2 summary records
`sourceRevision=8f3a19a891b8d432ff551c04789921575bb12f4f`,
`runId=35650634174`, `runAttempt=1`, runner `GitHub Actions 1000021966`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and base
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=12440
p95_us=117600 p99_us=117600 total_ms=385 throughput_ops_per_sec=38.88`.
The five soak-round p95/p99 values ranged from 31,686µs to 346,494µs and
throughput ranged from 23.53 to 97.18 ops/s. It also passed
`W07_ROLLOUT_LEDGER_POLICY_PASS`, `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`,
the credential-free production configuration policy fixtures,
`RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS` while preserving the NO-GO
boundary. It is the prior bounded hosted qualification evidence, not
production capacity or production acceptance.

The latest hosted mainline run
[`35652638242`](https://github.com/andymac4182/mount-rs/actions/runs/35652638242)
(job `106508480404`, revision `c6f0039`) completed green on Ubuntu 24.04 in
11m58s. Its retained artifact
`foundationdb-production-qualification-35652638242-1` has SHA-256
`366cb78d19bc5182a438a8459ebfe54efc510232e9dab667a815aba5eacfbee7`.
The schema-2 summary records
`sourceRevision=c6f0039471ebdd441a21648a1fb923abc38c9a6b`,
`runId=35652638242`, `runAttempt=1`, runner `GitHub Actions 1000022176`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and base
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7712
p95_us=228723 p99_us=228723 total_ms=353 throughput_ops_per_sec=42.41`.
The five soak-round p95/p99 values ranged from 27,598µs to 35,139µs and
throughput ranged from 77.45 to 95.05 ops/s. The positive configuration
fixture passed with `lease_ttl=explicit-bounded`; the inline-secret and
unsafe-TTL fixtures failed closed. It also passed
`W07_ROLLOUT_LEDGER_POLICY_PASS`, `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`,
`RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS` while preserving the NO-GO
boundary. This remains bounded qualification evidence, not production
capacity or production acceptance.

The previous shared-tip source gate tested revision
`2641962a6a65179abf4b8d785345fbe6af4be9b8` on 2026-09-22. Formatting, strict
locked workspace Clippy and the locked all-target workspace test suite all
passed; current FUSE sync-barrier/session, NFS, transport, SDK, CLI and
provider unit coverage passed. Provider, native-mount and external-service
rows remained explicitly ignored where their required harnesses were
unavailable. This is source qualification only, not hosted or production
acceptance.

The latest shared-tip source gate tested revision
`4ea3268306d71fe96c537cd0f3c4d4393f173a60` on 2026-09-22. Formatting, strict
locked workspace Clippy with `-D warnings`, and
`./scripts/cargo-shared test --workspace --all-targets --locked` all passed;
the current 9P/FUSE, NFS, transport, SDK, CLI and provider unit coverage
passed. Provider, native-mount and external-service rows remained explicitly
ignored where their required harnesses were unavailable. This is source
qualification only, not hosted or production acceptance.

The preceding hosted attempt
[`35632139449`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35632139449)
(job `106440649294`, revision `3109a2e`) is retained as a failed build
checkpoint: policy and setup passed, but the default N-API build stopped at
the missing `validate_body` protocol-context argument in
`FuseSession::interrupt_target`. It produced no provider or native runtime
evidence; the corrected `f16eec2` mainline was requalified above.

The follow-up network-cleanup run also emitted
`FOUNDATIONDB_RUSTFS_NETWORK_READY`,
`FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403`,
`FOUNDATIONDB_TEST_PASS topology=durable manifests=tests/foundationdb/Cargo.toml+integrations/mount-rs-foundationdb/Cargo.toml platform=linux/arm64 service_restart=pass soak_rounds=0`,
`RUSTFS_COMBO_PASS` and `RUSTFS_INTEGRATION_PASS` with exit 0.

The earlier hosted failure identified the Linux container-network boundary
rather than an FDB transaction failure. The fix attaches the owned RustFS
container to the FoundationDB client network under `mount-rs-rustfs` and
disconnects it during owned cleanup. The terminal hosted rerun above passed the
composition, N-API, Linux native CLI/FUSE and restart phases; the earlier run
remains retained as the failure that motivated the fix.

The repository now also contains the manually dispatched
`.github/workflows/foundationdb-production.yml` gate. It uses non-cancelling
concurrency, runs the durable composition with five isolated bounded soak
rounds plus Node, Linux native CLI/FUSE, restart and fresh-client checks, and retains a
redacted terminal log and a schema-versioned summary artifact. The summary is
required to carry the repository, source revision, ref, workflow, runner, run ID
and attempt that produced the markers, so the retained result can be reconciled
to one immutable workflow execution despite unrelated mainline pushes. This
makes the hosted qualification result auditable; it remains qualification
evidence and cannot close the production gates below.

## Production gate ledger

| Gate | Status | Required exit evidence |
| --- | --- | --- |
| P0 — scope, support matrix, SLO/RPO/RTO and ownership | Open; config policy implemented | Named production topology, supported FoundationDB/RustFS/metadata/client versions, traffic envelope, SLOs, RPO/RTO, on-call owner, rollback authority, and approved non-goals |
| P1 — production FoundationDB topology and rehearsal | Not started; config policy requires durable/shared-provider shape | Repeatable multi-process/HA topology, replication and storage policy, pinned images, network policy, capacity limits, clean-client readiness, restart/failover and deployment rehearsal |
| P2 — metadata and block-provider matrix | Qualification only | Explicit production provider choices and supported combinations; secure staging runs for FoundationDB metadata, RustFS/AWS-compatible blocks, Node, Rust CLI and native clients |
| P3 — authority identity, ACLs, TLS and secret lifecycle | Not started; config policy checks shared-provider mode, HTTPS blocks and external references | One write-capable authority identity per prefix; read-only consumer identities; actual tenant/credential/ACL negative test proving consumers cannot publish or overwrite; secret injection/rotation, TLS policy and redacted logs |
| P4 — replicated durability and storage failure protection | Not started | Backup/replication and sync policy review; node, disk, process and power-loss boundaries; integrity checks for metadata, fences, authority samples and immutable blocks; recovery evidence on the intended storage class |
| P5 — fencing, ambiguous commit and failover recovery | Partial qualification; local and hosted durable restart evidence | Secure multi-node tests covering stale writers, lease expiry/renewal, maybe-committed reconciliation, network delay/partition, authority loss, reviewed failover and no split-brain publication |
| P6 — backup, restore and disaster recovery | Not started | Consistent metadata/authority/block backup definition, encrypted retention, clean-environment restore, hash/revision verification, measured RPO/RTO and provider/region-loss procedure |
| P7 — observability, alerts and runbooks | Not started | Metrics and alerts for cluster health, authority publication age/errors, lease-fence/ESTALE, transaction retry/maybe-committed EIO, block errors, latency, capacity and cleanup/space pressure; tested on-call runbook |
| P8 — load, capacity, soak and cost envelope | Harness + five-round local and hosted qualification; measured latency marker verified; production evidence open | The opt-in harness supports bounded repeated real FoundationDB/RustFS composition rounds with unique prefixes and cleanup, and emits p50/p95/p99 operation-latency and throughput markers; latest hosted run `35652638242` recorded five isolated durable composition rounds at revision `c6f0039` with base `operations=15 p50_us=7712 p95_us=228723 p99_us=228723 total_ms=353 throughput_ops_per_sec=42.41`; soak p95/p99 ranged from 27,598µs to 35,139µs and throughput from 77.45 to 95.05 ops/s; production-shaped workload, concurrency, duration, retry/error budget, resource growth, safe capacity and scaling triggers are still required |
| P9 — upgrade, rollback and compatibility | Not started | Forward/backward keyspace and configuration compatibility, rolling provider/client upgrade, failed-upgrade rollback, retained-data downgrade boundary, lockfile/image/artifact provenance |
| P10 — security, privacy, tenancy and audit | Not started | Threat-model review, prefix/tenant isolation, data classification, encryption, audit retention, dependency/image review, abuse/rate limits, closed findings or approved exceptions |
| P11 — native client, mount and platform support | Qualification only; hosted Linux Node/CLI/native FUSE evidence is green at `35652638242` for revision `c6f0039` | An explicit advertised platform matrix; clean-install, native FDB client, Node/CLI, FUSE/NFS/FSKit lifecycle, concurrent access, restart/recovery and packaging/signing evidence for every advertised platform |
| P12 — release packaging, CI promotion and canary | Qualification CI plus policy gate | Locked and signed artifacts, SBOM/provenance, protected environment approvals, production-like canary, holdback, promotion checks, rollback automation and retained evidence packet |
| P13 — incident, failover and recovery rehearsal | Not started | Timed operator exercises for authority loss, cluster loss, stale client, storage exhaustion, bad deploy, credential expiry and restore; paging, runbook, integrity and RTO evidence |
| P14 — final launch audit and go/no-go | Not started | One-revision audit of P0–P13, known-limitations record, release-owner decision, canary exit evidence and explicit GO or NO-GO |

No P0–P14 gate is currently terminally accepted. A production gate may move to
complete only when the exit evidence is from the named production-like
environment and the owner records the result; implementation tests alone do
not close operations, security, native, or release gates.

## Credential-free production configuration policy

The repository now includes
`scripts/verify-w07-production-config.mjs` and positive/negative fixtures under
`tests/foundationdb/`. The gate checks the configuration shape that the
configuration-driven CLI accepts: split-store FoundationDB metadata, an
absolute cluster-file path, `durable: true`, the protected `shared-provider`
authority mode and non-empty stable prefixes; it also requires durable HTTPS
RustFS blocks and external `R2_ACCESS_KEY_ID`/`R2_SECRET_ACCESS_KEY`
references. It rejects placeholders, inline secrets, remote plaintext HTTP,
authority modes that are not suitable for independent production writers and
other unsafe shapes.

Run it without credentials or network access:

```sh
node scripts/verify-w07-production-config.mjs \
  tests/foundationdb/production-config-policy.json
```

The dedicated hosted qualification workflow runs the positive fixture plus
the inline-secret and invalid-lease-TTL negative fixtures. A
`W07_PRODUCTION_CONFIG_POLICY_PASS` line is only static deployment-shape
evidence; it now requires an explicit positive lease TTL bounded to 24 hours,
but the verifier does not open FoundationDB or RustFS, cannot prove
ACLs, certificate trust, replication, backups, capacity, monitoring or owner
approval, and does not change the **NO-GO** decision.

The same workflow also runs
`scripts/verify-w07-rollout-ledger.mjs`. Its
`W07_ROLLOUT_LEDGER_POLICY_PASS` marker is an internal consistency guard: while
the rollout is **NO-GO**, it requires W07.7 and all seven nested production
gates to remain open, and it requires the runbook's external-drill boundary.
It does not check any production environment and cannot close a gate by itself.
The adjacent `scripts/test-w07-rollout-ledger.mjs` step exercises the current
NO-GO ledger plus premature-GO, missing-nested-gate and missing-drill-boundary
cases, including a synthetic complete-GO document set. These are regression
tests for the tracking control only; they do not create production evidence.

The real composition test now emits
`FOUNDATIONDB_LATENCY_PASS workload=composition` with operation count,
p50/p95/p99 microsecond latency, total duration and aggregate operation
throughput. Hosted run
[`35601357569`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35601357569)
at revision `622d0d1` recorded
`operations=11 p50_us=9893 p95_us=28865 p99_us=28865 total_ms=124
throughput_ops_per_sec=88.62` on the bounded composition workload. The
guarded-authority follow-up
[`35606741719`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35606741719)
at revision `4aadbb1` recorded
`operations=11 p50_us=11702 p95_us=26193 p99_us=26193 total_ms=120
throughput_ops_per_sec=91.24` and passed the service-restart path. The
five-round hosted follow-up
[`35608336345`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35608336345)
at revision `976725e` recorded
`operations=11 p50_us=9990 p95_us=85061 p99_us=85061 total_ms=167
throughput_ops_per_sec=65.76`, passed five isolated soak rounds and the
service-restart path. These are measured qualification results, not
production-shaped load, capacity, retry/error-budget or scaling evidence.

The corrected five-round hosted run
[`35620006731`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35620006731)
at revision `aa3dae3` recorded `operations=15 p50_us=9593 p95_us=48162
p99_us=48162 total_ms=171 throughput_ops_per_sec=87.35`, passed the native
Linux FUSE/CLI lifecycle and retained a `qualification-pass` summary artifact.
This supersedes the earlier native-mount blocker for the tested revision only;
it remains bounded qualification, not production-shaped load or capacity
evidence.

The operational execution template is
[`W07-operations-runbook.md`](W07-operations-runbook.md). It defines the
admission checks, failure responses, backup/restore procedure, timed D01–D09
drills, observability handoff and evidence fields. Every drill remains
`Not executed — external production gate` until it runs against the named
production-like environment.

## FoundationDB deployment contract

The production lease path must use `with_production_lease_oracle` with a
protected `FoundationDbSharedLeaseOracle` (or an application-owned oracle that
declares `LeaseAuthorityKind::SharedProvider`). The default, development,
system-clock, and persisted single-authority paths remain fail-closed or
development-only for independent writers.

The deployment must prove and continuously enforce all of the following:

- exactly one write-capable authority identity owns each authority prefix;
- storage workers have read-only access to the authority record and cannot
  publish or overwrite it;
- the authority host has a monitored clock-skew bound and publishes more often
  than the shortest lease TTL;
- the authority republishes after restart before consumers are admitted;
- authority loss fails closed, or uses an explicitly reviewed failover
  authority; and
- credentials, cluster files, certificates, and rotation material are injected
  at runtime and never committed or printed.

The provider now exposes a validated `LeasePublicationPolicy` plus
`publish_system_now_ms_with_policy` and its explicit-sample counterpart. The
policy requires a publication cadence shorter than the lease TTL and a
forward-jump bound no larger than that TTL; publication fails closed before
writing when one proposed authority-time advance exceeds the bound. Unit and
real-cluster authority paths exercise the bounded publication guard. The
policy is an implementation safety boundary, not evidence of the production
host's clock monitor, identity policy, scheduler cadence or failover
procedure.

These are deployment controls. The library API and a shared FoundationDB
`Database` handle cannot prove the credential/tenant/ACL boundary by
themselves, so a production negative test with the actual identities is
required.

## Rollout sequence

1. Close P0 with the production target, support matrix, SLO/RPO/RTO, owners and
   non-goals.
2. Rehearse the secure, replicated FoundationDB/RustFS topology and record the
   exact images, cluster configuration, identity policy and storage class.
3. Run the locked provider, composition, native, failure, backup/restore,
   observability, load/soak and security gates using the procedures in
   [`W07-operations-runbook.md`](W07-operations-runbook.md) against that
   staging topology.
4. Deploy one canary with a holdback. Record the artifact digest, configuration
   digest, authority identity, smoke result, metrics, cleanup and rollback
   result before expanding.
5. Promote in stages only after the approved SLO window and alert/runbook
   checks pass. Keep the prior artifact and restore procedure available.
6. On rollback, stop new writers, preserve the metadata/authority snapshot and
   block prefix, restore the last known-good artifact/topology, verify reads,
   fences and ownership, then resume only after the release owner approves.

## Evidence and blocker rules

- Local Docker is useful qualification evidence but is not production
  authentication, power-loss durability, capacity, or operator evidence.
- A hosted FoundationDB/RustFS job is revision-specific provider evidence; an
  active, queued, skipped, cancelled, or failed job is not a pass.
- FoundationDB service restart, RustFS restart, authority republish, fresh
  client reopen, native CLI, Node, and macOS/Linux acceptance are separate
  boundaries and must retain their own run IDs.
- Missing credentials, infrastructure, certificates, monitoring, native
  runners, security review, or approvers are explicit blockers. They must not
  be replaced with synthesized configuration or a weaker local test.
- Every production result must retain redacted logs/markers, cleanup status,
  rollback outcome, and the exact revision tested. Later mainline changes do
  not retroactively change an earlier result.

The open gates above are the required follow-up to the demo. The release
decision remains **NO-GO** until the evidence packet and owner sign-off close
the applicable gates for the advertised production scope.
