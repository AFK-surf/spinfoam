#!/usr/bin/env python3
"""Package each native binary together with its SDK and usage documentation."""
import hashlib
from pathlib import Path
import sys
import tarfile

target = sys.argv[1]
name = f"spinfoam-{target}"
dist = Path("dist")
dist.mkdir(exist_ok=True)
archive = dist / f"{name}.tar.gz"
with tarfile.open(archive, "w:gz") as output:
    output.add(f"target/{target}/release/spinfoam", arcname=f"{name}/spinfoam")
    for source in ["LICENSE", "README.md", "sdk", "docs", "examples", "scripts/run-delegated.sh"]:
        output.add(source, arcname=f"{name}/{source}")
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
(dist / f"{name}.sha256").write_text(f"{checksum}  {archive.name}\n")
