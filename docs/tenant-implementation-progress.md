# Current contract and acceptance status

The current acceptance contract is the latest phase **0–8** plan. Older
phase-numbered entries below are historical delivery records. Final global
identity/membership foundations and tenant control/invitation APIs are accepted.
Scoped pricing, providers/bindings, financial/distribution reports, owner-only
key issuance and distribution policy mutations are also accepted. Live scoped
Responses replay now revalidates credentials and authorization versions.
Node/registration control and task cancellation/archival are accepted. Original-authority physical dispatch and node-queue claim checks are also accepted.
Local passthrough/node Responses and Conversation administration is accepted.
Explicit node-session ownership and tenant/currency-scoped node earnings and
withdrawal management, transaction-bound manual wallet/reservation control and
root-only global settings/ratio management are accepted. Native account-pool resource
administration, complete platform/operator routing, full frontend and final release
remain open.

The tenant control SDK is now verified against actual server HTTP responses,
including typed selected roles, explicit owner context, invitation secrets and
member lifecycle versions. Full tenant pages and workspace switching remain open.

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

## Current phases 3/5 — provider and binding administration accepted

Tenant provider/account and passthrough-binding CRUD, options and probes now
use path-bound active-admin membership and scoped DAO queries. Platform routes
use current root authority independently of tenant selection; legacy URLs map
to those same guards. Consumers of foreign/global grants do not acquire the
account or binding's management authority. Tenant payloads cannot publish global
grants or transfer ownership. Root transfers remain explicit and transactional.

Mutations retain current user/member/tenant-version checks under ordered locks,
resource tenant predicates, configuration/revision checks, pending Responses
blockers and atomic secret-free audit. Account binding counts are projected in
the authorized account SELECT for all lists/details, not filled with zero or
retrieved afterward using a naked ID. Probe connection material and current
actor authorization are read together from the primary. Refresh preserves that
configuration snapshot and refuses stale changes; runtime monitoring remains
separate from user management authority.

Repeated review corrected binding counts, safe audit metadata, a stale model
parser error assertion and a concurrent test's ambiguous identity-fence waiter.
The test now identifies its own single physical request connection; no suite
serialization, skipped test or deadlock retry was introduced. Invalid model
payloads still fail with a safe stable error code, never reflected secrets.

Final native default-parallel workspace: **2,479 passed, 0 failed, 30 default
ignored**, including desktop/mobile. Strict all-target/all-feature workspace
Clippy with `-D warnings`, formatting and whitespace passed. Provider/binding
regressions also passed with eight threads (42 tests, included in the workspace
total). No production database, service, credentials or deployment changed.

This closes provider/binding administration, not the entire phase 3/5 or release
contract. Tenant key-pool owner-only issuance, reporting and financial operations,
node/distribution/Responses administration, full operator routing, UI and final
release gates remain. Scratch preparation is not accepted runtime code. CI
is verified separately after this commit.

## Current phases 3/5 — tenant financial reporting accepted

Canonical tenant reporting now exposes usage and billing list/detail/statistics,
payment metadata list/detail, and member balance snapshots. Every route requires
the selected path-bound active administrator membership. Owner filters only
narrow the verified tenant; personal APIs are not widened. Usage list/count and
currency totals share one scoped predicate builder with half-open time windows
and stable pagination. Monetary totals are grouped by currency, never mixed.

Payment console DAO queries recheck current user/membership/tenant or current
root authority in the same primary query. Tenant payment projections do not
select payment capability URLs, callbacks, provider payloads, body/subject,
remarks, raw errors or credentials. Historical provider callbacks and settlement
remain unchanged. Wallet reports preserve the accepted read-only semantics:
no initialization, row lock, reclamation or financial mutation, and no fabricated
zero wallet for a foreign owner. Statistics retain the HeavyRead console budget.

Three new PostgreSQL/HTTP tests and focused query/budget checks cover tenant
list/detail/count boundaries, separate CNY/USD totals, safe payment fields,
member refusal, bounded strict queries and wallet no-write behavior. Existing
scoped usage/payment/wallet and credential tests remain in the full suite.
Final native default-parallel workspace: **2,485 passed, 0 failed, 30 default
ignored**, including desktop/mobile. Strict all-target/all-feature Clippy,
formatting, whitespace and exact reviewed-source hashes passed. No production
state or deployment changed. This is reporting acceptance, not permission to
mint funds or modify immutable billing records, nor complete subsystem release.

## Current phases 3/5 — tenant key-pool backend accepted

Tenant key metadata, creation/rotation requests, cancellation, owner-only claim
and owner decline now have canonical tenant/personal routes. An administrator
receives only an inert issuance intent. The current owner alone can claim the
raw key once; a claim response has private/no-store and no-cache headers.
Intent storage contains no credential hash, ciphertext or raw secret. Internal
authorization snapshots are excluded from API serialization and raw secrets
are excluded from Debug and audit. Metadata updates have owner-bound SQL,
monotonic optimistic revisions and distinct omitted/null expiration semantics.

A claim rechecks the exact signed tenant/member/user versions and the original
requester's current administrator grant while holding deterministic locks.
Demotion, suspension and later regrant do not revive old requests. Rotation
retains the old credential until the successful claim transaction revokes it,
creates the new owner-scoped key, records audit and finalizes the intent.
Audit failure rolls the entire savepoint back even when an outer caller catches
the error and commits. The existing scoped key CRUD and shared-parent lock
order remain unchanged rather than inheriting the draft's weakened predicates.

Greenfield 001_init.sql adds one tenant/owner-scoped intent table, immutable
identity/terminal-state guards and pending indexes. No incremental migration,
production database update, credential rotation or deployment was performed.
Ten new real PostgreSQL/HTTP tests cover one-time and concurrent claims, safe
fields, personal/admin/foreign scopes, actor/credential/version forgery,
metadata CAS, sequential rotation, regrant invalidation, audit rollback,
expiry and database ownership/terminal constraints. The original seven scoped
key tests also pass with eight threads. Two handler tests and a schema test
add exact contract checks. Full native default-parallel workspace: **2,498
passed, 0 failed, 30 default ignored**, including desktop/mobile. All-target
check, all-target/all-feature Clippy with -D warnings, formatting, whitespace
and frozen-source hashes passed. The 17-test key suite is included in that total.

This is backend acceptance, not the whole subsystem or client/UI delivery.
Client integration and supplemental combined rotation/history-deletion and
exact-PID queued-claim test writes were tool-denied and are not claimed as
coverage. Existing core deletion/queued-key tests remain; independent fresh
claim concurrency and atomic rotation tests passed. Node/task/distribution/
Responses administration, remaining platform/operator routing, client UI and
final release gates are still open. CI is verified separately after the push.

## Key-pool cache and CI112 follow-up accepted locally

Actual key claims, metadata updates, revocations and removals now retain an
outer transaction until the established display-cache mutation guard fences
commit/refill. A secret is never returned before that outer commit. The real
cache-hit regression first reproduced a stale zero active-key count after
claim; it now verifies claim, rename and revocation refresh while denied peer
claims leave cached snapshots intact. This preserves the presentation cache's
existing cross-instance maximum five-second TTL; inference authentication does
not use that cache and continues to validate current credentials in the database.

CI112 failed before the lifecycle isolation assertion: start_request timed out
while preparing one of five request fixtures. The test now inserts the same
schema-constrained initial rows in one bounded setup operation. No production
tracing code or its 250 ms timeout was changed. The four unrelated row locks,
500 ms healthy-request barrier, trace-quality checks and independent production
timeout tests remain. The exact scheduling/lock source of the CI preparation
delay was not determined from the failure log. No test was skipped, retried or
made globally serial. The complete 40-test database file passed twice at twelve
threads; the 18-test key suite passed at eight threads.

Final frozen native default-parallel workspace: **2,499 passed, 0 failed,
30 default ignored**, including desktop/mobile. All-target workspace check,
all-target/all-feature strict Clippy, formatting, whitespace and exact source
hashes passed. Counts above are subsets/repeats, not additional workspace tests.
Remote CI is checked after this follow-up push. No frontend, distribution,
production database, credential or deployment changes are included.

## Current phases 3/5 — distribution reporting accepted

This is a read/reporting subgate, not distribution-rule CRUD or overall phase closure.
Canonical tenant read routes expose distribution records/detail/stats and policy
metadata list/detail. Explicit platform paths carry a mandatory target tenant;
canonical personal paths retain beneficiary self, including for administrators.
One SQL builder binds the same actor, tenant, owner and status/level/time/currency
filters before pagination for every record query and per-currency aggregate.
Historical everyone records keep a nullable beneficiary rather than a fake UUID.

Legacy root read adapters use the current selected tenant with explicit platform
authority; they no longer filter by a beneficiary across all tenants. Their CNY
presentation remains explicitly CNY. Large legacy rule collections fail explicitly
and direct callers to bounded canonical pagination instead of silently truncating.
The canonical routes expose individual currencies without relabeling USD as CNY.

Personal earnings, cached overview origin and referral financial sums now require
verified tenant-plus-beneficiary scope and current primary membership/user/tenant
checks. Global self-referral relationships remain global metadata, not tenant
invitations; their financial columns are selected-tenant CNY. A new cache namespace
prevents replay of older all-tenant financial snapshots. Operator and inference
credentials cannot access tenant/platform individual reports.

Real PostgreSQL/HTTP regressions cover multi-tenant roles, actual cache hits and
revocation, fabricated authority, foreign IDs, nullable identities, CNY/USD totals,
identical filters/counts, literal Unicode search and a read-only transaction.
Existing fixed-query-count referral tests retain their large-page assertions.
Final default-parallel native workspace: **2,508 passed, 0 failed, 30 default
ignored**, including desktop/mobile. Strict all-target/all-feature workspace
Clippy, formatting, whitespace and all13 frozen-source hashes passed. The
12-test reporting/referral/display suite also passed with eight threads; it
is a subset of the workspace count, not an additional test total. Legacy
feature-enable checks now use the primary too, and the existing disable test
also verifies the personal earnings endpoint is denied.

No production database, credential, service or deployment changes. Policy writes,
node/tasks, Responses, remaining financial operations, operator routes, full UI and
final release remain unaccepted. Unfinished mutation drafts are not in this commit.

## Current phases 3/5 — distribution policy mutations accepted

Tenant policy creation, optimistic PATCH/DELETE and deterministic default override
now share one transaction-bound DAO with explicit root-targeted platform routes.
Tenant URLs require path-matching active admin membership; root without membership
uses a platform path and explicit target, while operator has no business-write grant.
Every write revalidates current signed identity/tenant/member versions under the
established parent-first locks, then locks the policy group before resource rows.
All row predicates retain tenant and ID; update/delete additionally require an exact
revision. Reasons and server Request IDs are audited with numeric/state before/after
snapshots; arbitrary names/descriptions and credential material are not audit data.

The owned transaction/savepoint explicitly rolls back errors, including injected
audit failure when an outer caller catches the error and commits. Duplicate default
repair retains deterministic created-at/ID ordering, audits every deactivation, and
no-op defaults preserve revisions. Precise 0..1 rates have at most four decimal places;
active beneficiary policies require active target membership. Disabling/deleting an
unavailable beneficiary remains possible without reactivating it. Policy ownership
is immutable in final greenfield SQL. Policy edits never rewrite historic commissions
or credit/debit balances. No incremental migration or production schema change.

The old unscoped production rule CRUD methods are removed. Bootstrap creates initial
rules through explicit current-root policy authority; accepted settlement has a
separately named read-only effective-policy resolver. Legacy mutation URLs now require
root plus an explicit tenant selector and a revision for update/delete. Unsupported
min/max monetary limits are rejected rather than reported as saved. A new typed client
API supports canonical tenant/platform policy CRUD, decimal strings and omitted/null
patch semantics; full frontend page migration remains phase 7, not this acceptance.

Eight new PostgreSQL/HTTP regressions cover bare handlers, foreign IDs, API keys,
forged actors, regrant/token invalidation, exact-PID queued demotion, revision races,
root/admin concurrent defaults, audit rollback and unchanged historic money. Existing
16 rule tests (including ten-way upsert and duplicate repair) were retained. Two new
client wire tests and four unit/schema/handler tests add exact contract checks.
Final default-parallel native workspace: **2,522 passed, 0 failed, 30 default ignored**,
including desktop/mobile. The 37-test DB scope/policy suite and 19-test client suite
are included subsets. All-target check, all-target/all-feature strict Clippy, Web and
client WASM compilation, formatting, whitespace and all 20 frozen-source hashes pass.
CI is verified separately after pushing. No deployment, production data or credentials
changed. Node/task, Responses administration/live authority, remaining financial
operations, operator routes, full UI and release gates remain open.

## Current phase 5 — live scoped Responses replay authorization accepted

Real SSE regressions reproduced established replay continuing after inference-key
revocation and user token-version invalidation. Replay now carries only verified
identity/version metadata, plus a JWT expiration verified against the original
signed credential; no bearer secret is persisted or retained by the stream.
Current active user/member/tenant, exact authorization versions, current key state
and expiry, and JWT expiry are enforced in the existing writer-side state and
event SELECTs. General resource scope checks also include global user status.
Valid owner JWT resource access remains supported; API-key-only behavior was not
silently imposed on the previously supported inference permission model.

Initial prefetched events are not retained across body polling. Each subsequent
bounded batch reauthorizes before delivery and emits at most four native events
as one byte batch, preserving event bytes and cursor semantics. This adds no
per-event database round trip. Data already sent cannot be recalled, but later
batch queries do not inherit an obsolete connection-level grant.

Accepted-work execution and settlement continue under the original immutable
resource/billing owner; stopping replay does not cancel or redirect accounting.
Three new real PostgreSQL/SSE tests cover seven cases: key revocation, user
suspension, token/member/tenant version changes, JWT expiry and key expiry. They
wait for the actual durable upstream-acceptance checkpoint, then verify replay
stops while exactly one original-owner charge completes without another inference.
Existing resource ownership, cancellation, history, stream and recovery tests
remain. This is live-read authorization, not tenant-admin Responses CRUD or
complete asynchronous new-work authorization acceptance.

Final default-parallel native workspace: **2,525 passed, 0 failed, 30 default
ignored**, including desktop/mobile. All-target check, all-target/all-feature
strict Clippy, formatting, whitespace and all five frozen-source hashes passed.
The resource/stream/owner focused rerun passed at eight threads and is a subset,
not an additional workspace count. No production schema, credentials, services
or deployment changed. CI is checked independently after push.

## Current phases 3/5/6 — node and registration control accepted

Canonical tenant, explicit-platform and personal node/task/registration metadata
queries now enforce current primary user/member/tenant and signed version scopes.
Tenant administrators manage node configuration, exclusion, recovery, revocation
and deletion; operator mutations are limited to exclude/recover, without raw
task content or registration credentials. Legacy admin adapters independently
check root and explicit targets/revisions. Task mutation delivery is still open.

Node and registration writes retain exact actor, immutable tenant/owner identity,
optimistic revisions and atomic audit/savepoint rollback. Node deletion refuses
to unlink retained tasks, submissions, streams or financial evidence. Revocation
drains existing sessions for their accepted lease completion only; recovery
never reactivates a consumed/rejected token or a draining session. New approvals
are separate pending requests. Owner secret retrieval commits authorization and
audit before a private/no-store response; history and administrative views have
no plaintext. Approval notifications contain no credential and report delivery
separately. Client types use canonical targets and redact credential Debug.

Independent resumption reproduced transaction-start NOW() regressing a newer
node revision. Final greenfield triggers now enforce monotonically advancing
node/registration revisions for real changes and retain revisions for no-ops.
The new real PostgreSQL regression covers older runtime transactions on both
resources; the claim test verifies stale admin revisions fail, refreshed ones
succeed, and repeated owner reads do not unnecessarily advance the revision.
The command API was refactored to a typed NodeMutation; all strict Clippy issues
were corrected without suppressions or dropping tests.

Final default-parallel native workspace: **2,541 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. The 54-test node/ingress/control
rerun is a subset. All-target check, all-target/all-feature Clippy with
-D warnings, Web/client WASM compilation, formatting, whitespace and all
18 frozen source hashes passed. CI is checked separately after pushing.
No production database, credential, service restart or deployment changed.
Task cancellation/archival, full Responses administration, remaining financial
operations, other platform/operator routes, full frontend and release gates
remain unaccepted.

## Current phases 3/5 — task cancellation and archival accepted

Canonical tenant, explicit-root and personal task commands now share one
transaction-bound scope with current signed identity/member/tenant versions,
resource owner predicates, expected_updated_at and atomic audit. Operator
diagnostics do not acquire task write authority. Task locks select lifecycle
metadata only, not private prompts or result bodies. The original tenant, user,
request, lease and settlement identity are never replaced by an administrator.

Unleased tasks can be cancelled before dispatch. A leased task accepts a durable
cancellation request only when its immutable native requirements require a
cancellation-aware worker. Its leased status and authentic completion channel
remain intact; lease-status exposes the stop signal. Unsupported leased workers
return an explicit conflict rather than a false cancellation success. A durable
terminal stream result wins over cancellation. A cancellation request is not a
claim that the worker has stopped; a racing genuine completion is still settled.

Terminal archival is explicit metadata retention, not physical deletion. Normal
task lists/counts omit archives; archived=true returns that history and scoped
detail remains available. Payloads, result and financial evidence are retained.
Greenfield paired actor/timestamp constraints and immutable markers prevent
identity reassignment, cancellation reversal, archival reversal and controlled
terminal task reactivation. Runtime updates advance exact revisions; claim and
requeue predicates exclude cancelled/archived tasks. Tenant/owner indexes bound
normal and archived pagination. No runtime upgrade or legacy role path was added.

Six new PostgreSQL/router control tests cover own/peer/foreign scopes, operator
rejection, authentic leased completion, terminal-result conflicts, same-revision
races, no-op replay, forged/stale actors, nested audit rollback and database
immutability. Two actual managed Responses-to-node regressions verify that an
unleased cancellation releases the original reservation without charging or
reexecuting, while a racing original-worker result is billed once to the original
user, not the administrator. A typed client wire test checks personal and explicit
platform targets. Eighty-four focused node/ingress/control/resource tests pass
with eight threads; all are subsets/repeats of the complete workspace.

Final native default-parallel workspace: **2,550 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. All-target check, all-target/all-
feature Clippy with -D warnings, client/Web WASM compilation, formatting,
whitespace and 19 frozen source hashes pass. No production database, secret,
service or deployment changes. CI is checked separately after this push.

This accepts task commands and their existing completion/settlement integration,
not every async-new-work authorization path or the complete tenant subsystem.
Full queued-work authorization version propagation, tenant Responses/Conversation
administration, remaining financial mutations, other platform/operator routes,
full frontend and release/rollback gates are still open.

## Current phase 5 — original-authority dispatch and queue claims accepted

New physical inference attempts carry the original tenant, actor/resource owner,
credential kind/key ID, user/member/tenant authorization versions and credential
expiry. The proof contains no bearer secret or role override and is not refreshed
from a user's later membership. All pool-backed server gateways install the primary
DB dispatch authorizer; missing or invalid request proof fails closed. Standalone
library embeddings still choose their explicit authorizer dependency.

After account queueing, quota admission, target snapshots and attempt-trace setup,
the gateway checks current primary authority immediately before each upstream
attempt. Authorization denials and dependency failures cannot trigger retry or
fallback, consume unused account quota, or penalize upstream account health.
Custom authorizer diagnostics are normalized to fixed non-retryable public codes;
the regression first reproduced the old error-vocabulary bypass, then passed after
normalization. These checks do not retroactively reauthorize accepted settlement.

Node task payloads require the same original dispatch identity. Greenfield INSERT
validation and ordinary/native claim predicates reject missing, malformed, stale,
revoked or expired proofs. Model/payload identity is immutable, so a regrant cannot
refresh an old queued task in place. Queue hints do not confer authority. Previously
leased completion keeps its original authenticated node/session/lease and billing
owner; invalid queued tasks remain retained until ordinary cleanup without a new
lease. Runtime proof snapshots are not alternate console-management capabilities.

Real PostgreSQL/provider tests cover unchanged positive controls, revoked/expired
keys, signed user/member/tenant version changes, JWT expiry, both node claim paths,
malformed and cross-owner proofs, fresh-request success after invalidation, and
preserved completion/idempotency for an existing lease. No production proof fallback
or ignored-test changes were added; fixtures construct isolated explicit identities.

The interrupted 31-file implementation was preserved and resumed on main. Final
native default-parallel workspace: **2,559 passed, 0 failed, 30 original default
ignored**, including desktop/mobile. All-target check, all-target/all-feature strict
Clippy, Web/client WASM compilation, formatting and whitespace pass. A focused
node/provider queue suite passed again at eight threads and is an included subset,
not an extra workspace count. All 31 source hashes match the tested snapshot.
CI is checked separately after push. No production data, schema, credentials,
service restart or deployment changed. Responses/Conversation management and the
remaining financial, platform/operator, frontend and release gates remain open.

## Current phases 3/5 — local Responses and Conversation administration accepted

Tenant-admin and explicit-root control routes now manage retained local resources
in passthrough and node_dispatch modes. Account-pool native resources still require
a separate adapter; unsupported modes are rejected rather than mapped to node work.
Lists, counts, details, input items, cancellation, deletion, conversation metadata
and item changes use current primary console authority, selected membership and
exact user/member/tenant versions and JWT expiry. Root support requires a bounded
reason, including a selected-origin version check when its JWT has one. Ordinary
members and inference keys do not acquire console management permissions.

All object reads/writes retain tenant + original user + execution mode + resource
ID. Administrative writes hold the same original-owner advisory lock as personal
operations, check exact revisions, then atomically append secret-free audit events.
The administrator is never substituted for user, request, execution lease or billing
ownership. Read audit failures withhold content; mutation audit failures roll back
content and revisions. Lists/counts use matching predicates and bounded pagination;
control responses are private/no-store. SDK methods preserve explicit target and
encode opaque resource/item IDs without interpreting them as route fragments.

Independent real PostgreSQL/HTTP review added both-family role/ownership matrices,
queued member/root/origin revocation and JWT expiry, audit-failure rollback and
immutable request/execution-family checks. A short-JWT test had accepted issuance
at 999ms of a second; it now uses an early bounded window, preserving production
lock timeouts and exact backend-PID wait observation instead of weakening assertions.

The full gate exposed a separate accepted-work bug: a late nonterminal SSE event
after user suspension reauthorized persistence as a public resource read and marked
an otherwise billed result interrupted. A deterministic held-upstream regression
first failed, then passed after internal append_event switched to the original
execution-lease and owner tuple under the existing advisory/row lock. It still
rejects deleted/cancelled rows and superseded leases. External replay remains live-
authorized; this internal persistence capability never starts a new inference.
New database guards preserve request/user/tenant/mode identity while retaining the
legitimate internal owner_id rotation used for recovery leases.

Final native default-parallel workspace: **2,577 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. The 31-test resource/control repeat
and separate expiry rerun are subsets/repeats. Strict all-target/all-feature Clippy,
all-target check, Web/client WASM, formatting and frozen-source checks pass.
No production database, secrets, services or deployment changed. CI is verified
separately after push. Native resource networking, remaining finance/platform/UI,
explicit session-table scope and final release acceptance remain open.

## Current phases 1/3/5 — explicit node session ownership accepted

`node_sessions` now records NOT NULL tenant and owner identities, tied to the
exact node by a composite foreign key and retained membership constraint. The
credential/tenant/owner/node/issued-at tuple cannot be reassigned. Registration
and capability renewal derive this identity from the locked real node; HTTP
authentication reads the same tuple, not request selectors.

The DAO separates credential candidate discovery from `NodeSessionScope` reads
and revocation. Bare-ID session helpers and unrestricted renewal methods were
removed. Poll, normal/native claim, discovery, heartbeat, native lease checks
and completion use the same ownership tuple. The existing accepted-lease drain,
completion grace, immutable capabilities, cancellation and accounting behavior
remain unchanged. Secret-bearing session entities no longer implement generic
serialization and Debug is redacted.

Independent PostgreSQL/HTTP regression checks wrong-tenant and wrong-owner
creation/read/revoke, raw composite-FK violations, transfer to another valid
node tuple, repeated revocation, credential redaction and actual HTTP identity
mismatch. Existing accepted completion and audit rollback regressions remain.

Final native default-parallel workspace: **2581 passed, 0 failed, 30
original default ignored**, including desktop/mobile. Focused node/ingress/
stream suite: 72 passed at eight threads (included subset). Strict whole-
workspace all-target/all-feature Clippy, all-target check, Web/client WASM,
format/whitespace and all 21 frozen source hashes pass. This closes only the
explicit node-session ownership gap, not all phases 3/5. Native account-pool
resource control, scoped tip/financial mutations, complete platform/operator
routes, frontend and final release gates remain open. No production database,
credentials, service restart or deployment changed. CI is checked after push.

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


## Current phases 3/5 — tenant earnings and withdrawals accepted

The entry checkout was already at `6862ec4` (node-session ownership, CI121
success), with an uncommitted financial slice. Entry sources were archived before
review; the older session draft was not reapplied. This slice replaces unscoped
tip/withdrawal access with explicit personal, tenant-admin and root-target scopes.
Current primary identity, membership/tenant versions and expiry are checked in
queries and again under retained transaction locks. A final expiry check covers
wallet/audit lock waits. Personal financial access never widens for administrators.

Node earnings now carry trusted tenant, original consumer/node owner and actual
ledger currency. Composite foreign keys bind their immutable sources; an existing
credit returns before consulting later policy. Accepted settlement still records
owed earnings for removed members, while those members cannot make new withdrawals.
Lists/counts/statistics share scope and currency predicates. Summaries distinguish
available, reserved and completed amounts instead of presenting pending approvals
as paid. Current withdrawals and balance conversion explicitly support CNY only;
other-currency earnings remain separate and are never implicitly converted.

Withdrawal request IDs are unique per tenant and owner. Matching concurrent
retries return one stored outcome and cannot consume future earnings. Conversion
and its original-owner balance transaction commit with the withdrawal and audit.
Tenant admins approve/reject own-tenant applications without payout secrets.
Explicit-root payout detail requires an audited reason; completion records an
external-payment attestation/reference, it does not initiate a real payment.
List projections never select ciphertext or recipient fingerprints. Payout access,
review and completion audit failures withhold private data or roll back changes,
even when an outer transaction catches the error and commits.

Database guards protect amounts, identity, terminal states and monotonic revisions.
Old global withdrawal URLs are removed; ten explicit tenant/platform paths are
inventoried and private/no-store. Personal client reads bypass display caches;
request UUIDs survive matching retries. Same-tenant token refresh can complete an
accepted UI command, but a different tenant/login or logout cannot receive its
late result. Full tenant-console UI acceptance remains a separate phase.

Review restored the original named earnings-uniqueness constraint, repaired old
opt-in fixtures to use unique namespaces, authentic dispatch snapshots and one
active registration, and removed their broad cross-fixture deletion. No production
constraint, default ignored marker or regression assertion was weakened.

Final default-parallel native workspace: **2,595 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. Eight formerly ignored tip-flow cases
were explicitly run separately at eight threads and all passed; they are not
included in the default-workspace pass count. Eleven financial isolation/audit
cases and the expiry case passed again as included-subset repeats. Strict all-
target/all-feature Clippy, all-target check, Web/client WASM, formatting, whitespace
and exact frozen18-source checks passed. CI is checked separately after pushing.

The old global ratio handler and manual wallet/reservation authorization still
need their separate platform consolidation. A ratio integration attempt was
refused by the tool safety gateway and was not applied or routed around; its
prototype is archived outside main. No production schema/data, credentials,
external payment/SMTP, service restart or deployment was changed by this slice.

## Current phases 3/5 — manual wallet and reservation control

Manual recharge, consumption, freeze and unfreeze now require an explicit root
financial tenant scope, the original target owner and the canonical request audit
identity. The signed credential, user/member/tenant versions and expiry are
revalidated inside the same retained transaction before claiming or replaying
idempotency. There is no loose actor-UUID management entry point. Every result
lookup and completion predicate includes tenant, owner and actor. Matching
retries replay the original outcome without duplicating money or audit events.

All four mutations and their tenant audit are atomic, including savepoint rollback
when an outer caller catches an error and commits. Existing insufficient-unfreeze
expiry repair remains possible only with an atomic denial audit and a final
current-authority check. Expiry after wallet or audit lock waits rolls back the
command. Exact DECIMAL(20,10) input checks prevent rounding from stranding a
pending claim. Database guards bind completed result snapshots to the original
ledger and prevent command identity or completed-result rewriting.

Reservation display is a single primary read-only SQL snapshot: the exact total
and bounded stable keyset page share tenant/owner/current-authority predicates.
No wallet creation, row lock, expiry reclamation or balance write occurs on GET.
Persisted active reservations, including expired entries not yet reclaimed, are
shown consistently. An inaccessible/nonexistent member is not a zero balance.

Tenant administrators may recover only an expired reservation with its current
ownership version; live requests are rejected. Root keeps explicit force recovery
with a reason and the existing warning about accepted late usage. Recovery and
audit commit together, preserve tenant/user/request/settlement identity, and retain
exact actor/version/reason retry behavior. Already accepted inference settlement
continues to use its distinct internal capability, not current console roles.

Seven canonical tenant/platform wallet paths are wired with explicit selectors,
strict request bodies, bounded ingress and private/no-store responses. Legacy
wallet URLs call the same new authority chain. The SDK uses exact decimal strings,
fresh reservation reads, encoded cursors and stable idempotency headers; a tenant
client cannot construct a root money-adjustment request. No full frontend page
redesign is claimed by this backend/client slice.

Independent review retained all 32 existing balance tests and added 12 real
PostgreSQL/HTTP cases plus three SDK wire tests. The new tests include forged
role/audit/credential rejection, no-op replay after revocation, four-kind audit
rollback, exact backend-PID lock-wait demotion and expiry, read-only paging,
expired-only tenant recovery, original-owner late-settlement behavior, direct SQL
result immutability, and denied-unfreeze repair auditing. Strict Clippy redundant
field diagnostics were corrected without suppression.

Final default-parallel native workspace: **2,611 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. Strict all-target/all-feature Clippy, native all-target
check and Web/client WASM compilation all passed; final source hashes match the
reviewed snapshot. Focused32+12 and SDK3 are included subsets, not extra workspace counts.
No production database, credentials, service, payment or deployment was changed.
The broader platform/settings, native resource adapter, UI and release gates
remain open; this does not close the complete tenant subsystem.


## Current phases 3/6 — global settings and earnings policy accepted

The baseline was `b833c42` with a clean main checkout; prior CI123 was confirmed
successful separately. Global configuration belongs to the platform, not the
default tenant. A current root console identity without any selected membership
can use canonical platform settings and earnings-ratio routes. Operator, ordinary
member and tenant-admin roles cannot obtain platform settings authority, including
when a handler is mounted without the outer platform middleware.

Every console query uses a primary, authority-bearing safe projection. One explicit
nonsecret-key registry drives both validation and SQL CASE masking: flagged secret
and unclassified values/descriptions are not materialized. Generic endpoints cannot
create arbitrary settings or update credential fields; dedicated workflows remain
required. Public settings retain their separate fixed public projection. SDK reads
use canonical platform paths and bypass client display caches; keys cannot inject
extra URL components.

General batch updates validate bounded inputs and exact decimal domains inside
the scoped DAO. Current user/selected-membership versions, root role and expiry
are rechecked under retained authority and sorted setting locks; paired recharge
limits are validated in the same transaction. Each changed key and its platform
audit commit atomically, including savepoint rollback after an outer catch/commit.
No-op values keep their version and do not duplicate change events. Errors do not
include raw setting values. Removing the old user-role startup seed does not add
any replacement role fallback.

Node earnings ratio updates require exact decimal strings, expected_updated_at
and an audit reason. Generic single/batch URLs cannot bypass that dedicated CAS.
Two different roots racing the same version have one winner. Before/after ratios
are recorded without credentials or arbitrary settings values. Final credential
checks cover row/audit waits. Existing already-credited earnings are not rewritten.

A real PostgreSQL regression exposed the old settings timestamp trigger overriding
the new monotonic revision. The conflicting legacy function/trigger was removed
from greenfield001; the single remaining identity/version guard handles runtime
writers and no-ops. Repeated startup seed inserts are safe and do not recreate
`default_user_role`. No incremental schema/compatibility migration was added.

Final default-parallel native workspace: **2,627 passed, 0 failed, 30 original
default ignored**, including desktop/mobile. All-target check, strict all-target/
all-feature Clippy, Web/client WASM, formatting, whitespace and 15 exact-source
hashes passed. The 12 new and 6 existing PostgreSQL/HTTP cases, 2 new and 8 existing
SDK cases, and exact-PID expiry repetition are included subsets, not extra totals.
The expiry observer checks actual backend/relation locks rather than truncated
SQL text. No production DB, credentials, payments, service or deployment changed.
Native and operator/UI preparations are not included in this acceptance; broad
platform routing, native resource adaptation and release gates remain separate.


## Current phase 6 — operator operational read allowlist accepted

Baseline `c6e095e` global settings was committed before integrating this slice.
Five canonical GET-only platform operations routes expose tenant health metadata,
platform/named-tenant usage aggregates, and safe process-capacity diagnostics.
Root and operator can use a global console session without default-tenant or
other-target membership. This does not authorize tenant membership routes, raw
monitoring traces, individual orders/wallets, secrets, Responses bodies or any
business write. It does not claim completion of all platform lifecycle endpoints.

Typed PlatformOperationsScope carries the actual platform role, original JWT
version/expiry and any selected membership's role and authorization versions.
Every DAO query rechecks that current state on the primary. A selected-member
revocation invalidates that selected context; an independently valid global
operator session retains its platform capability. User revocation/demotion or
expiry invalidates both. Read queries take no identity write fence or row locks.

Tenant list and total are one authorized SQL snapshot with literal substring
search, stable paging and fixed safe fields. No owner identifiers, emails,
provider credentials or request payloads are materialized by this model. Exact
named-tenant details and aggregate targets reject nil/nonexistent IDs. Aggregate
queries are bounded to 31 days, separate currency totals and retain tokens/amounts
as strings; no arbitrary per-user drilldown is exposed. Existing heavy-read
admission bounds aggregate requests without changing inference limits. SDK reads
use fresh canonical paths and encoded filters; it has no operational write API.

Eight new PostgreSQL/HTTP regressions cover bare handlers, multi-tenant roles,
current/revoked credentials, selected vs global context, exact paging/count,
literal search, inactive target diagnostics, currency aggregation and secret-free
capacity. Two SDK wire tests and one shared classification unit test are included.
Final default-parallel workspace: **2,638 passed, 0 failed, 30 original default
ignored**, including desktop/mobile. All-target native and Web/client WASM checks,
strict all-target/all-feature Clippy, format/whitespace and frozen10-source hashes
passed. Focused tests are subsets, not extra totals. No production state, secrets,
services or deployment changed. Native resource and frontend preparations remain
outside main and unaccepted; root lifecycle authority is the next separate slice.


## Current phases 3/6 — root identity and tenant lifecycle accepted

Baseline `aed6844` operational reads was committed before this implementation.
Platform user list/detail/profile/security/deletion and tenant lifecycle commands
now go through PlatformIdentity with an explicit original signed root global
scope. Global root does not need an arbitrary tenant membership. Operator and
tenant-admin identity cannot enter these routes, including bare-handler tests.
Canonical `/api/v1/platform/users` and detail/update/delete paths are wired; the
platform tenant detail and PATCH routes are complete. Retained URLs use the same
new model, not a second role or optional-tenant authorization branch.

Safe user projections read only identity fields and scalar last-login time, never
password or refresh-token rows. User/tenant list counts and paged rows share a
primary snapshot with literal substring filtering. Full current user/token,
selected membership/tenant and expiry state is checked inside the mutation
transaction before acting. Related owned-tenant parents and target/actor users
are locked in deterministic order. FinancialScope's existing wallet/withdrawal
callers preserve their APIs and accepted-work behavior; all their focused tests
were rerun after this shared locking change.

Mutations retain active-owner/last-root/retained-financial-history constraints and
commit canonical RequestId audits atomically. Failure rolls back both profile and
security changes or tenant/membership deletion, even after an outer catch/commit.
A valid self-demotion or selected-tenant state change intentionally invalidates its
own old JWT: the authorized operation completes while rows are held, then later
requests fail. Final wall-clock expiry after a target-row wait still rolls back.
The existing default-tenant destructive-operation guard is preserved, not treated
as payment/config ownership. No platform secret or resource owner is transferred.

Review found the old create-tenant SDK omitted the required owner_user_id. It now
requires a real UUID in constructor/request, rejects nil before HTTP, and keeps
canonical fresh reads. The existing platform tenant modal defaults to the verified
current global user with editable explicit owner input, uses platform capability
rather than generic admin display, and fences late completion across logins.
This is a targeted platform page repair, not a claim of the full phase-7 tenant UI.

Ten new isolated PG/HTTP cases cover primary scopes, safe projections, real request
IDs, exact-backend queued demotion and expiry, self-invalidating operations,
forged roles and audit rollback including physical deletions. Existing financial,
identity and tenant tests were retained. SDK owner-wire and nil-target tests were
updated without weakening server constraints. Focused regression 74 and SDK 36 are included
subsets of the final default-parallel workspace: **2,649 passed, 0 failed,
30 original default ignored**, including desktop/mobile. Strict all-target/
all-feature Clippy, native all-target and Web/client WASM checks, formatting,
whitespace and all 13 frozen source hashes passed. No production DB, credentials,
services, payments or deployment changed. Native resource adaptation, remaining
platform endpoint audit, complete UI and final security/release gates remain open.


## Current phase 7 — tenant control SDK and live HTTP contract accepted

This slice adds TenantControlApi for current-tenant context/configuration,
member list/detail/update/removal, invitations, owner transfer and audit pages.
The constructor requires a nonzero tenant UUID. IDs and pages are bounded;
search remains one encoded parameter. Read methods and /me bypass the client
showcase cache. Mutations use the single-dispatch HTTP path, including versioned
DELETE bodies; a 503 or uncertain transport outcome is not silently replayed.

SelectedTenant now consumes the server's required typed tenant_role field,
not an optional role field. No old-role alias or default is retained.
TenantContext exposes its existing owner_user_id under the same authorized
membership SQL predicate; no schema or resource ownership changes are involved.
Member DTOs keep membership_status distinct from global user_status.

InvitationToken accepts only the server's exact 64-hex fragment format, has no
serialization implementation and redacts Debug. Invitation creation Debug omits
its one-time recovery link. Acceptance strips token-bearing transport/reflected
errors before returning them to callers, while preserving authorization and
availability error categories. A future UI must keep tokens in temporary memory,
scrub fragments before navigation and fence callbacks by login/workspace.

Five new SDK tests cover role parsing, secret/error redaction, no unsafe retries,
version bodies, fresh reads, bounds and selector injection. Three additional
integration cases run the actual Rust client against a loopback Axum router and
isolated PostgreSQL. They validate real session/member/invitation/audit shapes,
one-time and revoked acceptance, owner transfer, stale versions and cross-tenant
rejection. Restoring a suspended member does not revive its old JWT; a removed
member cannot be restored without a fresh invitation. Valid config/owner changes
invalidate the old selected session without transferring resource ownership.

Default-parallel full workspace tests (including desktop/mobile), all-target
check, all-target/all-feature Clippy with -D warnings, Web/client WASM, formatting
and diff checks passed. Original ignored-test settings were unchanged. All eleven
source/manifest files match the frozen validation bytes. The full UI and native
account-pool administration are not delivered by this SDK subgate. No production
identity, database schema, credentials, payment, service restart or deployment
was changed. See tenant-client-control.md for the client contract and limitations.


## Current phase 7 — platform business-page capability separation accepted

The prior frontend helper treated tenant:manage as a grant to the existing
root business-console pages. The shared navigation, route layout, dashboard
platform queries and existing provider/pricing/payment/distribution/settings
views now use can_manage_platform, which consumes only the server-returned
platform users:manage capability. Role labels, console access and tenant vectors
never imply that grant. The separate operator operational allowlist is not a
root business-page capability. The backend remains the authorization boundary.

Four regression tests cover tenant-admin capability vectors across role labels,
operator/console-only denial, absent capabilities and live Dioxus store changes.
All 163 Web tests passed. The full default-parallel workspace passed 2661 tests,
with 0 failures and the original 30 ignored tests unchanged, including desktop
and mobile. Strict all-target/all-feature Clippy, native all-target and Web/client
WASM checks, formatting, whitespace and thirteen frozen source hashes passed.
The focused/Web cases are included subsets, not extra test totals.

This fixes existing platform-page presentation, not the workspace/member/invitation
UI delivery. Unintegrated new page sources were archived outside the repository;
their route/session wiring did not execute and they are not compiled or accepted.
No new UI route, backend permission, schema, production data or deployment changed.


## Current phases 3/6 — root raw monitoring and console probes accepted

Baseline 1164f9d (platform business-page capability separation) was committed
before this slice. Six raw-monitoring handlers now require current global root
console authority independently of outer middleware: overview, request list/detail,
summary, target health and bounded manual probes. Canonical platform paths and
retained admin URLs invoke the same handlers. Operator health/aggregate permissions
remain on the separately accepted operations API; neither tenant membership nor
an inference key grants access to these raw platform diagnostic resources.

MonitoringRead holds one primary repeatable-read snapshot with shared current
authorization locks in tenant/user/member order. It does not use the identity
administrative write fence and permits concurrent diagnostic readers. Canonical
server request auditing must commit before data is returned. Original signed
expiry is checked before and after audit insertion; failures withhold the result.
Successful responses are private, no-store. Queries join usage/node evidence with
original tenant and user as well as request ID, and audits record explicit query
or resource targets without prompt bodies or credentials.

Review found target-health previously ignored its tenant selector. Provider/node
health now filters resource owner tenant, and unassigned task counts and node
sessions preserve corresponding tenant/owner predicates. This differs explicitly
from consumer-tenant filtering in request/usage statistics. Monetary aggregation
continues to separate currencies. Client monitoring calls use canonical fresh
paths, validate exact request UUIDs and never retry an uncertain probe POST.

Console probing is no longer the unscoped internal runtime path. Candidate
selection reads IDs only and limits a manual batch to fifty unique real accounts;
implicit all-enabled batches above that size must be explicitly narrowed. The
existing four-way concurrency is preserved. Pre-effect request audit and target
account audit precede network activity. Complete original console authority is
checked when connection material is selected and before model discovery and every
capability probe step. A reproduced internal endpoint update did not advance the
application config version, so actual endpoint/provider/encrypted credential/model/
capability equality is also checked. These checks return a boolean and do not
expose secrets. Revocation or material changes stop later probe steps and do not
penalize upstream health. The final result boundary is authorized again; trusted
scheduled runtime probing remains a distinct non-client-selectable internal path.

Twelve new isolated PostgreSQL/HTTP regressions cover bare mounts, root/operator/
tenant/key separation, canonical request IDs, owner-qualified evidence, current
and forged scopes, audit rollback, concurrent read compatibility, exact-backend
role-change and expiry races, tenant health filtering, and real loopback upstream
revocation between capability requests. The expiry test extends only its local
timeouts and asserts actual post-audit authorization expiry, not a timeout false
positive. Two new SDK tests cover fresh paths, input encoding, UUID rejection and
single dispatch. Existing provider/operator/tenant tests and protocol probes remain.

Final independent default-parallel full workspace: 2675 passed, 0 failed, 30
original ignored tests unchanged, including desktop/mobile. All-target check,
strict all-target/all-feature Clippy, Web/client WASM, formatting, whitespace and
all eleven frozen source hashes passed. Twelve PG/HTTP and two SDK cases are
included subsets, not additional totals. No production schema, identities,
credentials, payments, service restart or deployment changed. Native account-pool
resource administration, actual tenant workspace/resource UI, remaining endpoint
review and final security/restore/release gates are still separate unfinished work.
See platform-monitoring-authorization.md for exact scope and lifecycle semantics.


## Current phase 7 — existing profile workspace switch race accepted

The shipped profile tenant selector now checks the initiating loaded global user,
returned user identity and exact selected tenant before installing a session.
The same workspace may refresh its token while selection is in flight; that
refresh no longer silently discards a successful selection or leaves it busy.
A different login or selected workspace cannot be overwritten by an old reply.
Successful selection creates a new UI ownership epoch and invalidates cached
reads while retaining remember-me. Old unauthorized requests do not replay as
the newly selected tenant. Session comparisons include selected-tenant identity.

Review caught a restored-browser no-op bug: an opaque stored token may not yet
have a local selected_tenant_id even though the loaded server profile does.
The profile is the source for the no-op comparison, so global selection remains
possible. No default tenant is invented and no server authorization is inferred.

Eight added regressions cover subject/tenant mismatch, nil identifiers, empty
credentials, refresh/selection races, stale callbacks, global selection and
restored profile context. All 171 Web tests and 2683 default-parallel workspace
tests passed, with 0 failures and the original 30 ignored tests unchanged.
Strict all-target/all-feature Clippy, native all-target and Web/client WASM
checks passed; four source hashes match the final verified bytes.

This is an existing profile-page repair, not the full workspace/member/invitation
page delivery. New route/App/fragment integration was not applied; those page
drafts remain archived outside main. No production DB, service, credentials,
payment, deployment or backend permission changes were made.


## Current phase 8 — foundation, foreign-identity and synthetic restore subgates accepted

The read-only `scripts/ci/check_tenant_contract.py` checks final global-identity
and membership fields/domains, composite membership identity, hashed one-time
invitations, the exact unique pending-email index, and all 47 classified schema
tables. It rejects retired executable authorization symbols and explicit legacy
users ownership SQL, while ignoring comments and quoted lookalikes. It reports
its limits: SQL aliases/dynamic queries, complete DAO ownership, browser acceptance
and production release are not certified by this lexical gate.

CI runs the checker and now triggers on schema-only, inventory-only and repository
exclusion changes. Fourteen checker tests and six restore safety tests were added;
all 23 CI Python tests pass including the three pre-existing report tests.

The independent ignored Go checkout was inspected without modification. Two JWT
tests reject its observed issuer/session/purpose/role shapes even under a local
test signature. An actual Axum/PostgreSQL case verifies foreign cookies and role
headers cannot authenticate Rust control routes or elevate a Rust inference key.
No deployed Go service or external reverse-proxy configuration is claimed tested.

The opt-in synthetic full-snapshot runner creates two labelled-test-only databases,
exercises identity/owner/key/audit invariants, dumps/restores all 47 tables and
compares row, constraint and trigger fingerprints. PostgreSQL reparses CHECK
expressions on empty temporary relations to account for equivalent dump/restore
cast representation; rules are not stripped or weakened. Restored constraints
remain enforced and startup schema replay preserves state. Private archives and
owned fixture databases are cleaned on success/failure. This is not a production
snapshot, legacy-data mapping or final maintenance-window rollback approval.

Final default-parallel workspace: 2686 passed, 0 failed, 30 original ignored
tests unchanged, including desktop/mobile. The 51 auth and six live-authorization
focused tests are included subsets. All-target native, Web/client WASM and strict
all-target/all-feature Clippy pass. Eight frozen source/workflow/contract hashes
match verification. Full native resource management, actual tenant UI, remaining
endpoint/ownership review and final production cutover are still unfinished.


## Current phases 3/5 — Responses control post-audit read expiry

Review reproduced a private read returning HTTP 200 after its original JWT
expired while the control audit INSERT was delayed. This was distinct from the
already-protected mutation paths and the existing queue-time authority checks.
The shared audit helper now checks the original credential expiry after INSERT
as well as before it. A failed check leaves the caller's transaction uncommitted
and prevents returning the private body/count. No extra database query is added.

Two regressions use independent disposable PostgreSQL databases and actual HTTP
requests for tenant-admin and global-root control scopes. A nontransactional
sequence proves each delayed audit was reached exactly once; an early auth
rejection or timeout cannot masquerade as the expected post-audit expiry.
Each scope exercises Response/Conversation detail, count and list paths. The
failed delayed audit is absent, private content is withheld, resource revisions
are unchanged and no additional inference occurs. Earlier completed list audits
are not incorrectly assumed to be part of the later count transaction.

Original tenant, resource owner, billing identity and accepted execution/worker
completion paths are untouched. This closes a control-read timing gap, not the
native account-pool resource adapter or complete tenant resource UI. The new
route/App/fragment integration remains unapplied and archived outside main.
No production schema, credentials, payment, service restart or deployment changed.

Final independent default-parallel workspace: 2688 passed, 0 failed, 30
original ignored tests unchanged, including desktop/mobile. Both new root/tenant
HTTP regressions also passed twice with eight test threads. Strict all-target/
all-feature Clippy, native all-target and Web/client WASM checks passed. Three
frozen source hashes matched at acceptance; the extra later read-only inspection
was denied and not executed or substituted. The existing independent validation
completed successfully. This subgate is accepted; the full subsystem is not.

Operational preparation for these deliveries removed seven verified unused Rust
target/cache directories, freeing approximately 13.85 GiB. Filesystem usage
fell from 96% to 81% immediately after cleanup. Current validation cache, source,
unaccepted drafts, Git history, business containers and database volumes were
retained. No production data, credentials or service deployments were changed.


## Current phase 7 — core tenant workspace and membership console accepted

The actual Web routes /tenant, /tenant/members, /tenant/invitations, /tenant/audit
and /invite are now wired into App/router/navigation. These are compiled pages,
not archived drafts or SDK-only delivery. The server's tenant capability vector
and selected membership gate tenant administration; platform labels/capabilities
do not create tenant authority. The existing platform business guard is retained.

Workspace configuration, member role/status/removal, ownership transfer, invitation
create/list/revoke/accept and audit pagination use the already-verified SDK. Writes
are single-dispatch with displayed revisions where required. Original selected
user/tenant, UI epoch and authorization versions fence asynchronous results. Global
selection never invents a default tenant; restored profiles remain authoritative.

Browser review reproduced dirty A-form state under B. Page-local keyed fragments,
not a key on a single static component, now remount all private form/dialog/link
state. Another real browser failure showed expired restored credentials clearing
an invitation before login and redirecting to Dashboard. Pending invitation binds
only after a verified profile is loaded; existing verified sessions still clear
it on logout or workspace change. Acceptance remains explicit, one-shot and
memory-only, with the fragment removed before Router construction. Standalone
invitation now mounts the existing shared theme styles rather than depending on
another route's head nodes. No new CSS rules or backend permissions were introduced.

Final default-parallel workspace: 2700 passed, 0 failed, 30 original ignored tests
unchanged, including desktop/mobile. All 183 Web tests passed (12 added cases,
included in the workspace total). Native all-target, Web/client WASM, strict
all-target/all-feature Clippy, formatting and the 23 Python checks passed. The
47-table foundation gate passed within its documented non-certification limits.

A real production-mode WASM bundle passed six Chromium scenario groups with
synthetic intercepted HTTP: member commands and revisions, literal filtering,
invitations/audit, dirty workspace switching, global/member/root/operator guards,
login and expired-restoration invitation resumption, single acceptance, config
save and ownership-transfer session invalidation. Browser page errors: zero.
The same checked-in runner now executes against CI's locally built production
image assets. UI fixture tests are not represented as production/backend tests.
Twenty-three source/workflow/test files were verified; no application source drift.

This accepts the core tenant console only. Tenant resource pages, operator UI,
native account-pool resource management, remaining object/endpoint review and
production cutover are still unfinished. No production database, secrets, payment,
SMTP, service restart or deployment changed. See tenant-console-core.md.


## Current phase 7 — read-only platform operations console accepted

The actual /platform/operations Web route is independent from root business
management. It consumes only verified platform health/aggregate/diagnostic
capabilities, never role labels or tenant grants. Global operator/root sessions
do not require membership in each target tenant. Partial grants only mount and
fetch their corresponding section. Node-operation authority alone is not enough.

Health lists and explicit-ID details use the existing fresh SDK with literal
search, bounded pagination and active/inactive filtering. Usage uses an explicit
UTC window of at most 31 days and a separate platform or real tenant selector;
invalid tenant input never means all tenants. Amounts and large token totals
remain exact strings and different currencies remain separate. Capacity only
renders seventeen allowlisted numeric counters; unexpected fields and strings
are not displayed, and missing counters are not replaced with zero.

The page-local keyed fragment and query-tagged resources prevent old identity,
capability or target results from being rendered. Pending/failed reads do not
reuse a previous snapshot. Normal same-workspace refresh remains supported.
No background polling, raw business resource reads or mutation controls exist.

Final independent default-parallel workspace: 2707 passed, 0 failed, 30
original ignored tests unchanged, including desktop/mobile. Web190 is an included
subset with seven new cases. Native all-target, strict all-target/all-feature
Clippy, Web/client WASM, format, whitespace and Python23 pass. Seventeen
frozen source/workflow/browser files match the tested versions.

Production compiled WASM passed three operations browser scenario groups with
synthetic HTTP, including delayed A-to-B aggregates, partial grants, exact values,
pagination, query encoding and error handling. The existing six-group tenant
console browser also passed against the same release bundle; zero page errors.
The operations runner is registered in CI alongside the tenant runner. These UI
fixtures do not replace backend authorization tests.

No backend permissions, schema, production DB, credentials, payments or deployment
changed. Tenant resource UI, operator node-control UI, native account-pool resource
management, remaining endpoint review and final release gates are unfinished.
See platform-operations-console.md for exact scope and limitations.


## Current phase 7 — tenant node, task and registration console accepted

The actual /tenant/nodes route now provides three tenant-admin panels using the
existing NodeControlApi: nodes, tasks and registration metadata. Global roles
do not bypass selected membership; ordinary members do not mount these controls.
No backend permission, schema, worker or settlement behavior changed.

Node configuration/exclusion/recovery/revocation/deletion carry the observed
resource version and a required bounded reason. Deletion evidence conflicts are
reported without removing history. Recovery displays the actual returned status.
Registration approval/rejection/revocation exposes previews only; consumed tokens
remain revocable, and notification status is separate from the approval result.
No owner-secret lookup or claim operation is available in this admin page.

Task cancellation is presented as a request, not proof a worker stopped. An
uncancellable leased task remains a conflict. Only terminal unarchived tasks
can be archived; default lists omit history and an explicit filter retrieves it.
Original request, resource owner and billing identities remain backend-owned.
Metadata details use the observed list snapshot and never fetch task bodies.

Tenant/user/epoch/version and submitted query keys fence all page results.
Commands are single-dispatch, including uncertain failures, and returned resource
identities are checked. A real browser scenario switches from A during a pending
task command to member workspace B: old success is not displayed or replayed.
Queries are bounded and literal; empty owner means only all owners of this tenant.

Final independent default-parallel workspace: 2712 passed, 0 failed, 30
original ignored tests unchanged, including desktop/mobile. Web195 is an included
subset with five added cases. Native all-target, strict all-target/all-feature
Clippy, Web/client WASM, format, whitespace, Python23 and foundation checks pass.
Sixteen source/workflow/browser hashes match final verification.

Production-mode WASM passed four new browser scenario groups, and the existing
six core-tenant plus three operations groups also passed against that bundle.
All HTTP data was synthetic and browser page errors were zero. The node runner
is registered in CI beside the other two. This is UI evidence, not a new live
worker, payment or backend-security test. Existing backend regressions remain.

Remaining: Provider/Key/pricing/finance/Responses resource UI, explicit-platform
node control UI, native account-pool resource management and final endpoint/
release gates. A further read-only node-authority audit was denied and not
executed; post-wait credential/origin proof review remains a separate item.
No production data, credentials, notifications or deployments were changed.
See tenant-node-console.md for precise capabilities and limitations.


## CI134 — signed JWT expiry boundary regression corrected

CI134/run35865168005 failed the existing
replay_connections_enforce_jwt_expiration_and_live_key_expiration integration
test. The fixture read one batch from a new five-second JWT replay and treated
any next data as evidence of post-expiry access, without waiting for signed exp.
A late initial native event can legally arrive while the credential is valid.

A test-only upstream gate now emits a distinct nonterminal delta after the first
replay poll. The fixture verifies it is durably pending for the original tenant,
user and response, with execution still active and the JWT not yet expired.
The old assertion was reproduced deterministically with five seconds remaining
on the JWT; the entire failing test took3.12 seconds. This is a fixture timing
error, not evidence that an already-expired JWT was accepted by production code.

The corrected test leaves that event unread, waits for the actual validated exp,
asserts the original execution remains live, and only then checks that replay
stops. JWT lifetime remains five seconds. Live-key expiry and the original stop,
owner, single-charge and no-extra-inference assertions remain. No production
stream, authorization, schema or accounting code changed.

The focused case, all33 resource tests with eight threads, and all three replay
connection tests passed. Final independent default-parallel workspace: 2712
passed, 0 failed, 30 original ignored unchanged, including desktop/mobile.
Native all-target, strict all-target/all-feature Clippy, Web/client WASM, format,
whitespace and Python23 checks pass. The one changed integration source hash
matched final verification. This repair does not represent another UI feature.

CI135 for the node UI independently passed, including all three production-image
browser runners. CI134's old failed attempt remains historical; a separate commit
carries this deterministic correction and receives its own CI run. Full native
resource management, remaining resource pages and final release remain unfinished.


## Current phase 7 — tenant pricing console accepted

The actual /tenant/pricing page now uses a dedicated tenant SDK and the current
selected membership/capability boundary. It does not reuse the platform pricing
URL or include tenant/platform selectors in write payloads. Explicit foreign,
platform, nil-ID or missing-version responses are rejected, not silently filtered.

The page provides list/search/pagination, create, versioned edit, make-default
and delete with exact decimal strings and RFC3339 windows. Immutable model,
dimension/currency/start fields remain unchanged on edit. An omitted expiration
retains the previous value. Default/delete retain existing server current-row
transaction semantics; this UI does not invent version-CAS for those endpoints.
All writes are single-dispatch. Page-local keyed scope and query ownership prevent
late A results or forms from being adopted after a switch to member workspace B.

Four SDK wire tests, six Web tests and one real SDK/Axum/PostgreSQL test were
added. The real contract test covers audited CRUD/default, precise amounts,
version conflicts and member/key/foreign denial. The independent full default-
parallel workspace, all-target native check, strict all-target/all-feature Clippy,
Web/client WASM, formatting and 23 Python tests all passed. Existing ignored tests
were unchanged. No backend authorization, schema or settlement code changed.

Production-compiled WASM passed three new pricing browser scenario groups with
zero page errors. All existing six tenant, three operator and four node browser
groups also passed on the same bundle. The new runner is added to CI's existing
production-image browser step. UI HTTP fixtures are synthetic; backend contract
coverage is the separately executed actual PostgreSQL/Axum test.

Nineteen non-document source/test/workflow files were frozen before the final
pipeline. An additional aggregate inspection was denied and not executed; the
independently launched complete validation finished with exit code zero. No claim
is made that the denied additional inspection ran. The runtime sources were not
edited during final validation.

Platform pricing still has separate legacy client/presentation issues identified
during review (explicit scope serialization and nil-UUID global labels). They are
not fixed by this tenant-only delivery. Provider/Key/finance/Responses pages,
native account-pool management, remaining object review and final deployment
gates also remain separate. No production DB, credentials, payment, SMTP or
service deployment changed. See tenant-pricing-console.md for exact scope.


## Current phase 7 — explicit platform pricing contracts and console accepted

The root /admin/pricing page and SDK now use canonical /api/v1/platform/pricing
requests with an explicit Platform or real Tenant target. Creation serializes the
required scope_type; list and mutation targets never inherit the selected tenant.
Global ownership uses explicit scope_type rather than a nil tenant. Missing scope
or response versions, foreign rows and incorrect mutation resource IDs fail closed.
Deletion and batch-default responses retain their actual resource identities.

The platform capability and verified user/selected-membership revisions own the
page identity. Keyed fragments reset target-local lists and forms; changing target
while an old command is pending does not publish or replay its completion. Edits
use observed versions and exact decimal strings, including scientific notation
from PostgreSQL. Platform-owned rows are visibly shared, not labelled by a nil ID;
their existing deletion prohibition remains. Writes are single-dispatch. Existing
backend grants, transaction semantics and cost estimation are not changed here.

Five new wire tests and three Web regressions cover exact scope/versions, fresh
reads, invalid/malformed results, no automatic replay and verified UI identity.
A real SDK/Axum/PostgreSQL test first reproduces the previous missing-scope HTTP422
without creating a row, then verifies explicit global/tenant CRUD/default, stale
versions, audit records and operator/tenant-admin/inference-key denial. A root
selected in A can explicitly administer B without membership in B.

Final independent default-parallel workspace: 2732 passed, 0 failed, 30 original
ignored tests unchanged, including desktop/mobile. Web204 and the targeted SDK/
HTTP checks are included subsets. All-target native, strict all-target/all-feature
Clippy, Web/client WASM, formatting, whitespace and Python23 pass. All twelve
frozen source/workflow/test hashes match the verified versions.

Production-mode compiled WASM passed three new platform-pricing browser groups;
all four existing core-tenant, operator, node and tenant-pricing runners passed
on the same bundle with no page errors. The new runner is registered in CI.
Synthetic browser HTTP is UI evidence, not a substitute for backend tests.

No production database, credentials, payment, SMTP, service or deployment changed.
Key SDK preparation remains separate and unintegrated; its UI preparation was
not executed. Other tenant resource pages, native account-pool management and
remaining endpoint/transaction/release gates are unfinished. See
platform-pricing-console.md for the precise contract and remaining boundaries.
