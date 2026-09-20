# mount-rs-webdav

Portable WebDAV (RFC 4918) request/reply session and HTTP server for an
`Arc<dyn mount_rs_core::FsDriver>`.

The implementation follows the WebDAV source oracle at mountx revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. It implements the class-1 resource
methods `OPTIONS`, `GET`, `HEAD`, `PUT`, `MKCOL`, `DELETE`, `COPY`, `MOVE`, and
bounded `PROPFIND`, with RFC-style path decoding, bounded XML/request bodies,
ETags, dates, byte ranges, recursive transfer/delete, and errno-to-HTTP mapping.
Class-2 locking and `PROPPATCH` are implemented for the driver properties that
the core contract can represent. Incoming requests are bounded and buffered
before dispatch; regular-file GET responses are streamed with positional reads
and are closed on completion or connection shutdown.

The HTTP integration tests bind an ephemeral loopback TCP socket and run as an
ordinary user on both macOS and Linux. They are protocol tests; they do not
claim native mount verification. A real client mount is an external
platform-specific prerequisite: Linux requires a separately installed
`davfs2`/FUSE setup and the relevant privileges or `/etc/fstab` policy; macOS
uses `/sbin/mount_webdav` and its system authorization policy. Neither is
invoked by this crate's test suite.

Known intentional remainder is documented in `src/lib.rs`: dead WebDAV
properties, extended MKCOL bodies, multi-range responses, and a native mount
probe are outside this crate. Unsupported HTTP methods return `405` with the
supported method list.

The runtime dependency set is intentionally transport-only: Hyper/Hyper-Util
and Tokio provide the HTTP/async server, Quick-XML and percent-encoding handle
bounded WebDAV XML and paths, URL/base64/httpdate/sha2/UUID implement the
protocol's URI, authentication, date, entity-tag, and lock-token rules, and
`http-body-util`/`bytes` provide bounded and streamed bodies. `reqwest` is
dev-only with default features disabled and Rustls enabled for rootless HTTP
integration tests. No backend filesystem dependency is pulled into the core
crate; there is no currently removable direct runtime dependency identified.
