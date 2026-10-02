# Tenant release and recovery gate

This document is the explicit hand-off boundary for the remaining operational
checks. The repository contract gate verifies that the procedure exists and is
documented; it does not claim that a deployment, browser session, backup or
rollback has actually run.

## Required preflight

Run the read-only source contract and the normal Rust/integration tests first:

```sh
python3 scripts/ci/check_tenant_contract.py
cargo test --workspace
```

The synthetic browser checks in CI use deterministic HTTP fixtures. A real
backend browser gate must run against a started service with PostgreSQL and
Redis, and must exercise both a tenant administrator and a foreign tenant
identity. Record the service revision, database snapshot identifier and the
cross-tenant denial assertions with the release evidence; do not treat fixture
responses as proof of a deployed backend.

With a task-owned service and short-lived test tokens, run the browser smoke
explicitly:

```sh
KC_BACKEND_BROWSER_APP_URL=http://127.0.0.1:8080 \
KC_BACKEND_BROWSER_TENANT_ID=TENANT_A_UUID \
KC_BACKEND_BROWSER_FOREIGN_TENANT_ID=TENANT_B_UUID \
KC_BACKEND_BROWSER_ADMIN_TOKEN=ADMIN_TEST_TOKEN \
KC_BACKEND_BROWSER_FOREIGN_TOKEN=FOREIGN_TEST_TOKEN \
node scripts/tests/tenant_backend_browser.mjs
```

The script requires all five variables, opens the actual app in Chromium,
verifies each identity can read only its own tenant, and verifies both
cross-tenant reads receive 401/403/404. It exits without making a request when
any variable is absent.

## Isolated restore rehearsal

Only run this command against a task-owned, labelled test container:

```sh
KC_TENANT_TEST_ACK_ISOLATED=1 python3 scripts/tests/tenant_restore_rehearsal.py \
  --container kc-tenant-test-db-EXPLICIT_TEST_INSTANCE
```

The rehearsal creates temporary source and destination databases, verifies the
complete synthetic snapshot and the tenant authorization invariants, and cleans
up both databases. It must not receive a production DSN, a live database name,
or a retained customer archive. A failed rehearsal blocks release until the
container and evidence are inspected.

## Release, rollback and recovery

The production runbook must name the immutable application image, schema
checksum, backup/snapshot identifier and the operator approving the change.
Take a fresh snapshot before cutover. If health checks or the real backend
browser gate fail, stop traffic, preserve audit evidence, restore the previous
known-good application image and database snapshot in the approved maintenance
window, then rerun the tenant isolation tests before reopening traffic. A
rollback is an operational deployment action; it is not performed by the
read-only repository gate.

The synthetic rehearsal is **NOT a production backup**, a retained-data
migration, a deployed service rollback, or permission to rotate live JWT trust.
It is evidence that the greenfield schema and tenant invariants can be restored
in an isolated test database. Production release remains blocked until the real
backend browser, snapshot/restore, and rollback evidence are attached to the
release record.
