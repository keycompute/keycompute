# Tenant Resource Matrix

The matrix is the source of truth for the hard-cutover authorization implementation. A query or job not listed here is blocked until it is classified.

| Resource | Scope | Tenant admin | Member | Platform |
|---|---|---|---|---|
| users | platform identity | manage members in own tenant | self only | root; operator allowlist |
| tenants | tenant control plane | settings in own tenant | read active context | root/operator lifecycle |
| tenant_memberships | tenant | full member CRUD except platform role | none | root/operator support |
| tenant_invitations | tenant | create/revoke/consume own tenant | none | root/operator support |
| produce_ai_keys | tenant + user owner | manage all own-tenant keys; secrets never read | own keys | root audited |
| accounts | tenant-owned | full CRUD in own tenant | use only | root/operator global |
| passthrough_bindings | tenant-owned | full CRUD in own tenant | use only | root/operator global |
| pricing_models | tenant or platform | tenant records only | read effective price | root/operator global |
| usage_logs | tenant + user owner | read own tenant | own records | operator aggregate only |
| user_balances | user-owned | view tenant records; mutation requires explicit billing action | own balance | root/operator payment policy |
| payment_orders | user/tenant billing | own tenant records | own orders | root/operator payment policy |
| balance_reservations | user/tenant request | inspect/release own tenant request | own request | root/operator recovery |
| responses/scoped_responses | tenant + owner | all own-tenant Responses | own or shared | root audited |
| response_affinities | tenant + owner | tenant lookup without changing owner | own resources | root audited |
| conversations | tenant + owner | own-tenant management | own/shared | root audited |
| distribution_rules | tenant | full CRUD in own tenant | none | root/operator global |
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
