# Current contract and acceptance status

The current acceptance contract is the latest phase **0–8** plan. Older
phase-numbered entries below are historical delivery records. The current
phase-1 field/provenance alignment has passed local acceptance; provider/binding/pricing scopes,
complete platform/resource route separation, remaining resource management,
client UI and final deployment remain unaccepted. Tenant control/member/invitation
APIs have passed the acceptance gate recorded below.

Foundation baseline: `f427ae7`, CI #106 success. The current checkout
was clean when this contract update began. Earlier `/tmp` provider/pricing
drafts are no longer present and are not an implementation dependency.


## Current phase 1 — final contract alignment accepted locally

Memberships now use `tenant_role`, `authz_version`, `active/suspended/removed`,
`invited_by`, `joined_at`, and `removed_at`, with no old-column or enum alias.
Invitation and audit models, SQL, JWT membership revisions, client DTOs, reset
helpers and regression fixtures use the same final names. Removing a member
retains resource and billing ownership and permanently revokes old credentials.
A rejoin requires a newly accepted invitation; the same inviter is allowed,
but changing inviter metadata or passing through suspended cannot restore access.
Current member/invitation/audit reads validate the actor in their data query.

Independent review caught a draft regression that removed global-user activity
from the owner invariant and incorrectly changed a script assertion to accept
owner suspension. Both were restored; suspension remains allowed after explicit
ownership transfer. The PostgreSQL tests also verify last-root protection,
concurrent root demotion, immutable audit retention and safe schema replay.

Final validation for this source set: `cargo test --workspace` **2,426 passed,
0 failed, 30 default ignored**, with desktop/mobile included; all-target/all-feature
workspace Clippy with `-D warnings`, formatting and whitespace checks passed.
The isolated DAO suite and the 11-check schema script passed. No production
database, JWT trust, running service or deployment was changed. CI is tracked
separately from local acceptance. This does not close phases 3–8; prepared
provider/pricing/control/operations/Responses/UI modules are not live code yet.

## Current phases 3/4 — tenant control API accepted

Canonical tenant context/configuration, member list/detail/update/removal,
invitation list/create/revoke/accept, ownership transfer and tenant audit routes
are now mounted under `/api/v1/tenants/{tenant_id}/**`. The path must match
the selected verified membership. Global root authority does not substitute
for tenant membership. Root tenant lifecycle routes and client calls use
`/api/v1/platform/tenants`; the old collection alias retains the same root
authorization, not an old role interpretation.

Writes recheck signed user/member/tenant versions under retained locks and
commit their audit with the change. Foreign member identifiers cannot lock
unrelated user rows. Accepted writes fence display caches; rejected writes
do not evict them. Personal resource scopes remain unchanged. Invitation
acceptance checks the current signed user version after lock waits.

Invitations retain hash-only one-time tokens and verified-email acceptance.
Notification success, failure, missing configuration and duplicate-pending
results are explicit; only initial creation returns a recovery link. Real
loopback SMTP tests verify successful delivery and failures after commit.
Request logs, tracing spans and nginx access/error paths protect invitation
and reset capabilities. A network-disabled nginx test verifies 502 failures
without leaking capability, query or Referer values; it also runs in CI.

Final full workspace: **2,446 passed, 0 failed, 30 default ignored**,
including desktop/mobile. All sixteen new control/invitation integration
tests also pass with eight threads. Strict all-target/all-feature Clippy
(`-D warnings`), formatting, whitespace and reviewed-source hashes passed.
An optional additional invitation-audit fault-injection test was not written
after a tool safety rejection and is not counted as coverage. Repeated review
found no remaining actionable issue in this accepted control API slice.

This does not close all resource routing or the complete subsystem: provider,
binding, pricing, tenant financial/node/distribution/Responses administration,
operator allowlists, full client UI and final release gates remain. No
production data, credentials, running service or deployment was changed.

## Current phases 3/5 — scoped tenant pricing accepted

Pricing reads, counts and mutations now use explicit tenant-admin or root
platform scopes. Tenant HTTP APIs are `/api/v1/tenants/{tenant_id}/pricing/**`;
root APIs are `/api/v1/platform/pricing/**`. Existing pricing URLs retain only
the new root authorization. Tenant payloads cannot set tenant ownership or
platform scope. Current signed user/tenant/membership versions are rechecked
under retained locks, including requests queued during administrator demotion.

Mutations and both audit streams share a transaction. Nested savepoints roll
back failures even when an outer caller catches the error and commits. Group
locking, version checks and bounded batches preserve atomicity and idempotency.
The original unique scope/model/dimension policy and protected platform prices
remain unchanged. Canonical server request IDs and non-secret before/after
prices are recorded; password and key fields stay redacted.

Committed pricing-cache revisions now fence all process-local and Redis caches.
The primary read includes tenant/platform revisions and scheduled validity
boundaries, including the existing cross-dimension platform fallback. Price
reads add no row writes or identity locks; previously accepted request snapshots
remain immutable. This schema addition is in greenfield `001_init.sql` only.

Final native workspace validation: **2,464 passed, 0 failed, 30 default ignored**,
including desktop/mobile. Strict all-target/all-feature Clippy, formatting,
whitespace and exact source hashes passed. The 20-test pricing DAO/cache/HTTP
suite also passed with eight threads; it is a subset of the workspace total.
Repeated review closed cache staleness, scheduled validity and audit metadata
issues before acceptance. No production data, credentials or services changed.

This is pricing acceptance within phases 3/5, not closure of either whole phase.
Account/binding, other tenant-resource administration, full operator routing,
client UI and final release remain open. Prepared provider drafts are not
included in this pricing delivery. CI is checked separately after the push.

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

## Phase 3 — scoped usage reads (partial delivery)

Personal usage, counts, model groups and totals now accept a validated
`TenantScope` and bind tenant plus owner in SQL, including for tenant admins.
Tenant-wide reads require the current active administrator membership and
active user/tenant rows. Platform reporting is aggregate-only, grouped by
currency, and checks the current active root/operator identity in SQL.
The unscoped user/tenant list and statistic methods were removed; existing
settlement identity lookups are not reinterpreted as public read authorization.
All changed personal HTTP paths use writer-authoritative queries. Cache-hit
regressions confirm revoked membership credentials are denied before cached
statistics are returned, while another active membership remains usable.

Two query unit tests and six PostgreSQL/HTTP tests cover resource IDs,
independent memberships, paging, half-open time ranges, role and status
changes, currency groups, forged scopes, and actual cache hits followed by
revocation. Fixed one pre-existing test path that could silently return after
fixture insertion failed, and corrected a strict-Clippy comparison warning.
Repeated review found no further actionable defect in this change set.
Final workspace: **2,412 passed, 0 failed, 30 default ignored**,
excluding desktop/mobile. Strict all-target/all-feature workspace Clippy,
formatting, whitespace and source-hash checks passed. The eight new tests are
included in that total. No production database, credential or service changed.

This completes the Usage read subgate, not phase 3 or the release gate.
API-key management, provider resources, accepted-work lookups and later
route/audit/job stages remain open.

## Phase 3 — scoped API-key management (partial delivery)

Key read APIs now have explicit personal, tenant and platform scopes, with
current actor checks in SQL and a metadata projection that excludes key hashes.
Personal endpoints never expand for administrators. Platform target tenants
are mandatory; platform authority does not derive from a tenant role.
The old unscoped create/list/find-ID/revoke/delete APIs were removed; hash
candidate lookup remains solely for credential validation, which rechecks the
key's tenant, user, hash and current state under locks. Last-used sampling also
binds the validated tenant/user without introducing a global write lock.

Create, revoke and removal acquire compatible tenant/user/membership locks,
recheck live authority, constrain the key write by tenant plus owner plus ID,
and commit a secret-free audit event in the same transaction. Real HTTP
request IDs are retained. Existing self-service revoke-then-remove semantics
and the complete platform member-key response are preserved. Tenant-admin
queries/mutations support other owners only inside that tenant, without
changing key or billing ownership. Platform mutations require a real active
root and a non-empty reason. Test fixture creation uses scoped production APIs.

Two unit tests plus seven PostgreSQL/HTTP regressions cover projection,
foreign IDs, current roles, per-tenant authority, actual inference-key denial,
request-ID auditing, audit-write rollback, and authority changes while a
mutation waits for locks. Existing authentication concurrency and key-parent
lifecycle tests pass. Final workspace: **2,421 passed, 0 failed,
30 default ignored**, excluding desktop/mobile; strict all-target,
all-feature workspace Clippy, formatting, whitespace and source hashes passed.
The nine new tests are included in that total. Repeated review found no further
actionable defect in this key-management slice. No production changes occurred.

This does not complete phase 3. Provider/account/binding/pricing management,
other financial and accepted-work scopes, and route/audit/job adoption remain.
CI status is tracked separately from the successful local verification; the
preceding integration CI failure must not be described as resolved merely by
these local results or by adding failure-report diagnostics.

## Phase 3 — scoped wallet display reads (partial delivery)

Ordinary wallet display APIs now require explicit personal, tenant-admin or
root/platform scopes. Personal requests bind the selected tenant and the actor
as owner regardless of administrator status. Tenant batch queries require the
current active tenant administrator and may inspect retained balances of
suspended or revoked members without reactivating them. Platform queries require
an actual active root plus a non-nil target tenant, including support reads of
inactive tenants. Operators and fabricated or stale management scopes are denied.

All wallet ownership joins bind tenant plus user. Batches are capped before
querying, deduplicated, and fail as a whole if any requested owner is foreign or
missing. A missing wallet becomes an uninitialized zero snapshot only for a
real member visible to the caller's scope. Query authorization and ownership
checks occur in the same primary SELECT. Display reads never create a wallet,
lock a balance, update timestamps, reclaim reservations, or perform financial
operations. The former tuple-only display query APIs were removed and both
the billing service and personal balance handler use the new scoped APIs.

Review preserved the six existing read-only regressions and added two query
unit tests plus three real-PostgreSQL tests for independent wallets, lifecycle
changes, retained funds, missing targets, and scope restrictions. The focused
15-test run and an additional 8-thread rerun of all nine wallet display tests
passed. Final workspace, excluding desktop/mobile: **2,426 passed,
0 failed, 30 default ignored**; strict all-target/all-feature Clippy with
`-D warnings`, formatting and candidate-file source hashes passed. Validation
used committed main plus exactly the six wallet files, excluding unrelated
unaccepted provider/pricing prototypes. The five new tests are included in the
workspace total. Final review found no additional actionable issue in this
wallet display slice. No production data, credentials or services changed.

This closes only the wallet display-read subgate, not financial mutations,
settlement/recovery, or phase 3. Provider/account/binding/pricing drafts remain
unaccepted: independent review still requires closure of handler authority,
mutation-actor/credential binding, and default/group lock-order regressions.
They are not included in this wallet delivery or counted as completed phases.

## Remaining phase gates

Phase 3: scoped resource DAOs. Phase 4: platform/tenant route separation. Phase 5: invitations, member administration and audit API/UI closure. Phase 6: cache and job authorization propagation. Phase 7: independent Go-service boundary verification. Phase 8: end-to-end security and client acceptance. Phase 9: verified offline cutover and release.
