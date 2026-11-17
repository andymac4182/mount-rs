# Support matrix

Which operating systems and architectures the `mount-rs` CLI is built for,
what is tested on each, and which features are missing where.

Every row is taken from a workflow in `.github/workflows/`. A platform that
no workflow builds or tests is not listed. When a workflow's matrix changes,
change this page in the same pull request.

Every CLI release is a **preview**: `cli-release.yml` publishes only
`v<version>-cli-preview*` tags, as GitHub prereleases.

## Release targets

Built by [`cli-release.yml`](../.github/workflows/cli-release.yml). Each
target is compiled on a runner of its own operating system and architecture;
nothing is cross-compiled. All targets use the CLI's default features.

| Target | Runner | Archive |
|--------|--------|---------|
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `mount-rs-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `mount-rs-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `mount-rs-<version>-x86_64-apple-darwin.tar.gz` |
| `aarch64-apple-darwin` | `macos-15` | `mount-rs-<version>-aarch64-apple-darwin.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-2025` | `mount-rs-<version>-x86_64-pc-windows-msvc.tar.gz` |

Each archive holds one file, `mount-rs` (`mount-rs.exe` on Windows; Windows
10 and later unpack it with the built-in `tar -xzf`). A release also carries
`release-manifest-<target>.json` and `release-sbom-<target>.json` for each
target, and one `SHA256SUMS` covering all of them.

The first release to carry all five targets is the first preview tag pushed
after this workflow change. `v0.2.0-cli-preview` and earlier carry
`aarch64-apple-darwin` only.

Limits that follow from how the targets are built:

- The Linux binaries link the GNU C library dynamically and are built on
  Ubuntu 24.04 (glibc 2.39). No workflow runs them on an older distribution,
  and there is no musl build.
- There is no Windows on Arm build and no 32-bit build.
- The macOS and Windows binaries are not code-signed or notarized.

The container image is separate. [`container.yml`](../.github/workflows/container.yml)
builds it for `linux/amd64` (`ubuntu-latest`) and `linux/arm64`
(`ubuntu-24.04-arm`), tests `serve-http`, persistence and shutdown in each
image, and publishes both under one tag on the same preview tags.

## What is tested on each target

| Target | CLI tests before packaging | Packaged binary smoke test | Full `serve-http` end-to-end test | Two-node cluster end-to-end test |
|--------|----------------------------|----------------------------|-----------------------------------|----------------------------------|
| `x86_64-unknown-linux-gnu` | yes | yes | yes, `serve-http-e2e` | yes, `serve-http-e2e` |
| `aarch64-unknown-linux-gnu` | yes | yes | yes, `serve-http-e2e-platforms` | no |
| `x86_64-apple-darwin` | yes | yes | yes, `serve-http-e2e-platforms` | no |
| `aarch64-apple-darwin` | yes | yes | yes, `serve-http-e2e-platforms` | no |
| `x86_64-pc-windows-msvc` | yes | yes | yes, `serve-http-e2e-platforms` | no |

- **CLI tests before packaging** (`cli-release.yml`, on every release and
  every build-only run): `cargo test --locked -p mount-rs-cli --all-targets`
  on the release runner. The subprocess tests in
  `apps/mount-rs-cli/tests/http_subprocess.rs` are Unix-only, so on Windows
  this column does not start the binary; the next two columns do.
- **Packaged binary smoke test** (`cli-release.yml`, same runs): the archive
  that would be published is unpacked and its binary is run on the runner
  that built it. `scripts/serve-http-e2e.py --smoke` checks `--version`,
  starts `serve-http` on loopback with a durable SQLite split-store drive,
  checks `/healthz`, `/readyz` and bearer-token refusal, creates a
  directory, puts a file, reads it back whole and by range, lists the
  directory, overwrites, truncates, renames and deletes, stops the server
  gracefully and requires exit code 0, restarts it and requires the data to
  be there, checks a second process on the same drive is refused, and stops
  gracefully a second way. A target whose smoke test fails is not published,
  and neither is any other target of that release.
- **Full `serve-http` end-to-end test** ([`ci.yml`](../.github/workflows/ci.yml),
  on every push and pull request): `scripts/serve-http-e2e.py` without
  `--smoke`, against a binary built from that commit. It adds path traversal
  and encoding refusals, the request, upload and directory-listing limits,
  parallel reads and writes, racing writers to one file, and an upload in
  flight when the stop is requested. It runs on `ubuntu-latest` in
  `serve-http-e2e`, and on `ubuntu-24.04-arm`, `macos-15`, `macos-15-intel`
  and `windows-2025` in `serve-http-e2e-platforms`.
- **Two-node cluster end-to-end test** (`ci.yml`, job `serve-http-e2e`):
  `scripts/serve-http-cluster-e2e.py`, two owners forwarding to each other
  through Redis discovery and mutually authenticated HTTP/3. Linux x86_64
  only: it needs a Redis service container, and GitHub runs service
  containers only on Linux runners. Cluster forwarding is not exercised as a
  running binary on macOS, Windows or Linux arm64.
- **Mixed-release cluster test** (`ci.yml`, job `cluster-mixed-release`):
  the same script with one server built from the commit under test and one
  from a pinned earlier commit of `main` (`031050cc`), in both arrangements.
  Linux x86_64 only, for the same reason. It tests that one pair of builds,
  not a pair of published releases; see
  [`serve-http-upgrade.md`](serve-http-upgrade.md#clusters).

The workspace's unit and integration tests (`ci.yml`, job `rust`) run on
`ubuntu-latest`, `macos-latest` and `windows-latest`, that is Linux x86_64,
macOS arm64 and Windows x86_64. They do not run on Linux arm64 or macOS
x86_64; on those two targets only the `mount-rs-cli` tests run, in
`cli-release.yml`.

## Graceful shutdown

`serve-http` drains in-flight requests, releases its writer leases and exits
0 when asked to stop. What asks it, and what the tests send:

| Platform | Graceful stop | Tested by the end-to-end and smoke tests | Not graceful |
|----------|---------------|------------------------------------------|--------------|
| Linux, macOS | `SIGTERM`, `SIGINT` (Ctrl-C) | `SIGTERM`, then `SIGINT` after the restart | `SIGKILL` |
| Windows | Ctrl-C, Ctrl-Break, closing the console window, system shutdown (how a Windows container is stopped) | Ctrl-C, then Ctrl-Break after the restart, each raised as a console control event on the server's own console | `taskkill /F`, Task Manager "End task", `TerminateProcess` |

Windows limits:

- There is no `SIGTERM`. A supervisor that can only terminate the process
  cannot stop `mount-rs` gracefully; the split-store writer lease then stays
  held until it expires.
- After the console window is closed or the system shuts down, Windows ends
  the process about five seconds later whether or not the drain has
  finished. A `drain_timeout_ms` above that is not honoured for those two
  events.
- Closing the console window and system shutdown are handled in the code but
  no workflow triggers them.
- User logoff is deliberately not treated as a stop.

## Features by platform

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| `serve-http` (HTTP API over loopback or a private network) | yes | yes | yes |
| `mount` over FUSE | yes | no | no |
| `mount` over 9P | yes | no | no |
| `mount` over NFS | yes (root) | yes | no |
| `mount` at all | yes | yes, NFS only | **no**: no transport can mount; `mount-rs probe` lists the reason for each |
| `SIGTERM` graceful stop | yes | yes | no, see above |
| FoundationDB metadata provider | not in release binaries | not in release binaries | not available |

- FUSE and 9P are Linux kernel interfaces. `transports/mount-rs-auto`
  reports both as unusable on every other platform, and NFS as unusable on
  anything but Linux and macOS, so on Windows the CLI is a `serve-http`
  (and `validate-config`, `probe`) binary, not a mounting one.
- Native mounts are tested in `ci.yml` on `ubuntu-latest` (FUSE, 9P, NFS)
  and `macos-latest` (NFS). No workflow tests a native mount on Linux arm64
  or macOS x86_64.
- The FoundationDB provider is behind the `foundationdb` cargo feature,
  which needs the FoundationDB client library at build and run time. Release
  binaries are built with default features and do not include it. The
  dependency is declared for Linux and macOS on x86_64 and aarch64 only.
