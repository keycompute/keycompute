# KeyCompute repository rules

## Database schema changes

- The project currently supports greenfield deployments only. There is no legacy production data to migrate or preserve.
- Keep the complete database schema in `crates/keycompute-db/migrations/001_init.sql`. Add or change tables, columns, constraints, indexes, and seed data directly in that file.
- Keep every statement safe to replay at startup: use `IF NOT EXISTS` for tables and indexes, and `ON CONFLICT` for seed inserts.
- Do not add `002_*.sql` (or any other incremental or compatibility migration), and do not add `ALTER TABLE` compatibility paths during this phase of the project.
- Keep SeaORM entities, API contracts, tests, and the schema assertions in `crates/keycompute-db/src/migrations.rs` aligned with `001_init.sql` whenever the schema changes.
- Revisit this rule only when the project explicitly begins supporting upgrades of databases that contain retained production data.
