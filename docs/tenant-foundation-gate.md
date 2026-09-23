# Tenant foundation and independent identity gates

These are executable regression gates, not a claim that every tenant resource,
frontend page or deployment step has been delivered. They supplement the real
Rust authorization/DAO/HTTP tests; they cannot infer object ownership from an
arbitrary dynamically generated SQL query.

## Read-only source gate

Run `python3 scripts/ci/check_tenant_contract.py` from the checkout. It exits
nonzero on missing/malformed contract sources or any detected drift. It reads
tracked and untracked nonignored Rust files and the complete greenfield schema;
it does not access a database, credentials or the network.

The gate checks the final identity tables and role/status domains, required
ownership fields, composite membership key, unique pending-invitation index,
absence of raw invitation tokens and platform-role grants, and exact coverage
of every schema table by `docs/tenant-schema-inventory.tsv`. Only `001_init.sql`
is permitted. No compatibility migration is introduced.

A lexical scanner ignores comments, raw/escaped string lookalikes and character
literals when checking retired authorization symbols. Actual SQL literals using
explicit `users.role` or `users.tenant_id` are rejected. This is not a Rust/SQL
semantic analyzer: renamed aliases, dynamically assembled queries and arbitrary
resource-ID predicates still require code review and executable security tests.
The JSON report lists these limitations rather than certifying complete isolation.

CI invokes the gate and its negative-fixture tests. Schema-only, inventory-only and repository-exclusion
changes are included in workflow path triggers; those changes must not silently
avoid authorization validation.

## Independent Go identity boundary

The local ignored `new/` checkout was inspected at commit
`043ff99a51ecad8229389ddd04f45f4b25a23ac6`. It is its own Git repository/module,
`github.com/QuantumNous/new-api`, with integer user IDs, independent sessions,
`new-api` issuer, `new-api-dashboard` audience and `sid/uv/sv/token_use` claims.
Its refresh cookies and purpose-derived signing keys are not Rust credentials.
Its compose configuration uses its own database and signing-secret variables.
No Go code is copied into or changed by this Rust delivery.

The Rust tests reject these foreign claim shapes even when signed deliberately
with the local test key and when issuer/subject labels are made to collide. Go
role numbers/names do not become platform or tenant roles. Actual HTTP tests
verify that foreign cookies, user/role headers and tenant selectors cannot
authenticate console routes or elevate a separately issued Rust inference key.
Such a key retains its fixed Rust tenant/user and inference-only capability.
These tests validate the Rust boundary; they do not claim to have run a deployed
Go service or validated every possible reverse-proxy configuration.

## Synthetic full-snapshot restore rehearsal

Run only against the labelled task-owned test PostgreSQL container:

```sh
KC_TENANT_TEST_ACK_ISOLATED=1 python3 scripts/tests/tenant_restore_rehearsal.py \
  --container kc-tenant-test-db-EXPLICIT_TEST_INSTANCE
```

The runner accepts no DSN, database name or external archive. It checks the
container namespace and existing test-task label, creates new source/destination
databases, exercises tenant invariants, and adds a hashed pending invitation.
It dumps the entire synthetic source, restores into an empty destination in one
transaction, and compares every public table's row fingerprints, constraints and
triggers. It then tests active-owner/last-root protection, revoked-key retention,
append-only audit behavior and unchanged startup schema replay.

PostgreSQL may re-express a CHECK array cast on its first dump/restore. CHECK
expressions are reparsed by PostgreSQL on empty temporary tables before comparing
their definitions. No predicate is stripped, and no persistent constraint is
disabled, dropped or weakened to pass comparison.

The local archive is private and temporary. Cleanup is attempted for both owned
databases on failure as well as success. Unit tests cover rejected production
containers, missing acknowledgement, schema/restore failure, fingerprint drift
and cleanup failure. Real execution remains an explicit test operation, not an
automatic production cutover.

A passing synthetic rehearsal is NOT a production backup, a retained-data
migration, a deployed service rollback, or permission to rotate live JWT trust.
The remaining full UI, native account-pool resource management, endpoint/security
matrix and deployment gates must still pass before the final maintenance window.
