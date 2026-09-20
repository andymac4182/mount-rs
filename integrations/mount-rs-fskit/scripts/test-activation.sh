#!/bin/sh
set -eu

# This is an opt-in host acceptance gate. It reports every prerequisite and
# attempts one isolated path-resource mount, but it never treats a compile-only
# or in-process worker pass as FSKit activation evidence.

if [ "$(uname -s)" != "Darwin" ]; then
    echo "SKIP: FSKit activation is macOS-only"
    exit 0
fi

required=${MOUNT_RS_FSKIT_ACTIVATION_REQUIRED:-0}
attempt_mount=${MOUNT_RS_FSKIT_ATTEMPT_MOUNT:-1}
app=${MOUNT_RS_FSKIT_APP:-/tmp/mount-rs-fskit-derived-host-arm64/Build/Products/Debug/mount-rs.app}
short_name=${MOUNT_RS_FSKIT_SHORT_NAME:-mount-rs-fskit}
expected_bundle=${MOUNT_RS_FSKIT_BUNDLE_IDENTIFIER:-com.andymac4182.mount-rs.fskit}
blocked=0

blocker() {
    blocked=1
    echo "BLOCKED: $*"
}

sdkroot=$(xcrun --sdk macosx --show-sdk-path)
sdkversion=$(xcrun --sdk macosx --show-sdk-version)
echo "SDK: macOS ${sdkversion} (${sdkroot})"
test -f "${sdkroot}/System/Library/Frameworks/FSKit.framework/Versions/A/FSKit.tbd" \
    || blocker "FSKit.framework is not present in the selected SDK"

case "$(uname -m)" in
    arm64) swift_target=arm64-apple-macos15.4 ;;
    x86_64) swift_target=x86_64-apple-macos15.4 ;;
    *) blocker "unsupported host architecture: $(uname -m)"; swift_target=arm64-apple-macos15.4 ;;
esac

identity_output=$(security find-identity -v -p codesigning 2>&1 || true)
echo "Code-signing identities:"
echo "${identity_output}"
identity_count=$(printf '%s\n' "${identity_output}" | awk '/valid identities found/ { print $1; exit }')
identity_count=${identity_count:-0}
case "${identity_count}" in
    ''|*[!0-9]*) identity_count=0 ;;
esac
echo "CODESIGN_IDENTITY_COUNT=${identity_count}"
if [ "${identity_count}" -eq 0 ]; then
    blocker "no valid Apple code-signing identity is installed"
fi

if [ -d "${app}" ]; then
    echo "APP=${app}"
    codesign_diagnostics=$(codesign -dvv "${app}" 2>&1 || true)
    echo "${codesign_diagnostics}"
    if ! codesign --verify --deep --strict "${app}" >/dev/null 2>&1; then
        blocker "host bundle is not validly code signed"
    fi
else
    blocker "host bundle does not exist: ${app}"
fi

module_cache=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-fskit-fsclient-cache.XXXXXX")
probe_output=$(mktemp "${TMPDIR:-/tmp}/mount-rs-fskit-fsclient-output.XXXXXX")
cleanup_probe() {
    rm -f "${probe_output}"
    rmdir "${module_cache}" 2>/dev/null || true
}
trap cleanup_probe EXIT

probe_status=0
xcrun swift \
    -module-cache-path "${module_cache}" \
    -sdk "${sdkroot}" \
    -target "${swift_target}" \
    -e 'import Foundation; import Dispatch; import FSKit; let semaphore = DispatchSemaphore(value: 0); FSClient.shared.fetchInstalledExtensions { extensions, error in if let error { print("FSCLIENT_ERROR \(error)") } else { let modules = extensions ?? []; print("FSCLIENT_EXTENSION_COUNT \(modules.count)"); for module in modules { print("FSCLIENT_EXTENSION \(module.bundleIdentifier) enabled=\(module.isEnabled) path=\(module.url.path)") } }; semaphore.signal() }; semaphore.wait()' \
    >"${probe_output}" 2>&1 || probe_status=$?
cat "${probe_output}"
if [ "${probe_status}" -ne 0 ]; then
    blocker "FSClient probe failed with exit ${probe_status}"
elif ! grep -Fq "FSCLIENT_EXTENSION_COUNT" "${probe_output}"; then
    blocker "FSClient did not return an installed-extension list"
elif ! grep -Fq "${expected_bundle} enabled=true" "${probe_output}"; then
    blocker "${expected_bundle} is not installed and enabled in FSClient"
fi

mount_source=''
mount_point=''
mount_succeeded=0
cleanup_mount() {
    if [ "${mount_succeeded}" -eq 1 ]; then
        /sbin/umount "${mount_point}" >/dev/null 2>&1 || true
    fi
    if [ -n "${mount_source}" ]; then
        rm -f "${mount_source}/seed.txt" "${mount_source}/from-mount.txt"
        rmdir "${mount_source}" 2>/dev/null || true
    fi
    if [ -n "${mount_point}" ]; then
        rmdir "${mount_point}" 2>/dev/null || true
    fi
    cleanup_probe
}
trap cleanup_mount EXIT

if [ "${attempt_mount}" = "1" ]; then
    mount_source=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-fskit-source.XXXXXX")
    mount_point=$(mktemp -d "${TMPDIR:-/tmp}/mount-rs-fskit-mount.XXXXXX")
    printf '%s\n' 'mount-rs-fskit activation seed' >"${mount_source}/seed.txt"
    mount_output=$(mktemp "${TMPDIR:-/tmp}/mount-rs-fskit-mount-output.XXXXXX")
    mount_status=0
    /sbin/mount -v -F -t "${short_name}" "${mount_source}" "${mount_point}" \
        >"${mount_output}" 2>&1 || mount_status=$?
    cat "${mount_output}"
    rm -f "${mount_output}"
    if [ "${mount_status}" -ne 0 ]; then
        blocker "mount(8) activation attempt failed with exit ${mount_status}"
    else
        mount_succeeded=1
        if [ ! -f "${mount_point}/seed.txt" ]; then
            blocker "activated mount did not expose the source file"
        else
            printf '%s\n' 'mount-rs-fskit activation write' >"${mount_point}/from-mount.txt"
            if [ "$(cat "${mount_source}/from-mount.txt" 2>/dev/null || true)" != "mount-rs-fskit activation write" ]; then
                blocker "activated mount did not persist a write to the path resource"
            else
                echo "FSKIT_MOUNT_READ_WRITE=PASS"
            fi
        fi
    fi
else
    echo "MOUNT_ATTEMPT=SKIPPED (MOUNT_RS_FSKIT_ATTEMPT_MOUNT=${attempt_mount})"
fi

if [ "${blocked}" -eq 0 ]; then
    echo "FSKIT_ACTIVATION=PASS"
    exit 0
fi

if [ "${required}" = "1" ]; then
    echo "FSKIT_ACTIVATION=FAIL (strict acceptance requested)"
    exit 1
fi

echo "FSKIT_ACTIVATION=BLOCKED (diagnostic mode; set MOUNT_RS_FSKIT_ACTIVATION_REQUIRED=1 to fail)"
exit 0
