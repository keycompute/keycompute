# Workload-specific long-stream capacity

A policy rejection is not necessarily a missing executor feature. Four tenants
with 32 in-flight requests each supply only 128 occupied generation slots, even
when the process global setting is 256. An assumed 40 arrivals/second held for
five seconds needs about 200 slots before front-door, delivery and settlement
work. A one-second queue cannot make a permanently overloaded service stable.

## Explicit plan, not a default-limit increase

```sh
python3 scripts/capacity/plan.py --rate 40 --stream-ms 5000 \
  --processing-ms 250 --tenants 4 --accounts 8 --headroom-percent 20 \
  --global-limit 256 --queue-ms 500 --writer-connections 10
```

The example assumes 5.25 seconds of mean slot ownership and 20% headroom. It
needs 252 cluster slots, at least 63 for the hottest equal-sized tenant, and 32
per shared account. It preserves the explicit 256 global budget instead of
silently increasing it. Its nominal waiting-arrival budget is 20 global / 5
per tenant with a 500 ms cutoff. These are sizing assumptions, not a proof of
latency or memory capacity. Exact final token quotas may reject work earlier.

The planner accepts explicit tenant/instance skew and conservatively allows a
hot tenant to concentrate on one instance; it never divides a shared account
limit by replica count. Uniform instance balance is an assumption that must be
measured. An infeasible plan returns no deployable settings. Writer pool size
is an operator trial input, not a formula derived from RPS. All validation and
financial fencing in the service remain unchanged.

## Required comparison

Use the separate-process TLS/AOF lab in `production-capacity-lab.md`. Hold the
same users, model, request rate and stream length fixed. Compare default tenant
admission with a workload-matched tenant budget; then vary writer connections
separately. For a 40 RPS / 5-second run use 256 client workers so the generator
can represent the workload without imposing its own 128-request ceiling.
Do not disable the protected queue merely to get a larger success count.

Add first-content and complete-response p99 targets to the success/ledger gates
using `plan.assess`. Model first-token delay belongs in the latency target.
For the example 10-chunk model the first content is intentionally delayed about
500 ms; a candidate target is 1500 ms first-content and 6500 ms completion.
A run that succeeds financially but misses that target is NOT accepted as a
latency-qualified capacity. Retain rejected operating points and all generator
lag/drop, queue, RSS, SQL/WAL and settlement evidence.

These files do not change production defaults or deploy a tuned plan. Apply a
validated profile only to the intended workload after checking actual CPU/RSS,
upstream quotas, failure recovery and load distribution. Multi-protocol and
multi-instance behavior needs its own acceptance suite.

## Recorded single-host validation, 2026-09-18

Using the production-shaped lab (real release server 2 CPU/1 GiB, PostgreSQL
1 CPU/512 MiB, Redis AOF, verified Nginx TLS) with four tenants/one user each,
eight accounts and 256 bounded client workers:

| Profile | Measured requests | Completed | Complete p99 | First-content p99 |
| --- | ---: | ---: | ---: | ---: |
| Default tenant 32, queue 128/16, 1000 ms; 20-second run | 800 | 543 | 6032 ms | 1510 ms |
| Tenant 63, queue 20/5, 500 ms, writer 10; 30-second run A | 1200 | 1200 | 5113 ms | 580 ms |
| Same matched settings, fresh run B | 1200 | 1200 | 5098 ms | 564 ms |
| Same matched settings, writer 20 | 1200 | 1200 | 5476 ms | 882 ms |

All profiles used 40 offered RPS and a five-second synthetic stream. All ledger,
reservation and fund checks converged, including the rejected control. Default
control is NOT a passing capacity. Both matched writer-10 runs met >=99% complete
success, zero generator drops and 1500/6500 ms first-content/completion p99 goals.
Increasing writer connections to 20 did not improve this sample and had a
10-second maximum; it is not the recommended default for this measured profile.
The process global cap remained 256 and shared account cap 32 in every case.
Matched settings change tenant capacity AND bounded waiting policy; this is a
profile validation, not a one-variable causal claim or an unlimited SLO guarantee.
The complete raw reports remain outside the repository; none contain production
traffic. Revalidate on target hardware and representative token/payload sizes.
