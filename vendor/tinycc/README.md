# TinyCC source provenance

Upstream: https://github.com/losfair/tinycc (branch `ebpf`)
Revision: `7069256d9287e8f6fcacd575fcbb0b83f2300058`

This extends the fork used by async-ebpf's bootstrap test with selectable compiler
output targets and optional virtual-file helpers. All adaptations live in that
fork; spinfoam carries no custom patch. `COPYING` is its LGPL license. Fetched
sources retain their original notices; spinfoam's Rust code remains MIT licensed.

Cargo's `build.rs` calls `scripts/build-tinycc.sh`, which fetches the exact Git
commit into `OUT_DIR/tinycc-source` and invokes its `async-ebpf-host/build.sh`
with `TCC_EBPF_TARGET=bpf`, `TCC_EBPF_VFS=1` and an 8 MiB arena/stack. The result,
`OUT_DIR/compiler.bpf`, is embedded using `include_bytes!`. No compiled compiler
objects, source tarballs or custom patches are committed in spinfoam. First builds
need network access to GitHub; the generated checkout is reused afterward.
Git, Perl and LLVM 19 tools must be in PATH. When Rust cross-compiles, the compiler
is generated using the build host's tools and still emits portable eBPF.

The compiler accepts integer C. Its BPF backend rejects signed division/modulo
and floating point; unsigned division/modulo are supported. spinfoam registers
only bounded memory-copy, virtual-file, output and diagnostic helpers. Each
request has fresh compiler state and no host filesystem, environment, network,
subprocess, or agent RPC capabilities. See the fork's `async-ebpf-host/README.md`
for the reusable helper ABI and its native-target bootstrap compatibility.

## Build or modify

With LLVM 19's bin directory in PATH:

```sh
cargo build --release --locked
```

Cargo rebuilds the compiler when its build script changes. To modify TinyCC,
commit the changes in the fork, then update the pinned revision in
`scripts/build-tinycc.sh` and `src/build/compiler.rs`, and rebuild spinfoam. For
direct compiler development, run `scripts/build-tinycc.sh /tmp/tcc-build` and use
the resulting checkout's documented build script. Keep its stack configuration
consistent with `src/build/compiler.rs`.

Binary packages include the exact corresponding TinyCC source from their Cargo
build, the license, spinfoam's source, Cargo files, SDK and build scripts. These
permit rebuilding with a modified compiler; no proprietary relinking inputs are
required. Reverse engineering for debugging modifications to the LGPL component
is permitted under its license. A finished spinfoam binary needs no Git or LLVM
to compile agent-authored C: TinyCC runs entirely inside async-ebpf.
