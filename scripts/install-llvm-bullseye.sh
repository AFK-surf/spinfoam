#!/bin/sh
# CI image setup; run inside the disposable rust:bullseye container.
set -eu
curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key -o /etc/apt/trusted.gpg.d/llvm.asc
echo 'deb https://apt.llvm.org/bullseye/ llvm-toolchain-bullseye-19 main' > /etc/apt/sources.list.d/llvm.list
apt-get update
apt-get install -y --no-install-recommends -t llvm-toolchain-bullseye-19 clang-19 llvm-19
