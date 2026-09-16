# Protocol v1

spinfoam is a Linux x86_64 child process. Its stdin and stdout must be pipes or Unix
sockets. Write one JSON-RPC 2.0 object per UTF-8 line; read stdout continuously,
including while waiting for a response. stderr is diagnostic output. Start by
negotiating version 1. JSON-RPC IDs are strings up to 128 bytes and must be unique
among outstanding requests. Parameters are named JSON objects; batches are not
supported. Control methods require requests with IDs; incoming notifications are
ignored. Outgoing state/log notifications are advisory and may be dropped.

```json
{"jsonrpc":"2.0","id":"init","method":"sf.initialize","params":{"protocol_version":1}}
```

The response includes `session_id`, SDK version, target, pinned async-ebpf revision,
compiler availability/fingerprint, limits, and `memory_target_enforced: false`.
Object IDs are unique within a session. They carry no ownership/group hierarchy.
A load creates an independent program even if another object has identical bytes.

## Objects

`sf.object.load` takes exactly one of `elf` (standard base64) or `artifact_id`,
plus optional `config` (any bounded JSON value, default null) and `capabilities`
(default empty). It validates/loads the ELF without starting it.

```json
{"jsonrpc":"2.0","id":"load","method":"sf.object.load","params":{"artifact_id":"ab1","config":{"run_id":123,"repository":"owner/repo","deduplication_key":"run:123"},"capabilities":[{"name":"github.run.read","arguments":{"run_id":123,"repository":"owner/repo"}},{"name":"agent.notify"}]}}
```

A capability permits an exact method name. Its optional `arguments` map constrains
specified **top-level** argument fields by exact JSON equality. Other fields may
be present. This is deliberately a small policy mechanism; the embedder remains
responsible for validating parameters and authorizing external actions. Runtime
helpers supply object identity themselves, so a guest cannot choose another ID.

Methods taking `{ "object_id": "o1" }`:

| Method | Result |
| --- | --- |
| `sf.object.start` | Schedule the object's single invocation; it can start once. |
| `sf.object.get` | Current state, wait reason, mailbox length, ELF SHA-256 and outcome. |
| `sf.object.stop` | Cancel and wait for execution/host-wait cleanup; return terminal status. |
| `sf.object.unload` | Stop if needed and release the object record; idempotent for missing IDs. |

States: `loaded`, `starting`, `running`, `stopping`, `stopped`, `exited`, `failed`.
`running` includes a `wait` of `timer`, `event`, `host_rpc`, or `none`. Completion
returns `{ "exit_code": <signed 64-bit value> }`; guest faults return an error and
`kind: "PROGRAM_FAULT"`. Use a C `sf_i64` entry return type when returning negative
SDK statuses; the VM reports the 64-bit eBPF return register. A start reply does
not guarantee that every future lazy-JIT path will compile.

`sf.object.list` accepts optional `after` (exclusive object ID cursor) and `limit`
(1–256, default 100). Returns `objects` and `next`; ordering is lexical by object
ID. Listing is not a snapshot across pages. `sf.stats` takes `{}` and reports
object count, pending/ignored host responses, bounded-output counters, uptime and
execution-thread count.

Restart by loading again, with a new object ID. There is no automatic restart,
live stack migration, or durable local state. Unload releases object ownership,
but async-ebpf's execution-context pool and allocator may retain resident pages;
already-started JIT workers can briefly outlive cancellation.

## Events

```json
{"jsonrpc":"2.0","id":"event1","method":"sf.event.deliver","params":{"object_id":"o1","event_id":"delivery-123","topic":"webhook","payload":{"kind":"deploy"}}}
```

A reply `{ "accepted": true, "duplicate": false }` means admission to volatile
memory, not processing or durable storage. The C program receives a JSON handle
for `{event_id, topic, payload}`. Event IDs and topics are at most 128 UTF-8 bytes.
A mailbox holds at most 32 events and 32 KiB of encoded event data; an individual
event envelope is at most 16 KiB. Overflow returns `MAILBOX_FULL` immediately.
Events can be queued before start. Stopped/exited/failed objects reject events.

Delivery is FIFO per object. The most recent 128 accepted event IDs are deduplicated
for the object's lifetime, including after consumption. Retries beyond that window
can be delivered again. spinfoam does not coalesce events; an embedder can coalesce
device updates before delivery. For durable processed acknowledgements, use an
explicit host capability and the event ID. The webhook example demonstrates this.

## Guest-to-host RPC

The embedder receives requests in the opposite direction on stdout:

```json
{"jsonrpc":"2.0","id":"sf:SESSION:42","method":"host.call","params":{"object_id":"o1","capability":"github.run.read","arguments":{"run_id":123},"timeout_ms":10000,"deadline_unix_ms":1800000010000,"max_result_bytes":16384}}
```

Reply on stdin using exactly that ID, with a JSON-RPC `result` or `error`:

```json
{"jsonrpc":"2.0","id":"sf:SESSION:42","result":{"status":"completed","conclusion":"success"}}
```

Always enforce the operation deadline in the embedder. A request can spend time
in the pipe; `deadline_unix_ms` expresses its original deadline, while `timeout_ms`
is its original duration. The runtime's own timeout uses a monotonic timer.
Results are bounded to 16 KiB of encoded JSON, nesting depth 32 and 4096 nodes.
An error response becomes `SF_HOST_ERROR`; an oversized result becomes `SF_LIMIT`.
Unknown, duplicate, timed-out and old-session replies are discarded and counted.

Cancellation/timeouts remove pending routing and queued unsent requests. For a
request handed to the writer, a best-effort notification is emitted:

```json
{"jsonrpc":"2.0","method":"host.cancel","params":{"id":"sf:SESSION:42"}}
```

Cancellation does not undo an external action. Keep application deduplication
keys for notifications and other side effects; the runtime never retries a host
RPC automatically. Use an acknowledged capability such as `agent.notify` for
important outcomes. A pipe write or a JSON-RPC notification is not durable delivery.

## Builds and artifacts

The runtime advertises `compiler.available: false` if no sandbox is configured or
its startup probe fails. It continues to accept existing ELF objects. There is no
unsandboxed compiler fallback.

```json
{"jsonrpc":"2.0","id":"build","method":"sf.build.submit","params":{"sdk_version":1,"entry":"main.c","files":{"main.c":"#include \"spinfoam.h\"\nSF_MAIN sf_i64 main(void) { sf_sleep_ms(1000); return 42; }"}}}
```

`files` is a map of relative source/header names to UTF-8 content, up to 32 files
and 128 KiB total content. Paths use ASCII letters/digits, underscores, dots,
hyphens and slashes; absolute paths, dot/dot-dot components, empty components,
file/directory conflicts and any component named `spinfoam.h` are rejected.
There are no custom compiler flags, plugins, environment variables or host paths.
The supplied SDK is read-only. The initial profile supports one C translation unit
plus headers, little-endian BPF v3, and 4096-byte frames.

Submission returns a `build_id` and status. `sf.build.status` and `sf.build.cancel`
take `{ "build_id": "b1" }`. States: `queued`, `running`, `succeeded`, `failed`,
`cancelled`. Cancellation waits for whole-job cleanup. `sf.build.finished` is an
advisory notification; query status for the authoritative result.

Successful `result` fields include `artifact_id`, `sha256`, `sdk_version`, `cached`,
`diagnostics` and `diagnostics_truncated`. Failed builds include `kind: "BUILD_FAILED"`
and an error. Capture drains compiler stderr regardless of truncation; raw capture
is limited to 64 KiB and returned text to 16 KiB so JSON escaping fits a frame.
ELF output is limited to 64 KiB. The compiler pipeline materializes zero globals as
PROGBITS with `llc --nozero-initialized-in-bss`, because this pinned VM does not
accept ordinary BSS relocations.

`sf.artifact.get` takes `{ "artifact_id": "ab1" }`, returning base64 `elf`, its hash,
SDK version and toolchain fingerprint. A successful build can be loaded directly
by artifact ID. Artifacts expire after 30 minutes and are bounded by 128 entries
and 8 MiB. Build history is bounded to approximately 128 records; active jobs are
never evicted. One build runs at a time with at most 16 active/queued jobs.
Cache keys include all sources, SDK, fixed profile, runtime binary and pinned
toolchain. Availability of a cache entry is not a durability guarantee.

## Errors and transport bounds

Standard JSON-RPC error codes cover parsing, malformed requests, methods and
parameters. Application errors carry a stable `error.data.kind`:
`NOT_INITIALIZED`, `BUSY`, `MAILBOX_FULL`, `OBJECT_NOT_FOUND`, `INVALID_OBJECT`,
`SANDBOX_UNAVAILABLE`, `BUILD_NOT_FOUND`, `ARTIFACT_NOT_FOUND`, and
`RESPONSE_TOO_LARGE`. Guest faults appear in object status. Unknown parameters
are rejected for control methods.

Frames including newline are limited to 256 KiB. An oversized or unterminated
frame closes the session. Malformed bounded JSON gets a parse error. There are
48 outstanding request slots, 64 reserved control-output slots, 4 MiB of queued
object output and 64 KiB queued output per object. Data output is round-robin;
control output gets priority with an eight-frame burst limit. Excess requests
get `BUSY`; if even its control reply cannot be queued, the session closes.
Logs are limited to 1024 bytes and ten attempts per second per object, and may be
dropped. These are operational bounds, not a total per-program memory quota.

`sf.shutdown` with `{}` acknowledges shutdown, then cancels all objects/builds,
reaps compiler processes, and drains output for at most two seconds. stdin EOF,
broken stdout, SIGINT and SIGTERM also trigger cleanup. Already-started JIT work
has a bounded shutdown wait. Runtime state does not survive process exit.
