# KeyCompute Tenant Authorization Model

This document defines the final authorization model for the Rust service and any service that shares its identity or authorization data.

The schema is greenfield-only in the current development phase. There is no runtime
compatibility path for the legacy `system/admin/user` role model, no dual-write,
and no fallback from a missing tenant scope to a global query.

## Final role axes

### PlatformRole

- `root`: platform security, tenant lifecycle, platform identities, global resources, and audited break-glass access.
- `operator`: an explicit allowlist of operational actions; no default access to tenant secrets, private Responses, or billing writes.
- `none`: no platform administration.

### TenantRole

- `admin`: all tenant-owned resource management, member management, and invitations in one active tenant.
- `member`: tenant resource use and personal resource management only.

A tenant role never grants a platform role. A platform role never silently grants access to private tenant data.

## Registration and membership lifecycle

Registration creates a global user identity, verified credential and optional
referral relation. It does not create a tenant, membership, balance or initial
credit. A newly registered user therefore has `platform_role = none`, no active
tenant memberships and a valid global console session.

Tenant membership is added explicitly by tenant administration or by accepting a
single-use invitation addressed to the user's verified email. Accepting an
invitation adds or reactivates that tenant membership; it does not remove any
other active membership. Users may belong to multiple tenants and select one
verified membership for tenant-scoped APIs, or clear the selection to return to a
global identity. The selected tenant is a request context, not the user's global
ownership or platform role.

## Resource ownership contract

Every resource is classified as one of:

- **platform**: system settings, platform identities, global pricing, and platform-owned providers.
- **tenant**: accounts, bindings, tenant pricing, tenant quotas, tenant distribution, tenant nodes, tenant usage, and tenant Responses.
- **user**: personal credentials, personal billing assets, and private resources owned by one user.
- **global shared**: explicitly published resources that other tenants may use without gaining management rights.

Tenant-scoped data always carries a server-verified `tenant_id`. User-owned data carries both `tenant_id` and `owner_user_id`. A request-supplied tenant ID is only a selector and must be checked against the verified membership or platform scope.

Tenant administrators may manage all tenant resources, including other members' tenant Responses. The original owner and billing subject remain immutable. Secrets are rotatable and revocable, never readable in plaintext.

Tenant-owned personal-resource `/me` endpoints remain `tenant_id + user_id`
scoped, even for a tenant admin. Global profile endpoints such as `/api/v1/me`
remain user-scoped and do not invent a tenant.

## Audit authority and target

Audit records have two independent dimensions:

- `scope_type` is the authority that performed the operation: `platform`,
  `tenant`, `user`, or `system`.
- `tenant_id` is the affected tenant, when the operation has a tenant target;
  it is not a proxy for the authority that performed the operation.

Creating, updating, suspending, or deleting a tenant is therefore a platform
operation with the target tenant retained in the audit row. A tenant admin
operating after selecting that tenant produces a tenant-scoped event. Personal
`/me` actions, invitation acceptance, and owner self-service node actions are
user-scoped even when they affect a tenant-owned resource. Background/bootstrap
work is system-scoped. Tenant audit views include only `scope_type = tenant`;
platform audit views can filter by the target `tenant_id`, including after a
tenant has been deleted.

## Authorization decision

The trusted service layer evaluates the intersection of:

1. credential kind;
2. platform role and platform action;
3. active tenant membership and tenant action;
4. target resource tenant;
5. owner/shared policy;
6. tenant and resource lifecycle state.

The final context contains:

- subject user ID;
- credential kind;
- platform role;
- active tenant ID;
- verified tenant role and membership status;
- token and authorization versions; tenant configuration `revision` is not an
  authorization input;
- calculated permissions.

API keys are fixed to their tenant and only receive inference permissions. Console sessions, inference keys, and node sessions are distinct credential kinds.

## Resource inventory and implementation scope

The final implementation must audit and replace authorization for:

- users, tenants, memberships, invitations, and audit events;
- produce AI keys, provider accounts, and passthrough bindings;
- pricing, usage, balances, payment, and distribution;
- Responses, conversations, gateway requests, and settlements;
- nodes, node sessions, node tasks, and scoped gateway resources;
- SQL queries, Redis/local cache keys, WebSockets, and background workers.

No handler may perform an unscoped object lookup for a tenant-sensitive resource. Platform-wide queries must use an explicit `PlatformScope`.

## Hard-cutover data model

The final schema contains global users with `platform_role`, tenants with an owner,
configuration `revision` and authorization version, `tenant_memberships` keyed by
`(tenant_id, user_id)`, one-time `tenant_invitations`, and `tenant_audit_events`.

The legacy `users.tenant_id`, `users.role`, `UserRole::System/Admin/User`, `Permission::SystemAdmin`, and `is_admin()` semantics are removed rather than retained as aliases.

A tenant membership is the only source of tenant role truth. JWT claims may select an active tenant but never authorize a role without server-side membership validation.

The `new/` Go service must consume this model if it shares identity or authorization data; otherwise the isolation boundary must be explicit and tested before schema cutover.

## Delivery gates

Each implementation phase is repeatedly reviewed and tested through the repository
contribution workflow. The phases are development gates, not deployable mixed-mode
states.

The final cutover requires schema assertions, authorization matrix tests, invitation concurrency tests, cross-tenant negative tests, cache and async isolation tests, Responses admin/member tests, formatting, workspace checks, Clippy, and a clean working tree.

If the repository contains another service that shares identity or authorization data, it must consume this same model in the same cutover. If it is isolated, that boundary must be explicit and tested.
