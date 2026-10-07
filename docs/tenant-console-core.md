# Core tenant console

The tenant console is a selected-workspace experience, not a second platform
administration console. Every signed-in identity can discover or enter the
workspace overview. Management controls appear only when the verified session
contains the corresponding tenant capability; platform roles remain independent.

## Routes and capabilities

- Global header switcher: personal/platform context plus active memberships.
- `/tenant`: role-aware workspace overview and zero-membership guidance.
- `/tenant/settings`: workspace name, description and default limits.
- `/tenant/members`: member role/status changes, removal and ownership transfer.
- `/tenant/invitations`: create, inspect and revoke invitations.
- `/tenant/audit`: paginated audit events and canonical request IDs.
- `/invite`: explicit one-time invitation acceptance, including return after login.

Registration leaves a user at the global identity with zero memberships. The
workspace page presents that state explicitly; an invitation acceptance adds the
new membership without removing memberships in other tenants. The acceptance
page can immediately select the new membership and enter its dashboard; later
switching remains available from the global header.

Tenant administration consumes the server's tenant capability vector and selected
membership. Navigation and route guards distinguish settings, members,
invitations, providers, keys, billing and pricing instead of using one broad UI
flag. The persisted roles remain deliberately small: Owner is the immutable
ownership identity, while Admin and Member are inviteable membership roles.
Platform role labels and platform capabilities never synthesize a tenant grant.
Root/operator global identities cannot enter tenant-scoped management or resource
pages without an actual qualifying selected membership. They can still open the
role-aware workspace overview and their separately authorized platform consoles.
Backend checks remain authoritative.

The workspace reads the already verified profile after opaque-token restoration;
it keeps a global identity when the user has no memberships and never invents a
tenant from missing local selection metadata. Tenant selection verifies the
original user and returned target through AuthStore's
existing compare-and-install operation. Editable configuration uses its own
monotonic `revision`, so ordinary name, description and limit updates do not
invalidate selected sessions. Ownership, tenant status and self-membership
changes still advance authorization versions and require a fresh selection or
sign-in. A configuration conflict keeps the local draft visible and exposes an
explicit refresh action that loads the latest revision before a corrected save.
Successful name changes synchronize the header and membership labels in the
current session. Other members' changes refresh the current page.

Platform tenant creation searches only active global users for the initial
owner, with the lifecycle filter enforced by the server query and checked again
by the client. On narrow mobile headers, secondary repository/theme shortcuts
yield space to the workspace selector while Home and account actions remain
available.

## Component and request isolation

A page-local keyed fragment owns form drafts, confirmation dialogs, pagination,
errors and one-time links. Keying only AppShell or a single static component was
insufficient: tenant A's dirty form could survive in B. The keyed-fragment unit
regression retains its dirty-A sentinel assertion. A workspace
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
old-result suppression. Route tests cover the workspace, settings, member,
invitation, audit and acceptance routes. Existing real
SDK/Axum/PostgreSQL tests continue to cover server contracts and tenant authority.

Code-level UI and integration regressions cover exact-revision member commands,
literal search, invitation lifecycle, uncertain-write single dispatch, workspace
switch isolation, route guards, fragment scrubbing, login resumption and direct
workspace entry after acceptance. They also verify that configuration saves do not
invalidate the selected authorization session, ownership transfer does, and an
expired restored token cannot consume an invitation before verified login. CI runs
these tests and verifies the production Web image can be built; backend isolation
continues to be proven by the real SDK/Axum/PostgreSQL suites.

Provider, Key, pricing, finance, financial-control, distribution, node/task and
managed Response/Conversation consoles are documented and tested separately.
Open gates are the remaining endpoint/object-scope review, the exhaustive security
matrix and final production backup/rollback/cutover evidence. The existence of the
core workspace and resource pages does not certify the whole tenant subsystem.
No production database, signing material, payments or deployment is changed here.
