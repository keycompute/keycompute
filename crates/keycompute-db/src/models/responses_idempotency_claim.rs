use crate::DbError;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

/// Matches the bounded Responses JSON body accepted by the gateway. The
/// database constraint provides defense in depth for direct callers.
pub const RESPONSES_IDEMPOTENCY_MAX_RESPONSE_BODY_BYTES: u64 = 96 * 1024 * 1024;
/// Hard safety bound for permanent idempotency identities owned by one tenant.
/// Requests can omit `Idempotency-Key` after reaching this bound, while
/// historical identities remain immutable.
pub const RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT: u64 = 100_000;

#[derive(Debug, FromQueryResult)]
struct ResponsesIdempotencyIdentityUsage {
    entry_count: i64,
}

/// Permanent tenant-scoped identity and replay state for one Responses key.
///
/// Claims deliberately retain the historical provider account UUID without a
/// foreign key. An account is operational configuration that administrators
/// may delete; the claim is a request-identity record that must continue to
/// reject rebinding after that deletion. Cached HTTP results are temporary.
#[derive(Debug, Clone, FromQueryResult, PartialEq, Eq)]
pub struct ResponsesIdempotencyClaim {
    pub tenant_id: Uuid,
    pub binding_id: String,
    pub request_fingerprint: String,
    pub billing_request_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub provider: String,
    pub model: Option<String>,
    pub account_id: Uuid,
    pub execution_state: String,
    pub execution_token: Uuid,
    pub lease_expires_at: DateTime<Utc>,
    pub upstream_dispatched_at: Option<DateTime<Utc>>,
    pub response_status: Option<i16>,
    pub response_headers: Option<Value>,
    pub response_body: Option<String>,
    pub response_body_bytes: Option<i64>,
    pub response_expires_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Lightweight claim state used on execution and replay admission paths.
/// The cached body is deliberately replaced with its server-side byte length
/// so callers can reserve memory capacity before materializing the TEXT value.
#[derive(Debug, Clone, FromQueryResult, PartialEq, Eq)]
pub struct ResponsesIdempotencyClaimMetadata {
    pub tenant_id: Uuid,
    pub binding_id: String,
    pub request_fingerprint: String,
    pub billing_request_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub provider: String,
    pub model: Option<String>,
    pub account_id: Uuid,
    pub execution_state: String,
    pub execution_token: Uuid,
    pub lease_expires_at: DateTime<Utc>,
    pub upstream_dispatched_at: Option<DateTime<Utc>>,
    pub response_expires_at: Option<DateTime<Utc>>,
    pub response_body_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
}

impl ResponsesIdempotencyClaim {
    /// Bind the first request to its immutable identity and selected account.
    /// Concurrent or later callers receive the authoritative first claim and
    /// `inserted = false`.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        binding_id: &str,
        request_fingerprint: &str,
        billing_request_id: Uuid,
        user_id: Uuid,
        produce_ai_key_id: Uuid,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
    ) -> Result<(Self, bool), DbError> {
        Self::bind_for_execution(
            db,
            tenant_id,
            binding_id,
            request_fingerprint,
            billing_request_id,
            user_id,
            produce_ai_key_id,
            provider,
            model,
            account_id,
            Uuid::new_v4(),
            Utc::now() + Duration::minutes(5),
        )
        .await
    }

    /// Bind a new request with an explicit fencing token and execution lease.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_for_execution(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        binding_id: &str,
        request_fingerprint: &str,
        billing_request_id: Uuid,
        user_id: Uuid,
        produce_ai_key_id: Uuid,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        execution_token: Uuid,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<(Self, bool), DbError> {
        Self::bind_for_execution_with_identity_quota(
            db,
            tenant_id,
            binding_id,
            request_fingerprint,
            billing_request_id,
            user_id,
            produce_ai_key_id,
            provider,
            model,
            account_id,
            execution_token,
            lease_expires_at,
            RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT,
        )
        .await
    }

    /// Bind an execution with an explicit tenant identity quota. Locking the
    /// tenant row serializes the count-and-insert decision across replicas.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_for_execution_with_identity_quota(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        binding_id: &str,
        request_fingerprint: &str,
        billing_request_id: Uuid,
        user_id: Uuid,
        produce_ai_key_id: Uuid,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        execution_token: Uuid,
        lease_expires_at: DateTime<Utc>,
        max_identities: u64,
    ) -> Result<(Self, bool), DbError> {
        let txn = db.begin().await?;
        let (metadata, inserted) = Self::bind_for_execution_metadata_with_identity_quota(
            &txn,
            tenant_id,
            binding_id,
            request_fingerprint,
            billing_request_id,
            user_id,
            produce_ai_key_id,
            provider,
            model,
            account_id,
            execution_token,
            lease_expires_at,
            max_identities,
        )
        .await?;
        let claim = Self::find_for_update(&txn, metadata.tenant_id, &metadata.binding_id)
            .await?
            .ok_or_else(|| DbError::Other("idempotency claim disappeared".to_string()))?;
        txn.commit().await?;
        Ok((claim, inserted))
    }

    /// Bind an execution without loading a potentially large cached response.
    /// The caller's transaction retains the tenant lock through all related
    /// admission checks.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_for_execution_metadata(
        db: &sea_orm::DatabaseTransaction,
        tenant_id: Uuid,
        binding_id: &str,
        request_fingerprint: &str,
        billing_request_id: Uuid,
        user_id: Uuid,
        produce_ai_key_id: Uuid,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        execution_token: Uuid,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<(ResponsesIdempotencyClaimMetadata, bool), DbError> {
        Self::bind_for_execution_metadata_with_identity_quota(
            db,
            tenant_id,
            binding_id,
            request_fingerprint,
            billing_request_id,
            user_id,
            produce_ai_key_id,
            provider,
            model,
            account_id,
            execution_token,
            lease_expires_at,
            RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn bind_for_execution_metadata_with_identity_quota(
        db: &sea_orm::DatabaseTransaction,
        tenant_id: Uuid,
        binding_id: &str,
        request_fingerprint: &str,
        billing_request_id: Uuid,
        user_id: Uuid,
        produce_ai_key_id: Uuid,
        provider: &str,
        model: Option<&str>,
        account_id: Uuid,
        execution_token: Uuid,
        lease_expires_at: DateTime<Utc>,
        max_identities: u64,
    ) -> Result<(ResponsesIdempotencyClaimMetadata, bool), DbError> {
        let usage =
            ResponsesIdempotencyIdentityUsage::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT responses_idempotency_claim_count AS entry_count \
                 FROM tenants WHERE id = $1 FOR UPDATE",
                [tenant_id.into()],
            ))
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("Tenant", tenant_id.to_string()))?;

        if let Some(claim) = Self::find_metadata_for_update(db, tenant_id, binding_id).await? {
            return Ok((claim, false));
        }

        let max_identities =
            i64::try_from(max_identities.min(RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT))
                .map_err(|_| {
                    DbError::Other("idempotency identity entry quota overflow".to_string())
                })?;
        if usage.entry_count >= max_identities {
            return Err(DbError::ResourceLimitExceeded {
                resource: "Responses idempotency identities".to_string(),
                limit: format!("{max_identities} entries per tenant"),
            });
        }

        let inserted = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO responses_idempotency_claims \
                 (tenant_id, binding_id, request_fingerprint, billing_request_id, user_id, \
                  produce_ai_key_id, provider, model, account_id, execution_token, \
                  lease_expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (tenant_id, binding_id) DO NOTHING",
                [
                    tenant_id.into(),
                    binding_id.into(),
                    request_fingerprint.into(),
                    billing_request_id.into(),
                    user_id.into(),
                    produce_ai_key_id.into(),
                    provider.into(),
                    model.into(),
                    account_id.into(),
                    execution_token.into(),
                    lease_expires_at.into(),
                ],
            ))
            .await?
            .rows_affected()
            == 1;
        if inserted {
            let updated = db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenants SET responses_idempotency_claim_count = \
                     responses_idempotency_claim_count + 1 WHERE id = $1",
                    [tenant_id.into()],
                ))
                .await?
                .rows_affected();
            if updated != 1 {
                return Err(DbError::Other(
                    "idempotency identity counter update failed".to_string(),
                ));
            }
        }
        let claim = Self::find_metadata_for_update(db, tenant_id, binding_id)
            .await?
            .ok_or_else(|| DbError::Other("idempotency claim disappeared".to_string()))?;
        Ok((claim, inserted))
    }

    /// Replace an expired pre-dispatch execution lease and return its new
    /// fencing state. Once dispatch has started, an ambiguous upstream outcome
    /// must remain permanently fenced rather than being retried.
    pub async fn reclaim_expired_execution(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<Option<Self>, DbError> {
        let updated = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims \
                 SET execution_token = $3, lease_expires_at = $4 \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND upstream_dispatched_at IS NULL \
                   AND lease_expires_at <= NOW()",
                [
                    tenant_id.into(),
                    binding_id.into(),
                    execution_token.into(),
                    lease_expires_at.into(),
                ],
            ))
            .await?
            .rows_affected();
        if updated == 0 {
            return Ok(None);
        }
        Self::find_for_update(db, tenant_id, binding_id).await
    }

    /// Fence the claim immediately before the paid upstream POST can start.
    /// This transition is deliberately irreversible: after it commits, a crash
    /// cannot prove that the provider did not accept the request.
    pub async fn mark_execution_dispatched(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
    ) -> Result<bool, DbError> {
        let updated = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims SET upstream_dispatched_at = NOW() \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND execution_token = $3 \
                   AND upstream_dispatched_at IS NULL",
                [tenant_id.into(), binding_id.into(), execution_token.into()],
            ))
            .await?
            .rows_affected();
        Ok(updated == 1)
    }

    /// Persist a terminal HTTP response if `execution_token` still owns the
    /// lease, then atomically evict the tenant's oldest replay bodies until both
    /// quotas are satisfied. Permanent key identities remain as `expired` rows.
    ///
    /// Locking the tenant row serializes quota enforcement across application
    /// replicas. A response that cannot itself fit the configured or hard body
    /// limit is finalized as non-replayable without storing its body.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_execution_with_quota(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
        response_status: i16,
        response_headers: Value,
        response_body: &str,
        response_expires_at: DateTime<Utc>,
        max_entries: u64,
        max_bytes: u64,
    ) -> Result<bool, DbError> {
        let body_bytes = u64::try_from(response_body.len())
            .map_err(|_| DbError::Other("idempotency response body size overflow".to_string()))?;
        let replayable = max_entries > 0
            && body_bytes <= max_bytes
            && body_bytes <= RESPONSES_IDEMPOTENCY_MAX_RESPONSE_BODY_BYTES;
        let max_entries = i64::try_from(max_entries)
            .map_err(|_| DbError::Other("idempotency replay entry quota overflow".to_string()))?;
        let max_bytes = i64::try_from(max_bytes)
            .map_err(|_| DbError::Other("idempotency replay byte quota overflow".to_string()))?;
        let body_bytes = i64::try_from(body_bytes)
            .map_err(|_| DbError::Other("idempotency response body size overflow".to_string()))?;

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

        if !replayable {
            let updated = txn
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE responses_idempotency_claims \
                     SET execution_state = 'expired', response_status = NULL, \
                         response_headers = NULL, response_body = NULL, \
                         response_body_bytes = NULL, response_expires_at = NULL, \
                         completed_at = NOW(), lease_expires_at = NOW() \
                     WHERE tenant_id = $1 AND binding_id = $2 \
                       AND execution_state = 'in_progress' AND execution_token = $3 \
                       AND upstream_dispatched_at IS NOT NULL",
                    [tenant_id.into(), binding_id.into(), execution_token.into()],
                ))
                .await?
                .rows_affected();
            txn.commit().await?;
            return Ok(updated == 1);
        }

        let updated = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims \
                 SET execution_state = 'completed', response_status = $4, \
                     response_headers = $5, response_body = $6, \
                     response_body_bytes = $7, response_expires_at = $8, completed_at = NOW(), \
                     lease_expires_at = NOW() \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND execution_token = $3 \
                   AND upstream_dispatched_at IS NOT NULL",
                [
                    tenant_id.into(),
                    binding_id.into(),
                    execution_token.into(),
                    response_status.into(),
                    response_headers.into(),
                    response_body.into(),
                    body_bytes.into(),
                    response_expires_at.into(),
                ],
            ))
            .await?
            .rows_affected();
        if updated == 0 {
            txn.rollback().await?;
            return Ok(false);
        }

        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "WITH ranked AS (\
                 SELECT binding_id, \
                        ROW_NUMBER() OVER (\
                            ORDER BY (binding_id = $2) DESC, completed_at DESC, binding_id DESC\
                        ) AS replay_rank, \
                        SUM(response_body_bytes) OVER (\
                            ORDER BY (binding_id = $2) DESC, completed_at DESC, binding_id DESC \
                            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\
                        ) AS cumulative_bytes \
                 FROM responses_idempotency_claims \
                 WHERE tenant_id = $1 AND execution_state = 'completed'\
             ) \
             UPDATE responses_idempotency_claims AS claims \
             SET execution_state = 'expired', response_status = NULL, \
                 response_headers = NULL, response_body = NULL, \
                 response_body_bytes = NULL, response_expires_at = NULL \
             FROM ranked \
             WHERE claims.tenant_id = $1 AND claims.binding_id = ranked.binding_id \
               AND (ranked.replay_rank > $3 OR ranked.cumulative_bytes > $4)",
            [
                tenant_id.into(),
                binding_id.into(),
                max_entries.into(),
                max_bytes.into(),
            ],
        ))
        .await?;
        txn.commit().await?;
        Ok(true)
    }

    /// Make a failed pre-dispatch holder immediately reclaimable without
    /// removing identity. Dispatched executions must remain fenced.
    pub async fn release_execution(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
    ) -> Result<bool, DbError> {
        let updated = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims SET lease_expires_at = NOW() \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND execution_token = $3 \
                   AND upstream_dispatched_at IS NULL",
                [tenant_id.into(), binding_id.into(), execution_token.into()],
            ))
            .await?
            .rows_affected();
        Ok(updated == 1)
    }

    /// Finalize a dispatched execution whose HTTP result cannot be replayed.
    /// The permanent `expired` identity prevents a later request from repeating
    /// an upstream operation whose outcome may already have side effects.
    pub async fn expire_dispatched_execution(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
    ) -> Result<bool, DbError> {
        let updated = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims \
                 SET execution_state = 'expired', completed_at = NOW(), lease_expires_at = NOW() \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND execution_token = $3 \
                   AND upstream_dispatched_at IS NOT NULL",
                [tenant_id.into(), binding_id.into(), execution_token.into()],
            ))
            .await?
            .rows_affected();
        Ok(updated == 1)
    }

    /// Delete a newly-created claim after a definitive local failure before
    /// any upstream dispatch. Callers must retain claims once dispatch may
    /// have occurred so retries remain fenced.
    pub async fn delete_unstarted_execution(
        db: &(impl ConnectionTrait + TransactionTrait),
        tenant_id: Uuid,
        binding_id: &str,
        execution_token: Uuid,
    ) -> Result<bool, DbError> {
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
            return Ok(false);
        }
        let deleted = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM responses_idempotency_claims \
                 WHERE tenant_id = $1 AND binding_id = $2 \
                   AND execution_state = 'in_progress' AND execution_token = $3 \
                   AND upstream_dispatched_at IS NULL",
                [tenant_id.into(), binding_id.into(), execution_token.into()],
            ))
            .await?
            .rows_affected();
        if deleted == 1 {
            let decremented = txn
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenants SET responses_idempotency_claim_count = \
                     responses_idempotency_claim_count - 1 \
                     WHERE id = $1 AND responses_idempotency_claim_count > 0",
                    [tenant_id.into()],
                ))
                .await?
                .rows_affected();
            if decremented != 1 {
                txn.rollback().await?;
                return Err(DbError::Other(
                    "idempotency identity counter decrement failed".to_string(),
                ));
            }
        }
        txn.commit().await?;
        Ok(deleted == 1)
    }

    /// Drop expired response bodies while retaining the permanent key binding.
    pub async fn expire_completed_responses(db: &impl ConnectionTrait) -> Result<u64, DbError> {
        let result = db
            .execute(Statement::from_string(
                DbBackend::Postgres,
                "UPDATE responses_idempotency_claims \
                 SET execution_state = 'expired', response_status = NULL, \
                     response_headers = NULL, response_body = NULL, \
                     response_body_bytes = NULL, response_expires_at = NULL \
                 WHERE execution_state = 'completed' AND response_expires_at <= NOW()"
                    .to_string(),
            ))
            .await?;
        Ok(result.rows_affected())
    }

    /// Read the authoritative claim while retaining a row lock for the
    /// caller's transaction.
    pub async fn find_for_key_share(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
    ) -> Result<Option<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM responses_idempotency_claims \
             WHERE tenant_id = $1 AND binding_id = $2 FOR KEY SHARE",
            [tenant_id.into(), binding_id.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    /// Inspect replay state and cached-body size without materializing the body.
    pub async fn find_metadata_for_key_share(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
    ) -> Result<Option<ResponsesIdempotencyClaimMetadata>, DbError> {
        Self::find_metadata(db, tenant_id, binding_id, "FOR KEY SHARE").await
    }

    /// Read and exclusively lock the authoritative execution state.
    pub async fn find_for_update(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
    ) -> Result<Option<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM responses_idempotency_claims \
             WHERE tenant_id = $1 AND binding_id = $2 FOR UPDATE",
            [tenant_id.into(), binding_id.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    async fn find_metadata_for_update(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
    ) -> Result<Option<ResponsesIdempotencyClaimMetadata>, DbError> {
        Self::find_metadata(db, tenant_id, binding_id, "FOR UPDATE").await
    }

    async fn find_metadata(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        binding_id: &str,
        lock: &str,
    ) -> Result<Option<ResponsesIdempotencyClaimMetadata>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT tenant_id, binding_id, request_fingerprint, billing_request_id, user_id, \
                 produce_ai_key_id, provider, model, account_id, execution_state, execution_token, \
                 lease_expires_at, upstream_dispatched_at, response_expires_at, \
                 response_body_bytes, created_at \
                 FROM responses_idempotency_claims \
                 WHERE tenant_id = $1 AND binding_id = $2 {lock}"
            ),
            [tenant_id.into(), binding_id.into()],
        );
        Ok(ResponsesIdempotencyClaimMetadata::find_by_statement(stmt)
            .one(db)
            .await?)
    }
}
