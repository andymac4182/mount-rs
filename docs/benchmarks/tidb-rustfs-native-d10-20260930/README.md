# Current TiDB + RustFS D10 profiles

These are two completed local ten-server runs with ten clients and ten Drives. Rates are whole-cluster logical cycles per second, including Open/Close. They are not physical IOPS. Both native runs passed their root-owned runtime checks and fresh full-content oracles. Dataset absence remains unverified; owned stopped data is retained.

The tables describe two distinct file profiles. F100 initially held 1,000 files / 4,096,000 bytes; F1000 held 10,000 files / 62,832,640 bytes. The five-second cells, cache state and execution histories differ. No causal scaling or optimization comparison is made.

Root runtime qualification and offline extraction are separate: the root witness binds native checks, settlement and oracle receipt hashes. The v3 extractor and this projection validate closed selected counters/rates; their source/measurement qualification flags remain false.

## Run configuration

- Measured checkout: clean `78e68035793e7fac994fbc87837fefc12b25145a`; release binary and 561 Rust/Cargo/build inputs were archived and pinned. The cached binary was initially compiled at `46a8f4c6`; the complete build closure was byte-identical after the workflow-only commit.
- Ten independent server processes on this macOS host; ten authenticated clients, ten separate Drives and five Partitions, with two Drives per Partition. Both activity modes and all eight patterns ran. Mostly-idle mode has one active client; all-active has ten.
- TiDB 8.5.7 metadata and a fresh RustFS 1.0.0 fixture, limited to two CPUs and 2 GiB. The retained TiDB fixture was unchanged. This is one laptop/Docker VM with shared storage, rather than ten production hosts.
- F100: 100 initial 4 KiB files per Drive. F1000: 990 files of 4 KiB, nine of 128 KiB and one of 1 MiB per Drive. Timed payload reads/overwrites use 4 KiB blocks; append/truncate and namespace churn have different operation counts.
- Each pattern nominally runs for five seconds; rates use its recorded active elapsed time, including final in-flight completion. Setup, full-content verification and phase observers are outside that denominator.
- Signed local ES256/JWK fixture authentication and the negotiated QUIC protocol were exercised. The SDK raw RAM cache is present; the composed distributed RAM/disk/peer cache, external OIDC issuer and OS mounting are outside these runs.
- Allocation profiling is disabled; resource and I/O profiling are enabled. These runs provide no allocation-count result.
- Initial and final checks verify every expected byte, EOF and membership through fresh backend stores. Each run also checked 100 server/Drive pairs with one acknowledged 4 KiB read and close per pair. Both runs verified Drive/Partition denials and revocation, and all ten workers exited cleanly without forced termination.
- Fresh-store checks do not establish RustFS restart, crash or power-loss durability; those behaviors were not exercised in these runs.

## All-active results

| Profile | Pattern | Cycles/s | Root SDK ms/call | File SDK ms/call | SDK PUT ms/call | HTTP PUT dispatch ms/attempt | Frontend CPU s | Max individual RSS after MiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| D10-F100 | sequential_read | 697.180 | 4.800 | 2.936 | — | — | 6.131 | 79.078 |
| D10-F100 | random_read | 696.146 | 4.978 | 2.944 | — | — | 6.087 | 79.078 |
| D10-F100 | sequential_overwrite | 217.583 | 4.186 | 2.780 | 18.585 | 18.520 | 3.382 | 78.406 |
| D10-F100 | random_overwrite | 236.942 | 3.776 | 2.524 | 17.300 | 17.235 | 3.610 | 78.594 |
| D10-F100 | mixed | 351.180 | 4.074 | 2.644 | 16.980 | 16.914 | 4.299 | 78.125 |
| D10-F100 | hot_file | 278.140 | 3.695 | 2.444 | 18.512 | 18.446 | 3.829 | 77.828 |
| D10-F100 | append_truncate | 257.441 | 4.125 | 2.816 | 19.694 | 19.626 | 3.735 | 77.906 |
| D10-F100 | churn | 48.245 | 16.842 | 12.163 | — | — | 2.936 | 77.469 |
| D10-F1000 | sequential_read | 553.296 | 5.512 | 3.465 | — | — | 5.624 | 104.859 |
| D10-F1000 | random_read | 598.145 | 5.267 | 3.359 | — | — | 6.010 | 105.062 |
| D10-F1000 | sequential_overwrite | 137.383 | 7.002 | 4.596 | 29.637 | 29.562 | 2.552 | 102.188 |
| D10-F1000 | random_overwrite | 234.345 | 4.040 | 2.518 | 17.338 | 17.274 | 3.604 | 102.344 |
| D10-F1000 | mixed | 317.742 | 4.502 | 2.940 | 18.388 | 18.322 | 3.994 | 102.172 |
| D10-F1000 | hot_file | 246.182 | 4.278 | 2.865 | 19.965 | 19.898 | 3.476 | 83.938 |
| D10-F1000 | append_truncate | 226.336 | 4.662 | 3.182 | 22.509 | 22.434 | 3.538 | 76.609 |
| D10-F1000 | churn | 13.474 | 58.192 | 49.478 | — | — | 1.866 | 76.234 |

## Read path

Both basic read patterns have exactly three selected inode SQL calls per cycle, each returning one row: one Open ROOT path and two FILE checks around payload access. The Open path performs one probe-marker GET and one data-marker GET before its ROOT query. Close releases local references for this concurrent nondelegated mode. The distinct Root/File SDK wrapper means include provider work and decoding; their shared SQL counter cannot separate pure ROOT SQL time from FILE SQL time.

| Profile | Read pattern | Cycles | Inode SQL calls | Data/probe marker GET calls | SDK block GET calls | Data-role HTTP GET dispatches beyond data-marker calls | Pool checkout us/call |
|---|---|---:|---:|---:|---:|---:|---:|
| D10-F100 | sequential_read | 3492 | 10476 | 3492/3492 | 3492 | 900 | 1.187 |
| D10-F100 | random_read | 3488 | 10464 | 3488/3488 | 3488 | 0 | 1.223 |
| D10-F1000 | sequential_read | 2776 | 8328 | 2776/2776 | 2776 | 2461 | 1.332 |
| D10-F1000 | random_read | 3002 | 9006 | 3002/3002 | 3002 | 1949 | 1.286 |

The generic adapter cache is distinct from the unconfigured service blob cache. F100 random_read issued 3,488 SDK block GET calls; all 3,488 observed primary-data GET dispatches are accounted for by data-marker calls. This does not prove zero backing physical reads or exact wire requests. The JSON retains cache endpoint gauges and the inclusive HTTP scope.

## Durable write path

SDK PUT elapsed time closely matches the observed object-store HTTP dispatch elapsed sum in these cells. HTTP dispatch includes the client request/transport service and remote wait; it does not isolate network time, RustFS persistence, or server CPU. A matching count is not an SDK retry counter. Successful empty PUT responses may be dropped by the adapter; body_dropped is not a PUT failure.

| Profile | Pattern | PUT calls / observed dispatches | HTTP/SDK inclusive elapsed ratio | Write-commit gate wait us/call | Write-commit gate hold ms/call |
|---|---|---:|---:|---:|---:|
| D10-F100 | sequential_overwrite | 1092/1092 | 0.996 | 0.292 | 16.712 |
| D10-F100 | random_overwrite | 1191/1191 | 0.996 | 0.273 | 15.171 |
| D10-F100 | mixed | 880/880 | 0.996 | 0.273 | 16.961 |
| D10-F100 | hot_file | 1046/1046 | 0.996 | 0.285 | 15.872 |
| D10-F100 | append_truncate | 652/652 | 0.997 | 0.200 | 19.779 |
| D10-F1000 | sequential_overwrite | 696/696 | 0.997 | 0.298 | 24.703 |
| D10-F1000 | random_overwrite | 1179/1179 | 0.996 | 0.264 | 15.269 |
| D10-F1000 | mixed | 798/798 | 0.996 | 0.251 | 18.960 |
| D10-F1000 | hot_file | 930/930 | 0.997 | 0.279 | 18.322 |
| D10-F1000 | append_truncate | 577/577 | 0.997 | 0.205 | 22.329 |

The small mean local gate waits identify little average queued local-gate time in these cells; they do not prove absence of tail contention. Gate hold/publication and SDK/HTTP/SQL spans overlap. Their elapsed sums must not be added as exclusive cycle costs. Write cycles include two marker pairs in the basic overwrite patterns, one during Open and one during publication authority verification.

## Structural churn

| Profile | Cycles | Structural publications | Inode SQL calls/cycle | Returned inode SQL rows/cycle | Structural wrapper ms/call | Commit ms/call |
|---|---:|---:|---:|---:|---:|---:|
| D10-F100 | 250 | 750 | 22.000 | 1331.400 | 46.744 | 16.999 |
| D10-F1000 | 73 | 219 | 22.000 | 12061.315 | 177.814 | 50.487 |

Each churn cycle has three structural publications and 22 calls in the inode-read metric family. Returned-row volume is much larger than the point-read path: this directly identifies a logical metadata row-amplification seam. In the measured archived-source audit, structural reads/publication scan members and root dentries under FOR UPDATE. Open ROOT SQL remains an exact header/dentry/file point query returning one row; local cached entry lookup/validation can scan the cached directory, without a whole-directory clone. These source facts must not be confused with a full SQL directory read during Open.

Create retains a persistent member/guard after unlink reduces nlink to zero, and concurrent nondelegated Close does not reap it in the measured source. Full-range structural work therefore covers live siblings plus retained tombstones; initial/live file counts do not equal total persistent membership after churn. This is a source-backed explanation of the seam, not measured TiDB lock wait, physical I/O, or a causal F100-to-F1000 estimate. The measured source already has scope-selected guard branches; selected root/file guards coexist with full membership and root-dentry range work. This distinction is preserved rather than claiming every guard read is a full scan.

## Timing and coverage

- New object-store observations are partial, cumulative per process and non-atomic. Old http_attempts coverage remains unavailable. No historical Wire100 HTTP evidence is invented.
- HTTP status array order is 1xx, 2xx, 3xx, 4xx, 5xx, other. Connector builds, HTTP errors and terminal body states are retained in JSON. SDK retry counts remain unavailable.
- HTTP maxima are retained process lifetime endpoints, not phase percentiles or subtractable counters. Zero recorded in-flight endpoints do not prove socket/backing drain.
- Frontend CPU sums cover eleven individual process boundary windows, including observers/background. Those windows differ from the active throughput clock; no CPU utilization or exclusive workload CPU is inferred.
- RSS values are process endpoints and lifetime peaks. The table gives the maximum individual endpoint; it is not a simultaneous cluster peak. No allocation or physical IOPS claim is made.
- Source/binary identity and exact native/oracle bindings come from the root witness. Raw oracle receipts were not independently replayed or revalidated by this projection.

## Evidence bindings

- Root native witness `production-current-d10-f100-f1000-root-witness.json`: `5da985cc79ca790a2e48fc2ae53cb0c07de2c3d224fef1f826d5bd69bbe233d9`.
- Extractor controls `production-stage-exact-v3-controls.receipt.json`: `5174388f6761f9a38de705b2676e351c6e6d4f0da7bc26ef29fbdf06bd62f0ac`.
- Frozen v3 extractor: `fe9d62baab425d7a132b374dfa9f5f6e38f0d6673d4cce982244d4c365aa51c1`.
- `production-stage-d10-f100-current-v3.json` (5368668 bytes): `a72934197ec704eb8c3127bdca4b1f8371ac3b6c0fd0a04b5e06d3fc3669094b`.
- `production-stage-d10-f1000-current-v3.json` (5370262 bytes): `93630f27d55d4703d1ed58a595a1f2176b11ec7f822a9ad12378925daae08883`.
- Measured source `providers/mount-rs-tidb/src/compact.rs`: `416114dcf3da34a2d498cca85a56d63a7c5ea73d069f86d9851a5ef01d10ab54`.
- Measured source `filesystems/mount-rs-chunked/src/lib.rs`: `dd20dc770a51ea6e0f5e49fed5fedb558b3d8a2b730a8e87e10d297552ab941c`.
- Measured source `src/storage/compact/root_file.rs`: `2c9cfab7d9b564e9680cbc952f621d7e4f0e663ec9c25d1b3a7092e846dc8870`.
- Measured source `src/storage/compact/indexed.rs`: `187f73a154c62aeed3f58f5f0ea8efc850938eb5b0cb7440a3501a1993b19e66`.
- D10-F100 source `5813bd3ffb55e17c32d9137d9db1078b1e4833f4d8b7b245c0b1ff348afec753`, binary `df7d3d69ae514f8d17bde8d03c556c9099cb06f9e58af94ba3d6c2212ac9b72c`, owner `369d9cb76dda14d7c9a720e1eb91335bea1e55560adcaff1258cdee7e630f8e0`, terminal `1f13bffdb45f90d6cf09589db3e0e7596d1e2396ed2801cb1ced089682d15cad`.
  - initial full oracle: 1000 files / 4096000 bytes; receipt `49dbef4f126451a6ebd3ea0f968e36f5ed0b8df3186fac7f01180d1484dd129c`.
  - final full oracle: 1000 files / 4128768 bytes; receipt `55ee544e9b1417e0e046c6cb48ddd43beed3e87680c5659ba834dac044ec96eb`.
- D10-F1000 source `5813bd3ffb55e17c32d9137d9db1078b1e4833f4d8b7b245c0b1ff348afec753`, binary `df7d3d69ae514f8d17bde8d03c556c9099cb06f9e58af94ba3d6c2212ac9b72c`, owner `2956677d68762ac79865d68cd3f1a0521da0c49017d7d5993d7fd78a890e25a0`, terminal `2b52eaec7b834b2cf62c697e7f81a95179f1698b91cb7581f5cdfaf42ba457e9`.
  - initial full oracle: 10000 files / 62832640 bytes; receipt `9fe91b8ee3fd57ecfec4f1c14c5891bbfbf5d3cac2d136084493ec0e3a809e3b`.
  - final full oracle: 10000 files / 62861312 bytes; receipt `62cd000a93bfa957539799370aecb363fa93236de5beabd4580e8ea1d246d2f6`.

All 32 stage cells, exact rational inputs, selected counter values, individual CPU/RSS/cache endpoints and producer source hashes are retained in the companion JSON. The companion compressed JSON retains the public projection.

## Compressed metrics artifact

`metrics.json.gz` is deterministic gzip (mtime=0), 162510 bytes; SHA-256 `c84d1413eedae3462813d9b1633571796699c89e66ea0a9836f500c87a530659`.
Decoded JSON: 2670586 bytes; SHA-256 `66e262f08f71074c0dab2e4f960db9e4bb0ebafd4a33c376b5a7cc2ed7d7c113`.

Read locally with standard Python; exact rates remain decimal strings and rational inputs:

```python
from pathlib import Path
import gzip, hashlib, json
packed = Path("metrics.json.gz").read_bytes()
assert hashlib.sha256(packed).hexdigest() == "c84d1413eedae3462813d9b1633571796699c89e66ea0a9836f500c87a530659"
plain = gzip.decompress(packed)
assert len(plain) == 2670586
assert hashlib.sha256(plain).hexdigest() == "66e262f08f71074c0dab2e4f960db9e4bb0ebafd4a33c376b5a7cc2ed7d7c113"
metrics = json.loads(plain)
```

## D100 / F1000 admission result

The next unchanged control used ten server processes, 100 clients on 100 Drives across 50 Partitions, and 1,000 files per Drive. It **failed during online namespace creation at the original 600-second phase deadline**. It completed no timed I/O cells and no fresh corpus oracles; no D100 throughput rate is reported.

The terminal records 116,138 RPC attempts, 116,038 acknowledgments and 100 uncertain requests. Those cumulative acknowledgments are not a verified file count or a timed IOPS result. Unknown creates were not replayed. The journal's namespace-file count is populated at the successful phase boundary, so its zero value does not prove no files were created.

All ten workers were reaped without forced termination, but they exited 101 and reported `worker replica drain unproven`; their SDK contexts were not confirmed cleanly closed. The outer owner stopped its fresh RustFS fixture and collector, preserved the original containers and source pins, and retained its data. This containment does not qualify clean SDK retirement, durable outcomes for uncertain creates, or dataset absence.

The failure is retained in [d100-f1000-failure.json](d100-f1000-failure.json), SHA-256 `4eec29afadd876085e8097d63d5aedb4ed9f337716b444146db6fd7593c7b542`. Owner receipt SHA-256: `86b283ee48606a6f9e6c213690153622b784a5fd459a0baded986e946e491c61`; native terminal: `91cb2702d01a01e2709d796cfab228c49f726ec37d420ddd9ea00cfe0c089320`.

Source review traced the failed replica drain to deliberate quarantine: cancelling an issued Open/create drops its mutation guard, quarantines that runtime lease and makes pool shutdown refuse a clean retirement acknowledgment. This is the existing fail-closed behavior for an uncertain mutation; it does not establish a teardown defect. No unquarantine or replacement API is exposed by this pool.

The next work is to bound structural membership/dentry work and qualify recovery from quarantined replicas. No larger point was run after this failure. The 10,000-client / 10,000-Drive / 5,000-Partition target remains unqualified.
