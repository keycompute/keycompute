# Settlement contention and measurement

Post-ledger balance mutations use their own process-local bounded per-user
completion lane BEFORE borrowing a writer connection. They do not share new
reservation tickets: fresh admission must not starve accepted completions. The
permit spans reservation settlement and its ownership-checked consume fallback,
not distribution or Node tips. Queue failures remain errors for durable replay;
no ledger, ownership fencing or database money lock is removed. Direct model
callers and other balance operations retain their original database protection.

Ordinary provider completions use one writer-fresh Node-task existence probe
instead of starting a tip transaction and reading the ledger/task to find no
node. Eligible Node work retains the original transaction and all authoritative
rechecks. The probe uses locking syntax for writer routing, not authorization.

Fixed-label stages now separate outbox persistence, immutable ledger write,
balance settlement/queue, distribution and tips. Writer transaction begin
measures pool acquisition PLUS BEGIN roundtrip, not pure connection wait.
Balance row-lock query time includes query execution, lock wait and transport;
never label it exact lock time or sum nested stage histograms as total latency.
Use isolated PostgreSQL pg_stat_statements, pg_stat_activity, pg_stat_wal and
track_io_timing/track_wal_io_timing for SQL counts, sampled waits and WAL/fsync.
No diagnostic reset or configuration write is performed on a production DB.

`balance_settlements` in the admin capacity JSON reports this local completion
lane; `balance_reservations` retains its original admission-only meaning.

The read-only `scripts/capacity/postgres_probe.py` collector requires a
nonce-labelled capacity container and a kc_load_ database, removes raw SQL text
from reports, and detects statistics resets/eviction instead of claiming a false
speedup. Activity samples are observations, not exact accumulated lock time.
Statement-level execution and WAL counters complement application stage timing;
they do not identify pure pool wait or fsync duration for one HTTP request.
