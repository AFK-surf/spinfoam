#!/usr/bin/env python3
"""Reject a GNU/Linux binary requiring a glibc newer than Bullseye's 2.31."""
import re
import subprocess
import sys

symbols = subprocess.check_output(["readelf", "--version-info", sys.argv[1]], text=True)
versions = {tuple(map(int, v.split("."))) for v in re.findall(r"GLIBC_([0-9.]+)", symbols)}
assert versions, "No glibc symbol versions found"
assert max(versions) <= (2, 31), f"Unsupported glibc: {max(versions)}"
print("Maximum required glibc:", ".".join(map(str, max(versions))))
