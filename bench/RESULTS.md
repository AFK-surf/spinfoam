# Local qualification results

## Embedded TinyCC — 2026-09-17

The benchmark now compiles its C fixture through `sf.build.submit` using the
embedded TinyCC guest. A single build produced the ELF template; every loaded object received its
own distinct global seed. The compiler remains enabled throughout the run.

On the same x86_64 Linux host described below, 10,000 TinyCC-generated loops all
stayed active on one execution thread and acknowledged their event/RPC workload:

| Measurement | 10,000 objects |
| --- | ---: |
| Incremental active PSS / object | 64,312 bytes |
| Incremental PSS / object after event/RPC | 67,194 bytes |
| Successful event/RPC acknowledgements | 10,000 |
| Load + start time | 3.07 s |
| Event delivery + RPC acknowledgement time | 1.56 s |
| p99 idle control-request round trip | 0.223 ms |
| PSS before the compiler ran | 3.91 MB |
| PSS after compiling, before object loading | 21.70 MB |
| Total PSS with active objects | 664.81 MB |
| Process threads during loading/JIT / after idle | 4 / 2 |
| Wait period after event workload | 60 s |

Per-object increments subtract the post-compilation baseline; the compiler's
retained allocations are reported separately rather than charged to each loop.
This is a short qualification of small loops, not a memory bound for arbitrary
programs. The compiler's 8 MiB guest arena is outside the loop memory target.

This run used the Bullseye-built binary with TinyCC fetched and compiled by Cargo,
pinned to fork commit `120e1619c7b3913e9ec2a10d06e431d17b23648e`. The checkout
was clean at `9da3d57` (the implementation was built at `073d0c9`; the next commit
only clarified README wording). The executable SHA-256 is
`d9843fa97703db2c4812bb1000dec5e00a0a0b5c7f0dc53ddc41346eb7b44ae9`.
[Raw measurement](measurements/tinycc-fork-10000.json) includes compiler provenance,
memory snapshots and unload retention. All four application
examples are also compiled through TinyCC and executed in integration tests on
Linux and macOS, x86_64 and arm64.

These short runs qualify the measured population of small monitoring loops;
they do not establish a bound for arbitrary programs or a 24-hour churn result.
Guest execution uses one Tokio OS thread, assisted by the preemption watcher and
two bounded loader/JIT workers. The workers exit when idle.

Execution contexts and allocator pages can remain pooled after unload: this run
retained about 543 MB PSS after unloading all 10,000 objects. Unloading therefore
does not promise an immediate low-RSS reset. The process's virtual mappings and
kernel page-table memory are reported separately in the raw measurements.
