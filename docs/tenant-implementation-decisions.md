# Binding tenant implementation decisions

The latest user request is authoritative where earlier sketches differ.

## Canonical names and endpoints

Platform roles: `root/operator/none`; tenant roles: `admin/member`.
Membership states: `active/suspended/revoked`; revision column: `version`.
Tenant state remains `active/inactive`, with `authz_version`.
User identity is global; a console identity may have no selected tenant.
Tenant APIs use `/api/v1/tenant/**`, selected through a validated membership.
Platform APIs use `/api/v1/platform/**`. Personal `/api/v1/me/**` never widens
its ownership range for an administrator. Inference retains `/v1/**`,
`/pt/v1/**` and `/nt/v1/**`; these are execution modes, not privilege levels.

## Administrative invariants

The owner must be an active tenant admin. A transaction cannot commit a tenant
with no active admin. Membership mutations serialize on the tenant parent;
deferred validation permits atomic creation and ownership transfer. Suspension
of a global user must not invalidate these invariants in any owned tenant.
Revoked membership rows retain historical resource and accounting references.
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

Application startup initializes the final `001_init.sql` only. The explicitly
requested maintenance-window conversion is a separate offline operation, not
an application migration or compatibility branch. It preserves IDs and ledger
history, requires a reviewed mapping for every legacy admin and tenant owner,
verifies snapshot restoration and final constraints, rotates JWT trust, and
restores the whole snapshot and prior deployment when a release gate fails.
No reset or conversion is permitted before snapshot and restore verification.

`system` maps to platform root. `user` maps to platform none and membership
member in its recorded tenant. Every legacy admin needs an explicit mapping;
neither operator nor tenant-admin authority is automatically assigned.

PostgreSQL constraint-trigger reference (version 16 used by this deployment):
https://www.postgresql.org/docs/16/sql-createtrigger.html
Tenant context and object authorization reference:
https://cheatsheetseries.owasp.org/cheatsheets/Multi_Tenant_Security_Cheat_Sheet.html
These references guide the checks; they are not a certification claim.
