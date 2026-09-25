# Remote Drive verification

## Scope

Add executable load, failure, resource-boundary, and bounded symbolic verification
for the remote provider. Keep existing native mounting and storage contracts.
Tests must verify data integrity and authorization outcomes, and record actual
executed commands. Formal claims apply only to production decisions called by
named harnesses with documented domains. Preserve unrelated work in the original
checkout. Hosted CI and production performance require their own results.

## Tasks

1. Real QUIC concurrent clients, independent Drives, mixed reads/writes, isolation,
   resource saturation, and a configurable longer soak runner with latency and
   throughput output. Small deterministic load runs in ordinary tests.
2. Real transport failure after mutation begins, malformed input and handshake,
   hard expiry, and no mutation replay. Add regression coverage for discovered bugs.
3. Production-used bounded Kani proofs for grant matching, permission/flags,
   frame admission, and handle admission. Run every harness separately.
4. Review all changes, run formatting, strict Clippy and workspace tests; wire
   repeatable load/formal CI, and publish bounds, results, and outstanding limits.

## Progress

- Initial handle regressions fail: stale in-flight open is admitted after revision
  change; counter overflow does not close the rejected backend handle.
- Handle fix passes both regressions: monotonic revision, terminal shutdown,
  revision-bound admission, and cleanup for capacity/counter rejection.
- Six production-decision Kani harnesses execute successfully with 25/25 covers.
- Actual QUIC failure tests pass, including delayed open crossing catalog revision
  and no replay after backend commit followed by disconnect.
- Default soak: 12,800 operations; extended run: 128,000 operations with 4 KiB
  payloads and verified contents, zero errors.
- Review requested a true stream-ceiling test. The blocking backend found the
  hello consumed initial transport credit. Complete hello FIN/closure plus one
  reserved transport credit allows all 32 request slots; the semaphore remains at 32.
- Hard-expiry renewal regression exposed a partly usable terminal handle session;
  serving a later stream now closes the connection and requires reconnection.
- Final static review clear: exact 32 active operation saturation and hello credit
  reservation are tested; renewal checks terminal state after all awaited validation.
- Final workspace formatting and strict Clippy pass. Workspace tests: 1,158 passed,
  zero failed, 86 ignored. Six named Kani proofs rerun successfully; 25/25 covers.
- CI wires short load/failure regressions, a manual bounded soak with artifacts,
  and pinned Linux formal decisions. Hosted qualification remains pending.
