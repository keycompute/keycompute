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

## Remaining phase gates

Phase 3: scoped resource DAOs. Phase 4: platform/tenant route separation. Phase 5: invitations, member administration and audit API/UI closure. Phase 6: cache and job authorization propagation. Phase 7: independent Go-service boundary verification. Phase 8: end-to-end security and client acceptance. Phase 9: verified offline cutover and release.
