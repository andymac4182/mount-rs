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
import hashlib, json, os, pathlib, re, sys, zlib
def unique_object(pairs):
    result={}
    for key,item in pairs:
        if key in result: raise ValueError('duplicate JSON key')
        result[key]=item
    return result
try:
    root=pathlib.Path(os.environ['MOUNT_RS_TARGET_OUTPUT'])
    value=json.loads((root/'terminal.json').read_text(),object_pairs_hook=unique_object)
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
    def retained(receipt, name):
        if receipt['file']!=name: raise ValueError('unexpected receipt path')
        with (root/name).open('rb') as stream: data=stream.read(16*1024*1024+1)
        if len(data)>16*1024*1024: raise ValueError('stored receipt limit')
        if hashlib.sha256(data).hexdigest()!=receipt['sha256']: raise ValueError('receipt digest')
        if name.endswith('.gz'):
            decoder=zlib.decompressobj(31)
            data=decoder.decompress(data,64*1024*1024+1)
            if len(data)>64*1024*1024 or not decoder.eof or decoder.unconsumed_tail or decoder.unused_data:
                raise ValueError('compressed receipt incomplete, trailing, or oversized')
        return json.loads(data,object_pairs_hook=unique_object)
    def u64(number):
        return type(number) is int and 0<=number<2**64
    profile_files=configuration['files']
    if type(profile_files) is not int or not 0<profile_files<=1000: raise ValueError('profile file count')
    def blocks(identity):
        return 1 if identity<990 else 32 if identity<999 else 256
    slots=sum(blocks(identity) for identity in range(profile_files))
    for field,wanted in (('namespace_files',drives*profile_files),('population_bytes',drives*slots*4096)):
        if not u64(value[field]) or value[field]!=wanted: raise ValueError('initial profile geometry')
    inventory=value['expected_state_receipts']
    if set(inventory)!={'schema','drive_count','pack_size','packs'} or inventory['schema']!='mount-rs-expected-ledger-inventory-v1':
        raise ValueError('ledger inventory schema')
    if type(inventory['drive_count']) is not int or inventory['drive_count']!=drives or type(inventory['pack_size']) is not int or inventory['pack_size']!=32:
        raise ValueError('ledger inventory geometry')
    receipts=inventory['packs']
    if type(receipts) is not list or len(receipts)!=(drives+31)//32: raise ValueError('ledger pack count')
    ids=set()
    final_files=final_bytes=0
    for pack,receipt in enumerate(receipts):
        file=None
        first=pack*32
        count=min(32,drives-first)
        if set(receipt)!={'pack','first_drive','count','file','sha256'}: raise ValueError('pack receipt shape')
        for field,wanted in (('pack',pack),('first_drive',first),('count',count)):
            if type(receipt[field]) is not int or receipt[field]!=wanted: raise ValueError('pack receipt identity')
        decoded=retained(receipt,f'expected/pack-{pack:05}.json.gz')
        if set(decoded)!={'schema','pack','first_drive','ledgers'} or decoded['schema']!='mount-rs-expected-ledger-pack-v1':
            raise ValueError('ledger pack schema')
        for field,wanted in (('pack',pack),('first_drive',first)):
            if type(decoded[field]) is not int or decoded[field]!=wanted: raise ValueError('pack body identity')
        if type(decoded['ledgers']) is not list or len(decoded['ledgers'])!=count: raise ValueError('pack roster')
        for position,ledger in enumerate(decoded['ledgers']):
            drive=first+position
            if set(ledger)!={'drive','files','generations','oracle'} or type(ledger['drive']) is not int or ledger['drive']!=drive or drive in ids:
                raise ValueError('ledger identity')
            ids.add(drive)
            if ledger['oracle']!='tuple-seeded4096-byte blocks; initial generation0': raise ValueError('ledger oracle')
            generations=ledger['generations']
            if type(generations) is not list or len(generations)!=slots or not all(u64(generation) for generation in generations):
                raise ValueError('dense generation ledger')
            if type(ledger['files']) is not dict: raise ValueError('ledger files')
            final_files+=len(ledger['files'])
            for file in ledger['files'].values():
                if type(file) is not dict or set(file)!={'identity','length','changed'}: raise ValueError('file state')
                identity,length=file['identity'],file['length']
                if type(identity) is not int or not 0<=identity<profile_files: raise ValueError('file identity')
                if not u64(length) or length%4096: raise ValueError('ledger length')
                if type(file['changed']) is not dict: raise ValueError('sparse generation map')
                for block,generation in file['changed'].items():
                    if not block.isascii() or not block.isdecimal() or str(int(block))!=block or not u64(int(block)):
                        raise ValueError('sparse block identity')
                    if not blocks(identity)<=int(block)<length//4096 or not u64(generation): raise ValueError('sparse generation entry')
                final_bytes+=length
                if not u64(final_bytes): raise ValueError('ledger byte total overflow')
        # No decoded pack survives to the next pack or either oracle join.
        del decoded,ledger,generations,file
    if ids!=set(range(drives)): raise ValueError('ledger coverage')
    if not all(u64(value[field]) for field in ('verified_files','verified_bytes')) or (value['verified_files'],value['verified_bytes'])!=(final_files,final_bytes): raise ValueError('final totals')
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
    # The two complete fresh joins assigned both decoded frame temporaries.
    # Release them before re-reading either anchor; only identity strings survive.
    del controller,frame
    # The stage payloads are immutable retained QUIC receipts, independently
    # bound to the original three controller boundary frames for each cell.
    patterns=('sequential_read','random_read','sequential_overwrite','random_overwrite',
        'mixed','hot_file','append_truncate','churn')
    scope='actual retained client connections; active workload plus idle liveness; snapshot observer outside active throughput interval; server transport retained separately in worker phase receipts'
    schema='mount-rs-controller-quic-v1'
    counter_fields={'tx_bytes','rx_bytes','tx_datagrams','rx_datagrams','tx_ios','rx_ios',
        'lost_packets','lost_bytes','sent_packets','congestion_events'}
    def digest_string(number):
        return type(number) is str and re.fullmatch(r'[0-9a-f]{64}',number) is not None
    def exact_identity(actual,expected):
        if type(actual) is not dict or set(actual)!=set(expected):
            raise ValueError('QUIC identity shape')
        if any(type(actual[key]) is not type(number) or actual[key]!=number
            for key,number in expected.items()):
            raise ValueError('QUIC identity binding')
    controller_pid=value['controller_resources']['pid']
    if type(controller_pid) is not int or not 0<controller_pid<2**32:
        raise ValueError('QUIC controller PID')
    if not all(digest_string(value['source'][key]) for key in ('digest','binary_sha256')):
        raise ValueError('QUIC source pins')
    catalog_backend=None
    for label in ('initial','final'):
        boundary=[row for row in value['phase_metrics']['boundaries']
            if row['phase']==f'{label}_fresh_oracle' and row['boundary']=='after'][0]
        frame=retained(boundary['controller'],
            f"metrics/g{boundary['generation']}-s{boundary['sequence']}.json.gz")
        pair=(frame['identity']['catalog_digest'],frame['identity']['backend_prefix'])
        del frame
        if not digest_string(pair[0]) or type(pair[1]) is not str or not pair[1]:
            raise ValueError('QUIC catalog/backend identity')
        if catalog_backend is None:
            catalog_backend=pair
        elif pair!=catalog_backend:
            raise ValueError('QUIC fresh oracle identity disagreement')
    def quic_document(artifact,name):
        if artifact['file']!=name or not digest_string(artifact['sha256']):
            raise ValueError('QUIC receipt path/digest shape')
        # Reuse the authoritative generic retained reader: hash before decode,
        # 16 MiB stored / 64 MiB decoded, one gzip member, and unique JSON keys.
        return retained(artifact,name)
    stages=value['stages']
    cells=[(mode,pattern) for mode in ('mostly_idle','all_active') for pattern in patterns]
    if type(stages) is not list or len(stages)!=len(cells):
        raise ValueError('QUIC stage count')
    for stage,(mode,pattern) in zip(stages,cells):
        if type(stage) is not dict or stage['mode']!=mode or stage['pattern']!=pattern:
            raise ValueError('QUIC stage identity/order')
        active=min(max(drives//100,1),100) if mode=='mostly_idle' else drives
        for key,expected in (('connected_clients',drives),('configured_active_clients',active)):
            if type(stage[key]) is not int or stage[key]!=expected:
                raise ValueError('QUIC stage client roster')
        sequences=stage['metric_sequences']
        if type(sequences) is not list or len(sequences)!=3:
            raise ValueError('QUIC stage sequence roster')
        if any(type(number) is not int or not 0<number<2**64 for number in sequences):
            raise ValueError('QUIC stage sequence type')
        if sequences!=list(range(sequences[0],sequences[0]+3)):
            raise ValueError('QUIC stage nonconsecutive sequences')
        phase=f'{mode}/{pattern}'
        boundaries=[row for row in value['phase_metrics']['boundaries'] if row['phase']==phase]
        labels=('before_active','after_active','after_idle')
        if len(boundaries)!=3 or {row['boundary'] for row in boundaries}!=set(labels):
            raise ValueError('QUIC controller boundary roster')
        rows=[]
        for label,sequence in zip(labels,sequences):
            matching=[row for row in boundaries if row['boundary']==label]
            if len(matching)!=1: raise ValueError('QUIC duplicate controller boundary')
            row=matching[0]
            if row['complete'] is not True:
                raise ValueError('QUIC controller boundary incomplete')
            if value['metrics_required_for_outcome'] and row['metrics_complete'] is not True:
                raise ValueError('QUIC controller boundary metrics')
            if type(row['generation']) is not int or not 0<=row['generation']<2**64:
                raise ValueError('QUIC generation type')
            if type(row['sequence']) is not int or row['sequence']!=sequence:
                raise ValueError('QUIC boundary sequence binding')
            rows.append(row)
        generation=rows[0]['generation']
        if any(row['generation']!=generation for row in rows):
            raise ValueError('QUIC stage generation disagreement')
        identity=None
        for row in rows:
            expected={'controller_pid':controller_pid,'generation':generation,
                'sequence':row['sequence'],'phase':phase,'boundary':row['boundary'],
                'source_digest':value['source']['digest'],'binary_digest':value['source']['binary_sha256'],
                'catalog_digest':catalog_backend[0],'backend_prefix':catalog_backend[1],
                'pid':controller_pid,'server':None,'role':'controller'}
            frame=retained(row['controller'],f"metrics/g{generation}-s{row['sequence']}.json.gz")
            exact_identity(frame['identity'],expected)
            if frame['capture_complete'] is not True:
                raise ValueError('QUIC controller frame incomplete')
            if value['metrics_required_for_outcome'] and frame['metrics_complete'] is not True:
                raise ValueError('QUIC controller frame metrics')
            if row['boundary']=='after_idle':
                identity=expected|{'mode':mode,'pattern':pattern}
            del frame
        boundary=stage['controller_quic_boundary']
        if type(boundary) is not dict or set(boundary)!={'scope','artifact'} or boundary['scope']!=scope:
            raise ValueError('QUIC stage retained shape')
        artifact=boundary['artifact']
        if type(artifact) is not dict or set(artifact)!={'schema','file','sha256','identity','lanes'}:
            raise ValueError('QUIC receipt shape')
        if artifact['schema']!=schema or type(artifact['lanes']) is not int or artifact['lanes']!=drives:
            raise ValueError('QUIC receipt schema/lanes')
        exact_identity(artifact['identity'],identity)
        document=quic_document(artifact,f'quic/g{generation}-s{sequences[2]}.json.gz')
        if type(document) is not dict or set(document)!={'schema','identity','scope','connections'}:
            raise ValueError('QUIC envelope shape')
        if document['schema']!=schema or document['scope']!=scope:
            raise ValueError('QUIC envelope schema/scope')
        exact_identity(document['identity'],identity)
        connections=document['connections']
        if type(connections) is not list or len(connections)!=drives:
            raise ValueError('QUIC lane roster')
        for lane,connection in enumerate(connections):
            if type(connection) is not dict or set(connection)!={'lane','quic'}:
                raise ValueError('QUIC connection shape')
            if type(connection['lane']) is not int or connection['lane']!=lane:
                raise ValueError('QUIC lane identity/order')
            counters=connection['quic']
            if type(counters) is not dict or set(counters)!=counter_fields:
                raise ValueError('QUIC counter shape')
            if any(type(number) is not int or not 0<=number<2**64 for number in counters.values()):
                raise ValueError('QUIC unsigned integer counter')
        # Release the complete decoded cell before the next receipt is read.
        del document,connections,connection,counters
except (OSError, ValueError, KeyError, TypeError, AssertionError, zlib.error) as error:
    print('Runner did not produce a new valid successful terminal artifact: '+type(error).__name__,file=sys.stderr)
    sys.exit(2)
PY
