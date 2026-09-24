#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export MOUNT_RS_FOUNDATIONDB_SERVICE_BENCHMARK=1
: "${MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR:?supply a retained output directory outside the disposable harness directory}"
exec sh "$repo_dir/scripts/test-foundationdb.sh"
