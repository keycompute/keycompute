# Upstream account quotas and capacity-aware routing

## Scope

With Redis enabled, each account ID has shared RPM, TPM and in-flight state
across application replicas, tenants, users and API keys. Existing caller
quotas remain independent. The process-local admission/queue budgets from
`generation-admission.md` still protect each server before account admission.
Without Redis, the account counters are explicitly process-local memory state.

The namespaces are `keycompute:account-quota:v1` and
`keycompute:account-inflight:v1`. Neither includes a caller tenant identity.
Every physical upstream attempt (including compatibility retries and fallbacks)
has its own UUID. Only the selected account is charged; routing/debug snapshots
never reserve execution capacity or consume RPM.

## Admission and lifetime

Before each dispatch the writer rechecks the account owner/caller tenant state,
visibility, enabled/health state, protocol capability, model eligibility,
endpoint and credentials. A stale route cannot dispatch against a changed
connection configuration. This is a fresh authorization check, not a lock held
across the remote HTTP call.

Admission reserves one shared in-flight slot and the conservative token
prediction using the existing atomic reservation scripts, then atomically
checks/debits account RPM. Intermediate rejection cleans up reservations before
dispatch. No stage uses check-then-increment counters. The stages are separate
atomic operations rather than a cross-command transaction: an interrupted
admission may conservatively retain a reservation until expiry, but cannot
contact the upstream without all stages succeeding. Quota dependency errors
fail closed rather than trying every account against an unavailable backend.

`gateway.admission.account_limit` is both a local per-account limit and the
shared account limit when Redis is configured. All replicas must use consistent
limits and execution timeouts. The shared in-flight lease window is the larger
of `gateway.timeout_secs` and `gateway.stream_timeout_secs`, plus 120 seconds,
with an upper bound of 24 hours. Every 15 seconds the executing owner strictly
renews both existing token and in-flight reservations within a five-second I/O
budget. A missing reservation is never recreated by a heartbeat; loss stops the
upstream stream without marking a healthy provider unhealthy or launching a
fallback after an ambiguous outcome.

Clean completion releases the in-flight slot and replaces predicted tokens
with final provider usage when available. An explicit non-success HTTP response
can release its slot, but its unknown token cost is conservatively retained.
When usage is not final, the admitted prediction stays charged instead of zero.
Cancellation, uncertain transport failures and process failure may retain shared
capacity until lease expiry. This deliberately favors avoiding overbooking over
immediate capacity reuse; the separate local permit is released when its owner
drops. Operators should account for that recovery delay during upstream outages.

These are gateway-attempt admission budgets, not a guarantee that a remote model
stops computing when the connection closes. Native background Responses may
continue remotely after HTTP completion and are not counted as open gateway
connections. Token predictions are not injected as output limits into provider
requests; exact final usage can exceed a prediction and blocks subsequent work.
Quota state relies on Redis durability and consistent deployment settings; this
is not a claim of exact counting across loss of the Redis dataset.

## Clock and scheduling

Redis RPM pruning/debit and current account completion accounting use Redis TIME,
not each application replica's wall clock. Explicit historical caller settlement
keeps its original occurrence timestamp and horizon.

Routing samples at most 32 eligible candidates per route by default: static
leaders plus rotating exploration. This bounds quota-read work even for a much
larger account pool. Selection is approximate, not a promise of the globally
least-loaded account. Writer-side discovery remains fresh and may still read
all matching account metadata; credentials, health and visibility are not cached.

Advisory snapshots live at most 100 ms from refresh start in a bounded 4096-entry
cache shared by clones of one routing engine. Per-account locks collapse
concurrent refreshes, with at most 16 backend refreshes across the engine and
8 consumers per route. The 1000 ms per-refresh deadline includes lock and global
slot waits. Cancellation releases all slots. Errors are not cached and expired
hints are never served on error. Authoritative per-attempt admission is uncached.

A Redis snapshot reads three counters on ONE role-checked connection instead
of three. Existing Lua, hash tags and corruption checks remain unchanged; this
is an advisory snapshot, not an atomic cross-counter transaction.

`gateway.routing_capacity` (and `KC__GATEWAY__ROUTING_CAPACITY__*`) configures
`candidate_limit`, `cache_entries`, `ttl_ms`, `concurrency`, and `timeout_ms`.
Zero TTL disables reuse. The maximum candidate cap is 256 and maximum TTL is
1000 ms. Rotating exploration prevents large equal pools from being excluded
permanently by truncation. Available accounts rank before exhausted accounts, then by existing
health penalty, maximum normalized RPM/TPM/in-flight utilization, static priority
and rotating equal-capacity tie order. Rotation runs before the three-account
fallback-plan limit, so an equal pool larger than three accounts can all become
primary. This is advisory scheduling, not a reservation: execution atomically
rechecks capacity and may reject or take an eligible fallback. Quota snapshot
failure is not interpreted as an idle account.

## Verification

Regression coverage includes multiple service instances sharing one Redis,
cross-tenant callers, concurrent account-slot admission, RPM/TPM boundaries,
idempotent completion, unknown usage, lost-lease cancellation and trace closure,
writer-side account changes, normalized load ranking, and equal-account rotation.
No production request benchmark or linear scaling factor is claimed.
