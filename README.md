# spinfoam

Massively concurrent background loops for AI agents. spinfoam is an independent
Rust process that runs C-authored userspace eBPF programs on Tokio and exposes
bidirectional JSON-RPC over stdio. Each loaded object is independent. The embedder
owns external services, credentials, persistence and distributed orchestration.

The async-ebpf dependency is pinned directly to GitHub commit
`7e8a7cbce195d68a1d579608b7a28b55f6fce7fd`. Linux x86_64 is the qualified target.
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

Install Clang/LLVM and Bubblewrap. LLVM 19.1.7 and Bubblewrap from Debian 13 were
used for qualification. Generate a manifest pinning the tool files and their
shared libraries, then keep those files immutable while the runtime is running:

```sh
target/release/spinfoam --write-toolchain-manifest toolchain.json
```

Run spinfoam inside a **delegated cgroup v2 subtree** with the `cpu`, `memory` and
`pids` controllers enabled for children. Its runtime process must be in a leaf
under that subtree; the subtree itself must be empty so domain controllers can
be enabled. The runtime user needs permission to create child cgroups and move
its own processes within the subtree. A systemd service with `Delegate=yes` can
provide delegation; your service launcher arranges the runtime leaf and enables
the controllers. spinfoam never escalates privileges or changes host delegation.

Pass the delegated subtree and manifest when launching the child:

```sh
target/release/spinfoam \
  --toolchain-manifest toolchain.json \
  --compiler-cgroup /sys/fs/cgroup/your-delegated-subtree
```

The startup probe executes a real sandboxed build. `sf.initialize` reports whether
it succeeded and why it failed otherwise. Builds fail closed if namespaces,
seccomp, pinned files or cgroup controls are unavailable; existing ELF execution
remains usable. There is no insecure bypass flag.

Each build uses separate user/mount/PID/network/IPC/UTS/cgroup namespaces, a
syscall allowlist, no-new-privileges, dropped capabilities, read-only source/SDK/
toolchain mounts and a 16 MiB writable tmpfs. Only the pinned libraries and tools
are mounted, not the host filesystem. Limits are 256 MiB memory, no swap, 16 tasks,
one CPU of bandwidth, five CPU seconds and a 15-second wall deadline. Cancellation
kills the entire build cgroup and reaps its processes. Builds return retrievable
ELF artifacts through the same JSON-RPC connection.

## Test and benchmark

Runtime tests compile real C fixtures, so `clang` and `llc` must be in PATH:

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The additional sandbox integration test requires the same cgroup delegation as
the production build service. Run the test process inside the runtime leaf:

```sh
SPINFOAM_TEST_CGROUP=/sys/fs/cgroup/your-delegated-subtree \
  cargo test --locked --test build -- --include-ignored --nocapture
```

It tests real compilation/artifact execution, host-file isolation, source-path
validation, changed toolchain pins, compiler resource exhaustion, cancellation,
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
