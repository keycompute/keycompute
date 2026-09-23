# Core tenant console

This slice wires the existing tenant-control SDK into actual Web routes. It does
not change backend authorization, the database schema or resource ownership.

## Routes and capabilities

- `/tenant`: selected workspace, global-session selection and tenant configuration.
- `/tenant/members`: member role/status changes, removal and ownership transfer.
- `/tenant/invitations`: create, inspect and revoke invitations.
- `/tenant/audit`: paginated audit events and canonical request IDs.
- `/invite`: explicit one-time invitation acceptance, including return after login.

Tenant administration consumes the server's tenant capability vector and selected
membership. Platform role labels and platform capabilities never synthesize a
tenant grant. Root/operator global identities cannot enter the tenant member
pages without an actual qualifying selected membership. Existing platform business
pages keep their independent platform guard. Backend checks remain authoritative.

The workspace reads the already verified profile after opaque-token restoration;
it never invents a default tenant from missing local selection metadata. Tenant
selection verifies the original user and returned target through AuthStore's
existing compare-and-install operation. Configuration changes and ownership or
self-membership changes that invalidate the current tenant JWT require signing
in again. Other members' changes refresh the current page.

## Component and request isolation

A page-local keyed fragment owns form drafts, confirmation dialogs, pagination,
errors and one-time links. Keying only AppShell or a single static component was
insufficient in a real-browser regression: tenant A's dirty form survived in B.
The keyed-fragment regression retains its dirty-A sentinel assertion. A workspace
or selected authorization-version change also cancels old component-owned work.
Results carry the exact user, tenant, UI epoch and selected authorization versions.
Pending or mismatched list results are not rendered. Same-workspace token refresh
is not treated as a new identity. Member search remains a literal encoded value.

Reads can use the existing coordinated authentication refresh. All tenant control
commands are single dispatch, including after 401, 503 or uncertain transport
outcomes. The UI tells the user to refresh records before deciding to resubmit.
Member mutations send the displayed positive revision. Removed members need a
new invitation. The owner cannot be demoted/removed through member controls;
ownership transfer explicitly selects another active administrator.

## Invitation secrets

The root App captures `/invite#token=...` before constructing the router, replaces
the browser history URL and retains only a non-serializable token in memory. A
history-redaction failure withholds the router instead of exposing a fallback
capability URL. The accepting user must explicitly confirm. Login may resume the
invitation, but it never accepts automatically. The pending token is consumed
before one request, binds only after a verified profile is loaded, survives the
first login and same-session refresh, and is
cleared after logout or a different workspace/login epoch. An ambiguous outcome
requires checking membership or reopening the original link; no background replay.
Creation displays the recovery link only inside the current keyed page. Duplicate
pending invitations do not recover an old token. Notification outcomes are shown
separately from invitation creation. No invitation token is persisted in browser
storage, serialized into audit metadata, or used as a frontend route parameter.

## Verification and limits

The native Web tests cover the actual shared scope/command helpers, capability
vectors, restored profiles, nil/version rejection, same-workspace refresh and
old-result suppression. Route tests include the five real routes. Existing real
SDK/Axum/PostgreSQL tests continue to cover server contracts and tenant authority.

`scripts/tests/tenant_console_browser.mjs` runs compiled WASM in fresh Chromium
contexts against synthetic intercepted HTTP. It covers member role/status/removal
with exact revisions, literal search, invitation creation/duplicate/revocation,
uncertain-write single dispatch, audit navigation, dirty-A-to-B isolation, member
and global guards, fragment scrubbing, login resumption and one-shot acceptance.
All API calls remain inside the synthetic origin; it never uses a real user's
browser session or credentials. CI exports assets from the locally built production
Web image and runs the same checks; no image is pushed by that validation step.

The browser script also exercises configuration save and ownership transfer,
followed by clearing the invalidated selected session. An expired restored token
must not consume the invitation before the first verified login. Browser evidence
is UI behavior with controlled HTTP, not a substitute for backend isolation tests.

Remaining work is explicit: tenant Provider/Key/pricing/finance/node/task/resource
pages, the operator console, native account-pool Responses/Conversation management,
remaining endpoint/object-scope review, and final production cutover. Core member
and invitation pages do not certify that the whole tenant subsystem is complete.
No production database, signing material, payments or deployment is changed here.
