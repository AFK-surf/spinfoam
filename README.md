# spinfoam

Massively concurrent background loops for AI agents. spinfoam is an independent
Rust process that runs C-authored userspace eBPF programs on Tokio and exposes
bidirectional JSON-RPC over stdio. Each loaded object is independent. The embedder
owns external services, credentials, persistence and distributed orchestration.

The async-ebpf dependency is pinned directly to GitHub commit
`7e8a7cbce195d68a1d579608b7a28b55f6fce7fd`. Linux GNU and macOS binaries are built for x86_64 and arm64.
The upstream formal verification applies to the x86_64 core; it does not
extend to the arm64 backend. Compiler builds use Bubblewrap on Linux and sandbox-exec on macOS.
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

Install LLVM and put `clang` and `llc` in PATH. Linux also needs Bubblewrap and
`ldd`; macOS uses `/usr/bin/sandbox-exec` and `otool`. spinfoam discovers the
tools and their shared libraries automatically. On macOS, use Homebrew LLVM
(`brew install llvm@19`), with `$(brew --prefix llvm@19)/bin` in PATH; Apple's
system Clang does not provide the required BPF toolchain.

Enable compilation with one flag:

```sh
target/release/spinfoam --enable-builds
```

No cgroup delegation or privileged launcher is needed. Linux must permit
unprivileged user namespaces.

The startup probe executes a real sandboxed build. `sf.initialize` reports whether
it succeeded and why it failed otherwise. Builds fail closed if namespaces,
seccomp, compiler tools or resource limits are unavailable; existing ELF execution
remains usable. There is no insecure bypass flag. Restart spinfoam after upgrading the compiler
tools or libraries; discovery and the build cache are scoped to one process.

On Linux, each build uses separate user/mount/PID/network/IPC/UTS namespaces, a
syscall allowlist, no-new-privileges, dropped capabilities, read-only source/SDK/
toolchain mounts and a 16 MiB writable tmpfs. Only the discovered libraries and tools
are mounted, not the host filesystem. Each compiler process has a 1 GiB address-space
limit, a 256 MiB data/allocation limit and a five-second CPU limit. These are
per-process limits, not an aggregate RSS quota or CPU bandwidth control. Compiler
threads share those budgets; a second seccomp filter denies new child processes.
The trusted worker launches Clang and LLVM sequentially. The build also has a
15-second wall deadline, 64-descriptor limit and 16 MiB file-size limit.

On Linux cancellation or timeout, Bubblewrap terminates the worker (PID 1 in its private
namespace), causing the kernel to kill the remaining sandbox processes. Builds
return retrievable ELF artifacts through the same JSON-RPC connection.

On macOS, each compiler runs directly under a deny-by-default Seatbelt profile
using `sandbox-exec`. Only source/SDK files, discovered tool libraries and system
libraries are readable. Each stage can write only its designated output file;
network access, forking and unrelated host files are denied. CPU, file-size and
descriptor limits apply per process, with a shared 15-second build deadline.
A monitor samples physical footprint every 25 ms and terminates a compiler above
256 MiB; this is a sampled memory budget that can overshoot, not Linux's hard
allocation/address-space limit. Cancellation kills and reaps the direct compiler
process. The compiler cannot spawn descendants.

## Test and benchmark

Runtime tests compile real C fixtures, so `clang` and `llc` must be in PATH:

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The additional sandbox integration test requires a working platform sandbox
and LLVM:

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

Linux compiler isolation needs unprivileged user namespaces and a recent Bubblewrap
supporting the required namespace and mount controls. Bullseye specifies binary
glibc compatibility, not that its stock kernel/Bubblewrap supports the sandbox.
macOS compiler isolation is tested on both Intel and Apple Silicon.
