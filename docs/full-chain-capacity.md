# Full-chain capacity validation

`e2e_full_chain_load` is an opt-in open-loop load profile using real TCP requests
through the production router, API-key authentication, PostgreSQL, Redis caller
and account quotas, balance reservation, lifecycle tracing and durable billing.
Only the model is synthetic: a local HTTP server returns deterministic JSON or
SSE and provider usage. It never invokes paid external providers. CI runs a
small smoke profile; its passing does NOT prove a production throughput target.

## Environment safety

Use a dedicated loopback PostgreSQL database named `kc_load_*`, initialized with
the real schema, and a dedicated local Redis 7+ using noeviction. The Redis must
be explicitly marked with `keycompute:test:load-instance=disposable-load-v1`.
Never set that marker on production. The runner requires
`KC_LOAD_ACK_ISOLATED=1`; fixture writes happen only after these checks.
Use `CACHE_REDIS_URL` for a separate evictable cache to exercise the optional L2.
The runner owns unique users, tenants, accounts and keys and cleans its fixtures.
It does not print credentials, reset databases, flush Redis or overwrite reports.

## Running a profile

Configure `DATABASE_URL`, `KC__DATABASE__URL`, `REDIS_URL`, `KC__REDIS__URL` and
optionally `CACHE_REDIS_URL` for those disposable services, then run:

```sh
KC_LOAD_ACK_ISOLATED=1 KC_LOAD_SECONDS=30 KC_LOAD_RATE=20 \
KC_LOAD_TENANTS=4 KC_LOAD_USERS_PER_TENANT=2 KC_LOAD_ACCOUNTS=8 \
KC_LOAD_MODE=sse KC_LOAD_STREAM_MS=1000 KC_LOAD_DB_CONNECTIONS=10 \
KC_LOAD_REPORT=/tmp/new-unique-load-report.json \
cargo test --release -p integration-tests --test e2e_full_chain_load -- --ignored --nocapture
```

Settings are bounded. The client admits at most 512 outstanding requests and
reports generator drops and scheduling lag instead of silently converting an
open-loop profile into closed-loop throughput. A profile fails below 99% complete
success, on generator drops or when durable settlement does not converge. Such
failure reports are retained and must not be reported as a passing capacity.
Warmups and their ledger rows are separate from measured request counts.

Compare JSON and SSE, 1 versus many users/tenants, and 1/10/50/100 accounts.
Increase the offered rate progressively and retain both passing and failing
results. The report records successful RPS including the completion tail,
status counts, p50/p95/p99/max header, first-content and complete-response latency,
client scheduling lag, managed-memory usage and sampled RSS. A 200 response with
an incomplete/error stream is NOT success. A complete SSE must have content,
expected provider usage, DONE and no body transport error. Ledger counts and
settled reservations reconcile against the mock's completed calls; fixture
available+frozen+consumed funds must equal recharged funds.

## Scope and reproducibility

This harness does not include TLS, Nginx or a remote provider, and the load
client, synthetic provider and gateway share a process. Sampled RSS is for that
whole process, not server-only and not a proven peak. Record host CPU/memory,
container limits, database/Redis versions, commit, build settings and endpoint
latency alongside reports. The repository release profile optimizes size (`z`);
do not compare it with a different optimization profile without labeling both.
The report labels debug versus release. Fixed-rate results are tested operating
points, not an exhaustive maximum stable throughput claim or linear scaling law.

## Production diagnostics

`GET /api/v1/admin/monitoring/capacity` is protected by existing admin middleware
and explicit SystemAdmin permission, not by a role string. It exposes no tenant,
account, key IDs or connection URLs. It reports local ingress/generation/account
and balance queue occupancy, managed payload bytes, writer connection/idle counts,
Redis command/cache/blocking pool counts and fixed-cardinality stage histograms.
A snapshot is process-local and approximate across concurrently changing counters;
aggregate replicas externally. It is not a cluster-wide quota count.

Stage histograms cover admission waits, authentication, routing, balance queue
and reservation, account admission, full upstream attempt and immediate
settlement. Outcomes are ok/error/cancelled. Durations include pool/dependency
waits; nested stages overlap and MUST NOT be blindly summed. Histograms retain
cumulative counts/buckets, so compute window deltas and bucket quantiles rather
than averaging percentile values. Existing trace records retain end-to-end and
first-content details; header latency is not stream completion latency.
