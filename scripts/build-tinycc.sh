#!/bin/sh
# Build the pinned compiler from Git into Cargo's generated-output directory.
set -eu
out=${1:?usage: build-tinycc.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(CDPATH= cd -- "$out" && pwd)
revision=7069256d9287e8f6fcacd575fcbb0b83f2300058
source_dir="$out/tinycc-source"
if [ ! -d "$source_dir/.git" ]; then
    git init -q "$source_dir"
    git -C "$source_dir" remote add origin https://github.com/losfair/tinycc.git
fi
if ! git -C "$source_dir" cat-file -e "$revision^{commit}" 2>/dev/null; then
    git -C "$source_dir" fetch --depth 1 origin "$revision"
fi
# This checkout contains only generated build inputs under OUT_DIR.
git -C "$source_dir" reset --hard "$revision"
git -C "$source_dir" clean -fdx
# The arena size must match src/build/compiler.rs, independently of the environment.
TCC_EBPF_TARGET=bpf TCC_EBPF_VFS=1 TCC_EBPF_STACK_SIZE=8388608 \
    "$source_dir/async-ebpf-host/build.sh" "$out/compiler.bpf"
