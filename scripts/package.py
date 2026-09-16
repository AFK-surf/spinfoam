#!/usr/bin/env python3
"""Package each native binary together with its SDK and usage documentation."""
import hashlib
import json
import asyncio
from pathlib import Path
import sys
import tarfile

target = sys.argv[1]
name = f"spinfoam-{target}"
binary = Path(f"target/{target}/release/spinfoam")
# Exercise the shipped executable's Tokio/stdio startup and clean shutdown.
requests = [
    {"jsonrpc": "2.0", "id": "init", "method": "sf.initialize",
     "params": {"protocol_version": 1}},
    {"jsonrpc": "2.0", "id": "stop", "method": "sf.shutdown", "params": {}},
]
async def smoke():
    proc = await asyncio.create_subprocess_exec(
        str(binary.resolve()), stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
    replies = {}
    try:
        for request in requests:
            proc.stdin.write((json.dumps(request) + "\n").encode())
            await proc.stdin.drain()
            reply = json.loads(await asyncio.wait_for(proc.stdout.readline(), 15))
            assert reply["id"] == request["id"] and "result" in reply, reply
            replies[reply["id"]] = reply
        assert replies["init"]["result"]["protocol_version"] == 1, replies
        assert await asyncio.wait_for(proc.wait(), 15) == 0
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()

asyncio.run(smoke())
dist = Path("dist")
dist.mkdir(exist_ok=True)
archive = dist / f"{name}.tar.gz"
with tarfile.open(archive, "w:gz") as output:
    output.add(f"target/{target}/release/spinfoam", arcname=f"{name}/spinfoam")
    for source in ["LICENSE", "README.md", "DESIGN.md", "sdk", "docs", "examples",
                   "bench/RESULTS.md", "bench/measurements", "scripts/run-delegated.sh"]:
        output.add(source, arcname=f"{name}/{source}")
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
(dist / f"{name}.sha256").write_text(f"{checksum}  {archive.name}\n")
