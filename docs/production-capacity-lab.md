# Isolated production-shaped capacity lab

`scripts/capacity/lab.py` runs the real release server, model simulator and load
client in DIFFERENT containers, behind the repository Nginx configuration with
TLS. The client verifies the generated lab certificate; it does not disable
certificate validation or use an external model. PostgreSQL enables fsync,
synchronous_commit, pg_stat_statements and I/O timing. Critical Redis uses AOF
everysec/noeviction; its disposable cache is another server.

Build fresh binaries from the candidate being evaluated, never reuse an old
production image executable:

```sh
cargo build --release --locked -p keycompute-server -p integration-tests \
  --bin keycompute-server --bin capacity_fixture
python3 scripts/capacity/lab.py --ack-isolated \
  --server-bin target/release/keycompute-server \
  --fixture-bin target/release/capacity_fixture \
  --output /tmp/new-unique-kc-lab --rate 20 --seconds 60
```

Prerequisites: Docker, OpenSSL and installed images named keycompute-server:latest
(runtime libraries only), keycompute-web:latest (Nginx), postgres:16-alpine,
redis:7-alpine and python:3.12-alpine. The runner does not pull images. Override
runtime/web/python image names explicitly when required. Supplied executables
must match the recorded source tree; hashes and dirty state are recorded, not
proof of reproducible compilation. Release uses repository opt-level=z.

Each run creates a random-labelled INTERNAL Docker network and new containers.
Only Nginx publishes a randomly assigned loopback TLS port. No existing volume,
service, database, host credential or external provider is used. Mutations and
cleanup are checked against the run label; collisions fail rather than attach.
Reports never overwrite. Private fixture keys, environment and raw logs stay
under a mode-0700 output tree. Do not publish private/ or logs/. Labels protect
normal mistakes, not hostile users with Docker administrator access.

Default limits match the repository: gateway 2 CPU/1 GiB per replica,
PostgreSQL 1 CPU/512 MiB, critical Redis .5 CPU/768 MiB (maxmemory 256 MiB),
cache .5 CPU/256 MiB (128 MiB maxmemory), Nginx .5 CPU/128 MiB. The independent
client has 2 CPU/512 MiB and the synthetic model 1 CPU/256 MiB. This remains one
host, not a distributed hardware capacity guarantee. Runtime background workers
are enabled as in the real binary. Automatic paid account probes remain disabled.

The bounded open-loop generator reports scheduled lag/drops; it never retries a
failed generation silently. Complete content, provider usage and stream DONE
are required. Success ratio must be >=99% with zero generator drops, plus
converged ledger/reservations and conserved funds. Failed runs retain reports.
Sampled container usage is separate for the gateway, client, model and stores.
PostgreSQL reports classify queries without publishing raw SQL; WAL counters
are cluster-wide and execution/lock/BEGIN stages overlap. Do not sum them.

`--replicas`, user/tenant/account counts, request payload, stream duration and
resource admission settings support controlled comparisons. Queue limits are
finite. A changed tenant count changes both policy capacity and data contention.
These are observed workload points, not the maximum RPS or a proof of linear
scaling. AOF everysec and SQL commits do not guarantee zero loss on storage
failure. Deployment, real provider behavior, a managed TLS chain and multi-host
latency remain separate acceptance work.

Nginx keeps its image's internal port-80 static health check while adding TLS
on port 443. Only 443 is published; the measured client always uses verified
TLS. PostgreSQL interval counters include warmups, observation and settlement
tail (not only measured requests); use the reported counts and intervals when
normalizing them. Application stage deltas bracket the measured client window.
