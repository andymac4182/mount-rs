# First-Initial CLOSE natural-drain qualification

A delayed/cancelled negotiation can send transport CLOSE as the first Initial
seen by a restarted server. Quinn-proto 0.11.18 accepted that packet into Draining,
but its first-packet path skipped the normal close bookkeeping. A later local
close skipped already-closed state too. The endpoint retained one connection
record and exposed only the 30 second idle timer.

The scoped patch performs the existing bookkeeping after successful first-packet
processing and before the coalesced remainder. It keeps Initial AEAD validation,
error propagation and the natural 3-PTO close grace. The full checksum-verified
upstream archive, unchanged manifest/features and MIT/Apache licenses are retained
under `vendor/quinn-proto-0.11.18`; only the first-packet source differs.

| Case | Unpatched | Patched |
| --- | --- | --- |
| Ordinary ClientHello then CLOSE | Natural drain at 3.072s | Natural drain at 3.072s |
| CLOSE as first Initial | Still open at 3s; next timer 30s | Natural drain at 2.997s; record removed |
| Real UDP first CLOSE then endpoint shutdown | `wait_idle` exceeds 4s | `wait_idle` completes within 4s; open records 0 |

The protocol cases use actual generated encrypted datagrams, explicit synthetic
instants, observed transport APPLICATION_ERROR, no Connected event, and only
natural endpoint events. The UDP case observes the accepted closed record before
shutdown. These tests do not fabricate drain events or remove records manually.
All intended REDs reached their semantic assertions with parent ownership,
source/log and deadline barriers settled.

## Local gates

[report.json](report.json) retains receipt/log hashes and scope limits.
17 gates passed: 16 owned Rust gates (95 test executions) and 54 modeled parent
controls. All 16 gates used the same 504 input hashes. This includes deterministic
and UDP close controls, cache stages, authenticated peer reconnect/cancellation,
Redis failure/cleanup cases, remote transport and uncertain-write checks, strict
Clippy and formatting. Warmed selected storage/core metric recording paths still
reported 0 added allocations; this is not a whole-stack allocation claim.

Reproduce from the repository root with a writable, checkout-specific target:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-quinn-check CARGO_BUILD_JOBS=1 ./scripts/cargo-shared test --locked -p mount-rs-blob-cache --test quinn_close --no-run
CARGO_TARGET_DIR=/private/tmp/mount-rs-quinn-check python3 -B scripts/test-remote-failures.py quinnordinary
CARGO_TARGET_DIR=/private/tmp/mount-rs-quinn-check python3 -B scripts/test-remote-failures.py quinnclose
CARGO_TARGET_DIR=/private/tmp/mount-rs-quinn-check python3 -B scripts/test-remote-failures.py quinnruntimeclose
```

The first command warms the exact locked target online; owned tests remain offline.
Cargo's root override applies to workspace-built CLI/server artifacts. A published
library does not propagate this patch into a consumer's root manifest.

## CI and remaining qualification

An allowlisted CI annotation on the preceding commit classified storage allocation
qualification's failure as `offline_download`/101. The missing crate and test
execution state remain unverified; no allocator assertion failure was observed. CI now warms the QUIC close target and
both allocation targets online with `--locked --no-run`. Hosted validation of
that change remains pending. No raw Actions logs or artifacts were accessed.

These observations qualify one lifecycle fix on loopback. They do not establish
production IOPS, 10,000-client saturation, native FoundationDB runtime, full backend
comparisons, or a complete formal inventory. The metadata preparation Full-scan
reduction is the next measured storage optimization and is not included here.
