# Graceful drain and account-lease observability

## Two phases, not immediate listener cancellation

The production signal handler drives `run_with_shutdown`; it no longer drops
Axum's server future as soon as a stop signal arrives. First the instance marks
itself draining, closes ingress and generation admission, and wakes queued
requests with a retryable rejection. Existing execution permits remain valid.
The account gate stays open for already admitted attempts and their legitimate
fallbacks; no new request may acquire a generation permit.

`GET /ready` becomes 503 while `/health` remains a liveness endpoint. Readiness
indicates acceptance state, not a new database/Redis health guarantee. Configure
the actual load balancer to stop assigning new work when readiness is false;
Docker health status alone is not a cluster load-balancer implementation.
Nginx proxies exact `/ready` to the backend, preserving 503 instead of returning
the SPA's fallback 200; other liveness/static routes are unchanged.

During the first phase the server continues polling its listener. New ordinary
requests are rejected before maintenance/auth/body work with 503 and Retry-After.
Only health probes, the authenticated admin capacity view, Node heartbeat, and
exact UUID Node completion routes remain reachable so a leased task can finish
its waiting request and operators can inspect the drain. Their
existing session authentication and completion validation still run. Registration
and polling cannot start more Node work through the draining instance.

HTTP response frames and handlers remain tracked; retained generation contexts
also keep the server alive through their final owners and settlement. Idle
Responses WebSockets close with code 1001. Active lanes stop accepting new input,
finish admitted work and flush their output before closing; unadmitted queued
creates hit the closed generation gate. Lane/writer tasks abort on owner drop.

After request, WebSocket and retained admission owners reach zero, the listener
stops accepting and Axum drains its remaining connections. The deadline bounds
both phases. Each accepted IO object has a force signal, including upgraded
sockets: dropping Serve alone would not cancel Axum's spawned connection drivers.
On deadline, pending handlers and transports are cancelled, with at most one
additional second for cooperative IO cleanup. The server reports an error rather
than claiming every request drained. Cancelling the entire serving future also
marks drain and signals force; it must not strand independently spawned drivers.

## Budgets and rollout

`server.shutdown_timeout_secs` / `KC__SERVER__SHUTDOWN_TIMEOUT_SECS` defaults to
120 seconds, valid 1–3600. Both production Compose templates allow 130 seconds
of `stop_grace_period` by default (`KC_SERVER_STOP_GRACE_PERIOD`). When changing
the application deadline, make supervisor grace longer than that deadline plus
cleanup and scheduling headroom. Requests longer than the deadline may be cut;
set the deployment budget based on the desired stream-drain policy, not merely
because the upstream timeout accepts a long stream.

No code change deploys or restarts existing containers. The drain does not prove
remote inference stops computing, nor exactly-once execution after an ambiguous
network failure. Existing durable usage outboxes, balance-owner fencing, Node
leases/sweeper and conservative account reservation expiry remain unchanged.
Already-persisted background recovery is independently bounded and may resume
on another process after exit; this is not a global distributed drain barrier.
Do not erase shared account reservations or replay upstream generation blindly
to accelerate shutdown. Keep the existing Redis/Pg durability deployment contract.

## Diagnostics and tests

The admin-only capacity endpoint includes draining/forced flags, HTTP in-flight,
WebSocket and open-transport counts. Account-lease counters distinguish admission,
quota rejection, dependency errors, lost renewal, released/retained completion,
settlement error and abandoned local ownership. The live-owner gauge is LOCAL:
Redis may conservatively retain capacity after a cancelled owner disappears.
It must not be displayed as available cluster quota. Labels contain no principal
or account identities. Repeated completion does not duplicate its terminal event;
telemetry does not release or modify a shared quota lease.

Regressions exercise real TCP streaming plus a completion request on a NEW
connection after draining begins, readiness and rejection, pending body/handler
force cancellation, detached permit lifetime, serving-future cancellation, idle
WebSocket closure, exact Node-route exceptions, authentication retention and
idempotent admission closure. Existing lease-loss, accounting-recovery and
full-chain billing tests remain required. Test teardown stops only test tasks,
not the machine or production services.
