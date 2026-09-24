# Tenant Responses and Conversation console

The /tenant/responses page manages locally stored passthrough and node_dispatch
resources through the existing tenant-only control API. Account-pool native
upstream management is not enabled by this page. Root/operator platform labels
without an active administrator membership cannot mount the tenant page.

## Scope and commands

List, owner filtering and paging retain the active verified workspace. Resource
identity includes tenant, original owner, execution mode, object kind and opaque
ID. List rows, inspectors and mutation editors use that full tuple rather than a
resource ID alone; operation/item/revision distinguish editor state. Private
metadata and content never enter component keys. The server remains authoritative.

The page supports response detail, input-item paging, cancellation and deletion;
conversation detail, metadata edit, item paging/append/removal and deletion.
Mutation commands carry the displayed revision and original owner. The client's
existing JSON/resource checks remain in force. Monetary identities and already
accepted settlement are not changed. A stale view produces a conflict, not a
replacement version. Refresh the list before another explicit attempt.

Reads use current workspace fences; writes are sent once, including uncertain
network and authentication failures. Late results cannot update another tenant's
page. Closing a page does not prove that an already sent server mutation stopped.
Private JSON is displayed as escaped text, not HTML; the page does not persist it
to browser storage. Inspectors and editors fit narrow viewports and scroll content.

## Validation boundaries

Native regressions cover full-identity collisions, mode/owner selection, metadata
and item limits, operation revisions and route registration. The actual compiled
WASM runner verifies private-text escaping, same-ID owners/modes, list reorder,
item cursors, versioned writes, uncertain single dispatch, workspace switches,
invalid/foreign responses and narrow-view presentation using synthetic HTTP.
Real SDK/Axum/PostgreSQL control tests remain part of the complete workspace suite.
Browser mocks are not a substitute for backend security tests.

No new backend grant, database table, billing behavior, credential, external
upstream call or production deployment is introduced. Remaining Key/Provider/
finance pages and account-pool native resources are separate deliverables.
