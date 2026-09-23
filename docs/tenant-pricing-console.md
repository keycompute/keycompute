# Tenant pricing console

The actual `/tenant/pricing` page uses the existing tenant pricing handlers.
No backend permission, pricing resolution, schema or settlement behavior changes.
The platform pricing page is a separate control plane and is not reused here.

## Scope and contract

`TenantPricingApi` requires a nonzero tenant UUID. The tenant ID is a selector,
not an authorization grant: the server still verifies the selected membership.
Reads use fresh requests. Responses require an explicit tenant scope, matching
tenant/resource IDs and a positive version; missing versions are never invented.
The client rejects foreign/platform rows instead of filtering them after loading.
Root/operator role labels alone do not mount this tenant administration page.

The page supports list/search/pagination, create, edit, make-default and delete.
Create/update payloads do not contain a tenant or platform scope selector. Edits
carry the displayed version. Delete and make-default use the server's current-row
transactional contract; this delivery does not invent optimistic version support
for those existing endpoints. Batch-default selection is not exposed by this UI.
Only tenant-owned rows are shown, not inherited platform pricing.

## Values and lifecycle

Amounts remain exact decimal strings. Console inputs accept nonnegative plain
DECIMAL(20,10) values without float conversion. Scientific notation returned by
the server is rendered as an exact decimal in the editor. Model, dimension,
currency and start time cannot be changed through an edit.

Optional dates use RFC3339 with a timezone. On creation an absent start uses
server time, and an absent end has no expiration. On editing an absent end keeps
the previous value; it is not an undocumented expiration-clear operation.
Commands are sent once, including authentication or uncertain network failures.
The UI asks the user to refresh records before manually deciding to resubmit.

The existing verified workspace scope and page-local keyed fragments own drafts,
confirmation state and asynchronous work. Submitted query keys own list results.
Changing tenants or selected authorization versions cannot publish an old result.
Same-workspace token refresh does not become a different resource owner.
Shared workspace navigation wraps rather than overflowing into another panel.

## Verification

Wiremock tests assert exact paths, decimal strings, versions, scope rejection,
fresh reads and no automatic mutation replay. Native Web tests exercise editor
precision, immutable identity, dates, translated labels and the canonical route.
An actual SDK/Axum/PostgreSQL test covers audited CRUD/default, version conflicts,
member and inference-key rejection and foreign-tenant resource denial.

The checked-in Chromium runner executes production-compiled WASM with synthetic
same-origin HTTP. It covers the CRUD flow, error handling, literal search, an
in-flight A command during a switch to member B, and foreign/global response
guards. Existing tenant, operations and node browser runners remain in CI.
Browser mocks are UI evidence, not a substitute for the real backend tests.
Platform pricing is documented separately in `platform-pricing-console.md`;
other remaining tenant resource pages are not certified by this delivery. No production deployment is performed.
