# mount-rs FSKit V1 and Rust-worker checkpoint

This directory contains the macOS FSKit V1 adapter, its standalone Rust
worker bridge, and a minimal containing application. It is an unsigned
compile/package/test checkpoint, not evidence of an installed or mounted
FSKit product.

The extension target uses the FSKit V1 surface available in the installed SDK
(`FSVolume.Operations`, `FSVolume.OpenCloseOperations`, and
`FSVolume.ReadWriteOperations`, with a macOS 15.4 deployment floor). The
implemented `MountRsFSVolume` translates item identity, metadata, namespace
operations, open/close state, and read/write callbacks into
`MountRsWorkerClient` calls. Swift does not implement a second filesystem.

On macOS 26 and later, FSKit supplies an `FSPathURLResource` for a path-backed
mount. `MountRsFileSystem` maps that resource to the existing rooted
`mount-rs-host` driver and keeps the Rust worker in the FSKit extension, so
each loaded resource has its own root and does not share a global XPC path.
The direct worker lifecycle test below writes bytes through this path-backed
configuration and reopens them through a second worker.

The standalone `mount-rs-fskit-bridge` crate owns the bounded frame protocol,
the JSON operation schema, persistent opaque handle table, read-only policy,
provider error conversion, and async `mount_rs_core::FsDriver` dispatch. Its
bundled C-ABI constructor accepts a bounded backend configuration for
`MemoryFs`, the rooted `mount-rs-host` driver, the durable single-database
`mount-rs-sqlite` driver, or the durable split-store composition of
`SqliteMetadataStore` and `SqliteBlockStore`. The split configuration requires
distinct metadata and block database paths and releases its writer lease
during worker shutdown.

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

It exercises the memory backend's chunked binary I/O, the host-backed
FSKit-style in-process path lifecycle, and both SQLite backends' reopen
behavior, including sparse/partial writes and byte-exact reads through new
workers. The test is not a substitute for a containing application: no
service is signed, embedded, installed, launched by launchd, or activated
through FSKit here.

The standalone service defaults to memory. A containing host can select a
provider at launch with these environment values (the host remains responsible
for supplying the paths and lifecycle):

```text
MOUNT_RS_FSKIT_BACKEND=memory|host|sqlite|splitSqlite
MOUNT_RS_FSKIT_ROOT=/path/to/host-root                         # host
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

The Rust C ABI rejects request lengths above 1 MiB plus the 20-byte header
before forming a slice of caller memory. Non-null C pointers still require
valid caller-owned regions; the numeric guard does not establish their lifetime
or non-aliasing.

Malformed outer frames produce no fabricated filesystem response. Operation
errors retain the Rust provider's errno/code/path/syscall information.
Data operations use fixed 128 KiB chunks; the Swift client transparently
splits larger FSKit reads and writes while preserving the caller's offset.

## Containing application

`MountRsHost` is intentionally a minimal `NSApplication` target. Its only
filesystem responsibility is embedding `MountRsFSKit.appex` in
`Contents/Extensions`, which is the ExtensionKit location required by macOS.
The Rust driver remains in the extension; the app is the installable container
that can later be signed, installed, and enabled in System Settings.

The current host has no valid Apple signing identity, so unsigned builds can
verify the bundle layout and code paths but cannot establish FSKit activation.
The signing gate must use a provisioning profile authorized for
`com.apple.developer.fskit.fsmodule`, then be followed by the user approval
step and a real `mount(8)`/`FSClient` lifecycle test.

## Rust-only alternatives reviewed

There is no maintained Rust-only FSKit target that removes Apple's app
extension boundary. The `fsk` crate provides a cross-platform Rust filesystem
trait and an install-once Swift bridge; `FSKitBridge` provides the same
architecture through a local backend protocol. Both are useful references,
but adopting either would still retain Swift and add another runtime boundary.
mount-rs therefore keeps the Swift layer limited to FSKit callback translation
and Rust ABI calls while the filesystem, storage, locking, and lifecycle logic
remain in Rust.

## Verified commands

Run from the repository root for Rust:

```sh
./scripts/cargo-shared fmt --manifest-path transports/mount-rs-fskit/Cargo.toml -- --check
./scripts/cargo-shared test --manifest-path transports/mount-rs-fskit/Cargo.toml --locked
./scripts/cargo-shared clippy --manifest-path transports/mount-rs-fskit/Cargo.toml \
  --all-targets --locked -- -D warnings
MACOSX_DEPLOYMENT_TARGET=15.4 ./scripts/cargo-shared build \
  --manifest-path transports/mount-rs-fskit/Cargo.toml \
  --locked --target aarch64-apple-darwin
```

Build the Rust archive before either XPC target. The deployment-floor
environment is required so Rust's object files link cleanly with the Swift
targets' `MACOSX_DEPLOYMENT_TARGET=15.4`; omitting it can produce a newer-SDK
object warning at the XPC link step. The XPC target selects the Rust archive
from `target/aarch64-apple-darwin/debug` for arm64 and
`target/x86_64-apple-darwin/debug` for x86_64.

The current Rust suite has 12 passing tests, including fixed wire bytes,
malformed input, caller-buffer sizing, Swift/Rust camel-case operation keys,
provider errno propagation, read-only rejection, handle shutdown, and real
`FsDriver` namespace/read/write dispatch, a rooted host-path binary round trip,
plus a durable split-SQLite binary reopen test.

The Swift frame seam test is standalone and does not start XPC:

```sh
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
MODULE_CACHE=/tmp/mount-rs-fskit-module-cache
mkdir -p "$MODULE_CACHE"
xcrun swiftc -module-cache-path "$MODULE_CACHE" \
  -target arm64-apple-macos15.4 \
  -sdk "$SDKROOT" \
  -swift-version 5 \
  transports/mount-rs-fskit/Sources/MountRsXPCDelegate.swift \
  transports/mount-rs-fskit/Tests/MountRsXPCDelegateTests.swift \
  -o /tmp/mount-rs-fskit-delegate-tests && \
  /tmp/mount-rs-fskit-delegate-tests
```

The real in-process XPC lifecycle test links the arm64 Rust static library:

```sh
cd transports/mount-rs-fskit
. ../../scripts/cargo-shared-env.sh
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
MODULE_CACHE=/tmp/mount-rs-fskit-module-cache-xpc
mkdir -p "$MODULE_CACHE"
xcrun swiftc -module-cache-path "$MODULE_CACHE" -target arm64-apple-macos15.4 -sdk "$SDKROOT" -swift-version 5 \
  Sources/MountRsXPCDelegate.swift Sources/MountRsRustWorker.swift \
  Tests/MountRsXPCServiceLifecycleTests.swift \
  -L "$CARGO_TARGET_DIR/aarch64-apple-darwin/debug" -lmount_rs_fskit_bridge \
  -o /tmp/mount-rs-fskit-xpc-lifecycle-tests
/tmp/mount-rs-fskit-xpc-lifecycle-tests
```

The project gates are unsigned and use the local macOS 26.5 SDK:

```sh
. scripts/cargo-shared-env.sh

xcodebuild -project transports/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsFSKit -configuration Debug -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived-volume-arm64 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build

xcodebuild -project transports/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsXPCService -configuration Debug -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived-xpc-arm64 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build

xcodebuild -project transports/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsHost -configuration Debug -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived-host-arm64 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build

test -f /tmp/mount-rs-fskit-derived-host-arm64/Build/Products/Debug/mount-rs.app/Contents/Extensions/MountRsFSKit.appex/Contents/MacOS/MountRsFSKit
```

The Xcode project first searches the exported `CARGO_TARGET_DIR` and keeps
the old project-local target path as a compatibility fallback. This prevents
the Swift targets from silently linking a stale Rust archive when the shared
Cargo target wrapper is used.

The opt-in activation gate reports the installed FSKit modules through
`FSClient`, validates the host bundle's signing state, and attempts one
isolated path-resource mount with `mount(8)`. It only reports activation as a
pass after a read and write through the mounted path; compile-only, in-process,
and uninstalled bundles remain explicit blockers:

```sh
MOUNT_RS_FSKIT_ACTIVATION_REQUIRED=1 \
MOUNT_RS_FSKIT_APP=/tmp/mount-rs-fskit-derived-host-arm64/Build/Products/Debug/mount-rs.app \
transports/mount-rs-fskit/scripts/test-activation.sh
```

Without `MOUNT_RS_FSKIT_ACTIVATION_REQUIRED=1`, the script is diagnostic-only
and exits successfully after printing `FSKIT_ACTIVATION=BLOCKED` when signing,
installation, or host privileges are unavailable. This is intentional: an
unsigned app extension must never be presented as an activated filesystem.

All three arm64 targets build and link. The extension also builds for x86_64
against the SDK. The XPC service cannot currently link x86_64 because the
local Rust toolchain has `aarch64-apple-darwin` but not
`x86_64-apple-darwin`; no toolchain target was installed for this checkpoint.

## Explicit boundaries

This checkpoint does not claim full FSKit acceptance. The extension and host
targets are deliberately unsigned (`CODE_SIGNING_ALLOWED=NO`); the extension
is skipped from direct install (`SKIP_INSTALL=YES`) and is only embedded in the
unsigned host package. The project still lacks a valid provisioning profile,
launchd registration, and FSKit activation test. The in-process lifecycle
tests are therefore transport and worker evidence only, not installed-service
or mounted-volume evidence.
Optional FSKit surfaces such as xattrs, kernel-offloaded I/O,
extent/preallocation, and special-node creation remain separate gaps. Those
gaps and the host packaging path must be closed and tested before any release
or mount claim.

The local SDK evidence used here is macOS 26.5.1 (build 25F80), arm64, with
Xcode 26.6 (build 17F113). No host-system FSKit state was changed.
