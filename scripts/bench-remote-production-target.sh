#!/usr/bin/env bash
# Actual independent-process, online mixed-file target. Never starts a provider.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MOUNT_RS_TARGET_MODE:?Set full or control explicitly}"
: "${MOUNT_RS_TARGET_OUTPUT:?Set a new retained output directory}"
case "$(uname -s)" in
  Darwin) ;;
  Linux) getconf GNU_LIBC_VERSION >/dev/null 2>&1 || { echo 'GNU Linux required' >&2; exit 2; } ;;
  *) echo 'Production target supports macOS or GNU Linux only' >&2; exit 2 ;;
esac
if [[ -e "$MOUNT_RS_TARGET_OUTPUT/terminal.json" ]]; then
  echo 'Refusing preexisting terminal artifact' >&2
  exit 2
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/mount-rs-public-compact-selection-cargo-target}"
status=0
./scripts/cargo-shared test --locked -p mount-rs-service --features resource-profiling \
  --test quic_production_target production_target_controller -- --ignored --exact --nocapture || status=$?
if (( status != 0 )); then exit "$status"; fi
python3 - <<'PY'
import json, os, pathlib, sys
try:
    value=json.loads((pathlib.Path(os.environ['MOUNT_RS_TARGET_OUTPUT'])/'terminal.json').read_text())
    if value['phase']!='terminal' or value['outcome']!='success': raise ValueError('unsuccessful terminal')
    if value['full_target']!=(os.environ['MOUNT_RS_TARGET_MODE']=='full'): raise ValueError('mode mismatch')
    workers=value['workers']
    if len(workers)!=10 or len({w['pid'] for w in workers})!=10: raise ValueError('worker count/identity mismatch')
    if not all(w['reap_confirmed'] is True and w['exit_code']==0 for w in workers): raise ValueError('unclean workers')
    if value['cleanup_errors']: raise ValueError('cleanup errors')
except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
    print('Runner did not produce a new valid successful terminal artifact: '+type(error).__name__,file=sys.stderr)
    sys.exit(2)
PY
