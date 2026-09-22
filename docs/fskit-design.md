# Native macOS FSKit transport design

Status: design-only checkpoint, 2026-09-20. This document does not add an
FSKit target, install an extension, activate a host extension, or change the
Rust core, CLI, or N-API exports.

The required outcome is a real macOS FSKit mount backed by the existing
platform-neutral filesystem and independently selected metadata/block
providers. FSKit is a new transport boundary, not another filesystem engine
and not a disguised NFS/FUSE fallback.

## Evidence and current state

The requirement is [Native macOS FSKit acceptance](../REQUIREMENTS.md#native-macos-fskit-acceptance).
The current repository and pinned upstream support the following:

| Area | State | Evidence |
| --- | --- | --- |
| Core filesystem and providers | Implemented in the working tree | `mount_rs_core::FsDriver` and the existing metadata/block provider crates are the delegation boundary. Rust provider construction is visible in [crates/mount-rs-sdk/src/providers.rs](../crates/mount-rs-sdk/src/providers.rs). |
| macOS native transport today | Implemented | The NFS native bridge recognises macOS and `/sbin/mount_nfs` in [transports/mount-rs-nfs/src/native.rs](../transports/mount-rs-nfs/src/native.rs). The automatic facade documents NFS as the macOS choice in [transports/mount-rs-auto/src/lib.rs](../transports/mount-rs-auto/src/lib.rs). |
| Node native loading | Implemented, FSKit-independent | The N-API loader has Darwin arm64, Darwin x64, and Darwin-universal artifact paths in [bindings/mount-rs-napi/index.js](../bindings/mount-rs-napi/index.js). Loading one of those artifacts does not imply that a signed or activated FSKit module exists. |
| Existing transport selection | Implemented, no FSKit entry | Rust `Transport` and the N-API `transport` option currently contain only `fuse`, `9p`, and `nfs`; see [transports/mount-rs-auto/src/lib.rs](../transports/mount-rs-auto/src/lib.rs) and [bindings/mount-rs-napi/src/lib.rs](../bindings/mount-rs-napi/src/lib.rs). The pinned upstream has the same union in `/tmp/mountx-source.uWiHfX/src/auto.ts:70-71`, preference at `:293-304`, and dispatch at `:372-387`. |
| FSKit source artifacts in this repository | Missing | A read-only search found no Swift sources, Xcode project/workspace, entitlements file, Swift package, or FSKit target. |
| FSKit source artifacts upstream | Missing | A read-only search of `/tmp/mountx-source.uWiHfX` found no FSKit, `FSVolume`, `FSUnaryFileSystem`, or file-system-extension implementation. |
| Local Apple toolchain | Compile gate verified; runtime still unverified | `uname -m` = `arm64`; `sw_vers` = macOS `26.5.1`, build `25F80`; `xcodebuild -version` = Xcode `26.6`, build `17F113`; SDK = macOS `26.5` at `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX26.5.sdk`. The standalone unsigned target in `transports/mount-rs-fskit/` type-checks and builds with both `arm64-apple-macos15.4` and `x86_64-apple-macos15.4` using `CODE_SIGNING_ALLOWED=NO`. This is not evidence of a valid FSKit entitlement, provisioning profile, extension activation, or mounted I/O. |
| Real FSKit mount and SQLite-on-FSKit behavior | Unverified/missing | No FSKit extension has been installed or activated, and no native mounted-path test has run. |

The pinned upstream's NFS implementation is useful for behavioral parity but
is not an FSKit implementation: `/tmp/mountx-source.uWiHfX/src/nfs/probe.ts:16-145`
limits the native client to Linux and Darwin, while
`/tmp/mountx-source.uWiHfX/src/nfs/mount.ts:21-66,390-426,552-671` handles the
macOS NFS helper, ownership, version, locking, and `nobrowse` rules.

## Apple API facts verified for the design

These links are the official Apple documentation consulted for this checkpoint:

- [FSKit overview](https://developer.apple.com/documentation/fskit?language=o_9)
  describes a user-space filesystem delivered as an app extension. The current
  overview presents the unary filesystem model; multi-volume
  `FSFileSystem` support is not a safe assumption for this design.
- [UnaryFileSystemExtension](https://developer.apple.com/documentation/fskit/unaryfilesystemextension)
  is the extension entry point for an `FSUnaryFileSystem` implementation.
- [FSVolume](https://developer.apple.com/documentation/fskit/fsvolume?changes=__1)
  is the per-volume object. In the installed macOS 26.5 SDK, the corresponding
  Objective-C headers expose `FSVolume.Operations` and
  `FSVolume.ReadWriteOperations`, both under FSKit V1 availability. The
  compile-only target does not implement a volume yet.
- Apple's online [FSVolume.Handler](https://developer.apple.com/documentation/fskit/fsvolume/handler)
  and [FSVolume.ReadWriteHandler](https://developer.apple.com/documentation/fskit/fsvolume/readwritehandler)
  pages describe a newer handler naming/API direction and mark parts of that
  surface beta or preliminary. Those names are not present in the installed
  SDK's `FSVolume.h`; they must not be used until the selected Xcode SDK
  actually exposes and compiles them.
- [FSPathURLResource](https://developer.apple.com/documentation/fskit/fspathurlresource?changes=l_9)
  and [FSResource](https://developer.apple.com/documentation/fskit/fsresource)
  define resources passed to the module. A security-scoped URL/resource must
  be treated as an opaque capability; do not put provider credentials in an
  Info.plist or an unprotected mount argument.
- Apple's [passthrough filesystem](https://developer.apple.com/documentation/FSKit/building-a-passthrough-file-system)
  sample shows an Xcode FSKit app-extension target, an FSKit entitlement, and
  module metadata in `Info.plist`. Its user-facing activation flow includes
  approval through macOS settings. That sample is a packaging reference, not
  proof that this repository can install or activate an extension.
- [FSClient](https://developer.apple.com/documentation/fskit/fsclient?changes=_5_9)
  exposes installed-module discovery and single-volume mount operations. Use
  its capability/error result as part of probing; do not infer availability
  from the presence of the Rust or Node binary.
- The [FSKit module entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.fskit.fsmodule?changes=l_8___6)
  is an entitlement to be supplied and verified by the signed extension
  target. It is not something the runtime can safely manufacture or bypass.
- [FSKit updates](https://developer.apple.com/documentation/updates/fskit)
  call out migration from `FSVolume.Operations` to `FSVolume.Handler` and
  related handler protocols. The implementation must pin the SDK API used and
  avoid copying older sample declarations without a compile check.

## Proposed architecture

### Package boundaries

Add a separate macOS integration, tentatively
`transports/mount-rs-fskit`, with these logical pieces:

1. A Rust host-side worker/control library that depends on the existing core
   and provider crates only. It owns provider construction, mount identity,
   request dispatch, lifecycle, and error translation. It must not depend on
   N-API or implement FSKit callbacks.
2. A thin Swift/Objective-C FSKit app-extension target that implements the
   Apple volume and handler protocols. It owns Apple types, caller context,
   file-system capabilities, and the conversion to a versioned transport
   protocol. It must not contain a second namespace, metadata store, block
   store, or SQLite implementation.
3. A signed macOS host/worker package that starts the Rust side before the
   FSKit volume is mounted and stops it after unmount. The exact containing-app
   and helper arrangement is an implementation decision gated by the sandbox
   and signing experiment below; it must not be inferred from the NAPI addon.

The first implementation should keep the Rust worker as a per-mount process or
service. Embedding the complete async Rust runtime directly in the FSKit
extension would couple the extension ABI and lifecycle to the filesystem core,
make crash/restart ownership ambiguous, and make the Node package an accidental
runtime dependency.

### IPC feasibility result and bounded next gate

The compile-only target intentionally does not select or implement an IPC
protocol. The available Apple guidance narrows the options enough to choose the
next experiment without designing a large wire protocol prematurely:

- Apple's [App Extension Programming Guide](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/ExtensionOverview.html)
  says an extension and its containing app have no direct communication; shared
  data requires a deliberately configured shared container.
- Apple's [App Groups entitlement](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.security.application-groups?changes=_2)
  explicitly permits UNIX-domain sockets between sandboxed group members only
  when the socket is in the app-group container. A random `/tmp` endpoint is
  therefore not a valid default for a sandboxed FSKit extension, and an app
  group would require registered entitlement/account work that is not approved
  here.
- Apple's [ExtensionFoundation app-extension guidance](https://developer.apple.com/documentation/extensionfoundation/adding-support-for-app-extensions-to-your-app)
  says XPC is the better choice for communication with an extension in a
  hardened sandbox. Apple's [XPC model](https://developer.apple.com/documentation/xpc)
  also provides launch-on-demand, process isolation, and crash/restart
  behavior. An XPC service is nevertheless a containing-app bundle component,
  so its ownership and packaging with an FSKit module still need a small
  target-level proof.

Decision for this checkpoint: XPC is the preferred feasibility candidate; UDS
is conditional on a verified app-group container. Neither is implemented yet.
The next bounded experiment is a minimal extension-to-helper liveness test on
the exact SDK/deployment target, with no filesystem operation protocol:

1. Build a minimal XPC service/connection pair and prove the extension can
   establish and close one authenticated session.
2. Record who launches/owns the helper, what happens on extension/helper exit,
   and which entitlements/signing inputs are required.
3. Only if XPC cannot be hosted in the required FSKit package, repeat the same
   liveness test with a socket in a registered app-group container.

Until that result exists, do not add a Rust worker, endpoint, app-group
entitlement, XPC service, or speculative request/response schema.

### Core delegation

The adapter maps current FSKit handler operations to the existing driver
contract:

| FSKit concern | Rust delegation | Required rule |
| --- | --- | --- |
| volume mount/unmount | worker lifecycle plus driver/provider open/close | Mount succeeds only after the worker handshake and provider readiness. Unmount is idempotent and reports a failed synchronization barrier. |
| lookup, create, remove, rename, links | `FsDriver` namespace operations | Preserve the driver's path, permission, read-only, and error semantics; do not resolve paths in Swift. |
| open/read/write/close | core file-handle operations via stable worker handle IDs | Keep handle ownership and offsets in the Rust side; bound read/write sizes and return short reads exactly. |
| attributes, enumeration, statfs | core metadata and directory/stat operations | Convert timestamps, IDs, file types, sizes, cookies, and capabilities without silently widening precision. |
| xattrs and special namespace operations | core operation if supported | Return an explicit unsupported error when the driver cannot provide the operation. Do not emulate persistence in the adapter. |
| synchronize/fsync | core handle sync and provider flush/barrier | A successful response means the selected durability contract was met; it is not a best-effort acknowledgement. |
| caller identity | FSKit context passed in the request | Validate the available identity in Rust and document any macOS-to-core mapping loss. Never treat an extension process identity as the caller. |
| read-only | one policy bit in worker and FSKit capabilities | Advertise read-only before mount and reject every mutating path in Rust as well as in the adapter. |

Provider selection remains independent for metadata and blocks. The worker may
construct memory, SQLite, PGlite, R2, or split compositions already supported
by the repository, subject to their existing credentials and durability
contracts. The FSKit module receives an opaque mount resource/configuration
reference, not provider secrets. A mixed metadata/block configuration must use
the same writer-fence, version, synchronization, and recovery rules as other
transports.

### SQLite-hosting safety

There are two different SQLite cases and both must remain explicit:

1. SQLite used internally as a mount-rs metadata/block provider is a worker
   implementation detail and must obey that provider's own durability tests.
2. A user opens a SQLite database file through the mounted FSKit path. This is
   a native acceptance gate and cannot be inferred from case 1 or from an NFS
   result.

For case 2, the worker/FSKit adapter must prove file locking, concurrent
readers/writers, random writes, truncate, synchronization, reopen, and
recovery after abrupt worker or host interruption. Start with SQLite rollback
journal/`DELETE` mode. Do not claim WAL until shared-memory, locking, barrier,
and recovery tests pass on the actual supported macOS versions. A failed or
unknown durability barrier must reach SQLite as an error, not as a successful
commit.

## Build, signing, and packaging plan

The FSKit deliverable is a signed macOS app-extension package plus its Rust
worker/control artifact. It is not a `.node` file and must not be loaded by
`bindings/mount-rs-napi/index.js` as if it were one. The current
compile-only target is intentionally unsigned and has no containing app.

### Rust side

- Add a separate workspace crate behind `target_os = "macos"` once the design
  is approved. Reuse `mount_rs_core`, provider crates, and the existing error
  types; do not add another filesystem implementation.
- Keep the IPC schema in a small, dependency-minimal module with explicit
  protocol version, maximum frame/payload sizes, cancellation, and typed
  errors. Add wire tests using a fake adapter before any native mount test.
- Produce the worker/control artifact for `aarch64-apple-darwin` first. Add
  `x86_64-apple-darwin` and a universal distribution only after a real
  extension launch and lifecycle test passes for that architecture.
- If the Swift target links Rust rather than spawning a worker, expose only a
  narrow C ABI shim and still keep all filesystem semantics in the Rust side.
  That is a fallback experiment, not the preferred first design.

### Apple side

- Create an Xcode FSKit app-extension target based on the current SDK's
  template and compile against the pinned `FSUnaryFileSystem`/
  `FSUnaryFileSystemOperations` API. For the installed SDK, future volume work
  must use the exact `FSVolume.Operations` and
  `FSVolume.ReadWriteOperations` declarations; do not copy the online
  `FSVolume.Handler` shape into an older SDK.
- The local Xcode 26.6 template's FSKit target sets
  `MACOSX_DEPLOYMENT_TARGET=15.4`. The local header defines FSKit V1 at
  macOS 15.4, V2 at macOS 26.0, and V2.4 at macOS 26.4. The compile-only
  target uses only V1 (`UnaryFileSystemExtension`, `FSUnaryFileSystem`, and
  `FSUnaryFileSystemOperations`) and passed with target triple
  `arm64-apple-macos15.4`.
- Generate and review the extension's `Info.plist` module metadata from the
  target template. Exact keys, short name, resource handling, and CLI mount
  options are SDK-versioned inputs and must be captured in the build review.
- Add and verify `com.apple.developer.fskit.fsmodule` through the normal Apple
  signing/provisioning path. Any app-group, helper, network, or IPC
  entitlement must be justified by the sandbox experiment and explicitly
  reviewed; do not add broad entitlements speculatively.
- Sign the containing app, extension, helper, and nested Rust artifacts as one
  distribution plan. Record team/signing identity, bundle IDs, entitlements,
  SDK, deployment target, and artifact hashes.
- Installation, Settings approval, activation, deactivation, and cleanup are
  user-controlled operations. No implementation or test may disable macOS
  protections or silently install a host extension.

### Supported OS and architecture policy

Current verified development facts are limited to macOS 26.5.1/build 25F80,
arm64, Xcode 26.6/build 17F113, and SDK 26.5. The initial development/acceptance
lane can target that exact arm64 environment, but it is not yet a published
minimum OS or a compatibility claim.

Before advertising support, record a matrix with:

- minimum macOS deployment target justified by the compiled FSKit API
  availability;
- actual OS build and architecture for every native acceptance run;
- arm64 result first, then x86_64 result if Intel support is retained;
- whether a universal artifact is produced and tested, rather than merely
  assembled with `lipo`; and
- the SDK/Xcode and signing/provisioning inputs used for the artifact.

The current **compile** minimum is macOS 15.4 because that is the local FSKit
V1 availability and template deployment target. The skeleton compiles for
arm64 and x86_64 at that deployment target, but this is not a universal or
runtime support claim. Until a signed artifact and real mounted pass exist, the
**functional support** status is unreleased and unsupported, even on the current
development Mac. Linux and non-macOS targets must continue to compile without
the FSKit crate or Apple SDK.

## Public integration contract

The eventual public behavior should be explicit and additive:

1. Add `fskit` as a named transport only after the native target exists. The
   Rust auto facade, CLI/config, and N-API option must all reject it with a
   clear platform/setup error when the current host cannot use it.
2. Add a capability probe that distinguishes unsupported OS/architecture,
   absent or mismatched signed module, missing user approval, unavailable
   worker/IPC, and provider/configuration errors. A probe is not a mounted-I/O
   claim.
3. A named `fskit` request must attempt FSKit exactly once and return its error;
   it must never silently select NFS or FUSE. Automatic selection should not
   include FSKit until a stable capability probe and native lifecycle gate
   exist. If it is later included, the preference and reason must be visible.
4. The Node `Mounted` lifecycle must expose the same explicit unmount/close
   ownership and error behavior as other transports. Loading the Darwin N-API
   artifact must not auto-install, activate, or request approval for FSKit.
5. CLI and Node examples must pass an opaque provider/config reference and
   document where credentials live. They must not serialize secret values into
   `Info.plist`, mount options visible to unrelated processes, or logs.

## Real native acceptance plan

Unit tests, compilation, mocks, another transport's green tests, or a probe
result do not close this gate. The native lane must be opt-in on a disposable,
user-owned macOS machine after the user has approved installation/activation.

Every run records OS/build, architecture, Xcode/SDK, deployment target,
extension bundle ID, signing/entitlements, artifact hashes, provider
composition, mount options, journal mode, mountpoint, and cleanup result.

### Required test groups

| Group | Acceptance cases |
| --- | --- |
| Mount/lifecycle | actual FSKit mount and visible mounted path; repeated mount/unmount; explicit close; forced worker shutdown; extension/worker restart; stale endpoint cleanup; no leaked mount after failure. |
| Namespace/metadata | lookup, create, mkdir, remove, rename, hard link, symlink/readlink, enumeration cookies, stat/statfs, timestamps, ownership, permissions, xattrs where supported, and unsupported-operation errors. |
| I/O | sequential and random reads/writes, short reads/EOF, truncate, append, sparse offsets, concurrent handles, bounded large requests, read-only mount, and error propagation after worker disconnect. |
| Synchronization | file sync, volume synchronize, provider flush, close ordering, failed-barrier reporting, reopen, and durability after process restart. |
| Providers | memory; persistent SQLite metadata/block; PGlite or other supported persistent provider; split metadata/block providers including a mixed composition; version-pinned view where the provider supports it. |
| SQLite hosted on FSKit | actual SQLite transactions, rollback-journal locking/contention, concurrent readers/writers, commit barriers, reopen/integrity check, abrupt interruption, recovery, and an explicit WAL result (supported or rejected with evidence). |
| Concurrency/recovery | many simultaneous operations, open-handle teardown during unmount, cancellation, extension relaunch, worker crash, and exactly-once error completion at the IPC boundary. |
| Performance | mounted-path benchmark with fixed dataset, cache state, provider/durability policy, concurrency, and latency/throughput percentiles; compare only like-for-like mounted runs. |

The native runner must use a unique temporary mountpoint, trap cleanup, verify
the path is unmounted, and fail if cleanup is ambiguous. It must not call
system-wide extension deactivation or delete unrelated installation state.
CI that cannot approve and activate the extension must report the remaining
real-macOS gate rather than marking this suite passed.

## Implementation order and acceptance gates

1. **SDK/API gate.** Compile the standalone unsigned FSKit target with
   `CODE_SIGNING_ALLOWED=NO`; record the exact unary and volume protocol
   declarations, availability, module metadata, and compile minimum. Resolve
   whether the target can establish the bounded XPC/UDS liveness test. Obtain
   explicit approval before any install or activation.
2. **IPC feasibility gate.** Build only the smallest XPC liveness pair first;
   test connect, one request/reply, close, helper exit, and restart. If XPC is
   not viable for the FSKit package, test UDS only inside a verified app-group
   container. Do not specify the filesystem operation protocol until one
   transport passes this gate.
3. **Rust integration gate.** Add the separate macOS crate and worker. Wire
   existing `FsDriver`, file handles, providers, read-only policy, identity,
   version selection, and synchronization. Test provider compositions and
   SQLite-hosting semantics at the Rust boundary.
4. **FSKit adapter gate.** Implement only the current SDK handler protocols,
   resource parsing, capability advertisement, and lifecycle translation.
   Verify that all unsupported operations and worker failures are surfaced.
5. **Packaging gate.** Produce signed arm64 artifacts, inspect entitlements and
   `Info.plist`, install only in the approved native lane, and prove mount,
   unmount, restart, and cleanup. Then decide whether x86_64/universal support
   is worth carrying.
6. **CLI/Node gate.** Add explicit `fskit` options, probe reasons, actionable
   setup errors, and no-fallback behavior. Add examples only after the signed
   native artifact is reproducible.
7. **Acceptance gate.** Run the complete native matrix above, including actual
   SQLite-hosting and benchmark evidence. Publish the supported OS/architecture
   table and remaining limitations; until then keep FSKit marked unsupported.

## Decisions still requiring evidence

- The minimum supported macOS release and whether current beta/preliminary
  handler APIs are acceptable for the release.
- UDS versus XPC after the signed sandbox experiment; the wire schema and
  lifecycle rules must remain identical either way.
- The exact FSKit resource/CLI option representation for an opaque mount
  configuration and how it is protected from disclosure.
- Worker ownership and restart policy across FSKit relaunches.
- Required entitlements, provisioning profile, notarization, and user approval
  flow for the chosen containing-app/package shape.
- Whether SQLite WAL can be supported at all; no WAL promise is made by this
  design.
- Whether automatic selection should ever prefer FSKit over macOS NFS. Explicit
  selection and honest failure are required first.
