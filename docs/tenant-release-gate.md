# Tenant release and recovery gate

This document is the explicit hand-off boundary for the remaining operational
checks. The repository contract gate verifies that the procedure exists and is
documented; it does not claim that a deployment, backup or
rollback has actually run.

## Required preflight

Run the read-only source contract and the normal Rust/integration tests first:

```sh
python3 scripts/ci/check_tenant_contract.py
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
cargo test --workspace --exclude integration-tests
cargo test --package integration-tests
```

The code-level end-to-end suite starts the application stack through Rust test
harnesses and exercises real SDK, Axum and PostgreSQL boundaries. It must cover a
tenant administrator and a foreign tenant identity, including bidirectional
cross-tenant reads returning the expected denial. Record the source revision,
database fixture identity and assertion results with the release evidence. Unit or
wire fixtures alone are not proof of the end-to-end boundary.

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
Take a fresh snapshot before cutover. If health checks or the backend code-level
end-to-end gate fail, stop traffic, preserve audit evidence, restore the previous
known-good application image and database snapshot in the approved maintenance
window, then rerun the tenant isolation tests before reopening traffic. A
rollback is an operational deployment action; it is not performed by the
read-only repository gate.

The synthetic rehearsal is **NOT a production backup**, a retained-data
migration, a deployed service rollback, or permission to rotate live JWT trust.
It is evidence that the greenfield schema and tenant invariants can be restored
in an isolated test database. Production release remains blocked until the backend
code-level end-to-end, snapshot/restore, and rollback evidence are attached to the
release record.
