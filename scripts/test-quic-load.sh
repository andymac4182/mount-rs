#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

case "${1:-ci}" in
  ci)
    exec ./scripts/cargo-shared test -p mount-rs-service --test quic_load --offline --locked -- --nocapture
    ;;
  soak)
    exec ./scripts/cargo-shared test -p mount-rs-service --test quic_load --offline --locked bounded_quic_soak -- --ignored --nocapture
    ;;
  *)
    echo "usage: $0 [ci|soak]" >&2
    exit 2
    ;;
esac
