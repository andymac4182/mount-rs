# Remote RustFS selector qualification

Explicit RustFS blocks are wired into the TiDB saturation and ten-process production harnesses. Metadata remains TiDB, separate Drives receive distinct block prefixes, and provider opens retain the established shared StorageContext. Missing or `metadata` selection preserves the existing mirrored backend. RustFS selection requires explicit configuration, durability and the existing owned fixture gates. Evidence records both backend roles; unavailable RustFS blob residency is separate from TiDB SQL counts.

| Final v2 gate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Service all targets, default | 297 | 0 | 17 |
| Service all targets, I/O profiling | 329 | 0 | 17 |
| Strict Clippy, default | exit 0 | — | — |
| Strict Clippy, I/O profiling | exit 0 | — | — |
| Formatting | exit 0 | — | — |

Test totals were recomputed from 25 stdout summaries per mode. Both full test runs include all nine focused cases: eight role/config/fixture controls and the existing owned subprocess cancellation control. Those eight controls perform no environment mutation, provider open or network I/O; the complete suite includes local runtime and subprocess tests.

The four test/lint terminal receipts report reaped owned children, absent owned groups, pipe EOF and removed owned temporary fixtures. Retained log hashes match actual bytes without overflow. The 739-entry source manifests remain identical before and after each gate, and every scoped path matches source-freeze v2:

`3af43219b5834a4ba84e397d88eb4d0a607aab167a01c55a96cd148ed64a029e`

These receipts qualify selector/configuration behavior and the touched service surfaces. No live TiDB/RustFS remote throughput arm or ten-process production workload has run for this change, and no throughput result is claimed. Public report fields contain no credentials, machine paths, endpoint URLs or process identities.

## Selecting the production blob store

Set `MOUNT_RS_REMOTE_SATURATION_BLOCK_PROVIDER=rustfs` for saturation, or
`MOUNT_RS_TARGET_BLOCK_PROVIDER=rustfs` for the production target, with metadata
provider `tidb`. Supply explicit `MOUNT_RS_RUSTFS_ENDPOINT`, `BUCKET`, `REGION`,
`ACCESS_KEY_ID`, `SECRET_ACCESS_KEY` (each prefixed `MOUNT_RS_RUSTFS_`), and
`MOUNT_RS_RUSTFS_DURABLE=1`. The owned fixture additionally requires its original
full `MOUNT_RS_REMOTE_RUSTFS_CID` and matching `MOUNT_RS_BACKING_RUSTFS_OWNER`.
Credentials belong in the private fixture environment.

The existing durable TiDB 3 PD / 3 TiKV gate remains. RustFS additionally checks
its configured loopback endpoint, pinned image, ownership, persistent writable
bind, local Docker socket, and measured VM allocation of at least 10 GiB with
four CPUs. The durability flag is a caller assertion; crash and power-loss
durability need separate tests. Offline TiDB blob preseed is rejected when blobs
are in RustFS.

[Machine-readable report](report.json) retains final receipt hashes, source
hashes, focused case names, formatting, source review, and earlier failed gates.
The [earlier native diagnostic](../tidb-rustfs-marker-diagnostic-20260928/README.md)
contains the first measured TiDB/RustFS results.
