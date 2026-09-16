# Local qualification results — 2026-09-16

The release binary ran distinct, independently loaded ELF objects on one Tokio
execution thread. Every object waited on events, then handled an event and an
acknowledged reverse RPC. The compiler was disabled during the population test;
its sandbox is tested separately. There are no aggregate program-memory quotas.

Host: Linux 6.12.101 Debian 13, x86_64, eight available CPUs, approximately 20 GB
RAM, `vm.max_map_count=1048576`. async-ebpf revision:
`7e8a7cbce195d68a1d579608b7a28b55f6fce7fd`.

| Measurement | 10,000 objects (initial run) | 20,000 objects |
| --- | ---: | ---: |
| Incremental active PSS / object | 62,654 bytes | 62,328 bytes |
| Incremental PSS / object after event/RPC | 63,244 bytes | 62,901 bytes |
| Successful event/RPC acknowledgements | 10,000 | 20,000 |
| Load + start time | 2.55 s | 5.48 s |
| Event delivery + RPC acknowledgement time | 0.61 s | 1.03 s |
| p99 idle control-request round trip | 0.134 ms | 0.167 ms |
| Active VMAs | 240,090 | 480,110 |
| Active virtual address space | 41.65 GB | 83.03 GB |
| Active page-table memory | 77.9 MB | 155.0 MB |
| Process threads during loading/JIT | 4 | 4 |
| Process threads after workers idle out | 2 | 2 |
| Wait period after event workload | 60 s | 120 s |

These are measured small monitoring loops, not a bound for arbitrary programs.
PSS differences subtract the initialized-process baseline and divide by object
count. Kernel page-table memory is reported separately. All four application
examples are also compiled and exercised with mocked embedder services in the
integration tests. The benchmark's ELF data differs for every loaded object; it
does not amortize one loaded VM over all loops.

The execution thread, preemption watcher, and two bounded loader/JIT workers
account for the four threads. Guest execution stays on the one execution thread.
The worker threads exit after becoming idle.

Unload retention is significant: process PSS after unloading 20,000 objects was
about 1.015 GB. async-ebpf retains execution contexts in a thread-local pool, and
the allocator also retains pages. This is documented behavior of the current
pinned dependency; unloading does not promise a low-RSS reset. A fresh process
releases the pool. A future bounded pool/trim API is an optimization, not a reason
to add a 1 MB runtime quota.

The 20,000-object measurement used the implementation at `2313db5`; documentation
and additional tests were being edited, so the raw result marks the checkout
dirty. Its release executable SHA-256 is
`c4b3ee4577bc55b3f44fab3eb333dedac00afff1a01b8b53adb03c9680e32d68`.
The initial 10,000 run preceded the final transport-hardening changes and did not
record a binary digest. Raw measurements are in [measurements/](measurements).

These short runs establish the requested population capacity for the measured
workload. They do not constitute the proposed 24-hour churn qualification, a
worst-case CPU latency bound, or an independently audited sandbox. The normal
suite and the explicit sandbox suite test fault containment, preemption,
backpressure, cancellation, ELF execution, source isolation and compiler resource
exhaustion. The sandbox suite was run with real cgroup delegation and the ordinary
runtime UID, without an unsandboxed compiler fallback.
