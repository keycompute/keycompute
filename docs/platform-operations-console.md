# Read-only platform operations console

`/platform/operations` is a real Web route inside the authenticated App layout,
independent of the root business-page guard. Navigation and individual panels
use only the verified platform capability vector. Platform role names, tenant
capabilities and `platform:node_operations` alone do not grant these read panels.

The page uses the existing `PlatformOperationsApi` and its fresh GET methods:
- `platform:tenant_health`: bounded tenant health list and explicit-ID detail.
- `platform:aggregate_stats`: all-tenant or explicit-tenant usage aggregates.
- `platform:diagnostics`: named numeric process capacity counters.

No backend permission, resource ownership or runtime schema is changed. Server
checks remain the security boundary. A global root/operator session can use the
page without selecting or joining each target tenant. An already selected
session retains its original membership and version requirements.

## Queries and presentation

Health search is literal, limited to 128 UTF-8 bytes, and rejects control
characters. Status is active/inactive; pages are bounded to 20 rows and the
server's maximum offset. Applying a filter resets pagination and detail state.

Usage always sends an explicit start-inclusive/end-exclusive UTC window of at
most 31 days. An empty or invalid explicit tenant cannot become a platform query.
The displayed target belongs to the submitted query, not an unsubmitted form.
Amounts and large token totals stay exact strings; currencies are never combined.

Capacity renders a fixed list of numeric counters. Unknown fields, strings in
numeric slots, connection URLs and raw nested JSON are never displayed. Missing
counters render as an em dash rather than an invented zero. The page explicitly
labels this as one application process, not cluster-wide capacity.

## Identity and request lifecycle

A page-local keyed fragment owns filters and results. Its identity includes the
verified user, UI epoch, selected membership versions and panel capabilities.
Reads compare that identity before and after coordinated authentication refresh.
A normal refresh within the same workspace is not a new identity. Capability
loss or workspace/login replacement discards old results and page state.

Results also carry the submitted query key. Pending or failed reads never render
a previously retained snapshot for a different target. Tabs load on demand and
have no background polling. No payment, wallet, membership or node mutation is
available from this console; it never reads Responses or Conversation bodies.

## Verification and remaining work

Native tests exercise capability separation, global/selected profile validation,
identity-change races, UTC window and literal-search limits, numeric projection
and routing. `scripts/tests/operations_console_browser.mjs` runs actual compiled
WASM against isolated synthetic HTTP, including delayed A-to-B report switching,
partial capabilities, pagination, failures and exact currency values. CI runs it
after the existing tenant console runner against the production image assets.

These are presentation tests, not a replacement for the existing real database
and HTTP operations authorization tests. Tenant business-resource pages, operator
node-control UI, native resource administration and final cutover remain separate.
