# Tenant financial-control console

`/tenant/finance/controls` is the selected tenant administrator's narrow control
surface. It is intentionally separate from the read-only `/tenant/finance` report
page and from root/platform money administration. A platform root/operator label
without a current tenant-admin membership does not mount these controls.

## Expired request reservation recovery

The administrator explicitly selects a member UUID and reads that member's active
request reservations through the existing tenant wallet route. The response must
match the requested owner, contain exact decimal strings, unique real request IDs,
nonzero opaque ownership versions, `active` state and a bounded continuation cursor.
Foreign or malformed rows fail closed; they are never filtered into an apparently
valid page. Reads remain fresh and do not create a wallet, reclaim expiry or move
money.

The browser only enables recovery when the displayed RFC3339 expiry is in the past,
but the server remains authoritative. Recovery sends the exact observed ownership
version plus a bounded reason. A future/live reservation, changed version or changed
financial authority is rejected by the server. The command is single-dispatch: a
401/409/503 or uncertain transport result is not automatically replayed. A confirmed
response must preserve tenant owner/request identity, exact decimal fields, reason,
a real releasing actor and the existing late-usage warning.

Recovery does not cancel upstream work. Already accepted usage may settle later and
debit the original wallet. The existing FinancialScope transaction preserves the
wallet -> reservation lock order, audit atomicity, tenant membership/version checks
and post-lock credential expiry checks. This page does not expose recharge, consume,
freeze, unfreeze or root force-recovery APIs.

## Withdrawal review

The page reads tenant withdrawal metadata only. Its DTO rejects unknown response
fields so encrypted recipient account/name payloads cannot silently enter this
control surface. The current backend projection contains no payout ciphertext or
recipient fingerprint. The UI renders owner, amount, status/revision, type and
created time; it does not call payout support-detail or payment-completion routes.

Only a `pending` Alipay withdrawal has Approve/Reject actions. Each command carries
the exact observed monotonic revision and a reason of at most 500 UTF-8 bytes. The
result must preserve immutable withdrawal ID, owner, request, type, amount and
currency, advance the revision, contain the current reviewer metadata, and remain
non-completed with no payout reference or balance transaction. Approval is a review
state transition only: it does not send money and cannot attest an external payout.
Root-only recipient access and completion remain separate audited platform flows.
Balance-conversion withdrawals are not review buttons because their accepted flow
completes transactionally at creation.

## Session and browser lifecycle

Both controls use the existing verified workspace identity. Lists are keyed by the
selected tenant plus submitted owner/filter/page state. Commands use the shared
single-dispatch workspace guard, so logout, user change or tenant switch cannot
publish an old private result into the new workspace. Same-workspace token refresh
is not treated as a different owner.

Tenant reservation and withdrawal reads also perform `ConsoleSessionProof` on the
writer after their final data query, matching the read-only financial report policy.
The underlying scoped SQL is still the primary authorization boundary; this final
check prevents a delayed result from being returned after the original signed
console session has become stale. Responses are private/no-store; wallet responses
also carry no-cache.

The compiled-browser acceptance runner uses synthetic same-origin HTTP and verifies
expired-only UI behavior, exact version/revision payloads, no platform payout calls,
uncertain-write single dispatch, workspace-race isolation, foreign-row fail-closed
behavior and tenant-admin route gates. It is UI evidence only. Real PostgreSQL/Axum
wallet/tip suites separately verify financial locks, audit rollback, current roles,
credential expiry, payout-secret separation and root-only completion.

No production database, balance, payout, payment provider, credential, SMTP, service
restart or deployment is changed by this console delivery.
