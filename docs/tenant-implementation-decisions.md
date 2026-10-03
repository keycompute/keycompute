# Binding tenant implementation decisions

The latest user request is authoritative where earlier sketches differ.

## Canonical names and endpoints

Platform roles: `root/operator/none`; tenant roles: `admin/member`.
Membership states: `active/suspended/removed`; revision column: `authz_version`.
Membership role column: `tenant_role`; provenance: `invited_by`, `joined_at`,
`removed_at`. Invitation role/provenance columns: `tenant_role`, `invited_by`,
`accepted_by`. Audit record fields: `platform_role`, `tenant_role`, `metadata`.
API-key and invitation revocation keep their distinct revoked semantics.
Tenant state remains `active/inactive`, with `authz_version`.
User identity is global; a console identity may have no selected tenant.
Tenant APIs use `/api/v1/tenants/{tenant_id}/**`. The path selector must equal
the current verified membership tenant; it never grants membership.
Platform APIs use `/api/v1/platform/**`. Personal `/api/v1/me/**` never widens
its ownership range for an administrator. Inference retains `/v1/**`,
`/pt/v1/**` and `/nt/v1/**`; these are execution modes, not privilege levels.

## Administrative invariants

The owner must be an active tenant admin. A transaction cannot commit a tenant
with no active admin. Membership mutations serialize on the tenant parent;
deferred validation permits atomic creation and ownership transfer. Suspension
of a global user must not invalidate these invariants in any owned tenant.
Removed membership rows retain historical resource and accounting references.
Tenant administration never changes platform roles or original resource owners.
Secrets can be replaced or revoked, not retrieved as original plaintext.
Operator authority is an explicit operational allowlist, not inherited tenant
administration. Root-sensitive access requires an explicit action and audit.

## Async acceptance and financial ownership

Revocation or authorization-version changes deny new work. Already accepted
work retains its original tenant and billing owner only to terminate safely,
release reservations and finish mandatory accounting. It cannot start a new
inference or continuation on the old authority. Management actor and resource
owner are separate; administrator actions cannot transfer charges to the actor.

## Go service boundary observed in this checkout

`new/` is excluded by `.gitignore:41` and has its own Git repository. Its module
is `github.com/QuantumNous/new-api`, not a Cargo workspace member. Its user IDs
are integers, roles are Common/Admin/Root, and authorization uses its own
session/Casbin model. Its compose file names a separate `new-api` database.
Tracked Rust sources and deployment configuration have no integration with it;
the running KeyCompute compose services do not contain this Go application.

These are independent identity namespaces. Go roles, cookies, sessions, JWTs,
and database tables are not Rust memberships or platform roles. Any optional
Go client integration must use a separately issued tenant-bound inference key
through the public Rust inference API. It receives no management privileges.
No changes to this independent ignored repository are included in Rust commits.

## Delivery

All work, review, commits and pushes take place on `main`, never a new branch.
Dependency updates needed to keep a stage buildable are part of that stage.
Deployment happens only after the complete security and cutover gates pass.

## Offline cutover

Application startup initializes the final `001_init.sql` only. The latest greenfield release
contract permits final-schema initialization or an isolated database rebuild
only after verified backup and restore checks. A reviewed identity/owner mapping
is required if any previous data is retained; no automatic legacy-admin upgrade
is permitted. JWT trust is rotated only during the final verified cutover.
A failed release gate restores the complete snapshot and previous deployment.
No reset or conversion is permitted before snapshot and restore verification.

`system` maps to platform root. `user` maps to platform none and membership
member in its recorded tenant. Every legacy admin needs an explicit mapping;
neither operator nor tenant-admin authority is automatically assigned.

PostgreSQL constraint-trigger reference (version 16 used by this deployment):
https://www.postgresql.org/docs/16/sql-createtrigger.html
Tenant context and object authorization reference:
https://cheatsheetseries.owasp.org/cheatsheets/Multi_Tenant_Security_Cheat_Sheet.html
These references guide the checks; they are not a certification claim.

## Current acceptance numbering (latest request)

The user’s latest phase 0–8 contract supersedes previous phase numbering.
Historical entries in the progress log are evidence, not retroactive acceptance.

| Current phase | Acceptance scope | Earlier record correspondence |
|---|---|---|
| 0 | Resource, route, cache/job and identity-mapping contracts | previous 0, amended here |
| 1 | Final global-identity/member/invitation/audit schema | previous 1 plus naming/provenance closure |
| 2 | Credential-aware, current-authority authorizer | previous 2 plus final-contract alignment |
| 3 | Explicit platform/tenant/personal routes, handlers and DAO scopes | previous 3 and 4 |
| 4 | Member/invitation acceptance and audit closure | previous 5 |
| 5 | Full tenant-resource management, cache and accepted-work isolation | previous 3 and 6 remaining |
| 6 | Root and operator capability allowlists | previous role matrix plus actual routes |
| 7 | Client types, navigation, tenant switch and resource UI | previous UI acceptance |
| 8 | Security/engineering acceptance, independent Go boundary and verified cutover | previous 7, 8 and 9 |

The only final membership removal name is `removed`, not a compatibility alias
for `revoked`. Previously committed user/member/usage/key/wallet read scopes
remain reusable behavior, but must compile and pass the final schema contract.
No phase passes from a document checkbox alone; actual runtime/SQL tests and
CI are tracked separately. No destructive production action precedes release
and snapshot-restoration gates.
