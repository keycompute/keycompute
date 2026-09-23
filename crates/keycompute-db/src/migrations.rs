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
    fn distribution_policy_schema_enforces_domain_and_immutable_ownership() {
        let sql = include_str!("../migrations/001_init.sql");
        for expected in [
            "commission_rate >= 0 AND commission_rate <= 1",
            "ck_distribution_policy_window",
            "ck_distribution_policy_name",
            "ck_distribution_policy_description",
            "guard_distribution_policy_identity",
            "NEW.id,NEW.tenant_id,NEW.beneficiary_scope,NEW.beneficiary_id",
        ] {
            assert!(
                sql.contains(expected),
                "missing policy invariant: {expected}"
            );
        }
    }

    #[test]
    fn key_issuance_schema_retains_scoped_identity_without_storing_secrets() {
        let sql = include_str!("../migrations/001_init.sql");
        let body = sql
            .split("CREATE TABLE IF NOT EXISTS tenant_key_issuance_intents (")
            .nth(1)
            .unwrap()
            .split("\n);")
            .next()
            .unwrap();
        for expected in [
            "FOREIGN KEY (tenant_id, owner_user_id)",
            "FOREIGN KEY (tenant_id, requested_by_user_id)",
            "created_key_id UUID",
            "tenant_authz_version BIGINT",
        ] {
            assert!(body.contains(expected));
        }
        for forbidden in [
            "token_hash ",
            "secret ",
            "ciphertext ",
            "produce_ai_key_hash ",
            "created_key_id UUID REFERENCES",
        ] {
            assert!(!body.contains(forbidden));
        }
        assert!(sql.contains("terminal key issuance is immutable"));
        assert!(sql.contains("replacement key must belong to issuance owner"));
    }

    #[test]
    fn node_financial_schema_binds_sources_and_preserves_withdrawal_identity() {
        let sql = include_str!("../migrations/001_init.sql");
        for expected in [
            "fk_node_tips_usage_owner",
            "fk_node_tips_node_owner",
            "fk_node_tips_owner_membership",
            "uq_tip_withdrawal_request UNIQUE(tenant_id,owner_user_id,request_id)",
            "fk_tip_withdrawal_credit_owner",
            "withdrawal identity and amount are immutable",
            "terminal withdrawal is immutable",
            "node earnings are immutable accounting records",
        ] {
            assert!(
                sql.contains(expected),
                "missing financial invariant: {expected}"
            );
        }
        for name in ["node_tips", "node_tip_withdrawals"] {
            let marker = format!("CREATE TABLE IF NOT EXISTS {name} (");
            let body = sql
                .split(&marker)
                .nth(1)
                .unwrap()
                .split("\n);")
                .next()
                .unwrap();
            assert!(body.contains("tenant_id UUID NOT NULL"));
            assert!(body.contains("owner_user_id UUID NOT NULL"));
        }
    }

    #[test]
    fn node_session_schema_has_explicit_immutable_tenant_ownership() {
        let sql = include_str!("../migrations/001_init.sql");
        let table = sql
            .split("CREATE TABLE IF NOT EXISTS node_sessions (")
            .nth(1)
            .unwrap()
            .split("\n);")
            .next()
            .unwrap();
        for expected in [
            "tenant_id UUID NOT NULL",
            "owner_user_id UUID NOT NULL",
            "FOREIGN KEY (tenant_id, node_id, owner_user_id)",
            "REFERENCES nodes(tenant_id, id, owner_user_id)",
            "FOREIGN KEY (tenant_id, owner_user_id)",
        ] {
            assert!(
                table.contains(expected),
                "missing session invariant: {expected}"
            );
        }
        assert!(
            sql.contains("CONSTRAINT uq_nodes_session_owner UNIQUE (tenant_id, id, owner_user_id)")
        );
        assert!(sql.contains("idx_node_sessions_tenant_owner"));
        assert!(sql.contains("NEW.id,NEW.tenant_id,NEW.owner_user_id,NEW.node_id,NEW.session_token_hash,NEW.issued_at"));
    }

    #[test]
    fn native_worker_permissions_are_immutable_session_metadata() {
        let sql = include_str!("../migrations/001_init.sql");
        assert!(sql.contains("native_operations_json JSONB NOT NULL DEFAULT '[]'::jsonb"));
        assert!(sql.contains("native_profiles_json JSONB NOT NULL DEFAULT '[]'::jsonb"));
        assert!(sql.contains("registered_models_json JSONB NOT NULL DEFAULT '[]'::jsonb"));
        assert!(sql.contains("accepting_tasks BOOLEAN NOT NULL DEFAULT TRUE"));
        assert!(sql.contains("native_requirements_json JSONB"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS node_native_streams"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS node_native_stream_events"));
        assert!(
            sql.split_whitespace()
                .collect::<String>()
                .contains("PRIMARYKEY(task_id,lease_id,seq)")
        );
    }

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
    fn identity_schema_has_global_users_scoped_memberships_and_immutable_audit() {
        let sql = include_str!("../migrations/001_init.sql");
        for expected in [
            "status VARCHAR(20) NOT NULL DEFAULT 'active'",
            "tenant_role VARCHAR(20) NOT NULL DEFAULT 'member'",
            "invited_by UUID REFERENCES users(id) ON DELETE RESTRICT",
            "joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW()",
            "removed_at TIMESTAMPTZ",
            "authz_version BIGINT NOT NULL DEFAULT 1",
            "CONSTRAINT ck_tenant_memberships_removed_lifecycle",
            "owner_identity.status='active'",
            "status IN ('active', 'suspended', 'removed')",
            "tenant_role VARCHAR(20)",
            "accepted_by UUID REFERENCES users(id) ON DELETE RESTRICT",
            "platform_role VARCHAR(20) NOT NULL",
            "metadata JSONB NOT NULL DEFAULT '{}'::jsonb",
            "scope_type VARCHAR(20)",
            "resource_id TEXT",
            "CREATE TABLE IF NOT EXISTS identity_admin_fence",
            "CREATE TRIGGER membership_authority_version",
            "CREATE TRIGGER tenant_audit_immutable",
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_user_balances_tenant_user",
            "CONSTRAINT uk_usage_logs_tenant_id_id UNIQUE (tenant_id, id)",
            "tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT",
            "CONSTRAINT fk_distribution_records_usage_tenant",
            "CREATE TABLE IF NOT EXISTS pricing_cache_revisions",
            "CREATE TRIGGER pricing_cache_revision",
            "pricing scope and group are immutable",
            "token_hash VARCHAR(64)",
            "revoked_at TIMESTAMPTZ",
        ] {
            assert!(
                sql.contains(expected),
                "missing identity schema fragment {expected}"
            );
        }
        assert!(!sql.contains("invited_by_user_id"));
        assert!(!sql.contains("accepted_by_user_id"));
        assert!(!sql.contains("actor_platform_role"));
        assert!(!sql.contains("actor_tenant_role"));
        assert!(!sql.contains("details JSONB"));
        assert!(!sql.contains("m.role"));
        assert!(!sql.contains("m.version"));
        assert!(!sql.contains("status IN ('active', 'suspended', 'revoked')"));
        assert!(!sql.contains("tenant_id UUID NOT NULL REFERENCES users"));
        assert!(!sql.contains("ALTER TABLE"));
        assert!(!sql.contains("00000000-0000-0000-0000-000000000000' AS tenant_id"));
    }

    #[test]
    fn initial_schema_contains_the_complete_fresh_deployment_schema() {
        for expected in [
            "CREATE TABLE IF NOT EXISTS gateway_requests",
            "responses_idempotency_claim_count BIGINT NOT NULL DEFAULT 0",
            "CONSTRAINT ck_tenants_responses_idempotency_claim_count",
            "CONSTRAINT ck_tenants_status CHECK",
            "CREATE TABLE IF NOT EXISTS gateway_request_attempts",
            "last_probe_at TIMESTAMPTZ",
            "last_probe_latency_ms BIGINT",
            "last_probe_status VARCHAR(32)",
            "last_probe_error_code VARCHAR(128)",
            "health_status VARCHAR(20) NOT NULL DEFAULT 'unknown'",
            "health_reason VARCHAR(128)",
            "health_penalty INTEGER NOT NULL DEFAULT 0",
            "health_consecutive_failures INTEGER NOT NULL DEFAULT 0",
            "health_success_count BIGINT NOT NULL DEFAULT 0",
            "health_failure_count BIGINT NOT NULL DEFAULT 0",
            "health_avg_latency_ms BIGINT",
            "health_last_success_at TIMESTAMPTZ",
            "health_last_failure_at TIMESTAMPTZ",
            "health_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()",
            "health_generation BIGINT NOT NULL DEFAULT 0",
            "CREATE TABLE IF NOT EXISTS passthrough_bindings",
            "CONSTRAINT ck_passthrough_bindings_revision_positive",
            "CONSTRAINT uk_passthrough_bindings_account_tenant",
            "CREATE INDEX IF NOT EXISTS idx_passthrough_bindings_tenant",
            "CREATE TABLE IF NOT EXISTS account_model_health",
            "CONSTRAINT ck_account_model_health_status",
            "CONSTRAINT ck_account_model_health_expiry",
            "CONSTRAINT ck_account_model_health_reason_safe",
            "account_config_version TIMESTAMPTZ NOT NULL DEFAULT NOW()",
            "CONSTRAINT ck_account_model_health_generation_nonnegative",
            "CREATE INDEX IF NOT EXISTS idx_account_model_health_expiry",
            "passthrough_binding",
            "route_type IN ('provider_account', 'passthrough_binding', 'node')",
            "route_type IN ('provider_account', 'passthrough_binding')",
            "CONSTRAINT ck_accounts_health_status",
            "CONSTRAINT ck_accounts_health_penalty",
            "CONSTRAINT ck_accounts_health_counters",
            "CREATE INDEX IF NOT EXISTS idx_accounts_health_routing",
            "CREATE INDEX IF NOT EXISTS idx_accounts_pool_enabled",
            "api_capabilities TEXT[] NOT NULL",
            "pool_enabled BOOLEAN NOT NULL DEFAULT FALSE",
            "CONSTRAINT ck_accounts_api_capabilities",
            "tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT",
            "CREATE TABLE IF NOT EXISTS tenant_distribution_rules (\n    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),\n    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE",
            "CONSTRAINT ck_accounts_probe_status",
            "CONSTRAINT ck_accounts_priority",
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
            "scope_type VARCHAR(20) NOT NULL DEFAULT 'tenant'",
            "CONSTRAINT ck_pricing_models_scope CHECK",
            "scope_type = 'platform' AND tenant_id IS NULL",
            "scope_type = 'tenant' AND tenant_id IS NOT NULL",
            "UNIQUE NULLS NOT DISTINCT (tenant_id, model_name, billing_dimension)",
            "version BIGINT NOT NULL DEFAULT 1",
            "CONSTRAINT ck_pricing_models_billing_dimension",
            "CONSTRAINT ck_pricing_models_model_name_nonempty",
            "CONSTRAINT ck_pricing_models_input_price_nonnegative",
            "CONSTRAINT ck_pricing_models_effective_window",
            "CREATE UNIQUE INDEX IF NOT EXISTS uk_pricing_models_default_scope",
            "CREATE TABLE IF NOT EXISTS pricing_audit_events",
            "CONSTRAINT ck_pricing_audit_action",
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
    #[test]
    fn scoped_resource_schema_has_ownership_retention_and_event_fences() {
        let schema = include_str!("../migrations/001_init.sql");
        for fragment in [
            "CREATE TABLE IF NOT EXISTS scoped_responses",
            "CREATE OR REPLACE FUNCTION guard_resource_identity()",
            "CREATE OR REPLACE FUNCTION guard_node_identity()",
            "CREATE OR REPLACE FUNCTION revoke_suspended_user_credentials()",
            "ON user_node_gateway_tokens(tenant_id,user_id)",
            "CREATE TABLE IF NOT EXISTS scoped_conversations",
            "CREATE TABLE IF NOT EXISTS scoped_response_events",
            "uk_scoped_responses_idempotency",
            "idx_scoped_responses_pending",
            "PRIMARY KEY(response_id,seq)",
            "tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE",
            "user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE",
            "access_mode TEXT NOT NULL CHECK (access_mode IN ('passthrough','node_dispatch'))",
            "owner_id UUID NOT NULL",
            "user_id UUID,",
            "idx_usage_logs_user_tenant_created",
            "idx_response_affinities_user",
        ] {
            assert!(
                schema.contains(fragment),
                "missing schema contract {fragment}"
            );
        }
    }
    #[test]
    fn tenant_identity_baseline_has_only_the_final_authority_model() {
        let users = V0001
            .split("CREATE TABLE IF NOT EXISTS users (")
            .nth(1)
            .unwrap()
            .split("\n);")
            .next()
            .unwrap();
        assert!(users.contains("platform_role"));
        assert!(!users.contains("tenant_id"));
        assert!(!users.contains("\n    role "));
        for required in [
            "PRIMARY KEY (tenant_id, user_id)",
            "CREATE CONSTRAINT TRIGGER identity_users_guard",
            "CREATE CONSTRAINT TRIGGER identity_tenants_guard",
            "CREATE CONSTRAINT TRIGGER identity_memberships_guard",
            "CREATE TRIGGER membership_credentials_revoked",
            "fk_node_registration_membership",
            "CREATE TRIGGER tenant_audit_immutable",
            "CREATE TRIGGER tenant_audit_no_truncate",
        ] {
            assert!(V0001.contains(required), "missing {required}");
        }
        assert!(!V0001.contains("root@keycompute.invalid"));
    }
    #[test]
    fn node_control_schema_prevents_credential_transfer_and_reactivation() {
        let sql = include_str!("../migrations/001_init.sql");
        for guard in [
            "guard_node_session_identity",
            "guard_node_task_control",
            "dispatch_identity_is_active",
            "node_task_dispatch_identity_guard",
            "node task payload and original dispatch identity are immutable",
            "task_cancel_actor_pair",
            "task_archive_actor_pair",
            "task request and ownership are immutable",
            "advance_node_control_revision",
            "node_control_revision BEFORE UPDATE ON nodes",
            "node_control_revision BEFORE UPDATE ON user_node_gateway_tokens",
            "GREATEST(clock_timestamp(), OLD.updated_at + INTERVAL '1 microsecond')",
            "guard_node_registration_identity",
            "node session identity is immutable",
            "terminal node registrations cannot be reapproved",
            "owner_user_id=NEW.user_id",
        ] {
            assert!(
                sql.contains(guard),
                "missing node control invariant: {guard}"
            );
        }
    }
    #[test]
    fn managed_resource_administration_indexes_keep_tenant_scope() {
        let sql = include_str!("../migrations/001_init.sql");
        assert!(sql.contains("guard_scoped_response_request_identity"));
        assert!(sql.contains("guard_scoped_conversation_identity"));
        for index in [
            "idx_scoped_responses_tenant_admin",
            "idx_scoped_responses_owner_admin",
            "idx_scoped_conversations_tenant_admin",
            "idx_scoped_conversations_owner_admin",
        ] {
            assert!(sql.contains(index), "missing management index {index}");
        }
        assert!(sql.contains(
            "ON scoped_responses(tenant_id,user_id,access_mode,created_at DESC,id DESC)"
        ));
    }
}
