# spinfoam

Massively concurrent background loops for AI agents. spinfoam is an independent
Rust process that runs C-authored userspace eBPF programs on Tokio and exposes
bidirectional JSON-RPC over stdio. Each loaded object is independent. The embedder
owns external services, credentials, persistence and distributed orchestration.

The async-ebpf dependency is pinned directly to GitHub commit
`7e8a7cbce195d68a1d579608b7a28b55f6fce7fd`. Linux GNU and macOS binaries are built for x86_64 and arm64.
The upstream formal verification applies to the x86_64 core; it does not
extend to the arm64 backend. The embedded TinyCC compiler runs inside async-ebpf on all four platforms.
Less than 1 MB per active program is a design target, **not an enforced quota**.

## Build and run

Install Git, Perl and LLVM 19 before building spinfoam. Put LLVM's `bin`
directory in PATH (on macOS: `brew install llvm@19`, then
`export PATH="$(brew --prefix llvm@19)/bin:$PATH"`). Cargo fetches pinned TinyCC
source from GitHub, compiles it to eBPF in `OUT_DIR`,
and embeds the result. Neither compiler objects nor source archives are committed.

```sh
cargo build --release --locked
target/release/spinfoam --dump-sdk > spinfoam.h
```

Spawn `target/release/spinfoam` with piped stdin/stdout and read stdout continuously.
The runtime supports object load/start/stop/unload, incoming events, timers, host
RPCs, bounded JSON/byte handles, status queries and cancellation. No kernel eBPF
support or privileged execution is needed for the runtime.

See [the protocol reference](docs/PROTOCOL.md) for requests and delivery semantics,
[the C SDK](sdk/spinfoam.h), and the four executable examples:

- [GitHub Actions status monitor](examples/github_actions.c)
- [Home Assistant state transitions](examples/homeassistant.c)
- [Keyword matching across web-page chunks](examples/web_keyword.c)
- [Webhook filtering and processed acknowledgements](examples/webhook.c)

Every SDK handle is owned: release it with `sf_drop`. The examples use abstract
host capabilities; your embedder implements and authorizes them. It can attach
exact argument constraints to capabilities when loading an object. No HTTP clients,
webhook listener, secrets or distributed coordination are embedded in spinfoam.

## Enable C builds

```sh
target/release/spinfoam --enable-builds
```

The binary embeds TinyCC compiled to eBPF, using the fork from
async-ebpf's bootstrap test, with reusable BPF-output and virtual-file support. The compiler itself runs in async-ebpf and emits
little-endian eBPF ELF objects. No installed compiler, platform sandbox, privileged
setup, or toolchain manifest is needed, including on macOS.

Each build gets a fresh compiler guest with an 8 MiB arena/stack and a memory-only
filesystem containing the submitted files and SDK. It has no filesystem, network,
process, or agent RPC capabilities. Builds yield to the same Tokio execution
thread, have a 15-second deadline, and support cancellation. One build runs at a
time. Compiler memory is separate from the per-loop memory design target.

The freestanding compiler supports integer C and one translation unit plus
headers. Floating-point compilation and signed division/modulo are unsupported by the fork's BPF backend. Build artifacts remain
subject to the runtime loader and JIT checks. See [compiler provenance and rebuild
instructions](vendor/tinycc/README.md) for the pinned fork implementation
and LGPL license. Building spinfoam requires the prerequisites listed above;
end-user C builds use only the embedded guest.

## Test and benchmark

With the build prerequisites installed:

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The regular build tests exercise compilation/artifact execution, host-file
isolation, source-path validation, operation with an empty PATH, resource
exhaustion, cancellation, caching and all four examples.

```sh
cargo build --release --locked
python3 bench/concurrency.py --count 10000 --soak-seconds 60
python3 bench/concurrency.py --count 20000 --soak-seconds 120 \
  --output bench/results/20000.json
```

The benchmark loads distinct ELF files, verifies every loop is suspended, delivers
one event to each, responds to guest RPCs, and records PSS/RSS, virtual mappings,
page tables, control latency and unload retention. All guest execution is on one
OS thread; async-ebpf also uses a preemption watcher and the process has up to two
loader/JIT workers. Guarded virtual mappings exceed resident memory substantially;
large populations require sufficient `vm.max_map_count`. Execution-stack pooling
retains a high-water mark after unload.

See [measured concurrency results](bench/RESULTS.md) for the 10,000- and 20,000-program runs.

[DESIGN.md](DESIGN.md) records the architecture and proposed qualification scope.


## Binary builds and releases

Every push and pull request builds all four native packages and runs runtime
tests on Linux and macOS, on both architectures. Linux packages are built inside
Debian Bullseye containers and checked for a maximum glibc requirement of 2.31.
macOS packages target macOS 11 or later. Download development packages from the
workflow's `binary-*` artifacts.

Pushing a tag publishes a GitHub release only after all builds and tests pass.
Use version tags such as `v0.1.0`; ordinary branch pushes and pull requests never
publish releases. Each release contains four tarballs and `SHA256SUMS`.
The tarballs include the binary, C SDK, examples and documentation.

Compiler execution and C builds are tested on both architectures on Linux and
macOS. Release packages also include the compiler's source, license and
spinfoam source needed to rebuild the binary with a modified compiler.
