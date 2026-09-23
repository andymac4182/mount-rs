# mount-rs RustFS provider

`RustFsConfig` builds a signed, path-style S3 client for a RustFS endpoint.
`RustFsBlockStore::from_config(&config, prefix, durable)` stores immutable blocks in one
bucket prefix. The shared `mount-rs-object-store-blocks` crate enforces block
identity, conditional creation, and prefix-scoped reconciliation.

RustFS stores file contents, not the mutable namespace. For two writable
mounts, compose these blocks with metadata that supports revision CAS, such as
FoundationDB or PGlite. Every process and host must use the same RustFS bucket
and block prefix, as well as the same metadata volume.
Declare `durable` for the actual service topology rather than relying on the
client type to infer it.

HTTP endpoints are restricted to loopback and the disposable Docker test
gateway. Use HTTPS for remote services. Endpoints contain only a host and
optional trailing slash; paths are rejected because object-store errors may
include the request URL. The configuration's `Debug` output redacts credentials.
