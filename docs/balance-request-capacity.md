# Balance request throughput and isolation

New generation reservations use a process-wide bounded admission queue BEFORE
borrowing a PostgreSQL connection: one active reservation per user, at most 64
active across users, 256 total queued / 32 per user, and a one-second queue wait.
Admission releases on cancellation and unused keys disappear. This is a local
resource optimization, NOT an account balance cache or distributed money lock.
Other monetary operations still use their authoritative database lock ordering.

Every reservation retains tenant-before-balance-before-reservation locking,
active/frozen-sum checks, expiry reclamation, exact Decimal arithmetic, owner
fencing and ledger/audit semantics. The common fresh-reservation path moves
balance and inserts its reservation in one data-modifying CTE. This removes one
network roundtrip while holding the hot balance lock. SQL failures/conflicting
request identities roll back both mutations. Existing request resizing/replay
continues through the original ownership-checked path.

Before taking row locks, transaction-local lock (1s), statement (2s) and idle
transaction (5s) deadlines bound stalled work. Pool acquisition retains its
configured finite timeout. No new blanket timeout interrupts COMMIT: the existing
detached worker drives completion before handing reservation ownership back.
Owner-token compensation and durable expiry remain mandatory; no blind upstream
retry or balance refund was added.

The same user remains serialized for actual balance mutation, including across
replicas. This deliberately preserves monetary correctness. The improvement is
shorter held time and fewer same-user connection waiters, not lock-free money or
an asserted throughput multiplier. Tests use real PostgreSQL to hold one user's
row while a two-connection service admits another user, and to verify concurrent
cross-user request-ID conflicts cannot debit twice. Full-chain benchmarks must
measure pool wait, reserve duration, completion and settlement separately.
