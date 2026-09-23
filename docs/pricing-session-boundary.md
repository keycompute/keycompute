# Original console sessions for pricing control

Pricing action grants and resource scopes are unchanged. Tenant administrators
manage their selected tenant; root may explicitly manage platform pricing or
another tenant without joining that target. Operator and inference-Key credentials
do not acquire pricing-management permissions.

## Reproduced boundary failures

Four isolated HTTP regressions first failed against the prior implementation.
Tenant, root cross-tenant and global creation returned HTTP 200 after the signed
JWT expired during audit insertion. A root request waiting on the identity fence
also succeeded after its selected membership was suspended and regranted.
Regranting current access must not renew an already-issued versioned JWT.

The tests prove the request reached the intended boundary: a nontransactional
sequence marks delayed pricing audits, and the concurrency test observes the exact
writer connection waiting on the administration fence. These are independent
local disposable databases, not shared global delays or production modifications.

## Shared provenance, separate authorization

`ConsoleSessionProof` retains the verified user, platform role, token version,
expiry and optional selected tenant/membership role and versions. It contains no
Bearer value and grants no action. Global sessions require no tenant tuple;
selected sessions require a complete, valid tuple. Existing extractors and scoped
DAOs remain the action/object authorization boundary.

One final bounded writer lookup compares current active states, roles and the
original version tuple. Deadline checks surround that lookup. Scoped mutation
DAOs already hold their deterministic authorization locks through the outer
transaction; this helper adds no global exclusive lock. Key control now uses this
same implementation while preserving its selected-tenant requirement, original
owner, shared personal locking and credential-versus-inert-intent cache behavior.

All five tenant and five platform pricing mutation handlers verify before their
outer commit. Rejection rolls back the pricing change, pricing audit and tenant
or platform audit together. Platform list and tenant list/detail reads revalidate
the original proof after their queries. The cost-estimation API is not changed.

A root selected in tenant A still needs no membership in target B. If A's selected
session proof changes while the request waits, that old session is rejected; a
fresh valid selected session or an independent global root session remains usable.
This is session invalidation, not a new target-membership restriction on root.

## Commit uncertainty and cache handling

After pre-commit authorization succeeds, every attempted commit invalidates local
pricing snapshots and fences display snapshots, including an unconfirmed commit
acknowledgement that may already have persisted. A known authorization rollback
does not invalidate pricing caches. The signed deadline is also checked after
commit acknowledgement and cache invalidation.

A late acknowledgement may therefore produce an expiry or unconfirmed-result
error even though the authorized transaction committed. This is explicitly not a
rollback promise. Clients must refresh records before a manual retry; no automatic
mutation replay is introduced. Key creation still withholds one-time secrets after
its response deadline. Existing billing identity and accepted settlement are not
changed by pricing or this session-proof refactor.

## Verification

Six isolated HTTP groups cover fourteen target/operation combinations across the
tenant, root-explicit-tenant and global modes; selected-membership regrant; and four
read forms delayed on an observed pricing-relation lock. Rejected writes leave the
entire pricing table and both audit-table fingerprints unchanged. Fresh valid
sessions subsequently succeed, including root cross-tenant access without target
membership. The global-price deletion prohibition is retained, not bypassed to
increase test coverage.

Two unit tests verify malformed/expired credential tuples and that provenance
alone grants no platform action. Existing key expiry/regrant, concurrent Key reads,
pricing scope/CAS/default/deadlock/cache and real client contracts are retained.
Full independent workspace and strict native/WASM checks are recorded in the
implementation progress. No database schema, production credentials, actual
payment, notification, service or deployment is changed. Other control-resource
transaction boundaries, resource UI, native resources and release gates remain.
