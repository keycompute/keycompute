# Tenant subsystem implementation status

## Phase 1 — global identity and membership foundation

The final identity baseline replaces `users.tenant_id` and `users.role` with global users, typed platform roles, tenant ownership, retained memberships, hash-only invitations and immutable audit snapshots. The same change updates dependent authentication, tenant-first wallets, pricing scopes, node ownership, session DTOs and client callers so main remains buildable without compatibility projections or dual writes.

This is a development checkpoint, not authorization to deploy the partially delivered subsystem. Full tenant administration, scoped resource APIs, job authorization, deployment isolation and cutover remain subject to subsequent phase gates.

### Review and corrections

- Checked current code rather than relying on reports from interrupted sessions; repaired frontend/backend session and explicit-wallet contracts.
- Reproduced stale invitations reactivating a suspended member in a real database. Deactivation now revokes invitations received by that identity, as well as invitations issued by it. Only a fresh invitation may re-establish membership; old API keys remain revoked.
- Restored identity-only JWT signature, issuer, algorithm, expiry, malformed-claim and missing-storage tests. Restored login/password-replacement, login/password-reset lock-order and stale-token-refresh integration regressions instead of silently dropping those protections.
- Updated pricing-cache assertions to require independent tenant-resolved entries; node ownership uses the node's resource tenant, never a global user's former tenant column.
- Rechecked owner/last-admin/root invariants, concurrent lock ordering, invitation one-time use, independent wallets, immutable audit and schema replay in disposable PostgreSQL databases.

### Verification

- Full workspace tests excluding platform-specific desktop/mobile targets: **2,386 passed, 0 failed, 30 default ignored** (including documentation examples and opt-in scenarios).
- Strict workspace Clippy, all targets and features, excluding desktop/mobile: passed with `-D warnings`.
- Web and client API WASM Clippy: passed with `-D warnings`.
- Real PostgreSQL schema-invariant runner: all ten check groups passed.
- Formatting and Git whitespace checks: passed.
- The local recovery-script execution test was blocked by the tool safety check and was not rerun through an alternative route. Its new tenant-specific assertions were source-reviewed; no local execution pass is claimed.
- Dependency `proc-macro-error2 2.0.1` emits a Cargo future-incompatibility notice; the strict source lint run succeeds.

No production database, credential, service or image has been changed. No new branch or worktree was created. A one-time cutover and rollback rehearsal are still required before deployment.

## Phase 2 — credential/action/scope authorization core

The authentication core now applies the same credential restrictions to both
`AuthContext` and HTTP extractors. Inference keys cannot acquire console
permissions from an expanded capability list; node/system credentials cannot
inherit user-console permissions. The central authorizer rejects nil subject,
tenant and resource-owner identifiers before any role-based allow decision.
Missing authentication storage returns a redacted 503 rather than an incorrect
401; malformed or missing credentials remain unauthorized.

### Review and verification

- Reproduced both credential-check inconsistency and malformed-scope acceptance
  with failing regressions before changing the production implementation.
- Added four pure boundary tests and five real-PostgreSQL/HTTP tests covering all
  platform roles, A/admin versus B/member, global users without membership,
  explicit tenant switching/clearing, personal ownership, fixed-tenant keys,
  authorization-version changes, deactivation and permanent membership-key
  revocation. Probe routes validate the authorization core only; they do not
  claim that the later tenant CRUD route phase has been implemented.
- Expanded auth/server/security regression: **640 passed, 0 failed, 0 ignored**.
- Final full workspace tests excluding desktop/mobile: **2,395 passed, 0 failed,
  30 default ignored**. These include the expanded tests, not additional tests
  to be added to that total.
- Strict workspace Clippy (all targets/features, excluding desktop/mobile),
  formatting and Git whitespace checks passed. Final review found no further
  actionable issue in this phase's changed code.
- An older API-flow assertion was updated from missing-storage 401 to 503, with
  an additional real-route assertion retaining 401 for malformed credentials.

This closes the core phase, not the subsystem release gate. No schema,
production service, credential, deployment or Go-service changes were made.
Resource DAO enforcement, full route-level adoption, mutation audit coverage
and asynchronous authorization remain the following stages.

## Phase 3 — payment query subgate (partial delivery)

The payment read/control-plane boundary is now implemented independently of the
remaining resource DAO work. Personal order detail and synchronization lookups
apply `tenant_id + user_id` in SQL, including for tenant administrators. Personal
lists, counts and statistics share the same scope predicate; tenant-admin and
platform query families are explicit, and raw platform payment management
requires a checked root platform scope rather than a tenant billing capability.
Unknown or foreign personal order IDs return the same 404 before provider work.

Review reproduced a real-router disclosure of another member's payment body and
URL before the fix. The new real-PostgreSQL tests cover same-tenant foreign
owners, cross-tenant orders, one global user's separate tenant orders, merchant
references, pagination/counts and member/operator rejection. Two pure DAO tests
verify the predicate and capability boundaries. Final repeated review found no
additional actionable issue in this retained payment-query change set.

Verification: full workspace tests excluding desktop/mobile **2,399 passed,
0 failed, 30 default ignored**; strict workspace Clippy with all targets/features
and `-D warnings`, formatting, whitespace checks and reviewed-source hash checks
passed. The four new tests are included in the full-workspace total.

This does **not** close phase 3. Legacy/runtime payment settlement queries and
other resource DAO families still require follow-up. Broader user/provider and
Responses prototypes were excluded after integration review found unresolved
ownership/transaction/lock-coordination issues. They are not part of this
checkpoint. No schema, callback/accounting, production service, credentials,
branch or deployment changes are included. Phases 4–9 remain pending.

## Phase 3 — user/member query subgate (partial delivery)

Global user administration now carries a checked `PlatformScope` into its
list/detail/count queries, including explicit member targets for platform
billing and tenant-list member counts. Queries also check the acting root's
current database status instead of relying only on a supplied scope value.

Tenant member reads use `TenantMemberRecord` and a mandatory `TenantScope`.
Detail, list and count share one tenant/actor predicate, including current
active admin membership, active global actor and active tenant. The projection
contains membership role/status/version and basic profile information, not
platform roles or global token versions. Optional status/search/user filters
only narrow that scope. Pagination has a stable member-ID tie-breaker and
literal search escapes wildcard characters.

The old unscoped user management list/count and tenant-member methods were
removed. Authentication/global identity lookup, startup identity counting and
existing security mutations remain separate; no tenant-facing global-profile
or platform-role mutation API was introduced.

Review corrected test call sites that still expected a global User record and
retained all prior assertions. Three new real-PostgreSQL/HTTP regressions and
two query-boundary unit tests cover A/member versus B/admin, foreign and
fabricated scopes, projection fields, platform root access without tenant
selection, member/operator denial, counts, pagination and literal search.
Final full workspace: **2,404 passed, 0 failed, 30 default ignored**
(excluding desktop/mobile). Strict workspace Clippy, formatting, whitespace
and reviewed-source hash checks passed. The five new tests are included in
that total. Final review found no further actionable issue in this change set.

This is not the phase-three completion gate: key mutations, other resource
DAO families, runtime/settlement queries and route/audit/job adoption remain.
No schema, production identity, service, deployment or Go-service change was
made. In particular, the separate API-key core proposal was not applied.

## Remaining phase gates

Phase 3: scoped resource DAOs. Phase 4: platform/tenant route separation. Phase 5: invitations, member administration and audit API/UI closure. Phase 6: cache and job authorization propagation. Phase 7: independent Go-service boundary verification. Phase 8: end-to-end security and client acceptance. Phase 9: verified offline cutover and release.
