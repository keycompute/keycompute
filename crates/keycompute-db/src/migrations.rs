//! Versioned PostgreSQL migration runner.

use crate::DbError;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};

const V0001: &str = include_str!("../migrations/001_init.sql");
const MIGRATION_LOCK_KEY: i64 = 0x4b_43_4d_49_47_52; // "KCMIGR"

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "baseline",
    sql: V0001,
}];

#[derive(Debug, FromQueryResult)]
struct AppliedMigration {
    version: i64,
    checksum: String,
}

fn checksum(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

/// Apply all migrations under a process-independent PostgreSQL advisory lock.
pub async fn run_migrations(db: &DatabaseConnection) -> Result<(), DbError> {
    loop {
        // A transaction-scoped advisory lock works correctly with a connection
        // pool (a session lock acquired through `DatabaseConnection::execute`
        // could be unlocked on a different pooled session). One loop applies
        // at most one migration, preserving the per-migration transaction rule.
        let tx = db.begin().await.map_err(schema_error)?;
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [MIGRATION_LOCK_KEY.into()],
        ))
        .await
        .map_err(schema_error)?;

        match run_migration_step(&tx).await {
            Ok(MigrationStep::Applied(migration)) => {
                tx.commit().await.map_err(schema_error)?;
                tracing::info!(
                    version = migration.version,
                    name = migration.name,
                    "database migration applied"
                );
            }
            Ok(MigrationStep::Complete) => {
                tx.commit().await.map_err(schema_error)?;
                return Ok(());
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
    }
}

enum MigrationStep {
    Applied(&'static Migration),
    Complete,
}

async fn run_migration_step(db: &impl ConnectionTrait) -> Result<MigrationStep, DbError> {
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
         version BIGINT PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, \
         applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
    )
    .await
    .map_err(schema_error)?;

    let applied = AppliedMigration::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT version, checksum FROM schema_migrations ORDER BY version".to_string(),
    ))
    .all(db)
    .await
    .map_err(schema_error)?;

    if applied.is_empty() && database_has_application_tables(db).await? {
        return Err(DbError::SchemaInitializationError(
            "database is non-empty but has no migration history; only fresh deployments are supported"
                .to_string(),
        ));
    }

    for (index, row) in applied.iter().enumerate() {
        let expected_version = index as i64 + 1;
        if row.version != expected_version {
            return Err(DbError::SchemaInitializationError(format!(
                "non-contiguous migration history: expected V{expected_version:04}, found V{:04}",
                row.version
            )));
        }
    }

    for row in &applied {
        let known = MIGRATIONS
            .iter()
            .find(|migration| migration.version == row.version)
            .ok_or_else(|| {
                DbError::SchemaInitializationError(format!(
                    "unknown applied migration version {}",
                    row.version
                ))
            })?;
        let expected = checksum(known.sql);
        if row.checksum != expected {
            return Err(DbError::SchemaInitializationError(format!(
                "migration V{:04} checksum mismatch: database={}, binary={expected}",
                row.version, row.checksum
            )));
        }
    }

    if let Some(migration) = MIGRATIONS
        .iter()
        .find(|migration| !applied.iter().any(|row| row.version == migration.version))
    {
        db.execute_unprepared(migration.sql)
            .await
            .map_err(schema_error)?;
        record_applied_migration(db, migration).await?;
        return Ok(MigrationStep::Applied(migration));
    }
    Ok(MigrationStep::Complete)
}

async fn record_applied_migration(
    db: &impl ConnectionTrait,
    migration: &Migration,
) -> Result<(), DbError> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO schema_migrations(version, name, checksum) VALUES ($1, $2, $3)",
        [
            migration.version.into(),
            migration.name.into(),
            checksum(migration.sql).into(),
        ],
    ))
    .await
    .map_err(schema_error)?;
    Ok(())
}

async fn database_has_application_tables(db: &impl ConnectionTrait) -> Result<bool, DbError> {
    let row = db.query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT 1 AS present FROM information_schema.tables WHERE table_schema = current_schema() AND table_name <> 'schema_migrations' LIMIT 1".to_string(),
    )).await.map_err(schema_error)?;
    Ok(row.is_some())
}

fn schema_error(error: sea_orm::DbErr) -> DbError {
    DbError::SchemaInitializationError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_strictly_ordered_and_checksums_are_stable() {
        assert_eq!(MIGRATIONS.len(), 1);
        assert_eq!(MIGRATIONS[0].version, 1);
        assert_eq!(MIGRATIONS[0].name, "baseline");
        assert!(
            MIGRATIONS
                .windows(2)
                .all(|pair| pair[0].version < pair[1].version)
        );
        assert!(
            MIGRATIONS
                .iter()
                .all(|migration| checksum(migration.sql).len() == 64)
        );
    }

    #[test]
    fn migration_directory_contains_only_the_initial_schema() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut sql_files = std::fs::read_dir(directory)
            .expect("migration directory should exist")
            .map(|entry| {
                entry
                    .expect("migration entry should be readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".sql"))
            .collect::<Vec<_>>();
        sql_files.sort();

        assert_eq!(sql_files, ["001_init.sql"]);
    }

    #[test]
    fn initial_schema_contains_the_complete_fresh_deployment_schema() {
        for expected in [
            "CREATE TABLE IF NOT EXISTS gateway_requests",
            "responses_idempotency_claim_count BIGINT NOT NULL DEFAULT 0",
            "CONSTRAINT ck_tenants_responses_idempotency_claim_count",
            "CREATE TABLE IF NOT EXISTS gateway_request_attempts",
            "last_probe_at TIMESTAMPTZ",
            "last_probe_latency_ms BIGINT",
            "last_probe_status VARCHAR(32)",
            "last_probe_error_code VARCHAR(128)",
            "api_capabilities TEXT[] NOT NULL",
            "CONSTRAINT ck_accounts_api_capabilities",
            "CONSTRAINT ck_accounts_probe_status",
            "CONSTRAINT uk_gateway_request_attempt_no",
            "CREATE UNIQUE INDEX IF NOT EXISTS uk_gateway_request_final_attempt",
            "CREATE INDEX IF NOT EXISTS idx_gateway_requests_pending_billing_finished",
            "CREATE TABLE IF NOT EXISTS responses_idempotency_claims",
            "PRIMARY KEY (tenant_id, binding_id)",
            "CONSTRAINT uk_responses_idempotency_claims_billing UNIQUE",
            "execution_state VARCHAR(32) NOT NULL DEFAULT 'in_progress'",
            "execution_token UUID NOT NULL DEFAULT gen_random_uuid()",
            "lease_expires_at TIMESTAMPTZ NOT NULL",
            "upstream_dispatched_at TIMESTAMPTZ",
            "response_status SMALLINT",
            "response_headers JSONB",
            "response_body TEXT",
            "response_body_bytes BIGINT",
            "response_expires_at TIMESTAMPTZ",
            "CONSTRAINT ck_responses_idempotency_claims_state",
            "CREATE INDEX IF NOT EXISTS idx_responses_idempotency_claims_response_expiry",
            "CREATE INDEX IF NOT EXISTS idx_responses_idempotency_claims_tenant_replay",
            "CREATE TABLE IF NOT EXISTS response_affinities",
            "response_id VARCHAR(2048) NOT NULL",
            "idempotency_id UUID UNIQUE",
            "PRIMARY KEY (tenant_id, response_id)",
            "account_id UUID REFERENCES accounts(id) ON DELETE RESTRICT",
            "model TEXT",
            "is_reservation BOOLEAN NOT NULL DEFAULT FALSE",
            "expires_at TIMESTAMPTZ NOT NULL",
            "local_context JSONB",
            "local_context_bytes BIGINT",
            "CONSTRAINT ck_response_affinities_local_context_size",
            "CONSTRAINT ck_response_affinities_account_owner",
            "settlement->>'account_id' = '00000000-0000-0000-0000-000000000000'",
            "settlement JSONB",
            "deleted_at TIMESTAMPTZ",
            "CREATE INDEX IF NOT EXISTS idx_response_affinities_local_warmups",
            "CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_due",
            "CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_recovery",
            "CREATE TABLE IF NOT EXISTS balance_reservations",
            "owner_token UUID NOT NULL DEFAULT gen_random_uuid()",
            "request_id UUID NOT NULL UNIQUE",
            "CHECK (status IN ('active', 'settled', 'released', 'expired'))",
            "CONSTRAINT ck_user_balances_frozen_nonnegative CHECK (frozen_balance >= 0)",
            "CONSTRAINT ck_user_balances_total_recharged_nonnegative CHECK (total_recharged >= 0)",
            "CONSTRAINT ck_user_balances_total_consumed_nonnegative CHECK (total_consumed >= 0)",
            "released_at TIMESTAMPTZ",
            "release_kind VARCHAR(20)",
            "release_reason TEXT",
            "released_by UUID REFERENCES users(id) ON DELETE SET NULL",
            "CONSTRAINT ck_balance_reservations_release_audit CHECK",
            "release_kind IN ('automatic', 'administrative')",
            "CHAR_LENGTH(BTRIM(release_reason)) <= 1000",
            "CONSTRAINT ck_balance_reservations_settlement_audit CHECK",
            "status = 'settled' AND usage_log_id IS NOT NULL AND settled_at IS NOT NULL",
            "status <> 'settled' AND usage_log_id IS NULL AND settled_at IS NULL",
            "CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_created",
            "CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_expiry",
            "CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_expiry",
            "CREATE UNIQUE INDEX IF NOT EXISTS uk_balance_reservations_usage_log",
            "CREATE TABLE IF NOT EXISTS balance_reservation_events",
            "event_sequence BIGINT GENERATED ALWAYS AS IDENTITY NOT NULL UNIQUE",
            "CHECK (event_type IN ('reserved', 'reowned', 'resized', 'settled', 'released', 'expired', 'updated'))",
            "CREATE INDEX IF NOT EXISTS idx_balance_reservation_events_request_sequence",
            "ON balance_reservation_events(request_id, event_sequence)",
            "CREATE INDEX IF NOT EXISTS idx_balance_reservation_events_reservation_sequence",
            "ON balance_reservation_events(reservation_id, event_sequence)",
            "CREATE OR REPLACE FUNCTION record_balance_reservation_event()",
            "CREATE TRIGGER trg_record_balance_reservation_event",
            "CREATE OR REPLACE FUNCTION reject_balance_reservation_event_mutation()",
            "CREATE TRIGGER trg_reject_balance_reservation_event_mutation",
            "CREATE UNIQUE INDEX IF NOT EXISTS uk_balance_transactions_consume_usage_log",
            "CREATE TABLE IF NOT EXISTS admin_balance_operations",
            "idempotency_key_hash VARCHAR(64) NOT NULL UNIQUE",
            "CHECK (operation_type IN ('recharge', 'consume', 'freeze', 'unfreeze'))",
            "AND CHAR_LENGTH(reason) <= 1000",
            "CONSTRAINT ck_admin_balance_operations_completion CHECK",
            "CREATE INDEX IF NOT EXISTS idx_admin_balance_operations_user_created",
            "CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_consumed_node_issued",
            "ON user_node_gateway_tokens(consumed_node_id, issued_at DESC, id DESC)",
            "WHERE consumed_node_id IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_node_tasks_status_created_at_desc",
            "ON node_tasks(status, created_at DESC, id DESC)",
        ] {
            assert!(V0001.contains(expected), "V0001 is missing {expected}");
        }
        assert!(V0001.contains(&format!(
            "responses_idempotency_claim_count BETWEEN 0 AND {}",
            crate::models::responses_idempotency_claim::RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT
        )));
        assert!(!V0001.contains("ALTER TABLE"));
        assert!(!V0001.contains("\nUPDATE "));
        assert!(!V0001.contains("\nDELETE FROM "));
        assert!(!V0001.contains("reservation_id UUID NOT NULL REFERENCES balance_reservations"));
    }

    #[test]
    fn initial_schema_indexes_match_bounded_admin_and_balance_queries() {
        for expected in [
            r#"CREATE INDEX IF NOT EXISTS idx_nodes_created_at_desc
    ON nodes(created_at DESC, id DESC);"#,
            r#"CREATE INDEX IF NOT EXISTS idx_user_node_gateway_tokens_pending_issued
    ON user_node_gateway_tokens(issued_at ASC, id ASC)
    WHERE status = 'pending';"#,
            r#"CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_created
    ON balance_reservations(user_id, created_at DESC, id DESC)
    INCLUDE (amount)
    WHERE status = 'active';"#,
            r#"CREATE INDEX IF NOT EXISTS idx_balance_reservations_active_user_expiry
    ON balance_reservations(user_id, expires_at, id)
    INCLUDE (amount)
    WHERE status = 'active';"#,
        ] {
            assert!(V0001.contains(expected), "V0001 is missing {expected}");
        }

        assert!(!V0001.contains("ON user_node_gateway_tokens(status) WHERE status = 'pending'"));
    }

    #[test]
    fn initial_schema_settlement_claim_index_matches_the_keyset_order() {
        let expected = r#"CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_due
    ON response_affinities(settlement_next_poll_at, tenant_id, response_id COLLATE "C")
    WHERE settlement IS NOT NULL;"#;

        assert!(V0001.contains(expected), "V0001 is missing {expected}");

        let recovery = r#"CREATE INDEX IF NOT EXISTS idx_response_affinities_settlement_recovery
    ON response_affinities(tenant_id, response_id)
    WHERE settlement IS NOT NULL;"#;
        assert!(V0001.contains(recovery), "V0001 is missing {recovery}");
    }
}
