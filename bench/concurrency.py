#!/usr/bin/env python3
"""Run real, independently loaded ELF programs through the public stdio protocol."""
import argparse
import asyncio
import base64
import json
import hashlib
import os
from pathlib import Path
import platform
import statistics
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
MARKER = 0x1122334455667788
SOURCE = '''#include "spinfoam.h"
volatile sf_u64 seed = 0x1122334455667788ULL;
SF_MAIN int main(void) {
    for (;;) {
        sf_handle e = sf_event_next(30000 + seed % 1000);
        if (e >= 0) {
            sf_handle ack = sf_host_call("benchmark.ack", e, 10000);
            if (ack >= 0) sf_drop(ack);
            sf_drop(e);
        }
    }
}
'''

class Client:
    async def open(self, binary, builds=False):
        self.proc = await asyncio.create_subprocess_exec(str(binary), *(["--enable-builds"] if builds else []), stdin=asyncio.subprocess.PIPE,
                                                        stdout=asyncio.subprocess.PIPE, limit=262144)
        self.pending = {}
        self.serial = 0
        self.lock = asyncio.Lock()
        self.acks = 0
        self.failures = []
        self.reader = asyncio.create_task(self.read())
        return await self.call('sf.initialize', {'protocol_version': 1})

    async def send(self, value):
        async with self.lock:
            self.proc.stdin.write(json.dumps(value, separators=(',', ':')).encode() + b'\n')
            await self.proc.stdin.drain()

    async def read(self):
        try:
            while line := await self.proc.stdout.readline():
                v = json.loads(line)
                if v.get('method') == 'host.call':
                    self.acks += 1
                    await self.send({'jsonrpc': '2.0', 'id': v['id'], 'result': {'accepted': True}})
                elif v.get('method') == 'sf.object.state' and v['params']['state'] == 'failed':
                    self.failures.append(v)
                elif 'id' in v:
                    future = self.pending.pop(v['id'], None)
                    if future is not None:
                        if 'error' in v:
                            future.set_exception(RuntimeError(v['error']))
                        else:
                            future.set_result(v['result'])
        finally:
            for f in self.pending.values():
                if not f.done():
                    f.set_exception(RuntimeError('spinfoam connection closed'))

    async def call(self, method, params):
        self.serial += 1
        key = str(self.serial)
        future = asyncio.get_running_loop().create_future()
        self.pending[key] = future
        await self.send({'jsonrpc': '2.0', 'id': key, 'method': method, 'params': params})
        return await asyncio.wait_for(future, 60)

    async def close(self):
        await self.call('sf.shutdown', {})
        await asyncio.wait_for(self.proc.wait(), 10)
        await self.reader
        assert self.proc.returncode == 0


def memory(pid):
    rollup = {}
    for line in Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines():
        words = line.split()
        if len(words) >= 3 and words[2] == 'kB':
            rollup[words[0].rstrip(':')] = int(words[1]) * 1024
    status = {}
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        words = line.split()
        if words[0] in ('VmSize:', 'VmPTE:'):
            status[words[0].rstrip(':')] = int(words[1]) * 1024
        if words[0] == 'Threads:':
            status['threads'] = int(words[1])
    return {'rss': rollup['Rss'], 'pss': rollup['Pss'],
            'vmas': len(Path(f'/proc/{pid}/maps').read_text().splitlines()), **status}


def compile_object():
    with tempfile.TemporaryDirectory() as temp:
        temp = Path(temp)
        (temp / 'main.c').write_text(SOURCE)
        subprocess.run(['clang', '-O2', '-target', 'bpfel', '-ffreestanding', '-fno-builtin',
                        '-nostdinc', '-I' + str(ROOT / 'sdk'), '-emit-llvm', '-c',
                        str(temp / 'main.c'), '-o', str(temp / 'main.bc')], check=True)
        subprocess.run(['llc', '-march=bpf', '-mcpu=v3', '-bpf-stack-size=4096',
                        '--nozero-initialized-in-bss', '-filetype=obj', str(temp / 'main.bc'),
                        '-o', str(temp / 'main.o')], check=True)
        return (temp / 'main.o').read_bytes()


async def run(args):
    client = Client()
    info = await client.open(args.binary.resolve(), args.compiler == 'tinycc')
    before_compile = memory(client.proc.pid)
    if args.compiler == 'tinycc':
        job = await client.call('sf.build.submit', {'sdk_version': 1, 'entry': 'main.c', 'files': {'main.c': SOURCE}})
        while True:
            status = await client.call('sf.build.status', {'build_id': job['build_id']})
            if status['state'] not in ('queued', 'running'):
                break
            await asyncio.sleep(0.01)
        assert status['state'] == 'succeeded', status
        artifact = await client.call('sf.artifact.get', {'artifact_id': status['result']['artifact_id']})
        elf = base64.b64decode(artifact['elf'])
    else:
        elf = compile_object()
    marker = struct.pack('<Q', MARKER)
    assert elf.count(marker) == 1
    baseline = memory(client.proc.pid)
    semaphore = asyncio.Semaphore(24)
    objects = []

    async def create(n):
        async with semaphore:
            unique = elf.replace(marker, struct.pack('<Q', n + 1))
            result = await client.call('sf.object.load', {'elf': base64.b64encode(unique).decode(),
                                                        'capabilities': [{'name': 'benchmark.ack'}]})
            oid = result['object_id']
            await client.call('sf.object.start', {'object_id': oid})
            objects.append(oid)

    started = time.monotonic()
    await asyncio.gather(*(create(n) for n in range(args.count)))
    load_seconds = time.monotonic() - started
    # Await all first-use JIT work and confirm every object is actually alive and suspended.
    for oid in objects:
        status = await client.call('sf.object.get', {'object_id': oid})
        for _ in range(500):
            if status['state'] != 'starting' and status['wait'] == 'event':
                break
            assert status['state'] in ('starting', 'running'), status
            await asyncio.sleep(0.01)
            status = await client.call('sf.object.get', {'object_id': oid})
        assert status['state'] == 'running' and status['wait'] == 'event', status
    active = memory(client.proc.pid)
    print(f'{args.count} active objects: {active}', flush=True)
    latencies = []
    for _ in range(100):
        start = time.monotonic()
        await client.call('sf.stats', {})
        latencies.append((time.monotonic() - start) * 1000)

    async def deliver(oid, sequence):
        async with semaphore:
            await client.call('sf.event.deliver', {'object_id': oid, 'event_id': str(sequence),
                                                  'topic': 'benchmark', 'payload': {'sequence': sequence}})

    start = time.monotonic()
    await asyncio.gather(*(deliver(oid, n) for n, oid in enumerate(objects)))
    while client.acks < args.count:
        if time.monotonic() - start > 60:
            raise RuntimeError(f'only {client.acks} acknowledgements')
        await asyncio.sleep(0.01)
    event_seconds = time.monotonic() - start
    await asyncio.sleep(0.1)
    mixed = memory(client.proc.pid)
    await asyncio.sleep(args.soak_seconds)
    after_soak = memory(client.proc.pid)
    assert not client.failures, client.failures[:3]
    for oid in objects:
        await client.call('sf.object.unload', {'object_id': oid})
    after_unload = memory(client.proc.pid)
    await client.close()
    result = {'count': args.count, 'platform': platform.platform(), 'cpu': platform.processor(),
              'cpu_count': os.cpu_count(),
              'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'source_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True)), 'git_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'vm_max_map_count': int(Path('/proc/sys/vm/max_map_count').read_text()), 'runtime': info,
              'compiler': args.compiler, 'before_compile': before_compile,
              'baseline': baseline, 'active': active, 'after_events': mixed, 'after_soak': after_soak,
              'after_unload': after_unload, 'load_seconds': load_seconds, 'event_seconds': event_seconds,
              'event_acknowledgements': client.acks, 'soak_seconds': args.soak_seconds,
              'control_p50_ms': statistics.median(latencies), 'control_p99_ms': sorted(latencies)[98],
              'incremental_active_pss_per_program': (active['pss'] - baseline['pss']) / args.count,
              'incremental_mixed_pss_per_program': (mixed['pss'] - baseline['pss']) / args.count}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/spinfoam')
    parser.add_argument('--compiler', choices=['tinycc', 'llvm'], default='tinycc')
    parser.add_argument('--count', type=int, default=10000)
    parser.add_argument('--soak-seconds', type=int, default=60)
    parser.add_argument('--output', type=Path, default=ROOT / 'bench/results/concurrency.json')
    asyncio.run(run(parser.parse_args()))
