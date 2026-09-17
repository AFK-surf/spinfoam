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
# Exercise the exact shipped binary, including its embedded compiler and emitted ELF.
async def smoke():
    proc = await asyncio.create_subprocess_exec(
        str(binary.resolve()), "--enable-builds",
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
    serial = 0

    async def call(method, params):
        nonlocal serial
        serial += 1
        key = str(serial)
        request = {"jsonrpc": "2.0", "id": key, "method": method, "params": params}
        proc.stdin.write((json.dumps(request) + "\n").encode())
        await proc.stdin.drain()
        while True:
            line = await asyncio.wait_for(proc.stdout.readline(), 20)
            assert line, f"unexpected EOF in {method}"
            reply = json.loads(line)
            if reply.get("id") == key:
                assert "result" in reply, reply
                return reply["result"]

    try:
        info = await call("sf.initialize", {"protocol_version": 1})
        assert info["compiler"]["available"] and info["compiler"]["embedded"], info
        job = await call("sf.build.submit", {"sdk_version": 1, "entry": "main.c", "files": {
            "main.c": '#include <spinfoam.h>\nvolatile sf_i64 counter; SF_MAIN sf_i64 main(void){sf_sleep_ms(1);return ++counter+41;}'}})
        for _ in range(2000):
            status = await call("sf.build.status", {"build_id": job["build_id"]})
            if status["state"] not in ("queued", "running"):
                break
            await asyncio.sleep(0.01)
        assert status["state"] == "succeeded", status
        obj = await call("sf.object.load", {"artifact_id": status["result"]["artifact_id"]})
        await call("sf.object.start", {"object_id": obj["object_id"]})
        for _ in range(1000):
            status = await call("sf.object.get", {"object_id": obj["object_id"]})
            if status["state"] not in ("starting", "running"):
                break
            await asyncio.sleep(0.01)
        assert status["state"] == "exited" and status["outcome"]["exit_code"] == 42, status
        await call("sf.shutdown", {})
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
                   "bench/RESULTS.md", "bench/measurements", "vendor/tinycc", "src",
                   "Cargo.toml", "Cargo.lock", "build.rs", "scripts/build-tinycc.sh"]:
        output.add(source, arcname=f"{name}/{source}")
    # Corresponding compiler source from the same Cargo build, without Git data.
    sources = list(Path(f"target/{target}/release/build").glob("spinfoam-*/out/tinycc-source"))
    assert len(sources) == 1, f"expected one compiler checkout for this build: {sources}"
    output.add(sources[0], arcname=f"{name}/tinycc-source",
               filter=lambda entry: None if ".git" in Path(entry.name).parts else entry)
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
(dist / f"{name}.sha256").write_text(f"{checksum}  {archive.name}\n")
