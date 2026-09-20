#!/bin/sh
# Strict backend acceptance: never convert missing live credentials into success.
set -eu
: "${MOUNTX_SOURCE:?MOUNTX_SOURCE must identify the pinned upstream checkout}"
: "${R2_ENDPOINT:?Configure the dedicated R2 test endpoint}"
: "${R2_BUCKET:?Configure the dedicated R2 test bucket}"
: "${R2_ACCESS_KEY_ID:?Configure R2 credentials outside source control}"
: "${R2_SECRET_ACCESS_KEY:?Configure R2 credentials outside source control}"
export MOUNT_RS_REQUIRE_R2=1
# test-pglite.sh starts the real local server and sets its required gate only
# after the socket is ready; the preliminary workspace tests run before that.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec sh "$repo_dir/scripts/test-all.sh"
