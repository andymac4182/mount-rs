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
# Runtime ownership checkpoints require the finite diagnostic recorder.
export MOUNT_RS_PROFILE_IO=1
status=0
./scripts/cargo-shared test --locked -p mount-rs-service --features sdk-runtime,resource-profiling \
  --test quic_production_target production_target_controller -- --ignored --exact --nocapture || status=$?
if (( status != 0 )); then exit "$status"; fi
python3 - <<'PY'
import gzip, hashlib, json, os, pathlib, sys
try:
    root=pathlib.Path(os.environ['MOUNT_RS_TARGET_OUTPUT'])
    value=json.loads((root/'terminal.json').read_text())
    if value['phase']!='terminal' or value['outcome']!='success': raise ValueError('unsuccessful terminal')
    mode=os.environ['MOUNT_RS_TARGET_MODE']
    if mode not in ('full','control'): raise ValueError('invalid mode')
    full_target=mode=='full'
    configuration=value['configuration']
    if type(value['full_target']) is not bool or type(configuration['full_target']) is not bool:
        raise ValueError('mode type mismatch')
    if value['full_target'] is not full_target or configuration['full_target'] is not full_target:
        raise ValueError('mode mismatch')
    if full_target:
        for field,expected in (('drives',10000),('files',1000),('seconds',30)):
            if type(configuration[field]) is not int or configuration[field]!=expected:
                raise ValueError('full target geometry mismatch')
    workers=value['workers']
    if len(workers)!=10 or len({w['pid'] for w in workers})!=10: raise ValueError('worker count/identity mismatch')
    if not all(w['reap_confirmed'] is True and w['exit_code']==0 for w in workers): raise ValueError('unclean workers')
    if value['cleanup_errors']: raise ValueError('cleanup errors')
    if value['verified_passes']!=2 or value['fresh_oracle_complete'] is not True or value['fresh_oracle_settled'] is not True:
        raise ValueError('fresh oracle incomplete')
    drives=value['configuration']['drives']
    if type(drives) is not int or not 0<drives<=10000: raise ValueError('invalid Drive count')
    if value['expected_state_observation']['complete'] is not True: raise ValueError('incomplete final ledger')
    receipts=value['expected_state_receipts']
    if len(receipts)!=drives: raise ValueError('ledger count')
    def retained(receipt, name):
        if receipt['file']!=name: raise ValueError('unexpected receipt path')
        data=(root/name).read_bytes()
        if hashlib.sha256(data).hexdigest()!=receipt['sha256']: raise ValueError('receipt digest')
        return json.loads(gzip.decompress(data) if name.endswith('.gz') else data)
    ids=set()
    final_files=final_bytes=0
    for receipt in receipts:
        drive=receipt['drive']
        if type(drive) is not int or not 0<=drive<drives or drive in ids: raise ValueError('ledger identity')
        ids.add(drive)
        ledger=retained(receipt, f'expected/drive-{drive}.json')
        if ledger['drive']!=drive or not isinstance(ledger['files'],dict): raise ValueError('ledger shape')
        final_files+=len(ledger['files'])
        for file in ledger['files'].values():
            length=file['length']
            if type(length) is not int or not 0<=length<2**64 or length%4096: raise ValueError('ledger length')
            final_bytes+=length
    if ids!=set(range(drives)): raise ValueError('ledger coverage')
    if (value['verified_files'],value['verified_bytes'])!=(final_files,final_bytes): raise ValueError('final totals')
    passes=value['fresh_oracle_passes']
    if len(passes)!=2: raise ValueError('pass count')
    fields={'pass','slot_limit','expected_drives','started_drives','completed_drives','live_slots',
        'expected_files','completed_files','checked_files','expected_bytes','completed_bytes',
        'compared_bytes','complete','settled'}
    for index,label in enumerate(('initial','final')):
        summary=passes[index]
        if set(summary)!=fields|{'after_boundary_complete','receipt'}: raise ValueError('pass shape')
        numeric=fields-{'pass','complete','settled'}
        if any(type(summary[field]) is not int or not 0<=summary[field]<2**64 for field in numeric): raise ValueError('pass numeric fields')
        if summary['pass']!=label or summary['slot_limit']!=8 or summary['live_slots']!=0: raise ValueError('pass identity')
        if any(summary[field]!=drives for field in ('expected_drives','started_drives','completed_drives')): raise ValueError('pass Drive coverage')
        if any(summary[field] is not True for field in ('complete','settled','after_boundary_complete')): raise ValueError('pass unsettled')
        files,byte_count=(value['namespace_files'],value['population_bytes']) if index==0 else (final_files,final_bytes)
        if any(summary[field]!=files for field in ('expected_files','completed_files','checked_files')): raise ValueError('pass file coverage')
        if any(summary[field]!=byte_count for field in ('expected_bytes','completed_bytes','compared_bytes')): raise ValueError('pass byte coverage')
        raw=retained(summary['receipt'], f'oracle-receipts/{label}.json')
        if set(raw)!=fields|{'completed_drive_ids'} or raw['completed_drive_ids']!=sorted(ids): raise ValueError('pass corpus roster')
        if any(type(drive) is not int for drive in raw['completed_drive_ids']): raise ValueError('pass corpus identity type')
        if any(type(raw[field]) is not type(summary[field]) for field in fields): raise ValueError('pass receipt field type')
        if any(raw[field]!=summary[field] for field in fields): raise ValueError('pass receipt mismatch')
        after=[row for row in value['phase_metrics']['boundaries'] if row['phase']==f'{label}_fresh_oracle' and row['boundary']=='after']
        if len(after)!=1 or after[0]['complete'] is not True: raise ValueError('pass boundary incomplete')
        if value['metrics_required_for_outcome'] and after[0]['metrics_complete'] is not True: raise ValueError('pass metrics incomplete')
        row=after[0]
        generation,sequence=row['generation'],row['sequence']
        if any(type(number) is not int or number<0 for number in (generation,sequence)): raise ValueError('boundary identity')
        name=f'metrics/g{generation}-s{sequence}.json.gz'
        controller=retained(row['controller'],name)
        common={'controller_pid':value['controller_resources']['pid'],'generation':generation,
            'sequence':sequence,'phase':f'{label}_fresh_oracle','boundary':'after',
            'source_digest':value['source']['digest'],'binary_digest':value['source']['binary_sha256']}
        for key in ('catalog_digest','backend_prefix'):
            common[key]=controller['identity'][key]
        def metric_frame(frame,pid,server,role):
            identity=common|{'pid':pid,'server':server,'role':role}
            if frame['identity']!=identity or frame['capture_complete'] is not True: raise ValueError('boundary frame identity/capture')
            if value['metrics_required_for_outcome'] and frame['metrics_complete'] is not True: raise ValueError('boundary frame metrics')
        metric_frame(controller,common['controller_pid'],None,'controller')
        measured=row['workers']
        if len(measured)!=10: raise ValueError('boundary worker count')
        servers=set()
        for worker in measured:
            server=worker['server']
            if type(server) is not int or not 0<=server<10 or server in servers: raise ValueError('boundary worker identity')
            servers.add(server)
            owned=[owner for owner in workers if owner['server']==server]
            if len(owned)!=1 or owned[0]['pid']!=worker['pid']: raise ValueError('boundary worker binding')
            frame=retained(worker,f'worker-{server}/metrics/g{generation}-s{sequence}.json.gz')
            metric_frame(frame,worker['pid'],server,'worker')
except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
    print('Runner did not produce a new valid successful terminal artifact: '+type(error).__name__,file=sys.stderr)
    sys.exit(2)
PY
