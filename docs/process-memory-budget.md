# Process payload memory budget

The server defaults to a shared 512 MiB retained-payload working-set budget,
configured by `gateway.managed_memory_mib` / `KC__GATEWAY__MANAGED_MEMORY_MIB`.
Values 64–16384 MiB are accepted. It is configured once before serving: multiple
AppState instances in a process must agree. It is NOT an allocator, exact RSS
limit, inference concurrency count or replacement for container limits.

The existing per-request, per-connection, large-object count and generation
concurrency limits continue to apply. A valid individual request may be rejected
when other requests already occupy the process byte budget. Do not multiply all
per-module maxima and assume they are simultaneously available.

## Covered ownership paths

- Generation HTTP buffering claims bytes before growing its buffer; conservative
  copy/tree headroom is reserved before JSON extraction. Input guards follow
  worker/context ownership and response delivery, including cancellation.
- Native JSON and SSE parsers account for retained raw buffers, JSON trees and
  projection/serialization copies. Payload event guards survive parser completion,
  executor queues and response-side queues. Parser buffers retain high-water
  claims until exit, deliberately conservative under long-lived streams.
- Final admitted HTTP data frames carry their own byte claims inside Bytes
  owners. Hyper or another consumer retaining/cloning a frame retains its claim.
- WebSocket handshakes reserve an 80 MiB wire-frame window before Tungstenite
  receives data. Queued request trees, stateless parent copies, continuation
  cache entries, decoder buffers and outbound bytes share the same budget.
- Node output projection and simulated streaming retain a shared output claim;
  normalized non-stream completion accumulation is also checked before growth.

Reservations never wait while holding a partial allocation: growth is fail-fast
and atomic under a short mutex. Last-owner Drop releases bytes synchronously;
no spawned cleanup can be lost. A permit clone shares ownership, not permission
to make unbudgeted deep copies. A separately owned copy needs its own claim or
explicitly reserved copy headroom. Small control/error metadata stays bounded.

Local parser/response memory exhaustion is not an upstream health failure.
The executor stops rather than retrying/falling back after a paid dispatch,
preserves conservative accounting, and exposes a native 503 before stream
commit. After response headers/content are committed, existing stream failure
semantics apply. Requests rejected before dispatch do not call the upstream.

## Scope and headroom

This controls managed generation payloads, not every allocation in a Rust
process. Runtime stacks, library internals, PostgreSQL result materialization,
small metadata/indexes, kernel sockets and allocator fragmentation need separate
headroom. The default 1 GiB server container leaves the other half for these
costs; that is an operating starting point, not a proof of a maximum RSS.
Already materialized database Node results are measured before further delivery
copies; database ingestion and non-generation administrative APIs still have
their separate limits. Monitor both managed bytes and actual RSS.

Legacy public library methods returning plain String/Value transfer allocation
ownership to their caller; native server paths use admitted envelopes. A custom
transport/provider that bypasses those envelopes must enforce its own bounds.
Direct cloning of custom payload objects is not transparently intercepted.

## Verification

`e2e_process_memory` runs in a dedicated test process with a 128 MiB budget. It
uses a real TCP WebSocket handshake, generation HTTP body middleware, native
OpenAI SSE parsing and frame consumption concurrently. It verifies shared
exhaustion, recovery after freeing one request, and final release only after the
last Bytes clone is dropped. It does not disable or reset another test process's
global budget. Core tests cover concurrent growth, failed growth, clone lifetime
and mixed claims. Protocol, billing and disconnect suites guard wire compatibility.

This test measures managed byte accounting, not production throughput or a
whole-process RSS ceiling. Full-chain load reports must record RSS separately.
