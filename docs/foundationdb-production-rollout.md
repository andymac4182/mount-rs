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
| [Hosted run `35655579378`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378), job `106518268071`, revision `0354116`, Ubuntu 24.04 | **PASS — latest mainline qualification with the lease-publication policy implementation** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; the positive production-config fixture passed with `lease_ttl=explicit-bounded`, while inline-secret and unsafe-TTL fixtures failed closed; durable three-server FoundationDB/RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=7227 p95_us=81981 p99_us=81981 total_ms=191 throughput_ops_per_sec=78.17`; five isolated soak rounds with p95/p99 from 188,168µs to 737,468µs and throughput from 9.40 to 39.58 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance/evidence validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `03541161311983f497983ef0de1bcb09b958b2e4`, completed in 11m06s. Artifact `foundationdb-production-qualification-35655579378-1` has SHA-256 `dff8fb664a62d70e7736098d842cc9d976e942358c203c75cdb028e9ea42c177`; its provenance records source revision `03541161311983f497983ef0de1bcb09b958b2e4`, run `35655579378`, attempt `1`, and runner `GitHub Actions 1000022432`. This remains bounded qualification; the base and soak latency variability is qualification telemetry, not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval remain open. |
| [Hosted run `35657842924`](https://github.com/andymac4182/mount-rs/actions/runs/35657842924), job `106525768365`, revision `87a500b1`, Ubuntu 24.04 | **PASS — latest mainline qualification with lease-publication marker enforcement** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` passed; the positive production-config fixture passed with `lease_ttl=explicit-bounded`, while inline-secret and unsafe-TTL fixtures failed closed; the required `FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000` marker was emitted; durable three-server FoundationDB/RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9426 p95_us=29053 p99_us=29053 total_ms=157 throughput_ops_per_sec=95.22`; five isolated soak rounds with p95/p99 from 27,345µs to 29,175µs and throughput from 85.77 to 94.49 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; service restart; schema-2 provenance/evidence validation and retained artifact passed. | Terminal hosted Linux qualification for the tested revision `87a500b13bba305e7a7a5c80c328395d80eb772b` only, completed in 11m50s. Artifact `foundationdb-production-qualification-35657842924-1` has SHA-256 `dc37b10f82ecd1c4c4513f4cf3ecc9a95e2ef37ccdadf0f4d69e56d30620846b`; its provenance records source revision `87a500b13bba305e7a7a5c80c328395d80eb772b`, run `35657842924`, attempt `1`, and runner `GitHub Actions 1000022644`. This remains bounded qualification; the base and soak telemetry is not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, or release approval remain open. |
| [Hosted attempt `35662679910`](https://github.com/andymac4182/mount-rs/actions/runs/35662679910), job `106541321674`, revision `8c16299e`, Ubuntu 24.04 | **NO HOSTED PASS** — rollout-ledger, policy, verifier, dependency, N-API and FUSE steps passed, but the composed FoundationDB/RustFS step stopped before provider execution because `tests/foundationdb/Cargo.lock` omitted the `sha2` edge now required by `mount-rs-r2` | Reproducibility/lockfile failure, not FoundationDB or RustFS runtime evidence. The one-line repair was published in `ccd3f671`; this run contributes no hosted qualification pass. |
| [Hosted run `35663914822`](https://github.com/andymac4182/mount-rs/actions/runs/35663914822), job `106545260051`, revision `147c53a8`, Ubuntu 24.04 | **PASS — mainline qualification after the FoundationDB qualification-lock repair** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` and `W07_QUALIFICATION_LOG_TEST_PASS cases=4` passed; the positive production-config fixture passed while inline-secret and unsafe-TTL fixtures failed closed; `FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000` was emitted; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9564 p95_us=39516 p99_us=39516 total_ms=163 throughput_ops_per_sec=91.71`; five isolated soak rounds with p95/p99 from 21,487µs to 24,210µs and throughput from 91.97 to 106.58 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. | Terminal hosted Linux qualification for the tested revision `147c53a8c1b098dc09e3c82958fbd013c6420ccc`, completed in 11m43s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023111`, and retained artifact `foundationdb-production-qualification-35663914822-1` with SHA-256 `098ab3a4bb1fd6ae84971cebbb67baf2e50d9cb271accdaa9351ec28e49c3042`. This remains bounded hosted Linux qualification; production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted run `35665547966`](https://github.com/andymac4182/mount-rs/actions/runs/35665547966), job `106550374750`, revision `1b98ed04`, Ubuntu 24.04 | **PASS — fresh mainline qualification from the published tip** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` and `W07_QUALIFICATION_LOG_TEST_PASS cases=4` passed; the positive production-config fixture passed while inline-secret and unsafe-TTL fixtures failed closed; `FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000` was emitted; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=26199 p95_us=321373 p99_us=321373 total_ms=1273 throughput_ops_per_sec=11.78`; five isolated soak rounds with p95/p99 from 16,527µs to 34,188µs and throughput from 53.87 to 136.00 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. | Terminal hosted Linux qualification for the tested revision `1b98ed0476fec5403fb5389503e69960b9661349`, completed in 9m47s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023256`, and retained artifact `foundationdb-production-qualification-35665547966-1` with SHA-256 `f37951fa1e20ce86bdc031d9529c1ace3506b03a9f2027b72b7186118eccbd02`. This remains bounded hosted Linux qualification; the base latency outlier is qualification telemetry rather than production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted run `35666991514`](https://github.com/andymac4182/mount-rs/actions/runs/35666991514), job `106554849953`, revision `5b9af323`, Ubuntu 24.04 | **PASS — current-tip qualification after concurrent mainline reintegration** — the NO-GO consistency guard passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6` and `W07_QUALIFICATION_LOG_TEST_PASS cases=4` passed; the positive production-config fixture passed while inline-secret and unsafe-TTL fixtures failed closed; `FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000` was emitted; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=8101 p95_us=35120 p99_us=35120 total_ms=141 throughput_ops_per_sec=105.73`; five isolated soak rounds with p95/p99 from 20,100µs to 21,587µs and throughput from 107.93 to 119.84 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. | Terminal hosted Linux qualification for the tested revision `5b9af323f1e6bf03725842f1b696d1ff02028fb9`, completed in 11m23s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023347`, and retained artifact `foundationdb-production-qualification-35666991514-1` with SHA-256 `7313f7ce6f7fb188d84d79a5c5d98df01a1319f071208f1058c5ab87a72acb12`. This remains bounded hosted Linux qualification; production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted run `35668646531`](https://github.com/andymac4182/mount-rs/actions/runs/35668646531), job `106559918401`, revision `564d0949`, Ubuntu 24.04 | **PASS — latest mainline qualification with the machine-readable production-evidence packet guard** — the NO-GO rollout ledger passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`, `W07_QUALIFICATION_LOG_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9720 p95_us=51773 p99_us=51773 total_ms=180 throughput_ops_per_sec=83.15`; five isolated soak rounds with p95/p99 from 17,332µs to 31,275µs and throughput from 102.88 to 129.93 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. | Terminal hosted Linux qualification for the tested revision `564d0949bd13d1dab5e2ff226f41e1bb554e4622`, completed in 10m27s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023513`, and retained artifact `foundationdb-production-qualification-35668646531-1` (ID `10670422714`) with SHA-256 `f6f663ff26bb925cf601353a8e2c4e7d48bf15fbaeb7ba20163e32eb3c8a3897`. This remains bounded hosted Linux qualification; the base latency result is qualification telemetry rather than production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted run `35669850643`](https://github.com/andymac4182/mount-rs/actions/runs/35669850643), job `106563637562`, revision `618ee5fe`, Ubuntu 24.04 | **PASS — exact published-tip qualification with the machine-readable production-evidence packet guard** — the NO-GO rollout ledger passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`, `W07_QUALIFICATION_LOG_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3704 p95_us=31810 p99_us=31810 total_ms=88 throughput_ops_per_sec=169.44`; five isolated soak rounds with p95/p99 from 13,087µs to 14,017µs and throughput from 203.80 to 215.46 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. | Terminal hosted Linux qualification for the exact published revision `618ee5fe8451fd77387ead066396b0dab4dec25b`, completed in 11m37s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023659`, and retained artifact `foundationdb-production-qualification-35669850643-1` (ID `10671065480`) with SHA-256 `0def7863ba535c8b210865cf0cc0c13770f6fde268be4db9e4da6669e6abbd5c`. This remains bounded hosted Linux qualification; the five-round result is not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted run `35671492720`](https://github.com/andymac4182/mount-rs/actions/runs/35671492720), job `106568742563`, revision `9460a62`, Ubuntu 24.04 | **PASS — workflow-artifact retention qualification** — the NO-GO rollout ledger passed; `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`, `W07_QUALIFICATION_LOG_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; durable three-server FoundationDB `double`/SSD plus RustFS composition; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3846 p95_us=38409 p99_us=38409 total_ms=96 throughput_ops_per_sec=155.90`; five isolated soak rounds with p95/p99 from 13,005µs to 14,243µs and throughput from 199.39 to 214.47 ops/s; live Node/N-API; native Linux CLI/FUSE mount and reopen; FoundationDB service restart/authority republish; RustFS restart/fault recovery and integration all passed. The retained artifact was downloaded and verified to contain the qualification log, summary and `docs/W07-production-evidence.json`, whose seven gates were all `open`. | Terminal hosted Linux qualification for the tested revision `9460a62dc3350a6d049fbad21a4fcf5a5c9294e3`, completed in 12m21s. The run used RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, Linux `amd64`, runner `GitHub Actions 1000023867`, and retained artifact `foundationdb-production-qualification-35671492720-1` (ID `10672131196`) with SHA-256 `815dfecadab366a9fca9a7501c4a36d771324d8f71b521e0e8082261cbe431c3`. This remains bounded hosted Linux qualification; the five-round result is not production capacity evidence, and production identity/ACL/TLS, backup/restore, production load/capacity, multi-day duration, failover, macOS, complete platform/package coverage, observability and release approval remain open. |
| [Hosted attempt `35673343510`](https://github.com/andymac4182/mount-rs/actions/runs/35673343510), job `106574502673`, revision `9ea7e493`, Ubuntu 24.04 | **NO HOSTED PASS** — all W07 policy, packet, dependency and build preconditions passed; the bounded workload completed its 400 iterations at 64-way concurrency, but the initial wrapper-required 1,000-IOPS floor measured 29.13 IOPS and failed closed with `IOPS_TARGET_NOT_MET`. | This is a workload-profile correction signal, not provider or production acceptance. The retained artifact is not a pass record; the corrected W07 profile uses a structural non-zero completion floor and retains the measured rate for a later owner-approved capacity comparison. |
| [Hosted run `35674506961`](https://github.com/andymac4182/mount-rs/actions/runs/35674506961), job `106578027627`, exact revision `a7eb3be47e3e5edfd4b3e8a516d1b8cbceaab199`, Ubuntu 24.04/Linux amd64 | **PASS — corrected bounded workload and terminal production-shaped qualification lane** — the NO-GO rollout-ledger guard passed with `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`; `W07_QUALIFICATION_LOG_TEST_PASS cases=5`, `W07_WORKLOAD_ARTIFACT_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; durable three-server FoundationDB `double`/SSD plus RustFS composition, live Node/N-API, native Linux CLI/FUSE mount and reopen, FoundationDB service restart/authority republish, RustFS restart/fault recovery and integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=2617 p95_us=88362 p99_us=88362 total_ms=134 throughput_ops_per_sec=111.17`; five isolated soak rounds had p95/p99 from 13,388µs to 15,041µs and throughput from 206.94 to 236.23 ops/s; `FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=70.83` passed with 400 successful writes, reads and deletes, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `a7eb3be47e3e5edfd4b3e8a516d1b8cbceaab199`, completed in 12m46s. The retained artifact `foundationdb-production-qualification-35674506961-1` has ID `10672253304` and SHA-256 `e558dea65ac63d87873d36e6bff09daa70f824310309c1c1e6dd87c109b7b5b1`; its provenance records runner `GitHub Actions 1000024242`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The artifact's packet remains `NO-GO` with all seven production gates open and zero evidence records; this is bounded hosted Linux qualification, not production capacity or rollout acceptance. |
| [Hosted run `35675987457`](https://github.com/andymac4182/mount-rs/actions/runs/35675987457), job `106582524993`, exact revision `1670ceba81b24c1ed39b8ab396671324c7f28193`, Ubuntu 24.04/Linux amd64 | **PASS — current published-tip requalification** — the NO-GO rollout-ledger guard passed with `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`; `W07_QUALIFICATION_LOG_TEST_PASS cases=5`, `W07_WORKLOAD_ARTIFACT_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; durable three-server FoundationDB `double`/SSD plus RustFS composition, live Node/N-API, native Linux CLI/FUSE mount and reopen, FoundationDB service restart/authority republish, RustFS restart/fault recovery and integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3394 p95_us=43146 p99_us=43146 total_ms=94 throughput_ops_per_sec=158.97`; five isolated soak rounds had p95/p99 from 12,701µs to 14,187µs and throughput from 218.16 to 225.92 ops/s; `FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=64.73` passed with 400 successful writes, reads and deletes, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `1670ceba81b24c1ed39b8ab396671324c7f28193`, completed in 11m57s. The retained artifact `foundationdb-production-qualification-35675987457-1` has ID `10673640911` and SHA-256 `0cb38b97b121ef8b6b69a1c4ef76d112bb24ba716b19eb6153ee52b13a3cc694`; its provenance records runner `GitHub Actions 1000024436`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The artifact's packet remains `NO-GO` with all seven production gates open and zero evidence records; this is bounded hosted Linux qualification, not production capacity or rollout acceptance. |
| [Hosted run `35680085315`](https://github.com/andymac4182/mount-rs/actions/runs/35680085315), job `106594973866`, exact revision `71972b28ca7ae561325342ddf466c8353546547a`, Ubuntu 24.04/Linux amd64 | **PASS — heartbeat-corrected published-tip requalification** — the NO-GO rollout-ledger guard passed with `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`; `W07_QUALIFICATION_LOG_TEST_PASS cases=5`, `W07_WORKLOAD_ARTIFACT_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; the bounded authority heartbeat emitted `FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING` at a 30-second interval with a 120-second maximum forward-jump bound; durable three-server FoundationDB `double`/SSD plus RustFS composition, live Node/N-API, native Linux CLI/FUSE mount and reopen, FoundationDB service restart/authority republish, RustFS restart/fault recovery and integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=2589 p95_us=28220 p99_us=28220 total_ms=68 throughput_ops_per_sec=218.60`; five isolated soak rounds passed; `FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=202.31` passed with 400 successful writes, reads and deletes, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `71972b28ca7ae561325342ddf466c8353546547a`, completed in 10m05s. The retained artifact `foundationdb-production-qualification-35680085315-1` has ID `10675087310` and SHA-256 `f9b47245760b4d03ddeeb9461b51bb1f29c8ce913920cbc629309ba024d2ecc5`; its provenance records runner `GitHub Actions 1000024955`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The artifact's packet remains `NO-GO` with all seven production gates open and zero evidence records; the qualification heartbeat is not production monitoring or deployment-credential evidence, and production authority, failover, capacity, observability, platform/package and owner gates remain open. |
| [Hosted run `35681642584`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35681642584), job `106599666288`, exact revision `113b13751acc4885ca5916a7f70fe109e1dbadd4`, Ubuntu 24.04/Linux amd64 | **PASS — telemetry-qualified current-tip requalification** — the NO-GO rollout-ledger guard passed with `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`; `W07_QUALIFICATION_LOG_TEST_PASS cases=5`, `W07_WORKLOAD_ARTIFACT_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; the independent authority heartbeat emitted `FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING` at a 30-second interval with a 120-second maximum forward-jump bound; the hosted shared-authority path emitted `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3 publication_successes=3 publication_failures=0 reader_attempts=5 reader_successes=4 reader_failures=1 last_published_time_ms=2030001 last_observed_time_ms=2030001`; durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, five-round soak and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3841 p95_us=321472 p99_us=321472 total_ms=495 throughput_ops_per_sec=30.29`; five isolated soak rounds passed with p95/p99 from 10,229µs to 10,887µs and throughput from 257.61 to 295.85 ops/s; `FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=441.76` passed with 400 successful writes, reads and deletes, 1,200 successful lifecycle operations, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `113b13751acc4885ca5916a7f70fe109e1dbadd4`, completed in 10m51s. The retained artifact `foundationdb-production-qualification-35681642584-1` has ID `10675750181` and SHA-256 `5305418e1b253bdc621815311eaf58f80234ef193253fe4b9f23e3c0d938b23c`; its provenance records runner `GitHub Actions 1000025169`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The artifact's packet remains `NO-GO` with all seven production gates open and zero evidence records; the stats snapshots and test heartbeat are qualification implementation evidence, not production collector, ACL, failover, capacity or owner evidence. |
| [Hosted run `35683503910`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35683503910), job `106606452492`, exact revision `0e0327535fbcda0b1544400c00e938911a8ed6ad`, Ubuntu 24.04/Linux amd64 | **PASS — telemetry-verifier current-tip requalification** — the NO-GO rollout-ledger guard passed with `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`; `W07_QUALIFICATION_LOG_TEST_PASS cases=7`, `W07_WORKLOAD_ARTIFACT_TEST_PASS cases=4`, `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO gates=7 closed=0 evidence_records=0 require_go=false` and `W07_PRODUCTION_EVIDENCE_TEST_PASS cases=12` passed; production-config positive/negative fixtures and the lease-publication policy passed; the independent authority heartbeat emitted `FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING` at a 30-second interval with a 120-second maximum forward-jump bound; the hosted shared-authority path emitted `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3 publication_successes=3 publication_failures=0 reader_attempts=5 reader_successes=4 reader_failures=1 last_published_time_ms=2030001 last_observed_time_ms=2030001`; durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, five-round soak and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3281 p95_us=29220 p99_us=29220 total_ms=80 throughput_ops_per_sec=185.99`; five isolated soak rounds passed with p95/p99 from 13,491µs to 22,635µs and throughput from 176.18 to 210.55 ops/s; `FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=368.57` passed with 400 successful writes, reads and deletes, 1,200 successful lifecycle operations, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `0e0327535fbcda0b1544400c00e938911a8ed6ad`, completed in 12m44s. The retained artifact `foundationdb-production-qualification-35683503910-1` has ID `10675579408` and SHA-256 `ebe4ef52c09fa50e1395a0d672e1db715c9d60deb120f43c85499750ba6c6362`; its provenance records runner `GitHub Actions 1000025458`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The artifact's packet remains `NO-GO` with all seven production gates open and zero evidence records; the stats snapshots, test heartbeat and stricter verifier are qualification implementation evidence, not production collector, ACL, failover, capacity or owner evidence. |
| [Hosted attempt `35685846631`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35685846631), job `106612350173`, exact revision `e9e2d30c6be06a5aa1f0e81e39fb5a0450c77a78`, Ubuntu 24.04/Linux amd64 | **NO HOSTED PASS** — all rollout, qualification, workload, production-evidence, configuration, prerequisite, N-API and Linux FUSE preconditions passed; the durable FoundationDB/RustFS step stopped with `cannot update the lock file /workspace/tests/foundationdb/Cargo.lock because --locked was passed to prevent this` after the provider feature graph gained its `tracing` dependency | Standalone qualification-workspace lockfile reproducibility failure, not FoundationDB, RustFS, workload, soak or production acceptance. This attempt contributes no runtime evidence; the correction adds the missing `tracing` edge to `tests/foundationdb/Cargo.lock` and requires a fresh exact-tip hosted requalification |
| [Hosted run `35686583792`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35686583792), job `106614608451`, exact revision `7ba3998a6f6a88b7eedefd98cba9262980d24ce9`, Ubuntu 24.04/Linux amd64 | **PASS — repaired exact-tip telemetry qualification** — the NO-GO rollout-ledger guard passed; all seven qualification-log, four workload-artifact and twelve production-evidence packet regression cases passed; production-config positive/negative fixtures and the lease-publication policy passed; the independent authority heartbeat emitted at a 30-second interval with a 120-second maximum forward-jump bound; the shared-authority path emitted `FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3 publication_successes=3 publication_failures=0 reader_attempts=5 reader_successes=4 reader_failures=1 last_published_time_ms=2030001 last_observed_time_ms=2030001`; durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, five-round soak and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3133 p95_us=32028 p99_us=32028 total_ms=79 throughput_ops_per_sec=187.57`; five soak rounds had p95/p99 from 12,020µs to 12,490µs and throughput from 227.19 to 235.58 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 166.29 IOPS, zero timeouts and zero cleanup failures. | Terminal hosted Linux qualification for exact revision `7ba3998a6f6a88b7eedefd98cba9262980d24ce9`, completed in 12m35s. The retained artifact `foundationdb-production-qualification-35686583792-1` has ID `10676804103` and SHA-256 `afccc878e46a096c6153f04fac017f3df19d37d147a2e83629fa14616f850e30`; its provenance records runner `GitHub Actions 1000025921`, Node 24.21.0, RustFS `1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`, FoundationDB image `sha256:0d19ffb2aa154259f2da0f8941e1549b4c331f47008af5755a83f6247edeabf1`, and split-store provider `mount-rs-split-foundationdb-r2`. The retained packet is still `NO-GO` with all seven production gates open and zero evidence records; this is repaired hosted implementation qualification, not production collector, ACL, failover, capacity or owner evidence. |
| Local evidence-integrity gate, 2026-09-22, exact revision `419fbf217b5c40e0e62371e9b41badc07582e3d5` | **PASS** — `node scripts/test-w07-qualification-log.mjs` passed 8 cases; the stricter verifier independently revalidated the retained `35686583792` log with six latency samples (base plus five soak rounds), and the summary now preserves every parsed sample | Evidence-integrity qualification only. The hosted run predates the eighth regression case; the required fresh current-tip execution is recorded at `35688516329` below. This does not close production identity, recovery, capacity, observability, platform or owner gates. |
| [Hosted run `35688516329`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35688516329), job `106620365076`, exact revision `98bccc459a41f5f6d631c5b9fc93701a166de50a`, Ubuntu 24.04/Linux amd64 | **PASS — current-tip evidence-integrity qualification** — the eight-case qualification-log verifier passed; the hosted log contains six validated latency samples (base plus five soak rounds), durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=3301 p95_us=33915 p99_us=33915 total_ms=84 throughput_ops_per_sec=178.01`; five soak rounds had p95/p99 from 12,357µs to 12,989µs and throughput from 218.28 to 231.29 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 374.56 IOPS, zero timeouts and zero cleanup failures | Terminal hosted Linux qualification completed in 12m15s. Artifact `foundationdb-production-qualification-35688516329-1` has ID `10677462512` and SHA-256 `1caac0c22279c3f37544eddab8671ae5ad17dde6f0dbee7e23b699dacbe4694e`; provenance records runner `GitHub Actions 1000026164`, schema-2 qualification summary, pinned Node/RustFS/FoundationDB images and provider `mount-rs-split-foundationdb-r2`. The retained packet remains `NO-GO` with all seven production gates open and zero evidence records; this is stronger hosted implementation qualification, not production identity/ACL, backup/recovery, capacity, observability, platform or release-owner evidence. |
| [Hosted attempt `35690122405`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35690122405), job `106625124766`, exact revision `07e455cd48b2962e2d06290bad35379b575d521e`, Ubuntu 24.04/Linux amd64 | **NO HOSTED PASS** — durable FoundationDB/RustFS, workload and all earlier policy gates passed, but final qualification-marker validation failed with `missing=provenance-marker-missing` because the first config-policy `tee` overwrote the run-bound provenance marker | Workflow integration failure, not provider acceptance. The append-only repair is published in `6b8639aa` and required a fresh exact-tip hosted requalification; this attempt contributes no qualification pass. |
| [Hosted run `35691106828`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35691106828), job `106628088824`, exact revision `6b8639aa618f301e8424b5619bc1dda559a75948`, Ubuntu 24.04/Linux amd64 | **PASS — current-tip run-bound provenance qualification** — the 11-case qualification-log regression suite passed; the raw log contains a provenance marker matching repository, workflow, ref, SHA, run, attempt and runner; the independent 30-second authority heartbeat and shared-authority stats passed; durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=2680 p95_us=13759 p99_us=13759 total_ms=58 throughput_ops_per_sec=254.79`; five soak rounds had p95/p99 from 12,701µs to 13,784µs and throughput from 241.60 to 248.00 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 259.35 IOPS, zero timeouts and zero cleanup failures | Terminal hosted Linux qualification completed in 12m10s. Artifact `foundationdb-production-qualification-35691106828-1` has ID `10678891553` and SHA-256 `f26ae7cb81dd298cc78153f27e573bdb1096a6ae038cc8f42a40d7cbeeab3c17`; provenance records runner `GitHub Actions 1000026510`, schema-2 summary and provider `mount-rs-split-foundationdb-r2`. The retained packet remains `NO-GO` with all seven production gates open and zero evidence records; this is stronger hosted implementation qualification, not production identity/ACL, backup/recovery, capacity, observability, platform or release-owner evidence. |
| [Hosted run `35692674674`](https://github.com/andymac4182/mount-rs/actions/runs/35692674674), job `106632794791`, exact revision `5a6d6507c6deac160f54a246a2d715c05fc35268`, Ubuntu 24.04/Linux amd64 | **PASS — extended current-tip run-bound provenance qualification** — the 11-case qualification-log regression suite passed; the raw log provenance marker matched repository, workflow, ref, SHA, run, attempt and runner; the independent 30-second authority heartbeat and shared-authority stats passed; durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen and RustFS integration all passed; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=4950 p95_us=439603 p99_us=439603 total_ms=700 throughput_ops_per_sec=21.41`; ten isolated soak rounds had p95/p99 from 70,204µs to 236,130µs and throughput from 18.19 to 96.22 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 110.18 IOPS, zero timeouts and zero cleanup failures | Terminal hosted Linux qualification completed in 11m26s. Artifact `foundationdb-production-qualification-35692674674-1` has ID `10679154003` and SHA-256 `44beef451832c307a53571bb86f92293b70c302d0add5104152059ed4c41d2ec`; provenance records runner `GitHub Actions 1000026766`, schema-2 summary with eleven latency samples (base plus ten soak rounds), and provider `mount-rs-split-foundationdb-r2`. The retained packet remains `NO-GO` with all seven production gates open and zero evidence records; this is extended hosted implementation qualification, not production identity/ACL, backup/recovery, capacity, observability, platform or release-owner evidence. |
| [Hosted run `35698630720`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35698630720), Linux job `106651101783`, macOS job `106651102065`, aggregate job `106654577928`, exact revision `a19d37b61fdbb769102df15714cc88a3ff5eea91`, Ubuntu 24.04/Linux amd64 plus `macos-latest` | **PASS — terminal cross-platform qualification packet** — the Linux policy/provenance, durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen, RustFS integration, 30-second/120-second heartbeat, shared stats, ten-round soak and bounded workload all passed; macOS FoundationDB provider/test, CLI `native_lifecycle`, and N-API `foundationdb` feature compilation passed; the always-run aggregate downloaded both platform artifacts and emitted `W07_PLATFORM_QUALIFICATION_PASS`; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=2469 p95_us=25063 p99_us=25063 total_ms=71 throughput_ops_per_sec=208.39`; ten isolated soak rounds had p95/p99 from 13,399µs to 13,906µs and throughput from 225.29 to 258.64 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 467.15 IOPS, zero timeouts and zero cleanup failures | Terminal hosted qualification completed with Linux, macOS and aggregate jobs green. Linux artifact `foundationdb-production-qualification-35698630720-1` has ID `10682020359` and SHA-256 `0b8ead4a8cf548322f71cf2a391b036f65ebf8412e52fc7720042487f61fcb11`; macOS artifact `foundationdb-macos-platform-35698630720-1` has ID `10680694836` and SHA-256 `19a86b0d050cdcb792e5c83e925ba0e79f9c3c72f073c21329600ed637c5b241`; aggregate artifact `foundationdb-cross-platform-qualification-35698630720-1` has ID `10681589806` and SHA-256 `a6666fe8afd143d6b525b95f917332dff6af54c1d65f913bfab44c3cc05632f8`; runner `GitHub Actions 1000027218`. The retained packet remains `NO-GO` with seven open production gates and zero evidence records; this is terminal cross-platform qualification, not live macOS service/cluster/mount, clean-install, signing/package, production capacity, identity/ACL, backup/restore, failover, observability or owner evidence. |
| [Hosted run `35700746198`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35700746198), Linux job `106657901740`, macOS job `106657901971`, aggregate job `106660728286`, exact revision `8520e362710a4b3fe00fd567cf00fcc13e64c222`, Ubuntu 24.04/Linux amd64 plus `macos-latest` | **PASS — exact-tip terminal cross-platform qualification packet** — the Linux policy/provenance, durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen, RustFS integration, 30-second/120-second heartbeat, shared stats, ten-round soak and bounded workload all passed; macOS FoundationDB provider/test, CLI `native_lifecycle`, and N-API `foundationdb` feature compilation passed; the aggregate downloaded both platform artifacts and emitted `W07_PLATFORM_QUALIFICATION_PASS`; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=1950 p95_us=24689 p99_us=24689 total_ms=61 throughput_ops_per_sec=242.82`; ten isolated soak rounds had p95/p99 from 9,727µs to 266,600µs and throughput from 47.11 to 346.34 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 323.68 IOPS, zero timeouts and zero cleanup failures | Terminal hosted qualification completed with all three jobs green. Linux artifact `foundationdb-production-qualification-35700746198-1` has ID `10682882571` and SHA-256 `88f5e05ceb926917e251cfb5d8a949ec2ec6448233e94b9496cbd7d07aafe807`; macOS artifact `foundationdb-macos-platform-35700746198-1` has ID `10682322954` and SHA-256 `76ed42b3cde607d702b93b0c5e1a1a75c0bf71da3ec7db14b18fd1d3b145bd36`; aggregate artifact `foundationdb-cross-platform-qualification-35700746198-1` has ID `10681913394` and SHA-256 `92712f0e09745a9a97345be3a55e9f86b8084b1b403415f62c8ec291ec26c82d`; runner `GitHub Actions 1000027426`. The retained packet remains `NO-GO` with seven open production gates and zero evidence records; this is exact-tip cross-platform qualification, not live macOS service/cluster/mount, clean-install, signing/package, production capacity, identity/ACL, backup/restore, failover, observability or owner evidence. |
| [Hosted run `35705886860`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35705886860), Linux job `106674581511`, macOS job `106674582039`, aggregate job `106678244742`, exact revision `ab74870c58ab768c65679ea80feb18a8f54cbe00`, Ubuntu 24.04/Linux amd64 plus `macos-latest` | **PASS — repaired exact-tip terminal cross-platform qualification with bound platform provenance** — the Linux policy/provenance, durable FoundationDB/RustFS, Node/N-API, Linux CLI/FUSE, service restart, authority republish, fresh-client reopen, RustFS integration, 30-second/120-second heartbeat, shared stats, ten-round soak and bounded workload all passed; macOS FoundationDB provider/test, CLI `native_lifecycle`, N-API `foundationdb` feature compilation and run-bound provenance passed; the repaired aggregate downloaded both platform artifacts and emitted `W07_PLATFORM_QUALIFICATION_PASS ... provenance=bound`; base `FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=2848 p95_us=13430 p99_us=13430 total_ms=57 throughput_ops_per_sec=260.46`; ten isolated soak rounds had p95/p99 from 11,732µs to 13,359µs and throughput from 241.11 to 268.03 ops/s; the bounded workload passed 400 writes, reads and deletes, 1,200 successful lifecycle operations, measured 228.83 IOPS, zero timeouts and zero cleanup failures | Terminal hosted qualification completed with all three jobs green. Linux artifact `foundationdb-production-qualification-35705886860-1` has ID `10684961250` and SHA-256 `b7243f25a5761ff33eb934dfdf3c6cb11e759152b6d89da81443f2b1a56a713d`; macOS artifact `foundationdb-macos-platform-35705886860-1` has ID `10684372277` and SHA-256 `4a6d62462416dda0708b6ffd99ae52ce922a3be9ee528a803044bad72c4890f3`; aggregate artifact `foundationdb-cross-platform-qualification-35705886860-1` has ID `10684129931` and SHA-256 `ce9efe4f961aca6f8b0906ba55b9039ebfa4690b5e864f071574fa9b8f2026b6`; Linux runner was `GitHub Actions 1000027673` and macOS runner was `GitHub Actions 1000027679`, intentionally distinct platform identities. Independent provenance, workload, platform, production-packet and rollout-ledger validators passed. The retained packet remains `NO-GO` with seven open production gates and zero evidence records; this is exact-tip cross-platform qualification, not live macOS service/cluster/mount, clean-install, signing/package, production capacity, identity/ACL, backup/restore, failover, observability or owner evidence. |
| Local source gate, 2026-09-22, exact revision `a1fe6c88` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, `./scripts/cargo-shared test --workspace --all-targets --locked`, and link-free `mount-rs-foundationdb` feature-enabled `check`/strict Clippy all passed on the current published tip | Source/provider qualification only. The all-target suite retains explicit environment-gated skips for external credentials, native mount privileges and service-backed rows; the host cannot treat this as production identity, platform, recovery, capacity, observability or release evidence. |
| Local source gate, 2026-09-22, revision `87a500b1` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, the locked all-target workspace test suite, and link-free feature-enabled FoundationDB `check`/Clippy checks all passed on the current revision. | Source and provider compilation qualification only. The full suite retains explicit environment-gated skips for external credentials/native mount harnesses; the host still cannot link/run native FoundationDB unit tests because `libfdb_c` is unavailable. Hosted Linux runtime evidence is recorded separately, and the production deployment gates remain open. |
| Local source gate, 2026-09-22, revision `57ade44` | **PASS** — `./scripts/cargo-shared fmt --all -- --check`, strict locked workspace Clippy with `-D warnings`, the locked all-target workspace test suite, and link-free feature-enabled FoundationDB `check`/Clippy checks all passed on the latest combined `origin/main` tip. | Source and provider compilation qualification only. The host still cannot link/run native FoundationDB unit tests because `libfdb_c` is unavailable; hosted Linux runtime evidence is recorded separately, and the production deployment gates remain open. |
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

The latest hosted mainline run
[`35657842924`](https://github.com/andymac4182/mount-rs/actions/runs/35657842924)
(job `106525768365`, revision `87a500b1`) completed green on Ubuntu 24.04 in
11m50s. Its retained artifact
`foundationdb-production-qualification-35657842924-1` has SHA-256
`dc37b10f82ecd1c4c4513f4cf3ecc9a95e2ef37ccdadf0f4d69e56d30620846b`.
The schema-2 summary records
`sourceRevision=87a500b13bba305e7a7a5c80c328395d80eb772b`,
`runId=35657842924`, `runAttempt=1`, runner `GitHub Actions 1000022644`,
the lease-publication policy marker
`lease_ttl_ms=120000 publication_interval_ms=30000 max_forward_jump_ms=120000`,
five soak rounds, `FOUNDATIONDB_CLI_PASS`, native restart/reopen, and base
`FOUNDATIONDB_LATENCY_PASS workload=composition operations=15 p50_us=9426
p95_us=29053 p99_us=29053 total_ms=157 throughput_ops_per_sec=95.22`.
The five soak-round p95/p99 values ranged from 27,345µs to 29,175µs and
throughput ranged from 85.77 to 94.49 ops/s. It also passed
`W07_ROLLOUT_LEDGER_POLICY_PASS`, `W07_ROLLOUT_LEDGER_TEST_PASS cases=6`,
the credential-free production configuration policy fixtures,
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
| P7 — observability, alerts and runbooks | Implementation surface only; collector, alert and drill evidence open | The FoundationDB authority/shared-reader `stats()` snapshots expose bounded publication/read attempts, successes/failures, last provider-time observations and local diagnostic timestamps for application-owned export; each publication/read attempt now also emits a bounded `tracing` event with fixed `mount_rs.foundationdb.authority` target/event names and no paths, prefixes, credentials or provider error text; production exit still requires collector/pager delivery, authority-age and failure alerts, dashboards, thresholds, runbook execution and named on-call ownership for cluster health, fencing, retries/maybe-committed outcomes, block errors, latency, capacity and cleanup pressure |
| P8 — load, capacity, soak and cost envelope | Harness + exact-tip terminal ten-round cross-platform qualification; production evidence open | The opt-in harness supports bounded repeated real FoundationDB/RustFS composition rounds with unique prefixes and cleanup, and emits p50/p95/p99 operation-latency and throughput markers; latest hosted run `35705886860` at exact revision `ab74870c58ab768c65679ea80feb18a8f54cbe00` recorded base `operations=15 p50_us=2848 p95_us=13430 p99_us=13430 total_ms=57 throughput_ops_per_sec=260.46`; ten isolated durable composition rounds passed with p95/p99 from 11,732µs to 13,359µs and throughput from 241.11 to 268.03 ops/s. The same run validated the 400-iteration, 64-concurrency, 4 KiB profile: 400 successful writes, reads and deletes, 1,200 successful lifecycle operations, measured 228.83 lifecycle IOPS, zero timeouts and zero cleanup failures. Linux artifact `foundationdb-production-qualification-35705886860-1` (ID `10684961250`, SHA-256 `b7243f25a5761ff33eb934dfdf3c6cb11e759152b6d89da81443f2b1a56a713d`) and the aggregate packet were retained; the measured rate is bounded qualification, not a capacity target. Production-shaped duration, retry/error budget, resource growth, safe capacity, cost and scaling triggers are still required |
| P9 — upgrade, rollback and compatibility | Not started | Forward/backward keyspace and configuration compatibility, rolling provider/client upgrade, failed-upgrade rollback, retained-data downgrade boundary, lockfile/image/artifact provenance |
| P10 — security, privacy, tenancy and audit | Not started | Threat-model review, prefix/tenant isolation, data classification, encryption, audit retention, dependency/image review, abuse/rate limits, closed findings or approved exceptions |
| P11 — native client, mount and platform support | Qualification only; repaired exact-tip hosted Linux/macOS packet is green at `35705886860` for exact revision `ab74870c58ab768c65679ea80feb18a8f54cbe00` | An explicit advertised platform matrix; clean-install, native FDB client, Node/CLI, FUSE/NFS/FSKit lifecycle, concurrent access, restart/recovery and packaging/signing evidence for every advertised platform. The current macOS result is feature compilation plus bound artifact provenance only; live macOS service/cluster/mount, clean-install and signed-package evidence remain open |
| P12 — release packaging, CI promotion and canary | Qualification CI plus policy gate | Locked and signed artifacts, SBOM/provenance, protected environment approvals, production-like canary, holdback, promotion checks, rollback automation and retained evidence packet |
| P13 — incident, failover and recovery rehearsal | Not started | Timed operator exercises for authority loss, cluster loss, stale client, storage exhaustion, bad deploy, credential expiry and restore; paging, runbook, integrity and RTO evidence |
| P14 — final launch audit and go/no-go | Not started | One-revision audit of P0–P13, known-limitations record, release-owner decision, canary exit evidence and explicit GO or NO-GO |
The current hosted qualification refresh for P8 and P11 is run
[`35691106828`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35691106828)
at exact revision `6b8639aa618f301e8424b5619bc1dda559a75948` (job
`106628088824`). It supersedes the earlier `35688516329` checkpoint for
current-tip implementation qualification: the current 11-case
qualification-log suite passed, the raw log's provenance marker matched the
repository, workflow, ref, SHA, run, attempt and runner, the six-sample
schema-2 latency summary was independently revalidated, the independent
30-second authority heartbeat stayed live across staged clients and service
restart, the shared authority stats marker reconciled three successful
publications and one fail-closed reader, the 400-iteration workload measured
259.35 IOPS with 1,200 successful lifecycle operations and zero
timeouts/cleanup failures, and the native Linux Node/CLI/FUSE and durable
restart paths passed. The retained artifact is
`foundationdb-production-qualification-35691106828-1` (ID `10678891553`,
SHA-256 `f26ae7cb81dd298cc78153f27e573bdb1096a6ae038cc8f42a40d7cbeeab3c17`).
This does not move P8 or P11 to production acceptance: the heartbeat and
process-local stats are bounded qualification instrumentation, not production
monitoring or deployment credentials, and the advertised platform matrix,
capacity envelope, failover, observability, packaging and owner evidence
remain open.
No P0–P14 gate is currently terminally accepted. A production gate may move to
complete only when the exit evidence is from the named production-like
environment and the owner records the result; implementation tests alone do
not close operations, security, native, or release gates.

The W07 hosted workflow now assembles a cross-platform qualification packet
after the Linux and macOS jobs. The aggregate requires both upstream jobs to be
terminally successful, downloads both retained artifacts, and runs
`scripts/verify-w07-platform-evidence.mjs` plus its credential-free regression
suite. The macOS artifact must also carry run-bound provenance matching the
Linux schema-2 repository, workflow, ref, source revision, run and attempt;
the macOS artifact also requires a non-empty platform runner identity, while
the Linux and macOS runner identities remain distinct. Mismatches fail closed.
The resulting
`W07_PLATFORM_QUALIFICATION_PASS` is still a qualification marker: it does not
establish a live macOS FoundationDB service or cluster,
native mount, clean install, signing/package provenance, or production GO.

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

The hosted log verifier also requires the exact accepted configuration shape,
both expected negative-fixture markers and a parsed lease-publication policy
whose publication interval is shorter than its TTL, whose forward-jump bound
does not exceed that TTL, and whose TTL is at most 24 hours. The credential-free
`scripts/test-w07-qualification-log.mjs` step exercises valid evidence plus
missing-negative-fixture, unsafe-cadence, unbounded-config and missing-soak-
latency cases. These are
evidence-integrity checks only; they do not create production credentials,
identity, failover, backup, capacity or owner evidence.

The qualification-log verifier also fails closed unless the hosted log contains
the bounded authority heartbeat marker and the shared-authority stats marker.
It validates that heartbeat cadence and forward-jump bounds match the emitted
lease policy, that the authority and reader attempt counters reconcile with
their success/failure counts, and that both paths observed a non-zero provider
time. The credential-free regression suite now exercises eleven cases,
including missing-heartbeat, inconsistent-stats, missing-soak-latency and
missing/mismatched run-bound provenance failures. This strengthens evidence
integrity for the production-shaped lane; it does not turn test heartbeat or
process-local counters into deployed monitoring, ACL, failover or owner proof.

The hosted workflow records a `W07_QUALIFICATION_PROVENANCE` marker before
provider execution and the verifier compares its repository, workflow, ref,
source revision, run ID, attempt and runner fields with the GitHub environment.
Missing or mismatched fields fail closed, so a retained qualification log
cannot be relabeled as evidence for another run. This binds the artifact to
the hosted execution context; it still does not authenticate a production
tenant, identity, ACL, failover, backup, capacity or owner result.

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

The workflow also validates the machine-readable
`docs/W07-production-evidence.json` packet with
`scripts/verify-w07-production-evidence.mjs` and exercises twelve
credential-free packet cases with `scripts/test-w07-production-evidence.mjs`.
The packet has one record for each W07.7 production gate and requires explicit
remaining actions while **NO-GO**; any future **GO** packet must provide a
concrete source revision, owner, target environment, terminal run, provider
versions, cleanup/rollback outcome and evidence reference for every closed
gate. The workflow also retains this packet beside the qualification log and
summary in the run artifact, so the seven-gate NO-GO state travels with each
bounded qualification result. This is admission/tracking integrity only and
cannot authenticate any production result or release approval.

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

The heartbeat-corrected current-tip hosted run
[`35680085315`](https://github.com/andymac4182/mount-rs/actions/runs/35680085315)
(job `106594973866`, exact revision
`71972b28ca7ae561325342ddf466c8353546547a`) completed green on Ubuntu
24.04/Linux `amd64` in 10m05s. It emitted the validated lease policy marker
(`lease_ttl_ms=120000`, `publication_interval_ms=30000`,
`max_forward_jump_ms=120000`) and ran an independent bounded authority
heartbeat at the 30-second cadence while checking heartbeat liveness across
the staged clients and service restart. Durable FoundationDB/RustFS,
Node/N-API, Linux FUSE/CLI, five-round soak, and the 400-iteration workload
all passed; the workload recorded 1,200 successful lifecycle operations,
202.31 measured IOPS, zero timeouts/cleanup failures, and composition latency
of p50 2,589µs/p95/p99 28,220µs. This proves the qualification harness no
longer fails merely because staged clients are not the cadence publisher; the
test heartbeat is not production monitoring, deployment-credential or
failover evidence. The retained artifact is
`foundationdb-production-qualification-35680085315-1` (ID `10675087310`,
SHA-256 `f9b47245760b4d03ddeeb9461b51bb1f29c8ce913920cbc629309ba024d2ecc5`),
and the seven-gate production packet remains **NO-GO**.

The telemetry-qualified current-tip hosted run
[`35681642584`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35681642584)
(job `106599666288`, exact revision
`113b13751acc4885ca5916a7f70fe109e1dbadd4`) completed green on Ubuntu
24.04/Linux `amd64` in 10m51s. It passed the rollout, policy, qualification,
workload and production-evidence regression cases, the bounded lease policy,
and the independent authority heartbeat at 30-second cadence with a 120-second
maximum forward-jump bound. The shared-authority path emitted
`FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3
publication_successes=3 publication_failures=0 reader_attempts=5
reader_successes=4 reader_failures=1 last_published_time_ms=2030001
last_observed_time_ms=2030001`; these counters cover the intended publication,
successful-read and fail-closed-read boundaries. Durable FoundationDB/RustFS,
Node/N-API, Linux FUSE/CLI, service restart, five-round soak and RustFS
integration all passed. The base composition marker was p50 3,841µs,
p95/p99 321,472µs and 30.29 ops/s; soak p95/p99 ranged from 10,229µs to
10,887µs and throughput from 257.61 to 295.85 ops/s. The retained workload
artifact records 400 successful writes, reads and deletes, 1,200 successful
lifecycle operations, 441.76 measured IOPS, zero timeouts and zero cleanup
failures. Artifact `foundationdb-production-qualification-35681642584-1`
(ID `10675750181`, SHA-256
`5305418e1b253bdc621815311eaf58f80234ef193253fe4b9f23e3c0d938b23c`) records
runner `GitHub Actions 1000025169` and the same pinned Node, RustFS,
FoundationDB and split-store provider provenance. This is a stronger
implementation qualification checkpoint, not production collector, ACL,
failover, capacity or owner evidence; the retained production packet remains
**NO-GO** with seven open gates and zero evidence records.

The telemetry-verifier current-tip hosted run [`35683503910`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35683503910)
(job `106606452492`, exact revision
`0e0327535fbcda0b1544400c00e938911a8ed6ad`) completed green on Ubuntu
24.04/Linux `amd64` in 12m44s. It passed the seven-case qualification-log
verifier, the six rollout-ledger cases, the workload-artifact and
production-evidence packet regressions, the bounded lease policy and the
independent 30-second authority heartbeat with a 120-second maximum
forward-jump bound. The shared-authority path emitted
`FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts=3
publication_successes=3 publication_failures=0 reader_attempts=5
reader_successes=4 reader_failures=1 last_published_time_ms=2030001
last_observed_time_ms=2030001`; durable FoundationDB/RustFS, Node/N-API,
Linux FUSE/CLI, service restart, five-round soak and RustFS integration all
passed. The base composition marker was p50 3,281µs, p95/p99 29,220µs and
185.99 ops/s; soak p95/p99 ranged from 13,491µs to 22,635µs and throughput
from 176.18 to 210.55 ops/s. The retained workload artifact records 400
successful writes, reads and deletes, 1,200 successful lifecycle operations,
368.57 measured IOPS, zero timeouts and zero cleanup failures. Artifact
`foundationdb-production-qualification-35683503910-1` (ID `10675579408`,
SHA-256
`ebe4ef52c09fa50e1395a0d672e1db715c9d60deb120f43c85499750ba6c6362`)
records runner `GitHub Actions 1000025458` and the pinned Node, RustFS,
FoundationDB and split-store provider provenance. This is a stronger
implementation qualification checkpoint, not production collector, ACL,
failover, capacity or owner evidence; the retained production packet remains
**NO-GO** with seven open gates and zero evidence records.

The repaired exact-tip qualification
[`35686583792`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35686583792)
(job `106614608451`, exact revision
`7ba3998a6f6a88b7eedefd98cba9262980d24ce9`) completed green in 12m35s on
Ubuntu 24.04/Linux `amd64` after the standalone `tests/foundationdb/Cargo.lock`
correction. The seven-case qualification-log, four-case workload-artifact and
twelve-case production-evidence regression suites passed; the 30-second /
120-second authority heartbeat and reconciled stats marker passed; durable
FoundationDB/RustFS, Node/N-API, Linux FUSE/CLI, service restart, five-round
soak and RustFS integration passed. Base composition recorded p50 3,133µs,
p95/p99 32,028µs and 187.57 ops/s; soak p95/p99 ranged from 12,020µs to
12,490µs and throughput from 227.19 to 235.58 ops/s. The retained workload
artifact records 1,200 successful lifecycle operations, 166.29 measured IOPS,
zero timeouts and zero cleanup failures. Artifact
`foundationdb-production-qualification-35686583792-1` (ID `10676804103`,
SHA-256
`afccc878e46a096c6153f04fac017f3df19d37d147a2e83629fa14616f850e30`)
records runner `GitHub Actions 1000025921` and the pinned Node, RustFS,
FoundationDB and split-store provider provenance. This is a repaired hosted
implementation qualification checkpoint; the retained packet remains
**NO-GO** with seven open gates and zero evidence records, so production
identity/ACL, failover/recovery, capacity, collector, platform and owner
evidence remain open.

The evidence-integrity follow-up at exact revision
`419fbf217b5c40e0e62371e9b41badc07582e3d5` strengthens the qualification-log
verifier from seven to eight regression cases. It now requires one valid
`FOUNDATIONDB_LATENCY_PASS` sample for the base composition and every declared
soak round, validates every parsed sample, and retains all samples in the
schema-2 summary. The retained `35686583792` log independently passes this
stricter verifier with six samples (base plus five soak rounds), and the local
eight-case regression suite passes. The hosted run itself predates the eighth
case, so a fresh current-tip hosted execution was required to record this CI
result; that result is recorded below. This is evidence-integrity hardening only; the production packet
remains **NO-GO** with seven open gates.

The first provenance-enabled hosted attempt
[`35690122405`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35690122405)
(job `106625124766`, exact revision
`07e455cd48b2962e2d06290bad35379b575d521e`) reached the durable and workload
passes but failed final marker validation because the config-policy `tee`
overwrote the raw-log provenance marker. It produced no qualification pass;
the append-only repair is tracked above.

The corrected current-tip execution is hosted run
[`35691106828`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35691106828)
(job `106628088824`, exact revision
`6b8639aa618f301e8424b5619bc1dda559a75948`). It completed green in 12m10s
on Ubuntu 24.04/Linux `amd64`; the current 11-case verifier passed, the raw
log provenance marker matched the exact run context, and the retained schema-2
summary contains the base plus five soak latency samples. The base marker was
p50 2,680µs, p95/p99 13,759µs and 254.79 ops/s; soak p95/p99 ranged from
12,701µs to 13,784µs with throughput from 241.60 to 248.00 ops/s. The
bounded 400-iteration, 64-concurrency, 4 KiB workload recorded 1,200
successful lifecycle operations, 259.35 measured IOPS, zero timeouts and zero
cleanup failures. Artifact
`foundationdb-production-qualification-35691106828-1` (ID `10678891553`,
SHA-256
`f26ae7cb81dd298cc78153f27e573bdb1096a6ae038cc8f42a40d7cbeeab3c17`)
was independently downloaded and revalidated with the exact provenance
environment. This is a current-tip hosted implementation qualification pass,
not production evidence; the seven-gate packet remains **NO-GO** with zero
production evidence records.

The extended-soak workflow change is published at current source
[`5a6d6507c6deac160f54a246a2d715c05fc35268`](https://github.com/andymac4182/mount-rs/commit/5a6d6507c6deac160f54a246a2d715c05fc35268)
and was exercised by hosted run
[`35692674674`](https://github.com/andymac4182/mount-rs/actions/runs/35692674674)
(job `106632794791`). The lane increased the isolated real-provider soak from
five to ten rounds while retaining the same operation mix, unique-prefix
cleanup and run-bound provenance guard. The run completed green in 11m26s;
the eleven-sample schema-2 summary recorded base p95/p99 439,603µs at 21.41
ops/s, ten-round soak p95/p99 from 70,204µs to 236,130µs at 18.19–96.22
ops/s, and the bounded workload recorded 1,200 successful lifecycle
operations at 110.18 IOPS with zero timeouts and cleanup failures. Artifact
`foundationdb-production-qualification-35692674674-1` (ID `10679154003`,
SHA-256 `44beef451832c307a53571bb86f92293b70c302d0add5104152059ed4c41d2ec`)
was independently downloaded and revalidated with the exact repository,
workflow, ref, source revision, run, attempt and runner environment. This is
extended hosted implementation qualification only; the seven-gate packet
remains **NO-GO** with zero production evidence records.

The terminal cross-platform requalification is hosted run
[`35698630720`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35698630720)
at exact revision `a19d37b61fdbb769102df15714cc88a3ff5eea91`, with Linux job
`106651101783`, macOS job `106651102065` and aggregate job `106654577928` all
green. Linux retained the run-bound provenance, authority heartbeat/stats,
durable FoundationDB/RustFS, Node/N-API, CLI/FUSE, restart, fresh-client and
RustFS paths; the base composition was p50 2,469µs, p95/p99 25,063µs and
208.39 ops/s, ten-round soak p95/p99 was 13,399–13,906µs at 225.29–258.64
ops/s, and the bounded workload recorded 1,200 successful lifecycle operations
at 467.15 IOPS with zero timeouts or cleanup failures. The macOS lane emitted
`W07_MACOS_FOUNDATIONDB_COMPILE_PASS`, and the aggregate verifier emitted
`W07_PLATFORM_QUALIFICATION_PASS` after downloading both platform artifacts.
The Linux, macOS and aggregate artifacts were independently revalidated with
the exact provenance and platform validators. This closes the terminal
qualification packet and feature-compile control only; it does not close live
macOS service/cluster/mount, clean-install, signing/package, production
capacity, identity/ACL, backup/restore, failover, observability or owner gates.
The seven-gate packet remains **NO-GO** with zero production evidence records.

The exact-tip requalification is hosted run
[`35700746198`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35700746198)
at revision `8520e362710a4b3fe00fd567cf00fcc13e64c222`, with Linux job
`106657901740`, macOS job `106657901971` and aggregate job `106660728286` all
green. The Linux packet independently matched repository, workflow, ref, SHA,
run, attempt and runner `GitHub Actions 1000027426`; the base composition was
p50 1,950µs, p95/p99 24,689µs and 242.82 ops/s, ten-round soak p95/p99 was
9,727–266,600µs at 47.11–346.34 ops/s, and the bounded workload recorded
1,200 successful lifecycle operations at 323.68 IOPS with zero timeouts or
cleanup failures. The macOS lane emitted
`W07_MACOS_FOUNDATIONDB_COMPILE_PASS`, and the aggregate emitted
`W07_PLATFORM_QUALIFICATION_PASS` after downloading both platform artifacts.
Independent exact-provenance, workload, platform, production-packet and
rollout-ledger validators passed. The packet remains qualification-only and
**NO-GO**: live macOS service/cluster/mount, clean-install, signing/package,
production capacity, identity/ACL, backup/restore, failover, observability and
owner evidence remain open.

The repaired exact-tip requalification is hosted run
[`35705886860`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35705886860)
at revision `ab74870c58ab768c65679ea80feb18a8f54cbe00`, with Linux job
`106674581511`, macOS job `106674582039` and aggregate job `106678244742` all
green. Linux matched repository, workflow, ref, SHA, run, attempt and runner
`GitHub Actions 1000027673`; macOS matched the shared fields with runner
`GitHub Actions 1000027679`. The base composition was p50 2,848µs, p95/p99
13,430µs and 260.46 ops/s; ten-round soak p95/p99 was 11,732–13,359µs at
241.11–268.03 ops/s; and the bounded workload recorded 1,200 successful
lifecycle operations at 228.83 IOPS with zero timeouts or cleanup failures.
The macOS compile and bound-provenance markers passed, and the aggregate
emitted `W07_PLATFORM_QUALIFICATION_PASS provenance=bound`. Independent
exact-provenance, workload, platform, production-packet and rollout-ledger
validators passed. The packet remains qualification-only and **NO-GO**: live
macOS service/cluster/mount, clean-install, signing/package, production
capacity, identity/ACL, backup/restore, failover, observability and owner
evidence remain open.

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

Hosted run
`[35655579378](https://github.com/andymacclenaghan/mount-rs/actions/runs/35655579378)`
tested the policy-bearing revision `03541161311983f497983ef0de1bcb09b958b2e4`
on `ubuntu-24.04`; its real-cluster authority/composition, consumer, restart
and native Linux paths passed. The run's static policy fixtures also failed
closed for inline secrets and unsafe lease TTLs. This confirms the bounded
implementation path only; deployment credentials, clock monitoring and
cadence telemetry, failover rehearsal and production owner sign-off remain
open.

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
