# mount-rs-webdav

Portable WebDAV (RFC 4918) request/reply session and HTTP server for an
`Arc<dyn mount_rs_core::FsDriver>`.

The implementation follows the WebDAV source oracle at mountx revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. It implements the class-1 resource
methods `OPTIONS`, `GET`, `HEAD`, `PUT`, `MKCOL`, `DELETE`, `COPY`, `MOVE`, and
bounded `PROPFIND`, with RFC-style path decoding, bounded XML/request bodies,
ETags, dates, byte ranges, recursive transfer/delete, and errno-to-HTTP mapping.
Class-2 locking and `PROPPATCH` are implemented for the driver properties that
the core contract can represent. `PUT` request bodies are consumed as bounded
transport chunks and written incrementally; XML request bodies are bounded and
buffered for parsing. A `PUT` opens its destination at the first non-empty
body chunk, matching the pinned oracle: a request-body failure may therefore
leave the bytes already written, and this transport does not claim atomic PUT
publication. Regular-file GET responses are streamed with positional reads
and are closed on completion or connection shutdown.
When a driver advertises `Capabilities::durable_writes`, every successful
filesystem mutation awaits that driver's `syncfs` barrier before WebDAV
acknowledges it; an unsupported or failed barrier is returned as a request
failure rather than a false durable success. Volatile drivers retain the
successful no-op default. WebDAV locks remain process-local session state and
are not presented as durable locks.

The HTTP integration tests bind an ephemeral loopback TCP socket and run as an
ordinary user on both macOS and Linux. They are protocol tests; they do not
claim native mount verification. `tests/native_mount.rs` is the separate,
ignored harness corresponding to the oracle's `test/webdav/mount.test.ts`.
Run it only with:
`MOUNT_RS_WEBDAV_NATIVE_TEST=1 cargo test -p mount-rs-webdav --test native_mount -- --ignored --nocapture`.
It hard-fails
missing prerequisites when explicitly selected and is never part of ordinary
or CI test runs. Linux requires `mount.davfs`, FUSE (`/dev/fuse` and the
kernel fuse filesystem), root, and `umount`; macOS requires `/sbin/mount_webdav`
and `umount` plus the host's authorization policy. We do not invoke that
harness here.

The oracle intentionally does not store dead WebDAV properties: named unknown
properties are `404`, and `PROPPATCH` set/remove instructions are `403
cannot-modify-protected-property` (except the writable `getlastmodified`
property when the driver advertises `times`). Non-empty MKCOL bodies are
`415`, because extended MKCOL is not defined. Multi-range `Range` headers are
ignored as unsupported and return the complete representation with `200`, not
an invented multipart format. These behaviors are covered by explicit tests.
Unsupported HTTP methods return `405` with the supported method list.

The runtime dependency set is intentionally transport-only: Hyper/Hyper-Util
and Tokio provide the HTTP/async server, Quick-XML and percent-encoding handle
bounded WebDAV XML and paths, URL/base64/httpdate/sha2/UUID implement the
protocol's URI, authentication, date, entity-tag, and lock-token rules, and
`http-body-util`/`bytes` provide bounded and streamed bodies. `reqwest` is
dev-only with default features disabled and Rustls enabled for rootless HTTP
integration tests. No backend filesystem dependency is pulled into the core
crate; there is no currently removable direct runtime dependency identified.
