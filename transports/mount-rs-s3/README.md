# mount-rs S3 gateway

`mount-rs-s3` exposes one or more `mount-rs-core::FsDriver` values through a
path-style S3-compatible HTTP gateway. It is a transport gateway, not a native
filesystem mount: the rootless tests exercise HTTP and S3 semantics only.

The server always binds loopback addresses only. SigV4 credentials authenticate
requests but do not provide TLS or authorize a non-loopback bind; put a
reviewed TLS/mTLS proxy in front of the loopback listener for remote access.
The crate supports the core object operations, ListObjectsV2, ranges and HTTP
conditionals, copy, DeleteObjects, and multipart create/upload/list/complete/
abort using the driver's reserved `.mountx-multipart` staging tree.

Streaming PUT and multipart completion publish through private staging files
and an atomic rename, so a failed integrity check or part read does not replace
an existing object. Drivers that do not advertise `atomic_rename` receive an
explicit `NotImplemented` response for those operations instead of a weaker
direct-write fallback; the staging buffer is bounded by `read_chunk_bytes`.
CopyObject uses the same bounded, cross-driver staging path and atomic
publication contract, so large copies do not accumulate the source object in
memory and failed copies do not replace an existing destination.
ListObjectsV2 uses continuation-aware depth-first traversal with prefix pruning
and retains at most one page plus one look-ahead candidate in memory; empty
directories and `/` delimiter common prefixes retain their S3 response shape.
Multipart and temporary streaming staging are bounded by
`S3SessionOptions::multipart_staging_max_bytes` (8 GiB by default) and reaped
after `multipart_staging_ttl_ms` (24 hours by default) when the driver advertises
timestamp support. Drivers without usable timestamps retain the quota bound and
explicit cleanup contract but are not reaped based on an unavailable mtime.
Capacity failures return `SlowDown`; `DeleteObjects` removes the corresponding
backing staging tree or returns an error instead of reporting an unperformed
deletion.

`cargo test -p mount-rs-s3` is rootless and runs on macOS and Linux. It does
not prove FUSE, NFS, macFUSE, or Linux kernel mount behavior. Native mount
verification is platform-specific and must be performed separately: macOS
requires a compatible macFUSE installation and user-approved mount location;
Linux requires the relevant FUSE/NFS support and user permissions (often
`/dev/fuse` and group membership). No native mount prerequisite is required
for this crate's transport tests.

Known parity boundaries are explicit: ListObjects V1, bucket create/delete,
GET/HEAD `partNumber`, and non-`/` list delimiters remain unsupported. The
`STREAMING-AWS4-HMAC-SHA256-PAYLOAD` and trailer-bearing streaming forms are
decoded; signed chunk and trailer chains are verified, while checksum trailer
values are accepted as framing metadata because that is what the oracle does
(it does not independently recompute CRC/SHA checksums). The bundled Axum
boundary buffers each request body up to the configured limit, so it is not yet
the upstream's fully incremental streaming server.
