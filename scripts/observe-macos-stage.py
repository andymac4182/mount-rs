#!/usr/bin/env python3
"""Phase-aligned host storage-driver counters, never provider-exclusive IOPS."""
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import time

KEYS = ('Operations (Read)', 'Operations (Write)', 'Bytes (Read)', 'Bytes (Write)')


def capture():
    started = time.monotonic_ns()
    raw = subprocess.run(
        ['/usr/sbin/ioreg', '-r', '-c', 'IOBlockStorageDriver', '-a'],
        capture_output=True, check=True, timeout=10,
    )
    devices = {}
    for entry in plistlib.loads(raw.stdout):
        stats = entry.get('Statistics', {})
        if all(key in stats for key in KEYS):
            devices[str(entry['IORegistryEntryID'])] = {key: int(stats[key]) for key in KEYS}
    if not devices:
        raise RuntimeError('host storage-driver counters unavailable')
    return {
        'captured_monotonic_ns': time.monotonic_ns(),
        'capture_started_ns': started, 'unix_ns': time.time_ns(), 'devices': devices,
    }


def reduce(before, after):
    if before['devices'].keys() != after['devices'].keys():
        raise ValueError('host device set changed')
    seconds = (after['captured_monotonic_ns'] - before['captured_monotonic_ns']) / 1e9
    if seconds <= 0:
        raise ValueError('invalid host counter interval')
    totals = dict.fromkeys(KEYS, 0)
    for device, old in before['devices'].items():
        for key in KEYS:
            delta = after['devices'][device][key] - old[key]
            if delta < 0:
                raise ValueError('host storage-driver counter reset')
            totals[key] += delta
    return {
        'counter_interval_seconds': seconds, 'deltas': totals,
        'rates_per_second': {key: value / seconds for key, value in totals.items()},
    }


def main():
    if len(sys.argv) == 5 and sys.argv[1] == 'cpu-trace-child':
        parent_pid, trace_path, status_path = sys.argv[2:]
        result = {'scope': 'only owned QUIC saturation process', 'parent_pid': int(parent_pid)}
        try:
            command = ['/usr/bin/xctrace', 'record', '--template', 'CPU Counters',
                       '--attach', parent_pid, '--time-limit', '5s', '--no-prompt',
                       '--output', trace_path]
            output = subprocess.run(command, capture_output=True, text=True, timeout=30)
            result.update({'returncode': output.returncode,
                           'stdout': output.stdout[-8000:], 'stderr': output.stderr[-8000:]})
        except subprocess.TimeoutExpired:
            result['error'] = 'owned CPU trace exceeded 30-second deadline'
        Path(status_path).write_text(json.dumps(result, indent=2) + '\n')
        return
    phase, mode, depth, stage_id, successes, failures = sys.argv[1:]
    if phase not in ('begin', 'end') or not re.fullmatch(r'[A-Za-z0-9._-]+', stage_id):
        raise ValueError('invalid stage hook arguments')
    folder = Path(os.environ['MOUNT_RS_HOST_IO_OUTPUT_DIR'])
    folder.mkdir(parents=True, exist_ok=True)
    snapshot = capture()
    snapshot.update({
        'mode': mode, 'total_queue_depth': int(depth),
        'successes': int(successes), 'failures': int(failures),
    })
    if phase == 'begin' and os.environ.get('MOUNT_RS_HOST_CPU_TRACE') == '1':
        parent_pid = os.getppid()
        parent_command = subprocess.run(
            ['/bin/ps', '-p', str(parent_pid), '-o', 'comm='],
            capture_output=True, text=True, check=True, timeout=5,
        ).stdout.strip()
        if not Path(parent_command).name.startswith('quic_tidb_saturation-'):
            raise ValueError('CPU trace target is not the owned saturation test parent')
        trace_path = folder / f'{stage_id}.trace'
        status_path = folder / f'{stage_id}-cpu-trace-status.json'
        tracer = subprocess.Popen(
            [sys.executable, __file__, 'cpu-trace-child', str(parent_pid),
             str(trace_path), str(status_path)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        snapshot['diagnostic_cpu_trace'] = {
            'owned_parent_pid': parent_pid, 'supervisor_pid': tracer.pid,
            'status_path': str(status_path), 'trace_path': str(trace_path),
            'scope': 'CPU Counters on owned process; diagnostic throughput only',
        }
    if phase == 'begin' and os.environ.get('MOUNT_RS_HOST_STACK_SAMPLE') == '1':
        sample_path = folder / f'{stage_id}-stacks.txt'
        with (folder / f'{stage_id}-sample-status.txt').open('wb') as status:
            sampler = subprocess.Popen(
                ['/usr/bin/sample', str(os.getppid()), '5', '10', '-mayDie', '-file', str(sample_path)],
                stdout=status, stderr=status,
            )
        snapshot['diagnostic_stack_sample'] = {
            'owned_parent_pid': os.getppid(), 'sampler_pid': sampler.pid,
            'duration_seconds': 5, 'interval_ms': 10,
            'scope': 'all thread stacks; includes waiting, suspends process; throughput is diagnostic',
        }
    (folder / f'{stage_id}-{phase}.json').write_text(json.dumps(snapshot, indent=2) + '\n')
    if phase == 'end':
        before = json.loads((folder / f'{stage_id}-begin.json').read_text())
        result = reduce(before, snapshot)
        result.update({
            'schema': 'mount-rs-host-driver-io-v1', 'mode': mode, 'stage_id': stage_id,
            'total_queue_depth': int(depth), 'successful_drive_operations': int(successes),
            'failed_drive_operations': int(failures),
            'scope': 'all host IOBlockStorageDriver operations; includes other apps, Docker VM, '
                     'OS caching/coalescing; not provider-exclusive or NAND IO',
        })
        (folder / f'{stage_id}-delta.json').write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
