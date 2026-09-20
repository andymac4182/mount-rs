# mount-rs FSKit V1 and Rust-worker checkpoint

This directory contains the macOS FSKit V1 adapter and its standalone Rust
worker bridge. It is an unsigned compile/test checkpoint, not evidence of an
installed or mounted FSKit product.

The extension target uses the FSKit V1 surface available in the installed SDK
(`FSVolume.Operations`, `FSVolume.OpenCloseOperations`, and
`FSVolume.ReadWriteOperations`, with a macOS 15.4 deployment floor). The
implemented `MountRsFSVolume` translates item identity, metadata, namespace
operations, open/close state, and read/write callbacks into
`MountRsWorkerClient` calls. Swift does not implement a second filesystem.

The standalone `mount-rs-fskit-bridge` crate owns the bounded frame protocol,
the JSON operation schema, persistent opaque handle table, read-only policy,
provider error conversion, and async `mount_rs_core::FsDriver` dispatch. Its
bundled C-ABI constructor accepts a bounded backend configuration for
`MemoryFs`, the durable single-database `mount-rs-sqlite` driver, or the
durable split-store composition of `SqliteMetadataStore` and
`SqliteBlockStore`. The split configuration requires distinct metadata and
block database paths and releases its writer lease during worker shutdown.

## XPC service

`MountRsXPCService` is a separate XPC service target. Its real
`XPCListener(service:)` receives typed Codable messages, passes the bounded
inner frame to `MountRsRustWorker`, and keeps the Rust worker alive for the
service lifetime. The service listener uses the default active initialization;
the test and service code do not double-activate it.

The in-process lifecycle test creates an anonymous `XPCListener` and
`XPCSession`, then verifies this path end to end:

```text
XPCSession -> MountRsXPCSessionTransport -> MountRsWorkerClient
           -> MountRsRustWorker -> Rust DriverWorker -> selected FsDriver
```

It exercises the memory backend's chunked binary I/O and both SQLite backends'
reopen behavior, including sparse/partial writes and byte-exact reads through
new XPC workers. The test is not a substitute for a containing application:
no service is signed, embedded, installed, launched by launchd, or activated
through FSKit here.

The standalone service defaults to memory. A containing host can select a
provider at launch with these environment values (the host remains responsible
for supplying the paths and lifecycle):

```text
MOUNT_RS_FSKIT_BACKEND=memory|sqlite|splitSqlite
MOUNT_RS_FSKIT_DATABASE_PATH=/path/to/filesystem.sqlite       # sqlite
MOUNT_RS_FSKIT_METADATA_PATH=/path/to/metadata.sqlite         # splitSqlite
MOUNT_RS_FSKIT_BLOCKS_PATH=/path/to/blocks.sqlite             # splitSqlite
MOUNT_RS_FSKIT_CHUNK_SIZE=65536                               # optional
MOUNT_RS_FSKIT_READ_ONLY=1                                   # optional
```

The inner frame is little-endian and bounded to a 1 MiB body:

```text
MRFS magic[4]
version:u16, kind:u8, flags:u8
request_id:u64, body_length:u32, body[body_length]
```

Malformed outer frames produce no fabricated filesystem response. Operation
errors retain the Rust provider's errno/code/path/syscall information.
Data operations use fixed 128 KiB chunks; the Swift client transparently
splits larger FSKit reads and writes while preserving the caller's offset.

## Verified commands

Run from the repository root for Rust:

```sh
cargo fmt --manifest-path integrations/mount-rs-fskit/Cargo.toml -- --check
cargo test --manifest-path integrations/mount-rs-fskit/Cargo.toml --locked
cargo clippy --manifest-path integrations/mount-rs-fskit/Cargo.toml \
  --all-targets --locked -- -D warnings
MACOSX_DEPLOYMENT_TARGET=15.4 cargo build \
  --manifest-path integrations/mount-rs-fskit/Cargo.toml \
  --locked --target aarch64-apple-darwin
```

Build the Rust archive before either XPC target. The deployment-floor
environment is required so Rust's object files link cleanly with the Swift
targets' `MACOSX_DEPLOYMENT_TARGET=15.4`; omitting it can produce a newer-SDK
object warning at the XPC link step. The XPC target selects the Rust archive
from `target/aarch64-apple-darwin/debug` for arm64 and
`target/x86_64-apple-darwin/debug` for x86_64.

The current Rust suite has 11 passing tests, including fixed wire bytes,
malformed input, caller-buffer sizing, Swift/Rust camel-case operation keys,
provider errno propagation, read-only rejection, handle shutdown, and real
`FsDriver` namespace/read/write dispatch plus a durable split-SQLite binary
reopen test.

The Swift frame seam test is standalone and does not start XPC:

```sh
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
MODULE_CACHE=/tmp/mount-rs-fskit-module-cache
mkdir -p "$MODULE_CACHE"
xcrun swiftc -module-cache-path "$MODULE_CACHE" \
  -target arm64-apple-macos15.4 \
  -sdk "$SDKROOT" \
  -swift-version 5 \
  integrations/mount-rs-fskit/Sources/MountRsXPCDelegate.swift \
  integrations/mount-rs-fskit/Tests/MountRsXPCDelegateTests.swift \
  -o /tmp/mount-rs-fskit-delegate-tests && \
  /tmp/mount-rs-fskit-delegate-tests
```

The real in-process XPC lifecycle test links the arm64 Rust static library:

```sh
cd integrations/mount-rs-fskit
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
MODULE_CACHE=/tmp/mount-rs-fskit-module-cache-xpc
mkdir -p "$MODULE_CACHE"
xcrun swiftc -module-cache-path "$MODULE_CACHE" -target arm64-apple-macos15.4 -sdk "$SDKROOT" -swift-version 5 \
  Sources/MountRsXPCDelegate.swift Sources/MountRsRustWorker.swift \
  Tests/MountRsXPCServiceLifecycleTests.swift \
  -L target/aarch64-apple-darwin/debug -lmount_rs_fskit_bridge \
  -o /tmp/mount-rs-fskit-xpc-lifecycle-tests
/tmp/mount-rs-fskit-xpc-lifecycle-tests
```

The project gates are unsigned and use the local macOS 26.5 SDK:

```sh
xcodebuild -project integrations/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsFSKit -configuration Debug -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived-volume-arm64 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build

xcodebuild -project integrations/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsXPCService -configuration Debug -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived-xpc-arm64 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
```

Both arm64 targets build and link. The extension also builds for x86_64
against the SDK. The XPC service cannot currently link x86_64 because the
local Rust toolchain has `aarch64-apple-darwin` but not
`x86_64-apple-darwin`; no toolchain target was installed for this checkpoint.

## Explicit boundaries

This checkpoint does not claim full FSKit acceptance. The Xcode targets are
deliberately unsigned (`CODE_SIGNING_ALLOWED=NO`) and skipped from install
(`SKIP_INSTALL=YES`); this project does not contain a containing application,
embedding phase, provisioning profile, launchd registration, or FSKit
activation test. The in-process XPC lifecycle test is therefore transport and
worker evidence only, not installed-service or mounted-volume evidence.
Optional FSKit surfaces such as xattrs, kernel-offloaded I/O,
extent/preallocation, and special-node creation remain separate gaps. Those
gaps and the host packaging path must be closed and tested before any release
or mount claim.

The local SDK evidence used here is macOS 26.5.1 (build 25F80), arm64, with
Xcode 26.6 (build 17F113). No host-system FSKit state was changed.
