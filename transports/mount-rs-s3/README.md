# mount-rs S3 gateway

`mount-rs-s3` exposes one or more `mount-rs-core::FsDriver` values through a
path-style S3-compatible HTTP gateway. It is a transport gateway, not a native
filesystem mount: the rootless tests exercise HTTP and S3 semantics only.

The unauthenticated server binds loopback addresses only. Configure SigV4
credentials before binding a non-loopback address. The crate supports the
core object operations, ListObjectsV2, ranges and HTTP conditionals, copy,
DeleteObjects, and multipart create/upload/list/complete/abort using the
driver's reserved `.mountx-multipart` staging tree.

`cargo test -p mount-rs-s3` is rootless and runs on macOS and Linux. It does
not prove FUSE, NFS, macFUSE, or Linux kernel mount behavior. Native mount
verification is platform-specific and must be performed separately: macOS
requires a compatible macFUSE installation and user-approved mount location;
Linux requires the relevant FUSE/NFS support and user permissions (often
`/dev/fuse` and group membership). No native mount prerequisite is required
for this crate's transport tests.

Known parity boundaries are explicit: ListObjects V1, bucket create/delete,
GET/HEAD `partNumber`, and non-`/` list delimiters remain unsupported. The
`STREAMING-AWS4-HMAC-SHA256-PAYLOAD` form is verified, including its chunk
signature chain; trailer-bearing streaming forms and checksum trailer
verification currently return `NotImplemented`. The bundled Axum boundary
buffers each request body up to the configured limit, so it is not yet the
upstream's fully incremental streaming server.
