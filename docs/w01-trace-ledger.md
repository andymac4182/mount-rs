# W01.3 deterministic trace ledger

This packet records the first W01.3 gate: the simple in-memory seeded trace
against the pinned TypeScript `mountx` memory oracle. It does not change the
filesystem implementation or the trace operations. The script remains able to
run its existing local backend set, but this ledger intentionally records only
the explicit `memory` run; remote, native, and provider acceptance belongs to
separate evidence.

## Reproduction contract

- Script: `scripts/check-trace-parity.mjs`
- Oracle repository: <https://github.com/pithings/mountx>
- Required oracle revision: `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`
- Oracle source: supplied by `MOUNTX_SOURCE`; the script resolves and reports
  its absolute path and rejects a different Git revision.
- Default seeds: `4182,1,42,65535,4294967295`
- Seed domain: decimal unsigned 32-bit integers (`0` through `4294967295`);
  empty entries, non-decimal values, and out-of-range values fail validation.
- First-gate backend selection: `MOUNT_RS_TRACE_BACKENDS=memory`

Without `MOUNT_RS_TRACE_BACKENDS`, the script retains its existing six local
backend defaults and its existing opt-in `PGLITE_DATABASE_URL`/`MOUNT_RS_TRACE_R2`
extensions. The first gate selects `memory` explicitly so this ledger makes no
new persistence, remote-provider, native, or live-service claim.

The trace generator is unchanged in substance: seven fixed setup operations,
500 LCG-selected operations, 50 periodic root listings, and 64 final
reconciliation operations. Every seed therefore produces **621 operations**.
The reconciliation includes `read`, `list`, `lstat`, and `readlink` for every
candidate path so a matching return code cannot conceal divergent final state.

Reproduce the first gate with:

```sh
MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX \
MOUNT_RS_TRACE_BACKENDS=memory \
MOUNT_RS_TRACE_SEEDS=4182,1,42,65535,4294967295 \
node scripts/check-trace-parity.mjs
```

## Result boundary

The final `TRACE_EVIDENCE_JSON ` line is one-line JSON with schema
`mount-rs/w01.3-trace-evidence@1`. Its `status` and process exit code have the
following meaning:

| Status | Exit | Meaning |
| --- | ---: | --- |
| `PASS` | 0 | The pinned oracle loaded and every selected seed/backend row passed for the reported operation count. |
| `SKIP` | 0 | `MOUNTX_SOURCE` was unset. This is an explicit prerequisite skip, never a parity pass; `results` is empty and `operationCount` is `null`. |
| `FAIL` | 1 | Seed/backend configuration, oracle revision/source, oracle loading, backend execution, operation count, or result parity failed. |

For a parity failure, the report keeps only the failing backend, seed,
operation index, command, expected value, actual value, and a bounded window of
two operations on either side. Backend process failures also retain bounded
stderr/stdout tails. This is the retained failure evidence; the full 621-step
trace is regenerated from the reported seed and pinned oracle.

## Evidence — 2026-09-20

Command run from the repository root:

```text
MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX MOUNT_RS_TRACE_BACKENDS=memory MOUNT_RS_TRACE_SEEDS=4182,1,42,65535,4294967295 node scripts/check-trace-parity.mjs
```

Observed oracle source `/tmp/mountx-source.uWiHfX` was at revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`, matching the pinned revision.

| Seed | Backend | Operations | Result |
| ---: | --- | ---: | --- |
| 4182 | `memory` | 621 | PASS |
| 1 | `memory` | 621 | PASS |
| 42 | `memory` | 621 | PASS |
| 65535 | `memory` | 621 | PASS |
| 4294967295 | `memory` | 621 | PASS |

Overall result: **PASS**, exit code `0`. No remote, native, provider, commit,
or push result is claimed by this ledger.
