#!/bin/sh
# CI image setup; run inside the disposable rust:bullseye container.
set -eu
curl -fsSL --retry 5 --retry-connrefused --connect-timeout 20 --max-time 120 https://apt.llvm.org/llvm-snapshot.gpg.key -o /etc/apt/trusted.gpg.d/llvm.asc
echo 'deb https://apt.llvm.org/bullseye/ llvm-toolchain-bullseye-19 main' > /etc/apt/sources.list.d/llvm.list
apt-get -o Acquire::Retries=3 update
apt-get -o Acquire::Retries=3 install -y --no-install-recommends -t llvm-toolchain-bullseye-19 clang-19 llvm-19
