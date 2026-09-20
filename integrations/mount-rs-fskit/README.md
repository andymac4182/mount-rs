# mount-rs FSKit compile-only target

This directory is intentionally **not** a Cargo workspace member and is not a
functional mount transport yet. It is the SDK/API gate for the required native
macOS FSKit integration.

The target is derived from the File System Extension template installed with
the local Xcode toolchain. It compiles only the macOS 15.4 FSKit V1 entry-point
surface:

- `UnaryFileSystemExtension`;
- `FSUnaryFileSystem` and `FSUnaryFileSystemOperations`; and
- `FSResource`, `FSTaskOptions`, `FSProbeResult`, and the `FSVolume` callback
  type.

The installed SDK's volume protocols are named `FSVolume.Operations` and
`FSVolume.ReadWriteOperations`. They are deliberately not implemented in this
checkpoint. The newer online `FSVolume.Handler` names are not available in the
installed SDK and must not be copied into this target.

The installed SDK defines `FSKIT_API_AVAILABILITY_V1` as macOS 15.4. The same
SDK defines later FSKit APIs at macOS 26.0 and 26.4. This target deliberately
does not reference those later APIs, so `MACOSX_DEPLOYMENT_TARGET=15.4` is the
verified compile minimum for this skeleton. It is not yet the supported minimum
for a functional mount-rs release.

## Compile-only proof

The command below performs an unsigned build against the installed SDK. It
does not install, activate, package, or sign the extension:

```sh
xcodebuild \
  -project integrations/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsFSKit \
  -configuration Debug \
  -sdk macosx26.5 \
  -derivedDataPath /tmp/mount-rs-fskit-derived \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  build
```

The same command was also run with `-arch x86_64` and a separate derived-data
directory. Both `arm64-apple-macos15.4` and `x86_64-apple-macos15.4` compile
and link successfully against the installed macOS 26.5 SDK.

The generated product is only compiler evidence. Because the target has no
containing application, no provisioning profile, and no signing identity, it
must not be copied into an installed application or passed to FSKit activation.

The compile-only class intentionally returns `.notRecognized`/an explicit
compile-only error. It does not claim a mount, resource format, IPC endpoint,
provider, or filesystem operation is implemented.

## Manifest and entitlement boundary

`Resources/Info.plist` uses the local Xcode FSKit template's
`EXAppExtensionAttributes` keys and extension point
`com.apple.fskit.fsmodule`. The empty resource capability flags are deliberate:
this checkpoint does not advertise path, generic URL, server, or block resource
support. `Resources/MountRsFSKit.entitlements` contains only the FSKit module
entitlement copied from the installed template; no app-group, network, helper,
or account-specific entitlement is present.

The target remains outside the root Cargo workspace until the Rust worker,
provider wiring, and signed containing-app/package plan are approved.

## IPC feasibility checkpoint

No IPC code is included here. Apple documents that sandboxed apps in an app
group can use UNIX-domain sockets only when the socket lives in the app-group
container, and that the app-group entitlement must be registered. That makes a
random `/tmp` socket an invalid default for a future sandboxed FSKit extension.
See [App Groups entitlement](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.security.application-groups?changes=_2).

Apple's ExtensionFoundation guidance says XPC is the better option for
communication with an app extension in a hardened sandbox, and Apple's XPC
service model supplies launch-on-demand, crash restart, and process isolation.
See [Adding support for app extensions](https://developer.apple.com/documentation/extensionfoundation/adding-support-for-app-extensions-to-your-app)
and [XPC](https://developer.apple.com/documentation/xpc).

Therefore the next implementation gate should prototype a minimal signed
extension-to-worker XPC connection first. A UDS design remains possible only
with a verified app-group/container entitlement and a lifecycle test. This
compile-only target does not select or add either transport.

## Local evidence

The compile gate was prepared against:

```text
macOS 26.5.1 (build 25F80), arm64
Xcode 26.6 (build 17F113)
macOS SDK 26.5
SDK path: /Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.5.sdk
FSKit template: .../macOS/Application Extension/File System Extension.xctemplate
```

No extension installation, activation, account signing, provisioning, or
host-system mutation is part of this checkpoint.
