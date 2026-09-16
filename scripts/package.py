#!/usr/bin/env python3
"""Package each native binary together with its SDK and usage documentation."""
import hashlib
import json
import subprocess
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
result = subprocess.run([str(binary.resolve())],
                        input="".join(json.dumps(r) + "\n" for r in requests),
                        text=True, capture_output=True, timeout=15, check=True)
replies = {r["id"]: r for r in map(json.loads, result.stdout.splitlines()) if "id" in r}
assert replies["init"]["result"]["protocol_version"] == 1, replies
assert "result" in replies["stop"], replies
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
