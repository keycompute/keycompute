use crate::{DbError, DbRouter};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Tenant-scoped ownership and durable local state for OpenAI Responses and
/// their referenced conversations. Tombstoned synthetic IDs also carry the
/// shared terminal-settlement outbox without becoming API-visible resources.
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize, PartialEq)]
pub struct ResponseAffinity {
    pub tenant_id: Uuid,
    pub response_id: String,
    pub provider: String,
    pub model: Option<String>,
    /// Owning upstream account. Root local warmups and accountless terminal
    /// settlement outboxes have no upstream owner.
    pub account_id: Option<Uuid>,
    pub is_reservation: bool,
    pub local_response: Option<Value>,
    pub local_context: Option<Value>,
    pub local_context_bytes: Option<i64>,
    pub settlement: Option<Value>,
    pub settlement_next_poll_at: Option<DateTime<Utc>>,
    pub settlement_lease_until: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Exclusive lower bound for one finite settlement-claim sweep.
///
/// The field order matches PostgreSQL's claim index and row comparison. The
/// response ID is compared with the database's `C` collation so Rust's bytewise
/// `String` ordering produces the same maximum cursor even though `UPDATE ...
/// RETURNING` does not guarantee row order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SettlementClaimCursor {
    settlement_next_poll_at: DateTime<Utc>,
    tenant_id: Uuid,
    response_id: String,
}

/// Exclusive lower bound for the read-only startup TPM recovery scan.
///
/// Startup recovery deliberately does not claim settlement work: it only
/// reconstructs the already-admitted Redis/memory reservations before HTTP
/// generation routes become reachable. The ordering is independent from the
/// settlement retry time and lease so delayed or currently claimed rows cannot
/// be omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementRecoveryCursor {
    tenant_id: Uuid,
    response_id: String,
}

/// Minimal durable-settlement projection used by the startup TPM barrier.
/// Response bodies and continuation contexts can be large and are unrelated
/// to restoring rate-limit capacity, so the scan must not materialize them.
#[derive(Debug, Clone, FromQueryResult, PartialEq)]
pub struct SettlementRecoveryRow {
    pub tenant_id: Uuid,
    pub response_id: String,
    pub provider: String,
    pub account_id: Option<Uuid>,
    pub settlement: Value,
}

/// The minimal fields needed to replay a KeyCompute-local warmup. Keeping this
/// projection separate prevents ordinary resource lookups from decoding the
/// potentially large continuation context.
#[derive(Debug, Clone, FromQueryResult, PartialEq)]
pub struct LocalResponseState {
    pub model: Option<String>,
    pub local_context: Value,
}

#[derive(FromQueryResult)]
struct LocalWarmupStorageUsage {
    entry_count: i64,
    total_bytes: i64,
}

const CLAIM_DUE_SETTLEMENTS_BEFORE_SQL: &str = "UPDATE response_affinities AS affinity \
     SET settlement_lease_until = clock_timestamp() + make_interval(secs => $2::double precision), \
         updated_at = GREATEST(clock_timestamp(), $3 + INTERVAL '1 microsecond') \
     FROM (SELECT tenant_id, response_id FROM response_affinities \
           WHERE settlement IS NOT NULL AND settlement_next_poll_at <= $3 \
             AND updated_at <= $3 \
             AND (settlement_lease_until IS NULL OR settlement_lease_until <= clock_timestamp()) \
           ORDER BY settlement_next_poll_at, tenant_id, response_id COLLATE \"C\" \
           FOR UPDATE SKIP LOCKED LIMIT $1) AS due \
     WHERE affinity.tenant_id = due.tenant_id AND affinity.response_id = due.response_id \
     RETURNING affinity.*";

const CLAIM_DUE_SETTLEMENTS_AFTER_BEFORE_SQL: &str = "UPDATE response_affinities AS affinity \
     SET settlement_lease_until = clock_timestamp() + make_interval(secs => $2::double precision), \
         updated_at = GREATEST(clock_timestamp(), $3 + INTERVAL '1 microsecond') \
     FROM (SELECT tenant_id, response_id FROM response_affinities \
           WHERE settlement IS NOT NULL AND settlement_next_poll_at <= $3 \
             AND updated_at <= $3 \
             AND (settlement_lease_until IS NULL OR settlement_lease_until <= clock_timestamp()) \
             AND (settlement_next_poll_at, tenant_id, response_id COLLATE \"C\") > \
                 ($4::timestamptz, $5::uuid, $6::varchar COLLATE \"C\") \
           ORDER BY settlement_next_poll_at, tenant_id, response_id COLLATE \"C\" \
           FOR UPDATE SKIP LOCKED LIMIT $1) AS due \
     WHERE affinity.tenant_id = due.tenant_id AND affinity.response_id = due.response_id \
     RETURNING affinity.*";

const SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_SQL: &str = "SELECT tenant_id, response_id, provider, account_id, settlement \
     FROM response_affinities \
     WHERE settlement IS NOT NULL AND updated_at <= $2 \
     ORDER BY tenant_id, response_id LIMIT $1";

const SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_AFTER_SQL: &str = "SELECT tenant_id, response_id, provider, account_id, settlement \
     FROM response_affinities \
     WHERE settlement IS NOT NULL AND updated_at <= $2 \
       AND (tenant_id, response_id) > ($3::uuid, $4::varchar) \
     ORDER BY tenant_id, response_id LIMIT $1";

fn local_warmup_fits_quota(
    existing_entries: i64,
    existing_bytes: i64,
    new_bytes: i64,
    max_entries: u64,
    max_bytes: u64,
) -> bool {
    let (Ok(existing_entries), Ok(existing_bytes), Ok(new_bytes)) = (
        u64::try_from(existing_entries),
        u64::try_from(existing_bytes),
        u64::try_from(new_bytes),
    ) else {
        return false;
    };
    existing_entries < max_entries
        && existing_bytes
            .checked_add(new_bytes)
            .is_some_and(|total| total <= max_bytes)
}

impl ResponseAffinity {
    /// Load and retain a key-share lock on an active resource route for the
    /// lifetime of the caller's transaction. Account mutation takes its lock
    /// first and then locks these rows, so execution reservations use the same
    /// order to avoid routing an opaque resource through rotated credentials.
    pub async fn find_active_for_key_share(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<Option<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 \
               AND NOT is_reservation AND deleted_at IS NULL AND expires_at > NOW() \
             FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    /// Report whether a response still owns unfinished durable billing work.
    /// Resource deletion must wait until this becomes false because the
    /// settlement worker retrieves the upstream response to obtain exact usage.
    pub async fn has_pending_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<bool, DbError> {
        #[derive(FromQueryResult)]
        struct PendingSettlement {
            pending: bool,
        }

        // This is an authorization/billing guard, not an eventually
        // consistent display read. The locking clause makes DbRouter use the
        // writer and also prevents the row from disappearing while the query
        // is being evaluated.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT TRUE AS pending FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
               AND settlement IS NOT NULL LIMIT 1 FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(PendingSettlement::find_by_statement(stmt)
            .one(db)
            .await?
            .is_some_and(|row| row.pending))
    }

    /// Lock every Responses route owned by an account and report whether any
    /// route still owns a durable background billing settlement or an active
    /// pre-dispatch reservation. Callers must
    /// already hold a `FOR UPDATE` lock on the account row so no new route can
    /// appear between this check and account deletion.
    pub async fn lock_account_routes_and_has_deletion_blocker(
        db: &impl ConnectionTrait,
        account_id: Uuid,
    ) -> Result<bool, DbError> {
        #[derive(FromQueryResult)]
        struct SettlementLock {
            blocks_deletion: bool,
        }

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT settlement IS NOT NULL OR is_reservation AS blocks_deletion \
             FROM response_affinities WHERE account_id = $1 FOR UPDATE",
            [account_id.into()],
        );
        let routes = SettlementLock::find_by_statement(stmt).all(db).await?;
        Ok(routes.into_iter().any(|route| route.blocks_deletion))
    }

    /// Remove account-owned resource routes that no longer own billing work
    /// before their account is deleted or its connection identity changes.
    /// Root local warmups have no account owner and therefore survive.
    /// Permanent HTTP idempotency claims live in their own account-independent table.
    /// The schema-level RESTRICT remains the final guard if a caller attempts
    /// to bypass the coordinated deletion flow.
    pub async fn delete_settled_account_routes(
        db: &impl ConnectionTrait,
        account_id: Uuid,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM response_affinities \
             WHERE account_id = $1 AND settlement IS NULL AND NOT is_reservation",
            [account_id.into()],
        );
        Ok(db.execute(stmt).await?.rows_affected())
    }

    pub async fn upsert_route(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO response_affinities \
             (tenant_id, response_id, provider, model, account_id, is_reservation, expires_at) \
             VALUES ($1, $2, $3, $4, $5, FALSE, $6) \
             ON CONFLICT (tenant_id, response_id) DO UPDATE SET \
             provider = EXCLUDED.provider, model = EXCLUDED.model, \
             account_id = EXCLUDED.account_id, is_reservation = FALSE, \
             deleted_at = NULL, expires_at = EXCLUDED.expires_at, \
             updated_at = GREATEST(response_affinities.updated_at, clock_timestamp()) \
             WHERE response_affinities.account_id IS NOT DISTINCT FROM EXCLUDED.account_id \
             RETURNING *",
            [
                tenant_id.into(),
                response_id.into(),
                provider.into(),
                model.into(),
                account_id.into(),
                expires_at.into(),
            ],
        );
        Self::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| Self::ownership_collision(response_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_route_with_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        expires_at: DateTime<Utc>,
        settlement: Value,
        next_poll_at: DateTime<Utc>,
    ) -> Result<Self, DbError> {
        Self::upsert_route_with_settlement_visibility(
            db,
            tenant_id,
            response_id,
            provider,
            model,
            Some(account_id),
            expires_at,
            settlement,
            next_poll_at,
            None,
        )
        .await
    }

    /// Persist billing/TPM work without making it addressable through
    /// KeyCompute's resource endpoints. Background Responses and already-
    /// terminal generation requests share this tombstoned outbox shape;
    /// clearing the settlement deletes the row atomically.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_hidden_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        model: Option<&str>,
        account_id: Option<Uuid>,
        expires_at: DateTime<Utc>,
        settlement: Value,
        next_poll_at: DateTime<Utc>,
    ) -> Result<Self, DbError> {
        Self::upsert_route_with_settlement_visibility(
            db,
            tenant_id,
            response_id,
            provider,
            model,
            account_id,
            expires_at,
            settlement,
            next_poll_at,
            Some(Utc::now()),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_route_with_settlement_visibility(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        model: Option<&str>,
        account_id: Option<Uuid>,
        expires_at: DateTime<Utc>,
        settlement: Value,
        next_poll_at: DateTime<Utc>,
        deleted_at: Option<DateTime<Utc>>,
    ) -> Result<Self, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO response_affinities \
             (tenant_id, response_id, provider, model, account_id, is_reservation, expires_at, \
              settlement, settlement_next_poll_at, deleted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, FALSE, $6, $7, $8, $9, clock_timestamp()) \
             ON CONFLICT (tenant_id, response_id) DO UPDATE SET \
             provider = EXCLUDED.provider, model = EXCLUDED.model, \
             account_id = EXCLUDED.account_id, is_reservation = FALSE, \
             settlement = EXCLUDED.settlement, \
             settlement_next_poll_at = EXCLUDED.settlement_next_poll_at, \
             settlement_lease_until = NULL, \
             deleted_at = CASE \
                 WHEN response_affinities.deleted_at IS NOT NULL \
                 THEN response_affinities.deleted_at \
                 WHEN EXCLUDED.deleted_at IS NULL THEN NULL \
                 ELSE response_affinities.deleted_at END, \
             expires_at = EXCLUDED.expires_at, \
             updated_at = GREATEST(response_affinities.updated_at, clock_timestamp()) \
             WHERE response_affinities.account_id IS NOT DISTINCT FROM EXCLUDED.account_id \
             RETURNING *",
            [
                tenant_id.into(),
                response_id.into(),
                provider.into(),
                model.into(),
                account_id.into(),
                expires_at.into(),
                settlement.into(),
                next_poll_at.into(),
                deleted_at.into(),
            ],
        );
        Self::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| Self::ownership_collision(response_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_local(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        account_id: Option<Uuid>,
        local_response: Value,
        local_context: Value,
        local_context_bytes: i64,
        expires_at: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let model = local_response
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO response_affinities \
             (tenant_id, response_id, provider, model, account_id, is_reservation, local_response, local_context, local_context_bytes, expires_at) \
             VALUES ($1, $2, $3, $4, $5, FALSE, $6, $7, $8, $9) \
             ON CONFLICT (tenant_id, response_id) DO UPDATE SET \
             provider = EXCLUDED.provider, model = EXCLUDED.model, \
             account_id = EXCLUDED.account_id, is_reservation = FALSE, \
             local_response = EXCLUDED.local_response, local_context = EXCLUDED.local_context, \
             local_context_bytes = EXCLUDED.local_context_bytes, \
             deleted_at = NULL, expires_at = EXCLUDED.expires_at, \
             updated_at = GREATEST(response_affinities.updated_at, clock_timestamp()) \
             WHERE response_affinities.account_id IS NOT DISTINCT FROM EXCLUDED.account_id",
            [
                tenant_id.into(),
                response_id.into(),
                provider.into(),
                model.into(),
                account_id.into(),
                local_response.into(),
                local_context.into(),
                local_context_bytes.into(),
                expires_at.into(),
            ],
        );
        if db.execute(stmt).await?.rows_affected() == 1 {
            Ok(())
        } else {
            Err(Self::ownership_collision(response_id))
        }
    }

    /// Store a local warmup while atomically enforcing the tenant's active
    /// warmup quota. Locking the tenant row serializes quota-aware inserts
    /// across application replicas without locking or decoding large JSONB
    /// values.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_local_with_quota(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        response_id: &str,
        provider: &str,
        account_id: Option<Uuid>,
        local_response: Value,
        local_context: Value,
        local_context_bytes: i64,
        expires_at: DateTime<Utc>,
        max_entries: u64,
        max_bytes: u64,
    ) -> Result<(), DbError> {
        let txn = db.begin().await?;
        let tenant = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM tenants WHERE id = $1 FOR UPDATE",
                [tenant_id.into()],
            ))
            .await?;
        if tenant.is_none() {
            txn.rollback().await?;
            return Err(DbError::not_found("Tenant", tenant_id.to_string()));
        }

        let usage = LocalWarmupStorageUsage::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS entry_count, \
                        COALESCE(SUM(local_context_bytes), 0)::BIGINT AS total_bytes \
                 FROM response_affinities \
                 WHERE tenant_id = $1 AND response_id <> $2 AND NOT is_reservation \
                   AND deleted_at IS NULL AND expires_at > NOW() \
                   AND local_response IS NOT NULL",
            [tenant_id.into(), response_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or_else(|| DbError::Other("local warmup usage query returned no row".to_string()))?;
        if !local_warmup_fits_quota(
            usage.entry_count,
            usage.total_bytes,
            local_context_bytes,
            max_entries,
            max_bytes,
        ) {
            txn.rollback().await?;
            return Err(DbError::ResourceLimitExceeded {
                resource: "stored Responses warmups".to_string(),
                limit: format!("{max_entries} entries or {max_bytes} bytes per tenant"),
            });
        }

        Self::upsert_local(
            &txn,
            tenant_id,
            response_id,
            provider,
            account_id,
            local_response,
            local_context,
            local_context_bytes,
            expires_at,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    fn ownership_collision(response_id: &str) -> DbError {
        DbError::DuplicateKey {
            entity: "response affinity ownership".to_string(),
            field: "response_id".to_string(),
            value: response_id.to_string(),
        }
    }

    /// Reserve an account before a Responses request is dispatched. The
    /// reservation is replaced by the real affinity once a resp_* ID exists,
    /// or explicitly removed on terminal failure.
    pub async fn reserve_account(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        reservation_id: &str,
        provider: &str,
        account_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO response_affinities \
             (tenant_id, response_id, provider, account_id, is_reservation, expires_at) \
             VALUES ($1, $2, $3, $4, TRUE, $5) \
             ON CONFLICT (tenant_id, response_id) DO UPDATE SET \
             provider = EXCLUDED.provider, account_id = EXCLUDED.account_id, \
             is_reservation = TRUE, expires_at = EXCLUDED.expires_at, updated_at = NOW()",
            [
                tenant_id.into(),
                reservation_id.into(),
                provider.into(),
                account_id.into(),
                expires_at.into(),
            ],
        );
        db.execute(stmt).await?;
        Ok(())
    }

    pub async fn delete_reservation(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        reservation_id: &str,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND is_reservation",
            [tenant_id.into(), reservation_id.into()],
        );
        Ok(db.execute(stmt).await?.rows_affected())
    }

    pub async fn find_active(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<Option<Self>, DbError> {
        // Affinity determines tenant ownership and the upstream account. It
        // must never be served from a lagging replica.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
               AND deleted_at IS NULL AND expires_at > NOW() FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    /// Load only the local response document. Retrieve/delete/cancel do not
    /// need the potentially large continuation context.
    pub async fn find_active_local_response(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<Option<Value>, DbError> {
        #[derive(FromQueryResult)]
        struct LocalResponseDocument {
            local_response: Value,
        }

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT local_response FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
               AND deleted_at IS NULL AND expires_at > NOW() \
               AND local_response IS NOT NULL FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(LocalResponseDocument::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.local_response))
    }

    /// Load the local replay projection after the caller has admitted its
    /// resident-size estimate.
    pub async fn find_active_local_state(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<Option<LocalResponseState>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT model, local_context FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
               AND deleted_at IS NULL AND expires_at > NOW() \
               AND local_response IS NOT NULL AND local_context IS NOT NULL FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(LocalResponseState::find_by_statement(stmt).one(db).await?)
    }

    /// Return the conservative resident-size estimate of a local continuation
    /// without transferring the potentially large JSON value to the
    /// application. Callers use it to acquire memory admission first.
    pub async fn find_active_local_context_size(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<Option<u64>, DbError> {
        #[derive(FromQueryResult)]
        struct LocalContextSize {
            context_bytes: i64,
        }

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT local_context_bytes AS context_bytes \
             FROM response_affinities \
             WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
               AND deleted_at IS NULL AND expires_at > NOW() \
               AND local_response IS NOT NULL AND local_context IS NOT NULL \
             FOR KEY SHARE",
            [tenant_id.into(), response_id.into()],
        );
        Ok(LocalContextSize::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| u64::try_from(row.context_bytes).unwrap_or(u64::MAX)))
    }

    pub async fn delete_route_preserving_settlement(
        db: &DbRouter,
        tenant_id: Uuid,
        response_id: &str,
    ) -> Result<u64, DbError> {
        // An ownerless root warmup can never receive an upstream settlement, so
        // remove it directly. Account-owned responses retain a tombstone even
        // when settlement has not arrived yet: physically deleting that interim
        // row could let a terminal upsert recreate a visible route.
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "WITH deleted_local AS ( \
                 DELETE FROM response_affinities \
                 WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
                   AND account_id IS NULL RETURNING 1 \
             ), tombstoned_owned AS ( \
                 UPDATE response_affinities \
                 SET deleted_at = COALESCE(deleted_at, NOW()), local_response = NULL, \
                     local_context = NULL, local_context_bytes = NULL, \
                     updated_at = GREATEST(updated_at, clock_timestamp()) \
                 WHERE tenant_id = $1 AND response_id = $2 AND NOT is_reservation \
                   AND account_id IS NOT NULL RETURNING 1 \
             ) SELECT (SELECT COUNT(*) FROM deleted_local) + \
                      (SELECT COUNT(*) FROM tombstoned_owned) AS affected",
            [tenant_id.into(), response_id.into()],
        );
        // The top-level statement is a SELECT over data-modifying CTEs, so it
        // cannot rely on DbRouter's leading-statement classifier. Pin this
        // ownership mutation to the writer at the model boundary.
        let result =
            db.write_conn().query_one(stmt).await?.ok_or_else(|| {
                DbError::Other("response deletion returned no result".to_string())
            })?;
        let affected: i64 = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;
        u64::try_from(affected)
            .map_err(|_| DbError::Other("response deletion returned an invalid count".to_string()))
    }

    pub async fn delete_expired(db: &impl ConnectionTrait) -> Result<u64, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "DELETE FROM response_affinities \
             WHERE expires_at <= NOW() AND settlement IS NULL"
                .to_string(),
        );
        Ok(db.execute(stmt).await?.rows_affected())
    }

    /// Atomically lease due work across replicas.
    pub async fn claim_due_settlements(
        db: &impl ConnectionTrait,
        limit: u64,
        lease_until: DateTime<Utc>,
    ) -> Result<Vec<Self>, DbError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities AS affinity \
             SET settlement_lease_until = $2, \
                 updated_at = GREATEST(affinity.updated_at, clock_timestamp()) \
             FROM (SELECT tenant_id, response_id FROM response_affinities \
                   WHERE settlement IS NOT NULL AND settlement_next_poll_at <= NOW() \
                     AND (settlement_lease_until IS NULL OR settlement_lease_until <= NOW()) \
                   ORDER BY settlement_next_poll_at FOR UPDATE SKIP LOCKED LIMIT $1) AS due \
             WHERE affinity.tenant_id = due.tenant_id AND affinity.response_id = due.response_id \
             RETURNING affinity.*",
            [limit.into(), lease_until.into()],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }

    /// Atomically lease due work for a duration measured by the database
    /// clock. Durable workers use this form so application/DB clock skew cannot
    /// accidentally turn a short crash-recovery lease into a long one.
    pub async fn claim_due_settlements_for(
        db: &impl ConnectionTrait,
        limit: u64,
        lease_duration: std::time::Duration,
    ) -> Result<Vec<Self>, DbError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let lease_seconds = i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX);
        if lease_seconds <= 0 {
            return Err(DbError::Other(
                "settlement lease duration must be positive".to_string(),
            ));
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities AS affinity \
             SET settlement_lease_until = clock_timestamp() + make_interval(secs => $2::double precision), \
                 updated_at = GREATEST(affinity.updated_at, clock_timestamp()) \
             FROM (SELECT tenant_id, response_id FROM response_affinities \
                   WHERE settlement IS NOT NULL AND settlement_next_poll_at <= NOW() \
                     AND (settlement_lease_until IS NULL OR settlement_lease_until <= NOW()) \
                   ORDER BY settlement_next_poll_at FOR UPDATE SKIP LOCKED LIMIT $1) AS due \
             WHERE affinity.tenant_id = due.tenant_id AND affinity.response_id = due.response_id \
             RETURNING affinity.*",
            [limit.into(), lease_seconds.into()],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }

    /// Read the writer's wall clock before draining a finite snapshot of due
    /// settlement work. Keeping the cutoff in the database clock domain avoids
    /// application/DB clock skew while ensuring work inserted during a drain is
    /// left for the next maintenance tick.
    pub async fn settlement_claim_cutoff(db: &DbRouter) -> Result<DateTime<Utc>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT clock_timestamp() AS claim_cutoff".to_string(),
        );
        let row =
            db.write_conn().query_one(stmt).await?.ok_or_else(|| {
                DbError::Other("settlement claim clock returned no row".to_string())
            })?;
        row.try_get_by_index(0).map_err(DbError::DatabaseError)
    }

    /// Read one keyset page of every durable settlement that may own a TPM
    /// reservation at process startup.
    ///
    /// This scan intentionally ignores both `settlement_next_poll_at` and
    /// `settlement_lease_until`: a terminal outbox can be delayed to let inline
    /// settlement win, and another replica may currently own the monetary
    /// worker, but neither condition proves that this process may safely open
    /// generation admission with an empty local rate-limit backend. Reading
    /// from the writer prevents replica lag from omitting recently committed
    /// outboxes. `snapshot_cutoff` makes a multi-page scan finite while other
    /// serving replicas continue to write; those newer requests already own
    /// live limiter reservations on the instance that admitted them. No
    /// settlement lease is acquired or changed here.
    pub async fn scan_settlements_for_tpm_recovery(
        db: &DbRouter,
        limit: u64,
        snapshot_cutoff: DateTime<Utc>,
        after: Option<&SettlementRecoveryCursor>,
    ) -> Result<(Vec<SettlementRecoveryRow>, Option<SettlementRecoveryCursor>), DbError> {
        if limit == 0 {
            return Err(DbError::Other(
                "settlement TPM recovery page size must be positive".to_string(),
            ));
        }
        let limit = i64::try_from(limit)
            .map_err(|_| DbError::Other("settlement TPM recovery page is too large".to_string()))?;
        let stmt = match after {
            Some(after) => Statement::from_sql_and_values(
                DbBackend::Postgres,
                SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_AFTER_SQL,
                [
                    limit.into(),
                    snapshot_cutoff.into(),
                    after.tenant_id.into(),
                    after.response_id.clone().into(),
                ],
            ),
            None => Statement::from_sql_and_values(
                DbBackend::Postgres,
                SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_SQL,
                [limit.into(), snapshot_cutoff.into()],
            ),
        };
        let rows = SettlementRecoveryRow::find_by_statement(stmt)
            .all(db.write_conn())
            .await?;
        let next = rows.last().map(|row| SettlementRecoveryCursor {
            tenant_id: row.tenant_id,
            response_id: row.response_id.clone(),
        });
        Ok((rows, next))
    }

    /// Atomically lease one batch from the due set that existed at `due_before`.
    /// `updated_at` freezes the drain snapshot: newly inserted or rescheduled
    /// work cannot keep an otherwise finite batch loop alive indefinitely.
    pub async fn claim_due_settlements_before_for(
        db: &impl ConnectionTrait,
        limit: u64,
        lease_duration: std::time::Duration,
        due_before: DateTime<Utc>,
        after: Option<&SettlementClaimCursor>,
    ) -> Result<(Vec<Self>, Option<SettlementClaimCursor>), DbError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let lease_seconds = i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX);
        if lease_seconds <= 0 {
            return Err(DbError::Other(
                "settlement lease duration must be positive".to_string(),
            ));
        }
        let stmt = match after {
            Some(after) => Statement::from_sql_and_values(
                DbBackend::Postgres,
                CLAIM_DUE_SETTLEMENTS_AFTER_BEFORE_SQL,
                [
                    limit.into(),
                    lease_seconds.into(),
                    due_before.into(),
                    after.settlement_next_poll_at.into(),
                    after.tenant_id.into(),
                    after.response_id.clone().into(),
                ],
            ),
            None => Statement::from_sql_and_values(
                DbBackend::Postgres,
                CLAIM_DUE_SETTLEMENTS_BEFORE_SQL,
                [limit.into(), lease_seconds.into(), due_before.into()],
            ),
        };
        let claimed = Self::find_by_statement(stmt).all(db).await?;
        let mut next_cursor = None;
        for affinity in &claimed {
            let settlement_next_poll_at = affinity.settlement_next_poll_at.ok_or_else(|| {
                DbError::Other(format!(
                    "claimed settlement {} has no next poll timestamp",
                    affinity.response_id
                ))
            })?;
            let candidate = SettlementClaimCursor {
                settlement_next_poll_at,
                tenant_id: affinity.tenant_id,
                response_id: affinity.response_id.clone(),
            };
            if next_cursor
                .as_ref()
                .is_none_or(|current| candidate > *current)
            {
                next_cursor = Some(candidate);
            }
        }
        Ok((claimed, next_cursor))
    }

    /// Extend a settlement claim only while the caller still owns the exact
    /// lease generation. The returned database timestamp becomes the CAS token
    /// for the next renewal, reschedule, or acknowledgement.
    pub async fn renew_claimed_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        expected_lease_until: DateTime<Utc>,
        lease_duration: std::time::Duration,
    ) -> Result<Option<DateTime<Utc>>, DbError> {
        let lease_seconds = i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX);
        if lease_seconds <= 0 {
            return Err(DbError::Other(
                "settlement lease duration must be positive".to_string(),
            ));
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities \
             SET settlement_lease_until = clock_timestamp() + make_interval(secs => $4::double precision), \
                 updated_at = GREATEST(updated_at, clock_timestamp()) \
             WHERE tenant_id = $1 AND response_id = $2 \
               AND settlement IS NOT NULL AND settlement_lease_until = $3 \
               AND settlement_lease_until > clock_timestamp() \
             RETURNING settlement_lease_until",
            [
                tenant_id.into(),
                response_id.into(),
                expected_lease_until.into(),
                lease_seconds.into(),
            ],
        );
        let Some(result) = db.query_one(stmt).await? else {
            return Ok(None);
        };
        result
            .try_get_by_index(0)
            .map(Some)
            .map_err(DbError::DatabaseError)
    }

    /// Relinquish a claim without changing its durable payload or retry time.
    /// This is used when a worker cannot keep the corresponding external TPM
    /// lease alive after extending the DB lease.
    pub async fn relinquish_claimed_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        expected_lease_until: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities \
             SET settlement_lease_until = NULL, \
                 updated_at = GREATEST(updated_at, clock_timestamp()) \
             WHERE tenant_id = $1 AND response_id = $2 \
               AND settlement IS NOT NULL AND settlement_lease_until = $3",
            [
                tenant_id.into(),
                response_id.into(),
                expected_lease_until.into(),
            ],
        );
        Ok(db.execute(stmt).await?.rows_affected())
    }

    /// Reschedule work only while the caller still owns the lease returned by
    /// `claim_due_settlements`. A worker that outlives its lease must not
    /// overwrite a newer worker's settlement state.
    pub async fn reschedule_claimed_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        settlement: Value,
        next_poll_at: DateTime<Utc>,
        expected_lease_until: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities SET settlement_next_poll_at = $3, settlement = $4, \
             settlement_lease_until = NULL, \
             updated_at = GREATEST(updated_at, clock_timestamp()) \
             WHERE tenant_id = $1 AND response_id = $2 \
               AND settlement IS NOT NULL AND settlement_lease_until = $5",
            [
                tenant_id.into(),
                response_id.into(),
                next_poll_at.into(),
                settlement.into(),
                expected_lease_until.into(),
            ],
        );
        Ok(db.execute(stmt).await?.rows_affected())
    }

    /// Clear work only while the caller still owns the claimed lease. Hidden
    /// settlement rows are deleted; visible resources retain their affinity.
    pub async fn clear_claimed_settlement(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        response_id: &str,
        expected_lease_until: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "WITH target AS (\
                 SELECT tenant_id, response_id, deleted_at FROM response_affinities \
                 WHERE tenant_id = $1 AND response_id = $2 \
                   AND settlement IS NOT NULL AND settlement_lease_until = $3 \
                 FOR UPDATE\
             ), deleted AS (\
                 DELETE FROM response_affinities AS affinity USING target \
                 WHERE affinity.tenant_id = target.tenant_id \
                   AND affinity.response_id = target.response_id \
                   AND target.deleted_at IS NOT NULL \
                 RETURNING 1\
             ), updated AS (\
                 UPDATE response_affinities AS affinity \
                 SET settlement = NULL, settlement_next_poll_at = NULL, \
                     settlement_lease_until = NULL, updated_at = NOW() \
                 FROM target \
                 WHERE affinity.tenant_id = target.tenant_id \
                   AND affinity.response_id = target.response_id \
                   AND target.deleted_at IS NULL \
                 RETURNING 1\
             ) SELECT (SELECT COUNT(*) FROM deleted) + \
                      (SELECT COUNT(*) FROM updated) AS affected",
            [
                tenant_id.into(),
                response_id.into(),
                expected_lease_until.into(),
            ],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("settlement clear returned no result".to_string()))?;
        let affected: i64 = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;
        u64::try_from(affected)
            .map_err(|_| DbError::Other("settlement clear returned an invalid count".to_string()))
    }

    /// Acknowledge every terminal outbox row for one logical billing request.
    /// Hidden `store:false` rows are removed; visible resource affinities keep
    /// their routing identity and only release the settlement payload.
    pub async fn clear_completed_settlements(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        billing_request_id: Uuid,
    ) -> Result<(), DbError> {
        let billing_request_id = billing_request_id.to_string();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM response_affinities \
             WHERE tenant_id = $1 AND deleted_at IS NOT NULL AND settlement IS NOT NULL \
               AND COALESCE(settlement->>'billing_request_id', settlement->>'request_id') = $2",
            [tenant_id.into(), billing_request_id.clone().into()],
        ))
        .await?;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities SET settlement = NULL, \
                    settlement_next_poll_at = NULL, settlement_lease_until = NULL, \
                    updated_at = NOW() \
             WHERE tenant_id = $1 AND deleted_at IS NULL AND settlement IS NOT NULL \
               AND COALESCE(settlement->>'billing_request_id', settlement->>'request_id') = $2",
            [tenant_id.into(), billing_request_id.into()],
        ))
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLAIM_DUE_SETTLEMENTS_AFTER_BEFORE_SQL, CLAIM_DUE_SETTLEMENTS_BEFORE_SQL, ResponseAffinity,
        SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_AFTER_SQL, SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_SQL,
        SettlementClaimCursor, local_warmup_fits_quota,
    };
    use crate::DbRouter;
    use async_trait::async_trait;
    use sea_orm::{
        Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement, Value,
    };
    use std::collections::BTreeMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use uuid::Uuid;

    #[derive(Debug)]
    struct DeleteRouteProxy {
        affected: i64,
        query_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProxyDatabaseTrait for DeleteRouteProxy {
        async fn query(&self, _statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            Ok(vec![ProxyRow::new(BTreeMap::from([(
                "affected".to_string(),
                Value::BigInt(Some(self.affected)),
            )]))])
        }

        async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
            Ok(ProxyExecResult::default())
        }
    }

    async fn delete_route_connection(
        affected: i64,
    ) -> (sea_orm::DatabaseConnection, Arc<AtomicUsize>) {
        let query_count = Arc::new(AtomicUsize::new(0));
        let connection = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(DeleteRouteProxy {
                affected,
                query_count: Arc::clone(&query_count),
            })),
        )
        .await
        .unwrap();
        (connection, query_count)
    }

    #[tokio::test]
    async fn route_deletion_bypasses_read_replicas() {
        let (writer, writer_queries) = delete_route_connection(1).await;
        let (reader, reader_queries) = delete_route_connection(0).await;
        let router = DbRouter::with_read_connections_for_test(writer, vec![reader]);

        assert_eq!(
            ResponseAffinity::delete_route_preserving_settlement(
                router.as_ref(),
                Uuid::new_v4(),
                "resp_writer_only",
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(writer_queries.load(Ordering::Relaxed), 1);
        assert_eq!(reader_queries.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn local_warmup_quota_accepts_boundaries_and_rejects_overflow() {
        assert!(local_warmup_fits_quota(1, 60, 40, 2, 100));
        assert!(!local_warmup_fits_quota(2, 60, 1, 2, 100));
        assert!(!local_warmup_fits_quota(1, 60, 41, 2, 100));
        assert!(!local_warmup_fits_quota(-1, 0, 1, 2, 100));
        assert!(!local_warmup_fits_quota(0, 0, -1, 2, 100));
        assert!(!local_warmup_fits_quota(0, i64::MAX, 1, 1, i64::MAX as u64));
    }

    #[test]
    fn finite_claim_sweep_excludes_rows_touched_after_its_cutoff() {
        assert!(CLAIM_DUE_SETTLEMENTS_BEFORE_SQL.contains("updated_at <= $3"));
        assert!(
            CLAIM_DUE_SETTLEMENTS_BEFORE_SQL.contains(
                "updated_at = GREATEST(clock_timestamp(), $3 + INTERVAL '1 microsecond')"
            ),
            "claiming or quickly relinquishing a row must move it strictly beyond this sweep's cutoff"
        );
        assert!(
            CLAIM_DUE_SETTLEMENTS_AFTER_BEFORE_SQL
                .contains("(settlement_next_poll_at, tenant_id, response_id COLLATE \"C\") >")
        );
        assert!(
            CLAIM_DUE_SETTLEMENTS_AFTER_BEFORE_SQL
                .contains("ORDER BY settlement_next_poll_at, tenant_id, response_id COLLATE \"C\"")
        );
    }

    #[test]
    fn settlement_claim_cursor_uses_the_complete_database_order() {
        let next_poll_at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let tenant_id = Uuid::from_u128(1);
        let earlier = SettlementClaimCursor {
            settlement_next_poll_at: next_poll_at,
            tenant_id,
            response_id: "resp_a".to_string(),
        };
        let later_response = SettlementClaimCursor {
            settlement_next_poll_at: next_poll_at,
            tenant_id,
            response_id: "resp_b".to_string(),
        };
        let later_tenant = SettlementClaimCursor {
            settlement_next_poll_at: next_poll_at,
            tenant_id: Uuid::from_u128(2),
            response_id: "resp_a".to_string(),
        };

        assert!(earlier < later_response);
        assert!(later_response < later_tenant);
    }

    #[test]
    fn startup_tpm_recovery_scan_ignores_retry_and_worker_lease_state() {
        for sql in [
            SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_SQL,
            SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_AFTER_SQL,
        ] {
            assert!(sql.starts_with(
                "SELECT tenant_id, response_id, provider, account_id, settlement FROM"
            ));
            assert!(!sql.contains("SELECT *"));
            assert!(!sql.contains("local_response"));
            assert!(!sql.contains("local_context"));
            assert!(sql.contains("WHERE settlement IS NOT NULL AND updated_at <= $2"));
            assert!(!sql.contains("settlement_next_poll_at <="));
            assert!(!sql.contains("settlement_lease_until"));
            assert!(sql.contains("ORDER BY tenant_id, response_id"));
            assert!(!sql.contains("FOR UPDATE"));
        }
        assert!(
            SCAN_SETTLEMENTS_FOR_TPM_RECOVERY_AFTER_SQL
                .contains("(tenant_id, response_id) > ($3::uuid, $4::varchar)")
        );
    }
}
