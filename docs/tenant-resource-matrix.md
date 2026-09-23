# Tenant Resource Matrix

The matrix is the source of truth for the hard-cutover authorization implementation. A query or job not listed here is blocked until it is classified.

| Resource | Scope | Tenant admin | Member | Platform |
|---|---|---|---|---|
| users | platform identity | manage members in own tenant | self only | root; operator allowlist |
| tenants | tenant control plane | settings in own tenant | read active context | root/operator lifecycle |
| tenant_memberships | tenant | full member CRUD except platform role | none | root explicit support; operator diagnostics only |
| tenant_invitations | tenant | create/revoke/consume own tenant | none | root explicit support; operator diagnostics only |
| produce_ai_keys | tenant + user owner | manage all own-tenant keys; secrets never read | own keys | root audited |
| accounts | tenant-owned | full CRUD in own tenant | use only | root global management; operator diagnostics |
| passthrough_bindings | tenant-owned | full CRUD in own tenant | use only | root global management; operator diagnostics |
| pricing_models | tenant or platform | tenant records only | read effective price | root global management; operator diagnostics |
| usage_logs | tenant + user owner | read own tenant | own records | operator aggregate only |
| user_balances | user-owned | view tenant records; mutation requires explicit billing action | own balance | root audited billing; operator aggregate read |
| payment_orders | user/tenant billing | own tenant records | own orders | root audited billing; operator aggregate read |
| balance_reservations | user/tenant request | read-only inspection; recover only expired own-tenant reservations | own request | root explicit audited force recovery; operator diagnostics |
| responses/scoped_responses | tenant + owner | all own-tenant Responses | own or shared | root audited |
| response_affinities | tenant + owner | tenant lookup without changing owner | own resources | root audited |
| conversations | tenant + owner | own-tenant management | own/shared | root audited |
| distribution_rules | tenant | full CRUD in own tenant | none | root global management; operator diagnostics |
| nodes/node_tasks | tenant + owner | own-tenant management | assigned use | root/operator operations |
| system_settings | platform | none | none | root only |
| global shared accounts | global shared | consume only | consume only | owner/root write |
| audit events | tenant/platform | read own tenant, cannot delete | no delete | root/operator policy |

## Invariants

1. Every tenant-scoped read, write, count, export, cache entry, and job carries a verified tenant scope.
2. Every user-owned resource validates (tenant_id, owner_user_id) together.
3. Tenant admin operations never change platform_role, resource owner, or billing subject.
4. Global sharing grants use rights only; it never grants management rights.
5. Personal /me endpoints always use the authenticated user ID.
6. Platform-wide queries require an explicit platform authorization decision.

## Required query families

DAO APIs must expose explicit families instead of nullable tenant selectors:

- find_platform_* for root/operator platform views;
- find_in_tenant_* for tenant admin views;
- find_for_user_* for personal views;
- find_shared_* for deliberately published global resources.

A direct find_by_id is forbidden for a tenant-sensitive resource unless the caller has already supplied a verified scope object and the DAO enforces it.

## Non-resource data

The following are not tenant invitations and must not be reused for membership:

- pending_registrations for registration verification;
- user_referrals for distribution referral chains;
- distribution referral links.

Membership invitation records must include tenant, target email, requested tenant role, hashed one-time token, expiry, status, and audit actor.

## Source inventory

`tenant-schema-inventory.tsv` classifies every table observed at phase 0.
`tenant-route-inventory.tsv` records all current route declarations and their
new trust domains; final routes are implemented in the route phase.
`tenant-cache-job-inventory.tsv` records cache and spawned-work reference sites.
`tenant-legacy-paths.tsv` is the removal inventory, not a permitted legacy API.
The line numbers are phase-0 navigation aids, not final source positions.

`tenant-implementation-decisions.md` resolves earlier draft differences in favor
of the latest request, including main-only delivery, membership status names,
explicit tenant selection and the independently deployed Go-service boundary.

## Wallet control state policy

Canonical root wallet commands require an explicit target tenant and owner,
current signed console authority, durable idempotency, and atomic audit. Tenant
administrators cannot mint, debit or freeze another member’s money. Their
reservation recovery capability requires the current ownership version and an
already-expired reservation. This restriction applies resource state in addition
to role; it does not transfer the request or billing owner. Root force recovery
retains the warning that already accepted late usage may still debit the original
wallet. Personal inference settlement remains an internal original-owner capability.

Console reservation pages are one primary read-only snapshot. They show the exact
persisted active-reservation total, including expired rows not yet reclaimed,
and never reclaim money as a side effect of a GET. Internal reclamation helpers
remain separate from these console queries.

## Platform settings and shared payment configuration

System settings and the node earnings ratio are platform-owned, not owned by a
default tenant. All console settings reads/writes require a current root global
scope, even when the root has no selected membership. A tenant admin or operator
role does not grant settings management. Canonical and retained URLs share this
same boundary. Sensitive and unclassified setting values never leave the safe
SQL projection; secret changes belong to dedicated credential workflows.

General updates validate the entire batch and paired payment limits inside the
audited transaction. The earnings ratio uses its own exact-decimal, versioned,
reason-audited command and cannot be updated through generic setting URLs.
Platform-wide payment policy does not change existing order/wallet tenant or
owner identity. Broader platform operation and final release gates remain separate.

## Operator operational reads

`/api/v1/platform/operations/**` exposes only tenant health metadata, bounded
per-currency usage aggregates, and an explicit process-capacity field allowlist.
Both root and operator may use these platform-scoped views without joining each
target tenant. They do not gain membership privileges on tenant URLs. Raw orders,
wallets, credentials, individual request traces, conversation content and business
mutations are not part of this grant. Existing broader platform lifecycle and
raw monitoring endpoints remain independently protected.

All operational queries revalidate current platform role, user/token state,
selected membership/tenant versions and expiry. Explicit `Platform` and `Tenant`
target variants distinguish aggregate domains; missing or nil tenant IDs never
become a wildcard. List/count use the same snapshot and filters. Financial totals
remain separate by currency with exact decimal strings. Aggregate routes use
the existing bounded heavy-read admission class, not a higher inference quota.
