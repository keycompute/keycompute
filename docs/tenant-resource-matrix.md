# Tenant Resource Matrix

The matrix is the source of truth for the hard-cutover authorization implementation. A query or job not listed here is blocked until it is classified.

| Resource | Scope | Tenant admin | Member | Platform |
|---|---|---|---|---|
| users | platform identity | manage ordinary membership state in own tenant, not global users or peer administrators | self only | root/operator platform administration; only root may manage an operator and neither may modify a root |
| tenants | tenant control plane | settings in own tenant | read active context | root/operator implicit administration through explicit platform-target APIs |
| tenant_memberships | tenant | manage ordinary members and promote them; existing administrators cannot be changed | read own selected context | root/operator are never membership rows; explicit platform-target administration may change administrator memberships |
| tenant_invitations | tenant | create/revoke in own tenant | accept an invitation addressed to the verified user | platform authority never turns root/operator into an invitee or member |
| produce_ai_keys | tenant + user owner | manage all own-tenant keys; secrets never read | own keys | root/operator audited administration with explicit tenant target |
| accounts | tenant-owned | full CRUD in own tenant | use only | root/operator platform management with explicit ownership scope |
| passthrough_bindings | tenant-owned | full CRUD in own tenant | use only | root/operator platform management with explicit ownership scope |
| pricing_models | tenant or platform | tenant records only | read effective price | root/operator platform or explicit-tenant management |
| usage_logs | tenant + user owner | read own tenant | own records | root/operator explicit-tenant administration and platform aggregate views |
| user_balances | user-owned | view tenant records; mutation requires explicit billing action | own balance | root/operator explicit audited billing administration |
| payment_orders | user/tenant billing | own tenant records | own orders | root/operator explicit audited billing administration |
| balance_reservations | user/tenant request | read-only inspection; recover only expired own-tenant reservations | own request | root/operator explicit audited administration and force recovery |
| responses/scoped_responses | tenant + owner | all own-tenant Responses | own or shared | root/operator audited administration with explicit tenant and owner targets |
| response_affinities | tenant + owner | tenant lookup without changing owner | own resources | root/operator audited administration with explicit tenant and owner targets |
| conversations | tenant + owner | own-tenant management | own/shared | root/operator audited administration with explicit tenant and owner targets |
| distribution_rules | tenant | full CRUD in own tenant | none | root/operator explicit-tenant management |
| nodes/node_tasks | tenant + owner | own-tenant management | assigned use | root/operator operations |
| system_settings | platform | none | none | root/operator management |
| global shared accounts | global shared | consume only | consume only | root/operator management |
| audit events | authority + optional target tenant | read own tenant-scoped events, cannot delete | no delete | root/operator platform timeline, including target-tenant filters |

## Invariants

1. Every tenant-scoped read, write, count, export, cache entry, and job carries a verified tenant scope.
2. Every user-owned resource validates (tenant_id, owner_user_id) together.
3. Tenant admin operations never change platform_role, resource owner, or billing subject.
4. Global sharing grants use rights only; it never grants management rights.
5. Personal /me endpoints always use the authenticated user ID.
6. Platform-wide queries require an explicit platform authorization decision.
7. Audit authority (`scope_type`) and affected tenant (`tenant_id`) are
   independent; platform operations never become tenant operations merely
   because they target a tenant.
8. Root and operator identities have implicit administrative authority over
   every tenant, but they never acquire a tenant membership or tenant role. The ordinary tenant URL remains membership-bound; a platform
   operation must carry the target tenant explicitly through a platform route.
9. Tenants have no owner. They may have multiple peer administrators, and every
   persisted member is an ordinary non-platform user. A platform-created tenant
   begins with no members; root/operator may directly grant an eligible ordinary
   user an administrator membership without an invitation.
10. Creating a tenant never creates a membership for the root/operator actor.
    Each ordinary user may have at most one non-removed tenant membership.
    Tenant administrators use invitations; platform administrators may use
    explicit direct membership administration, while their own authority remains
    implicit and global.
11. Tenant administrators are peers. A tenant-scoped command cannot demote,
    suspend, restore, or remove any administrator membership, including the
    caller's own. Only an audited root/operator platform-target command may
    change a non-removed administrator membership. A removed row is historical;
    re-entry requires a fresh invitation (or an explicit platform grant) and may
    assign the newly granted role.

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
`tenant-object-coverage.tsv` records representative route-to-handler-to-scope-
DAO-to-test evidence for high-risk tenant and platform resources; it is
intentionally a staged coverage matrix rather than a replacement for the
complete route inventory.
`tenant-cache-job-inventory.tsv` records cache and spawned-work reference sites.
`tenant-cache-job-coverage.tsv` records staged runtime-test evidence for selected
high-risk cache/job sites; it is expanded as each subsystem is adopted.
`tenant-dynamic-sql-review.tsv` records targeted dynamically assembled SQL
builders, required tenant/owner predicate tokens and their integration tests.
`tenant-legacy-paths.tsv` is the removal inventory, not a permitted legacy API.
The line numbers are phase-0 navigation aids, not final source positions.

`tenant-implementation-decisions.md` resolves earlier draft differences in favor
of the latest request, including main-only delivery, membership status names,
explicit tenant selection and the independently deployed Go-service boundary.

## Wallet control state policy

Canonical platform wallet commands require an explicit target tenant and owner,
current signed console authority, durable idempotency, and atomic audit. Tenant
administrators cannot mint, debit or freeze another member’s money. Their
reservation recovery capability requires the current ownership version and an
already-expired reservation. This restriction applies resource state in addition
to role; it does not transfer the request or billing owner. Platform force recovery
retains the warning that already accepted late usage may still debit the original
wallet. Personal inference settlement remains an internal original-owner capability.

Console reservation pages are one primary read-only snapshot. They show the exact
persisted active-reservation total, including expired rows not yet reclaimed,
and never reclaim money as a side effect of a GET. Internal reclamation helpers
remain separate from these console queries.

## Platform settings and shared payment configuration

System settings and the node earnings ratio are platform-owned, not owned by a
tenant. All console settings reads/writes require a current root/operator global
scope; no selected membership is involved. A tenant-admin role alone does not
grant settings management. Canonical and retained URLs share this same boundary.
Sensitive and unclassified setting values never leave the safe SQL projection;
secret changes belong to dedicated credential workflows.

General updates validate the entire batch and paired payment limits inside the
audited transaction. The earnings ratio uses its own exact-decimal, versioned,
reason-audited command and cannot be updated through generic setting URLs.
Platform-wide payment policy does not change existing order/wallet tenant or
owner identity. Broader platform operation and final release gates remain separate.

## Platform identities and tenant administration

Root and operator are global identities without a selected tenant. Both roles
may administer any tenant through an explicit platform-target operation and do
not appear in tenant member or administrator lists. This is an authorization
grant, not synthetic membership. Tenant handlers must not infer this grant from
a selected workspace and must not insert platform identities into
`tenant_memberships`.

Root and operator have the same tenant-resource authority. Their only identity
hierarchy difference is protected-user management: root may manage operator;
operator may not modify root or operator; root can never be deleted or modified
by another actor.

### Current platform-parity audit

Explicit platform targeting is implemented for tenant lifecycle and control,
members, invitations, tenant audit, API-key metadata and
issuance, detailed usage/billing/payment reporting, tip earnings and withdrawal
administration, provider and passthrough management, pricing, nodes and tasks,
distribution, wallet commands, Responses and conversations, and routing
diagnostics. Platform-created provider accounts remain global by the ownership
rule; this is intentional rather than a missing tenant selector.

The membership-bound `/api/v1/tenants/{tenant_id}/...` routes continue to reject
root/operator global sessions. Platform authority is exposed only through an
equivalent `/api/v1/platform/tenants/{tenant_id}/...` target and must never be
implemented by synthesizing membership or relaxing a tenant-route extractor.

## Operator operational reads

`/api/v1/platform/operations/**` remains the bounded read-only operational view
for tenant health metadata, per-currency usage aggregates, and an explicit
process-capacity field allowlist. Both root and operator may use these views
without joining each target tenant. This bounded view does not replace their
separate, explicit platform-target administration APIs, and it never grants
membership privileges on tenant URLs.

All operational queries revalidate current platform role, user/token state and
expiry on the primary. Tenant selection is deliberately not an authority input
for these bounded reads. Explicit `Platform` and `Tenant` target variants
distinguish aggregate domains; missing or nil tenant IDs never become a wildcard.
List/count use the same snapshot and filters. Financial totals remain separate by
currency with exact decimal strings. Aggregate routes use the existing bounded
heavy-read admission class, not a higher inference quota.

## Platform user and tenant lifecycle

Canonical platform user CRUD and tenant create/update/delete/detail use original
signed global authority inside retained database transactions. They do not
require joining a tenant. Operator has platform lifecycle authority except over
root or operator identities; root may manage operator identities, while root
identities remain protected from modification and deletion. Safe user projection
includes last-login time only, not credential rows. Lists/counts share a
literal-filtered primary snapshot.

Target tenants and target/actor users are locked in deterministic order;
last-root, assigned-owner and retained-resource constraints remain authoritative.
An authorized self-role change or selected-tenant deactivation may invalidate the
current token as its intended result. It commits under the existing locked grant
and final wall-clock expiry check, then the old token fails subsequent requests.
Mutation audit failure rolls back identity/profile/membership/lifecycle changes.
The create-tenant client supplies only tenant metadata. Initial access is
granted later and does not change the platform actor's membership state.
