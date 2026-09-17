# Redis resource isolation

KeyCompute uses three independent connection pools per application instance,
all pointing to the configured Redis endpoint and database:

| Pool | Default maximum | Work |
| --- | ---: | --- |
| Fast commands (`pool_size`) | 10 | RPM/TPM, caches, task publication, completion notification, cleanup |
| Node claims (`node_poll_pool_size`) | 10 | Blocking task-queue BRPOP |
| Node results (`node_result_pool_size`) | 10 | Blocking completion-notification BRPOP |

Cloning a pool shares its capacity; it does not isolate blocking commands.
The Node constructor creates two new pools. Polls and result waits are separate
because many idle nodes must not prevent request workers from receiving results,
and many result waiters must not prevent nodes from claiming queued work.
The fast pool remains shared by short commands only.

## Configuration

Development `[redis]` keys and production `KC__REDIS__*` variables match:

| TOML key | Environment suffix | Default |
| --- | --- | ---: |
| `pool_size` | `POOL_SIZE` | 10 |
| `node_poll_pool_size` | `NODE_POLL_POOL_SIZE` | 10 |
| `node_result_pool_size` | `NODE_RESULT_POOL_SIZE` | 10 |
| `connect_timeout_secs` | `CONNECT_TIMEOUT_SECS` | 5 seconds |
| `pool_wait_timeout_ms` | `POOL_WAIT_TIMEOUT_MS` | 1000 ms |
| `command_timeout_ms` | `COMMAND_TIMEOUT_MS` | 5000 ms |

Capacity values must be 1–65536. Wait and command timeouts must be 1–60000 ms.
The pool wait budget applies to waiting for a slot and to recycling probes;
connection establishment has its separate connect timeout. All RuntimeStore URL factories use these finite defaults; wrapping an externally
created pool with `with_pool` retains that pool's caller-owned settings.
The command timeout
bounds short-command response waits even when Redis accepts a socket but stops
replying. These are distinct budgets, not one end-to-end request timeout.

Maximum pooled connections are the sum of all three capacities, multiplied by
application replicas. Pools connect lazily. Budget Redis `maxclients`, file
descriptors and memory for that sum plus administrative and other connections.
Blocking slots limit waiting connections, not the number of durable Node tasks.

## Cancellation and deadlines

A dropped redis-rs multiplexed request future does not cancel an already sent
BRPOP. An interrupted blocking operation therefore detaches its connection from
deadpool and drops the last handle, aborting the canonical redis-rs driver and
closing the socket. Such connections must not be recycled or cloned elsewhere.
Only a completely received successful reply (including normal Redis timeout/Nil)
returns the connection to its blocking pool. Transport errors and outer task
cancellation discard it. No additional Redis CLIENT UNBLOCK privilege is needed.

Pool acquisition is included in a Node poll's remaining blocking budget. BRPOP
uses fractional seconds and has 250 ms of finite transport/scheduling grace;
an enclosing request cancellation still wins. Zero/infinite waits are rejected.
A saturated pool returns a bounded error. Poll handlers stop that polling cycle
and keep their existing retry hint instead of acquiring once for every model.

Result notifications remain wake-up hints. PostgreSQL remains authoritative,
including when notification delivery fails or a result pool is full. The
fallback reads the writer and its retry cadence remains at least one second,
so fast Redis failure does not create a 100 ms database polling loop per task.
Queue publication and completion notifications still use the fast pool. Existing
idempotent task claiming, completion and sweeper recovery are unchanged.

## Verification

The Node Redis unit regressions use a real configured Redis server. They match
exact CLIENT IDs before checking isolation or cancelling tasks; sleeps alone are
not evidence that BRPOP started. They cover saturated pools, forward progress
of actual rate-limit checks and producers, cancellation cleanup, connection
reuse after normal replies/timeouts, transport errors, and finite fast-pool waits.
CI must provide Redis. Local developers can run:

```sh
REDIS_URL=redis://127.0.0.1:6379 cargo test -p node-gateway redis_isolation
```

Use a dedicated test Redis, not production. A loopback RESP test server also verifies that a missing BRPOP reply reaches its
internal deadline and the discarded socket closes, without pausing CI Redis.
The existing Node end-to-end suite
continues to exercise database-backed claim/completion/recovery behavior.

## Scope

This isolates client connection resources, not Redis server CPU, memory or
failure domains. It does not change eviction policy, implement tenant/account
in-flight admission, or claim a measured QPS increase. A timeout on a mutating
Redis command can have an ambiguous outcome; it must not introduce blind retries
or weaken existing fail-closed and durable recovery behavior.
