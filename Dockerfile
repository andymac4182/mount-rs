# syntax=docker/dockerfile:1.7
#
# The mount-rs CLI, for running `serve-http` and its cluster forwarding in a
# container. Build with `docker build -t mount-rs .`; the release workflow
# (.github/workflows/container.yml) builds it natively for linux/amd64 and
# linux/arm64 and publishes it to ghcr.io/andymac4182/mount-rs.
#
# The HTTP listener binds loopback unless the config sets "allow_non_loopback":
# put a TLS proxy in the same network namespace (a sidecar in the same pod, or
# `--network container:<name>`), or listen on 0.0.0.0 on a private network.
# The forwarding listener may bind any address. See docs/serve-http-runbook.md.

FROM rust:1.95.0-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --locked --release -p mount-rs-cli \
    && install -m 0755 target/release/mount-rs /usr/local/bin/mount-rs \
    && mount-rs --version

FROM debian:trixie-slim
ARG MOUNT_RS_REVISION=unknown
LABEL org.opencontainers.image.title="mount-rs" \
      org.opencontainers.image.description="Filesystem drives over HTTP, NFS, 9P, WebDAV and FUSE" \
      org.opencontainers.image.source="https://github.com/andymac4182/mount-rs" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.revision="${MOUNT_RS_REVISION}"
# CA certificates for object storage and Redis over TLS.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /var/lib/mount-rs \
        --create-home --shell /usr/sbin/nologin mount-rs
COPY --from=build /usr/local/bin/mount-rs /usr/local/bin/mount-rs
USER 10001:10001
WORKDIR /var/lib/mount-rs
# Keep the config and its data here; relative paths in a config resolve
# relative to the config file.
VOLUME ["/var/lib/mount-rs"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/mount-rs"]
CMD ["--help"]
