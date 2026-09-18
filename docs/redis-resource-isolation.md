# Redis resource and storage isolation

## Deployment contract

KeyCompute's Redis-backed server requires Redis 7 or newer. Critical state and
rebuildable price caches must use different writable Redis servers, not merely
different logical databases or credentials. The server reads INFO to verify
version, role, memory policy and server run_id; it never changes live Redis config.

| Server / pool | Default maximum connections | Responsibility |
| --- | ---: | --- |
| Critical / commands (`pool_size`) | 10 | Caller and account quotas, runtime affinity, task publication, notifications, cleanup |
| Critical / Node claims (`node_poll_pool_size`) | 10 | Blocking task-queue BRPOP |
| Critical / Node results (`node_result_pool_size`) | 10 | Blocking completion-notification BRPOP |
| Critical / cache identity verification | 1, only with cache configured | Verify physical separation without borrowing command slots |
| Disposable / cache (`cache_pool_size`) | 10 | Rebuildable price values and best-effort cache stampede locks |

Pools are independently constructed and connect lazily. Cloning a pool does not
isolate its slots. Replica counts multiply these budgets: with all defaults and
cache enabled, budget 31 critical and 10 cache connections per application
instance, plus administrative connections. This is not an inference concurrency
or throughput guarantee.

Both production Compose templates preserve the existing critical data volume
and configure these policies:

| Redis role | maxmemory | Container limit | Persistence |
| --- | ---: | ---: | --- |
| Critical state | 256 MiB, noeviction | 768 MiB | AOF everysec and RDB |
| Disposable cache | 128 MiB, allkeys-lru | 256 MiB | No AOF, no snapshots |

The additional memory is headroom, not a proof that a fork, persistence backlog
or pathological command can never exhaust RAM. Monitor RSS, maxmemory usage,
mem_not_counted_for_evict, rejected writes, latency and persistence failures.
AOF everysec does not guarantee zero data loss on machine/storage failure.

## Configuration and rollout

Development `[redis]` keys have matching production `KC__REDIS__*` variables:

| Key | Default |
| --- | --- |
| `url` | Critical endpoint; production must configure it |
| `pool_size`, `node_poll_pool_size`, `node_result_pool_size` | 10 each |
| `connect_timeout_secs` | 5 seconds |
| `pool_wait_timeout_ms` | 1000 ms |
| `command_timeout_ms` | 5000 ms |
| `cache_url` | Unset: distributed price cache disabled, never replaced by critical Redis |
| `cache_pool_size` | 10 |
| `cache_timeout_ms` | 500 ms |

Capacity values must be 1–65536, and millisecond budgets must be 1–60000.
Compose defaults `CACHE_URL` to the separate redis-cache service; an explicit
empty environment override disables it. Development can omit `cache_url`.

Before deploying the new server to an existing installation, configure the
critical Redis as a writable primary with noeviction and sufficient memory
headroom, while retaining its data volume. Otherwise new server connections
fail closed. Build from the reviewed commit; editing a Compose file does not
update a running container. Do not reset unrelated working-directory changes.
The code change itself does not deploy or restart any production service.

All role-bearing pools verify on creation AND every checkout of a recycled
connection. Invalid idle sockets are discarded. INFO failure, unsupported Redis,
or the wrong role cannot silently pass. Cache identity checks use a dedicated
metadata pool, avoiding a new dependency on limiter/producer connection slots.
Checks add read-only INFO work; benchmark the full request path when sizing.
A role check cannot reverse evictions or stop an administrator changing policy
while a command is already checked out. Restrict CONFIG and topology changes to
operators; grant application ACLs only their required commands including INFO,
TIME and scripting commands. Do not reconfigure an active critical instance to
an evictable role.

Generic RuntimeStore factories and externally supplied pools remain library
APIs with caller-owned policy. Production AppState and Node pools select the
critical role explicitly.

## Script behavior under memory pressure

Redis 7 flagged Eval scripts are used deliberately. Scripts that can add state
start at byte zero with `#!lua` and do not opt into allow-oom:

- RPM debit, TPM reserve/renew/restore and terminal reconciliation.
- Node task republishing and result-notification replacement.

They are denied before any script command under OOM. In particular a rejected
republish/notification does not remove the old queue entry before failing.
A legacy cleanup-first script can otherwise keep allocating above maxmemory.
Normal task LPUSH and cache SET remain ordinary OOM-sensitive commands.

Only bounded cleanup scripts use `#!lua flags=allow-oom`: RPM expiry pruning,
TPM expiry/query cleanup, pending-reservation release and owner-checked cache
lock deletion. TPM cleanup adds no request IDs or expiry members; it validates
existing state and updates only its nonincreasing aggregate with SET XX KEEPTTL.
It preserves terminal deduplication records and cannot create missing aggregates.
These are bounded metadata operations, not a claim of literally zero transient
allocation inside Redis. They permit cleanup even while new work is refused.

Renewal and terminal reconciliation can grow or restore state, so they also
fail closed under OOM. Existing predictions are not removed on admission denial.
Account renewal failures stop the executor through its existing lease-loss path;
settlement errors retain conservative state and existing durable recovery paths.
After memory recovers, retries retain terminal idempotency. Do not bypass failed
quota writes or blindly replay upstream requests. No script gains cross-slot
privileges; TPM keys still use the same Redis Cluster hash tag.

## Optional cache recovery

A configured cache pool remains attached after an unsuccessful startup probe.
Requests retry one probe at a time with exponential backoff from 1 to 30 seconds.
A cancelled probe releases its slot; an old success cannot erase a newer failure.
The complete optional-cache checkout is bounded by cache_timeout_ms, including
recycling and role verification, not merely TCP connection establishment.
Optional cache lock acquisition has the same independent finite budget.
During backoff, pricing reads fall back to the authoritative source without
waiting for a distributed cache lock. A later successful role-checked checkout
restores caching without restarting the application. No background task retains
AppState or continuously probes an unused cache. `CacheService::is_available`
indicates configuration, not live reachability; `probe()` tests a checkout.

## Blocking operations and durability

A dropped redis-rs request future does not cancel an already-sent BRPOP. Cancelled,
failed or timed-out blocking connections are detached from deadpool and closed.
Only completely received successful replies, including normal Nil timeout,
recycle the connection. Node polls include pool acquisition in their exact
remaining deadline, with 250 ms of finite response grace and outer cancellation.

Result notifications are wake-up hints. PostgreSQL remains authoritative;
failed publications are recoverable by the existing sweeper. Result fallback
reads the writer and is paced to at least one second per cycle. OOM, connection
isolation and noeviction do not replace database-backed task claim, completion,
settlement and recovery semantics.

## Verification

`e2e_redis_roles` checks separate servers, cache eviction and critical-state
survival. `e2e_redis_safety` checks real RPM/TPM and Node operations under OOM,
pre-mutation rejection, cleanup, recovery, existing-socket policy drift and
optional cache restoration without consuming command-pool capacity.

Fault tests mutate only an explicitly provided `FAULT_REDIS_URL` whose instance
contains `keycompute:test:fault-instance = disposable-redis-v1`. Never put that
marker on production. CI creates a separate fault Redis and a separate 8 MiB
cache Redis. Normal Redis tests use `REDIS_URL`; only fault tests change config.
Default local runs without a fault endpoint do not run destructive tests.
