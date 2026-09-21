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

## Remaining phase gates

Phase 2: finish centralized credential/action/scope enforcement and full live authorization-version tests. Phase 3: scoped resource DAOs. Phase 4: platform/tenant route separation. Phase 5: invitations, member administration and audit API/UI closure. Phase 6: cache and job authorization propagation. Phase 7: independent Go-service boundary verification. Phase 8: end-to-end security and client acceptance. Phase 9: verified offline cutover and release.
