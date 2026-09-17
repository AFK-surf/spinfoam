# spinfoam

Massively concurrent background loops for AI agents. spinfoam is an independent
Rust process that runs C-authored userspace eBPF programs on Tokio and exposes
bidirectional JSON-RPC over stdio. Each loaded object is independent. The embedder
owns external services, credentials, persistence and distributed orchestration.

The async-ebpf dependency is pinned directly to GitHub commit
`7e8a7cbce195d68a1d579608b7a28b55f6fce7fd`. Linux GNU and macOS binaries are built for x86_64 and arm64.
The upstream formal verification applies to the x86_64 core; it does not
extend to the arm64 backend. The compiler sandbox requires Linux.
Less than 1 MB per active program is a design target, **not an enforced quota**.

## Build and run

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

## Enable sandboxed builds

Install Clang/LLVM and Bubblewrap and put `clang`, `llc`, `bwrap` and `ldd`
in PATH. spinfoam discovers the tools and their shared libraries automatically.
LLVM 19.1.7 and Bubblewrap 0.12.0 from Debian 13 were used for local qualification.

Enable compilation with one flag:

```sh
target/release/spinfoam --enable-builds
```

No cgroup delegation or privileged launcher is needed. The host must permit
unprivileged user namespaces.

The startup probe executes a real sandboxed build. `sf.initialize` reports whether
it succeeded and why it failed otherwise. Builds fail closed if namespaces,
seccomp, compiler tools or resource limits are unavailable; existing ELF execution
remains usable. There is no insecure bypass flag. Restart spinfoam after upgrading the compiler
tools or libraries; discovery and the build cache are scoped to one process.

Each build uses separate user/mount/PID/network/IPC/UTS namespaces, a
syscall allowlist, no-new-privileges, dropped capabilities, read-only source/SDK/
toolchain mounts and a 16 MiB writable tmpfs. Only the discovered libraries and tools
are mounted, not the host filesystem. Each compiler process has a 1 GiB address-space
limit, a 256 MiB data/allocation limit and a five-second CPU limit. These are
per-process limits, not an aggregate RSS quota or CPU bandwidth control. Compiler
threads share those budgets; a second seccomp filter denies new child processes.
The trusted worker launches Clang and LLVM sequentially. The build also has a
15-second wall deadline, 64-descriptor limit and 16 MiB file-size limit.

On cancellation or timeout, Bubblewrap terminates the worker (PID 1 in its private
namespace), causing the kernel to kill the remaining sandbox processes. Builds
return retrievable ELF artifacts through the same JSON-RPC connection.

## Test and benchmark

Runtime tests compile real C fixtures, so `clang` and `llc` must be in PATH:

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The additional Linux sandbox integration test needs Clang/LLVM, Bubblewrap and
working unprivileged user namespaces:

```sh
cargo test --release --locked --test build -- --include-ignored --nocapture
```

It tests real compilation/artifact execution, host-file isolation, source-path
validation, missing compiler tools, compiler resource exhaustion, cancellation,
cleanup, caching and all four examples.

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

On macOS, use uploaded eBPF objects or build them on a Linux spinfoam instance;
the local compiler service reports unavailable. Linux compiler isolation needs
unprivileged user namespaces and a recent bubblewrap
supporting the required namespace and mount controls. Bullseye specifies binary
glibc compatibility, not that its stock kernel/bubblewrap supports the sandbox.
