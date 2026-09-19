//! Explicit tenant/model bindings and per-account model health.
//!
//! A model binding is deliberately separate from the ordinary account pool:
//! callers can resolve one (and only one) account for a tenant/model pair,
//! while the health table keeps model observations isolated from account-wide
//! health.  The helpers in this module perform the writer-side visibility and
//! capability checks used by management and dispatch paths.

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MODEL_BINDING_CAPABILITY: &str = "chat_completions";
pub const MODEL_BINDING_MAX_PAGE_SIZE: i64 = 200;
pub const MODEL_BINDING_MAX_MODEL_LENGTH: usize = 255;
pub const MODEL_HEALTH_MAX_REASON_LENGTH: usize = 128;

/// A management/API representation of an explicit model binding.
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct ModelBinding {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub api_capability: String,
    pub model: String,
    pub account_id: Uuid,
    pub enabled: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateModelBindingRequest {
    pub tenant_id: Uuid,
    #[serde(default = "default_binding_capability")]
    pub api_capability: String,
    pub model: String,
    pub account_id: Uuid,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateModelBindingRequest {
    /// A binding's model can be changed only as an explicit revisioned update.
    /// Account switches use the same revision fence and never retarget an
    /// already admitted request.
    pub model: Option<String>,
    pub account_id: Option<Uuid>,
    pub enabled: Option<bool>,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct ModelBindingCount {
    pub total: i64,
}

/// Independent health for one account/capability/model tuple.
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct AccountModelHealth {
    pub account_id: Uuid,
    pub api_capability: String,
    pub model: String,
    pub status: String,
    pub reason_code: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub account_config_version: DateTime<Utc>,
    pub generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountModelHealthProbe {
    pub account_id: Uuid,
    pub api_capability: String,
    pub model: String,
    pub status: String,
    pub reason_code: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub account_config_version: DateTime<Utc>,
    pub expected_generation: i64,
}

fn default_binding_capability() -> String {
    MODEL_BINDING_CAPABILITY.to_string()
}

fn validate_capability(capability: &str) -> Result<(), DbError> {
    if capability != MODEL_BINDING_CAPABILITY {
        return Err(DbError::Other(format!(
            "model bindings only support {MODEL_BINDING_CAPABILITY}"
        )));
    }
    Ok(())
}

fn validate_health_capability(capability: &str) -> Result<(), DbError> {
    if !matches!(capability, "chat_completions" | "responses" | "messages") {
        return Err(DbError::Other(format!(
            "unsupported account model health capability: {capability}"
        )));
    }
    Ok(())
}

/// Validate canonical model spelling without applying aliases or rewrites.
pub fn validate_model(model: &str) -> Result<(), DbError> {
    if model.is_empty()
        || model.trim() != model
        || model.len() > MODEL_BINDING_MAX_MODEL_LENGTH
        || model.chars().any(char::is_control)
        || model.to_ascii_lowercase().starts_with("node:")
    {
        return Err(DbError::Other(
            "model must be a non-empty canonical model name".to_string(),
        ));
    }
    Ok(())
}

fn validate_health_status(status: &str) -> Result<(), DbError> {
    if !matches!(status, "unknown" | "healthy" | "degraded" | "unhealthy") {
        return Err(DbError::Other(format!(
            "invalid account model health status: {status}"
        )));
    }
    Ok(())
}

fn validate_reason(reason_code: Option<&str>) -> Result<(), DbError> {
    if let Some(reason) = reason_code
        && (reason.trim() != reason
            || reason.is_empty()
            || reason.len() > MODEL_HEALTH_MAX_REASON_LENGTH
            || reason.chars().any(|character| {
                !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.' | ':')
            }))
    {
        return Err(DbError::Other(
            "health reason must be a short safe code".to_string(),
        ));
    }
    Ok(())
}

fn validate_page(limit: i64, offset: i64) -> Result<(), DbError> {
    if !(1..=MODEL_BINDING_MAX_PAGE_SIZE).contains(&limit) || offset < 0 {
        return Err(DbError::Other(format!(
            "model binding pagination must use limit 1..={MODEL_BINDING_MAX_PAGE_SIZE} and non-negative offset"
        )));
    }
    Ok(())
}

impl ModelBinding {
    /// Create a binding after checking tenant lifecycle and account visibility.
    /// The account is never copied into the binding; endpoint and credential
    /// changes therefore remain authoritative in `accounts`.
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateModelBindingRequest,
    ) -> Result<Self, DbError> {
        validate_capability(&req.api_capability)?;
        validate_model(&req.model)?;
        Self::validate_account_for_tenant(
            db,
            req.tenant_id,
            req.account_id,
            &req.api_capability,
            &req.model,
        )
        .await?;

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO model_bindings
                (tenant_id, api_capability, model, account_id, enabled)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
            [
                req.tenant_id.into(),
                req.api_capability.as_str().into(),
                req.model.as_str().into(),
                req.account_id.into(),
                req.enabled.unwrap_or(true).into(),
            ],
        );
        ModelBinding::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("model binding insert returned no row".to_string()))
    }

    pub async fn find_by_id(db: &impl ConnectionTrait, id: Uuid) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM model_bindings WHERE id = $1",
            [id.into()],
        ))
        .one(db)
        .await?)
    }

    /// Load a binding for a tenant without consulting the ordinary account
    /// pool. Disabled rows are returned so callers can distinguish 503 from a
    /// missing binding (404).
    pub async fn find_by_tenant_model(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        api_capability: &str,
        model: &str,
    ) -> Result<Option<Self>, DbError> {
        validate_model(model)?;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT mb.*
            FROM model_bindings mb
            JOIN tenants target_tenant ON target_tenant.id = mb.tenant_id
            WHERE mb.tenant_id = $1
              AND mb.api_capability = $2
              AND mb.model = $3
              AND target_tenant.status = 'active'
            "#,
            [tenant_id.into(), api_capability.into(), model.into()],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM model_bindings WHERE tenant_id = $1 ORDER BY model, id",
            [tenant_id.into()],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }

    /// Bounded management listing. `tenant_id = None` is reserved for a
    /// system-admin caller; authorization must be enforced by the service
    /// before invoking this method.
    pub async fn find_all_filtered(
        db: &impl ConnectionTrait,
        tenant_id: Option<Uuid>,
        enabled: Option<bool>,
        model: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        validate_page(limit, offset)?;
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT mb.* FROM model_bindings mb
            WHERE ($1::uuid IS NULL OR mb.tenant_id = $1)
              AND ($2::boolean IS NULL OR mb.enabled = $2)
              AND ($3::text IS NULL OR mb.model = $3)
            ORDER BY mb.model, mb.created_at, mb.id
            LIMIT $4 OFFSET $5
            "#,
            [
                tenant_id.into(),
                enabled.into(),
                model.into(),
                limit.into(),
                offset.into(),
            ],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }

    pub async fn count_filtered(
        db: &impl ConnectionTrait,
        tenant_id: Option<Uuid>,
        enabled: Option<bool>,
        model: Option<&str>,
    ) -> Result<i64, DbError> {
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let row = ModelBindingCount::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT COUNT(*)::BIGINT AS total FROM model_bindings
               WHERE ($1::uuid IS NULL OR tenant_id = $1)
                 AND ($2::boolean IS NULL OR enabled = $2)
                 AND ($3::text IS NULL OR model = $3)"#,
            [tenant_id.into(), enabled.into(), model.into()],
        ))
        .one(db)
        .await?
        .ok_or_else(|| DbError::Other("model binding count query returned no row".to_string()))?;
        Ok(row.total.max(0))
    }

    /// Update a binding under its optimistic revision fence. Account switches
    /// and enabled changes are explicit writes; a stale revision never wins.
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateModelBindingRequest,
    ) -> Result<Self, DbError> {
        if req.expected_revision <= 0 {
            return Err(DbError::Other(
                "expected binding revision must be positive".to_string(),
            ));
        }
        let model = req.model.as_deref().unwrap_or(&self.model);
        validate_model(model)?;
        let account_id = req.account_id.unwrap_or(self.account_id);
        Self::validate_account_for_tenant(
            db,
            self.tenant_id,
            account_id,
            &self.api_capability,
            model,
        )
        .await?;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE model_bindings
            SET model = COALESCE($1, model),
                account_id = COALESCE($2, account_id),
                enabled = COALESCE($3, enabled),
                revision = revision + 1,
                updated_at = GREATEST(updated_at, NOW()) + INTERVAL '1 microsecond'
            WHERE id = $4 AND revision = $5
            RETURNING *
            "#,
            [
                req.model.clone().into(),
                req.account_id.into(),
                req.enabled.into(),
                self.id.into(),
                req.expected_revision.into(),
            ],
        );
        Self::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::OptimisticConflict {
                entity: "model binding".to_string(),
                id: self.id.to_string(),
            })
    }

    pub async fn delete_if_revision(
        &self,
        db: &impl ConnectionTrait,
        expected_revision: i64,
    ) -> Result<(), DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM model_bindings WHERE id = $1 AND revision = $2",
                [self.id.into(), expected_revision.into()],
            ))
            .await?;
        if result.rows_affected() != 1 {
            return Err(DbError::OptimisticConflict {
                entity: "model binding".to_string(),
                id: self.id.to_string(),
            });
        }
        Ok(())
    }

    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM model_bindings WHERE id = $1",
            [self.id.into()],
        ))
        .await?;
        Ok(())
    }

    /// Writer-side validation shared by create/update/probe management paths.
    /// Global accounts may be bound explicitly by any active tenant, but no
    /// tenant-private account can cross tenant boundaries.
    pub async fn validate_account_for_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        account_id: Uuid,
        api_capability: &str,
        model: &str,
    ) -> Result<(), DbError> {
        validate_capability(api_capability)?;
        validate_model(model)?;
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                SELECT 1
                FROM accounts a
                JOIN tenants owner_tenant ON owner_tenant.id = a.tenant_id
                JOIN tenants target_tenant ON target_tenant.id = $2
                WHERE a.id = $1
                  AND target_tenant.status = 'active'
                  AND owner_tenant.status = 'active'
                  AND (a.tenant_id = $2 OR a.visibility = 'global')
                  AND LOWER(a.provider) = 'openai'
                  AND a.api_capabilities @> ARRAY[$3]::TEXT[]
                  AND $4 = ANY(a.models_supported)
                "#,
                [
                    account_id.into(),
                    tenant_id.into(),
                    api_capability.into(),
                    model.into(),
                ],
            ))
            .await?;
        if row.is_none() {
            return Err(DbError::Other(
                "account is not visible, active, openai, or configured for this model/capability"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

impl AccountModelHealth {
    pub async fn find(
        db: &impl ConnectionTrait,
        account_id: Uuid,
        api_capability: &str,
        model: &str,
    ) -> Result<Option<Self>, DbError> {
        validate_health_capability(api_capability)?;
        validate_model(model)?;
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT * FROM account_model_health
               WHERE account_id = $1 AND api_capability = $2 AND model = $3"#,
            [account_id.into(), api_capability.into(), model.into()],
        ))
        .one(db)
        .await?)
    }

    /// CAS-write a probe result. The account's `updated_at` is checked in the
    /// same statement, and `expected_generation` fences concurrent probes.
    /// A stale result returns `Ok(None)` and has no effect.
    pub async fn upsert_probe_if_current(
        db: &impl ConnectionTrait,
        probe: &AccountModelHealthProbe,
    ) -> Result<Option<Self>, DbError> {
        Self::upsert_probe_inner(db, probe, None).await
    }

    /// Fence an administrator's exact-model probe to the binding observed
    /// before I/O as well as the account configuration and health generation.
    pub async fn upsert_binding_probe_if_current(
        db: &impl ConnectionTrait,
        probe: &AccountModelHealthProbe,
        binding: &ModelBinding,
    ) -> Result<Option<Self>, DbError> {
        if probe.account_id != binding.account_id
            || probe.model != binding.model
            || probe.api_capability != binding.api_capability
        {
            return Err(DbError::Other("probe does not match model binding".into()));
        }
        Self::upsert_probe_inner(db, probe, Some((binding.id, binding.revision))).await
    }

    async fn upsert_probe_inner(
        db: &impl ConnectionTrait,
        probe: &AccountModelHealthProbe,
        binding: Option<(Uuid, i64)>,
    ) -> Result<Option<Self>, DbError> {
        validate_health_capability(&probe.api_capability)?;
        validate_model(&probe.model)?;
        validate_health_status(&probe.status)?;
        validate_reason(probe.reason_code.as_deref())?;
        if probe.expires_at <= probe.checked_at {
            return Err(DbError::Other(
                "model health expiry must be after checked_at".to_string(),
            ));
        }
        if probe.expected_generation < 0 {
            return Err(DbError::Other(
                "expected health generation is negative".to_string(),
            ));
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            WITH current_account AS (
                SELECT a.id, a.updated_at AS account_config_version
                FROM accounts a
                WHERE a.id = $1 AND a.updated_at = $7
                  AND ($9 = 0 OR EXISTS (
                      SELECT 1 FROM account_model_health h
                      WHERE h.account_id=a.id AND h.api_capability=$2 AND h.model=$3
                        AND h.generation=$9
                  ))
                  AND ($10::UUID IS NULL OR EXISTS (
                      SELECT 1 FROM model_bindings mb
                      JOIN tenants caller ON caller.id=mb.tenant_id
                      JOIN tenants owner ON owner.id=a.tenant_id
                      WHERE mb.id=$10 AND mb.revision=$11 AND mb.account_id=a.id
                        AND mb.api_capability=$2 AND mb.model=$3
                        AND caller.status='active' AND owner.status='active'
                        AND (a.tenant_id=mb.tenant_id OR a.visibility='global')
                        AND a.enabled=TRUE AND a.provider='openai'
                        AND $3=ANY(a.models_supported)
                        AND a.api_capabilities @> ARRAY[$2]::TEXT[]
                  ))
            ), upserted AS (
                INSERT INTO account_model_health
                    (account_id, api_capability, model, status, reason_code,
                     checked_at, expires_at, account_config_version, generation)
                SELECT $1, $2, $3, $4, $5, $6, $8, account_config_version,
                       $9 + 1
                FROM current_account
                ON CONFLICT (account_id, api_capability, model) DO UPDATE
                SET status = EXCLUDED.status,
                    reason_code = EXCLUDED.reason_code,
                    checked_at = EXCLUDED.checked_at,
                    expires_at = EXCLUDED.expires_at,
                    account_config_version = EXCLUDED.account_config_version,
                    generation = account_model_health.generation + 1,
                    updated_at = NOW()
                WHERE account_model_health.generation = $9
                RETURNING *
            )
            SELECT * FROM upserted
            "#,
            [
                probe.account_id.into(),
                probe.api_capability.as_str().into(),
                probe.model.as_str().into(),
                probe.status.as_str().into(),
                probe.reason_code.clone().into(),
                probe.checked_at.into(),
                probe.account_config_version.into(),
                probe.expires_at.into(),
                probe.expected_generation.into(),
                binding.map(|v| v.0).into(),
                binding.map(|v| v.1).into(),
            ],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    /// Record a runtime observation without allowing an event from an older
    /// account configuration or generation to overwrite a newer probe.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_runtime_if_current(
        db: &impl ConnectionTrait,
        account_id: Uuid,
        api_capability: &str,
        model: &str,
        account_config_version: DateTime<Utc>,
        expected_generation: i64,
        status: &str,
        reason_code: Option<&str>,
        checked_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Option<Self>, DbError> {
        validate_health_capability(api_capability)?;
        validate_model(model)?;
        validate_health_status(status)?;
        validate_reason(reason_code)?;
        if expires_at <= checked_at || expected_generation < 0 {
            return Err(DbError::Other(
                "invalid model health runtime observation window or generation".to_string(),
            ));
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE account_model_health h
            SET status = $6, reason_code = $7, checked_at = $8,
                expires_at = $9, updated_at = NOW(), generation = generation + 1
            FROM accounts a
            WHERE h.account_id = $1
              AND h.api_capability = $2
              AND h.model = $3
              AND h.account_id = a.id
              AND a.updated_at = $4
              AND h.account_config_version = $4
              AND h.generation = $5
            RETURNING h.*
            "#,
            [
                account_id.into(),
                api_capability.into(),
                model.into(),
                account_config_version.into(),
                expected_generation.into(),
                status.into(),
                reason_code.into(),
                checked_at.into(),
                expires_at.into(),
            ],
        );
        Ok(Self::find_by_statement(stmt).one(db).await?)
    }

    /// Mark all model observations for an account unknown after a deliberate
    /// administrative reset. The account's current timestamp is copied into
    /// the fence, and each row receives a new generation.
    pub async fn reset_for_account(
        db: &impl ConnectionTrait,
        account_id: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE account_model_health h
            SET status = 'unknown', reason_code = NULL, checked_at = NULL,
                expires_at = NULL, account_config_version = a.updated_at,
                generation = h.generation + 1, updated_at = NOW()
            FROM accounts a
            WHERE h.account_id = $1 AND a.id = h.account_id
            RETURNING h.*
            "#,
            [account_id.into()],
        );
        Ok(Self::find_by_statement(stmt).all(db).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_models_are_canonical_and_never_node_reserved() {
        assert!(validate_model("gpt-4o").is_ok());
        assert!(validate_model("").is_err());
        assert!(validate_model(" gpt-4o").is_err());
        assert!(validate_model("node:local").is_err());
    }

    #[test]
    fn health_status_and_reason_are_bounded() {
        assert!(validate_health_status("healthy").is_ok());
        assert!(validate_health_status("broken").is_err());
        assert!(validate_reason(Some("upstream_5xx")).is_ok());
        assert!(validate_reason(Some("bad code")).is_err());
        assert!(validate_reason(Some("bad\ncode")).is_err());
    }
}
