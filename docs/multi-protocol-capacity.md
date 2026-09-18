# Multi-protocol capacity acceptance

The capacity suite runs Chat, native Responses, Anthropic, WebSocket and Node
workloads through the existing isolated TLS/AOF lab. It also tests large bodies,
two gateway replicas, and a replica drain/rejoin during ongoing requests.
Run `python3 scripts/capacity/suite.py --help` for required binary paths.
Use a fresh output directory; failures are retained and stop the suite.

All model fixtures declare eight input and eight output tokens regardless of
payload size. They test transport and final usage accounting, not actual model
context lengths or inference speed. The verifier requires that same 8/8 usage
in every ledger row, zero active reservations and conserved balances.
Large-output HTTP readers also verify the delivered content byte count.

The WSS client uses Tungstenite and Rustls with the generated lab CA. It shares
one connection across at most sixteen correlated lanes. This is not a benchmark
of many independent WebSocket connections. Node fixtures seed a temporary
session, then use the normal heartbeat, poll and completion routes.

## Request and estimation boundaries

Nginx now matches the finite protocol request budgets: Chat 96 MiB, Responses
80 MiB, Messages 32 MiB and Node control 2 MiB. Uploads on these paths are sent
to the application without Nginx buffering the complete body first. The existing
application authentication, per-request sizes and shared memory budget remain.
Other API limits are unchanged.

Token estimation uses o200k for strings of at most 8192 UTF-8 bytes. Larger
strings use a saturated UTF-8 byte-count estimate so unbounded BPE merge work
cannot monopolize an async worker. This is provisional and may substantially
overestimate tokens and reserve more quota or money. Positive final provider
usage still replaces the estimate; missing usage retains the existing estimated
accounting semantics. It is not exact token counting for every provider, does
not change model output limits, and does not start detached tokenizer jobs.

## Recovery and interpretation

Drain/rejoin is limited to one replica created by the current lab. The runner
removes it from new Nginx upstream selection, allows admitted work to drain,
restarts it, waits for readiness and adds it back. It checks final ledger and
balance convergence. This is controlled rollout recovery, not a hard power-loss
or exactly-once inference guarantee. A shared host and synthetic model remain
limitations. Sampled resource values and short operating points must not be
presented as a maximum production throughput or linear scaling result.

During reload, the test client checks a fully consumed idle connection before
sending a new request and discards sockets already readable/closed by Nginx.
This is not a POST retry. An error after sending begins remains a failed sample,
with a typed transport diagnostic. Tests enforce that a request without a
response is never replayed. Reload recovery still requires at least 99 percent
complete success, zero generator drops and durable accounting convergence.

The suite records short operating points, not maximum protocol capacities.
Large-output bytes are synthetic; Node session registration and power-loss
recovery are not simulated by the controlled drain/rejoin case. Existing lease,
usage-outbox and disconnect regression suites remain part of final validation.

Recovery injection waits for an explicit signal written after client warmup and
initial diagnostics. It does not use elapsed container startup time as evidence
that measured traffic has begun. A cancelled or failed startup terminates that
wait rather than injecting a fault into an uninitialized workload.
