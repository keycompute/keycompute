# Tenant node, task and registration console

The real `/tenant/nodes` route belongs to the selected tenant administrator
layout. It also performs its own verified membership/capability check. Global
root/operator roles never synthesize tenant membership. Existing personal node
pages and explicit platform APIs retain their separate boundaries.

The page uses the already-shipped `NodeControlApi`; no server permission, DAO,
schema, worker lifecycle or financial identity is changed in this UI delivery.

## Resource panels

Nodes: tenant-local list and metadata, display-name/failure-threshold changes,
exclude, recover, revoke admission, and deletion where the server permits it.
Recovery displays the server's returned state rather than assuming online.
Deletion conflicts retain the resource and report the error; history is not
removed to make a UI action succeed.

Tasks: list, metadata, cancellation requests and terminal archival. The default
query excludes archived tasks; the explicit archive filter reads history.
Cancellation is never presented as proof that a worker stopped. A leased task
without a cancellation contract can be refused. Archival preserves evidence and
original task/request/user identities. Unknown or terminal task states never
receive a speculative cancel action.

Registrations: list and metadata, pending approval/rejection, and revocation.
Consumed registrations remain revocable. The admin receives only preview and
lifecycle metadata, never the registration secret or an owner-claim operation.
Notification status is displayed separately from the approval result.

## Scope and command lifecycle

Each page and tab owns state through a dynamic keyed fragment. List resources
carry the user, tenant, UI epoch, authorization versions and submitted query.
Changing scope discards old dialogs/results. Returned list and mutation identities
are checked against the selected tenant and original owner before presentation.
Metadata details are the observed list snapshot, not an unscoped detail lookup.

Filtering is bounded and literal; an empty owner means all owners inside the
already explicit tenant, never all tenants. Node/registration queries never send
a task-only archive selector. Every mutation requires a bounded reason and the
observed `updated_at`; node configuration validates name and threshold ranges.

Commands are single dispatch through the existing tenant helper. No automatic
401/503/network replay is introduced. A conflict or ambiguous response remains an
error with a refresh-before-retry hint. Existing backend authorization, revision,
audit and accepted-work completion protections remain authoritative.

## Validation scope

Native tests cover command/filter construction, revision and input constraints,
state-specific actions, real route integration and bilingual dynamic labels.
The browser runner exercises compiled WASM using intercepted synthetic metadata
and command responses. It covers refusal without fake success, exact revisions,
notification separation, archive filtering, context switches, in-flight result
suppression, and non-admin/global-role denial. These UI tests are not worker,
backend-authorization or production-settlement tests. Existing full-workspace
PostgreSQL/HTTP and settlement regressions are run independently.
