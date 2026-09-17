# Generation resource admission

Resource budgets are independent of billable RPM/TPM and balance reservations.
They are **per application process**, not cluster-wide business quotas. Run
capacity tests before raising defaults; replica counts multiply local budgets.

| Budget | Active default | Waiting default |
| --- | ---: | ---: |
| Request setup / authentication | 256 | 128 |
| All generation requests | 256 | 128 |
| Per authenticated tenant | 32 | 16 |
| Per actual upstream account | 32 | 16 |

The account scheduler also has a process total of 256 active / 128 queued.
All queue waits have a default one-second deadline. Setting a queue size to zero
selects fail-fast. See `[gateway.admission]` and matching
`KC__GATEWAY__ADMISSION__*` settings in both production Compose configurations.

The ingress budget runs before authentication for the generation POST routes.
Tenant/global capacity is acquired after authoritative authentication and before
body buffering. HTTP response frames (including trailers), the executor and
settlement context clones retain the execution permit; returning HTTP headers
alone does not release it. Cancellation removes a queued ticket synchronously.
WebSocket handshakes do not count as generation; each response.create acquires
its own permit after reauthentication, including warmups. Existing socket and
large-body byte budgets continue to apply independently.

The keyed scheduler acquires total and per-key slots atomically. A saturated key
waiting in the bounded queue holds no total execution slot and does not block
eligible requests belonging to other keys. Keys disappear after their final
active/queued owner is gone, so arbitrary tenant/account IDs cannot grow a
permanent map. State updates never await while holding a mutex.

Upstream account slots are acquired for **each actual attempt**, including
fallback/compatibility retries, and held through the stream. Capacity rejection
skips retries for that account, may try an allowed fallback, and does not mark
an otherwise healthy provider unhealthy. No fallback occurs after client content
has committed. Exhaustion returns a sanitized 503 with Retry-After before stream
commit; existing protocol stream-error semantics apply after commit.

Node requests share global/tenant admission and retain it through waiting and
settlement; they do not consume a provider-account slot. Recovery workers for
already-persisted background Responses retain their separate bounded scheduling.
This change is not a count of all remote inference still running after a client
cancellation or process restart, and it does not replace upstream account quotas.

Queue rejection before dispatch does not debit upstream work. Existing ingress
business RPM checks occur before the executor and can already be recorded when
a later account-capacity check rejects; resource permits do not refund or widen
business quota decisions. Failed/ambiguous Redis writes remain fail-closed.

Regression tests cover burst bounds, queue-full/timeouts, cancellation, cleanup,
eligible-key fairness, context/body lifetime, admission before body/auth work,
upstream call suppression, fallback and active-stream cancellation.
