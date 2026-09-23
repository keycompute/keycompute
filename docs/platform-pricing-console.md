# Explicit platform pricing console

The root pricing page at `/admin/pricing` now uses canonical
`/api/v1/platform/pricing` requests with an explicit typed `PricingTarget`.
The selected console membership is not a pricing selector. The page initially
shows an explicitly labelled platform target; operators and tenant-only admins
do not receive this platform business capability. Backend authorization remains
unchanged and authoritative.

## Wire contract

Platform and tenant targets serialize `scope_type=platform` or
`scope_type=tenant&tenant_id=<real UUID>`. Creation flattens that same target into
its JSON body. It no longer omits the server-required `scope_type` field.
The SDK never represents global ownership with a nil UUID or synthesizes a
missing response version. List rows must match the requested scope, real resource
identity and positive version. Inconsistent targets, missing fields and malformed
mutation results fail rather than being filtered or counted as success.

Reads are fresh and bounded. Whole-list collection retains paging but returns an
explicit error beyond 100 pages instead of returning an incomplete collection.
Updates use the displayed positive version; delete, default and batch-default
retain the server's current-row transactional semantics, not an invented CAS.
Their requests always carry the explicit target and verify returned resource IDs.
The SDK refuses deletion of platform-owned rows, as the server already does.

Money remains an exact string. Editors expand scientific notation from PostgreSQL
without a float conversion; input fits nonnegative DECIMAL(20,10). The separate
cost-estimation response API is unchanged. Creating a tenant row requires an
explicit real tenant; invalid input never means platform or all tenants.

## UI lifecycle

Verified user/selected-membership versions and platform capability bind the page
identity. Page-local keyed fragments reset the active target and form state on
identity changes, and target changes remount their resource lists and editors.
An edit is keyed by its resource/revision. Same-workspace token refresh is allowed;
old identity results cannot update a new page. Writes are single dispatch,
including uncertain failures. A closed editor does not prove that a previously
sent command was cancelled server-side; reload records before manually retrying.

The target selector is separate from the current active target label. Changing
its draft does not apply a new query until Apply target succeeds. Exact target
labels remain visible while an invalid selector is rejected. Root need not join
the target tenant; the real backend test exercises root selected in A managing B.

## Verification

Wire tests cover flat creation scopes, fresh paging, exact values, mandatory
versions, wrong identities, invalid selectors and no automatic mutation replay.
A real SDK/Axum/PostgreSQL regression first submits the old missing-scope payload
and observes HTTP 422 with no row created, then verifies explicit global and
cross-tenant CRUD/default, current versions, audited writes and operator,
tenant-admin and inference-key denial. All fixtures are isolated test data.

The checked-in Chromium runner exercises compiled WASM with synthetic same-origin
HTTP. It checks platform/tenant target separation, exact editor values, explicit
writes, failure handling, delayed old-target completions and forbidden UI scopes.
The existing tenant, operations, node and tenant-pricing browser runners remain.
UI fixtures are not represented as production or backend security tests.

This delivery changes no database schema, backend grant, production credential,
payment, service or deployment. Native account-pool resources, other resource
pages and remaining transaction/endpoint hardening remain separate gates.
