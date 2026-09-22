//! Account-to-tenant passthrough grants. Model capabilities and credentials
//! remain account-owned; model health is operational state, not a binding.
use super::{account::Account, query::escape_like_pattern, upstream_access::lock_configuration};
use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, FromQueryResult,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[path = "passthrough_binding_scope.rs"]
mod scope;
pub use scope::{
    PassthroughAccountOption, PassthroughBindingListFilter, PassthroughBindingManagementView,
    PreparedPassthroughProbe,
};

pub const PASSTHROUGH_BINDING_MAX_PAGE_SIZE: i64 = 200;
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PassthroughBinding {
    pub id: Uuid,
    pub account_id: Uuid,
    pub tenant_id: Uuid,
    pub is_global: bool,
    pub pool_enabled: bool,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePassthroughBindingRequest {
    pub account_id: Uuid,
    pub tenant_id: Uuid,
    #[serde(default)]
    pub is_global: bool,
    #[serde(default)]
    pub pool_enabled: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePassthroughBindingRequest {
    pub account_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub is_global: Option<bool>,
    pub pool_enabled: Option<bool>,
    pub expected_revision: i64,
}
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PassthroughBindingCount {
    pub total: i64,
}

fn conflict(id: Uuid) -> DbError {
    DbError::OptimisticConflict {
        entity: "passthrough binding".into(),
        id: id.to_string(),
    }
}

impl PassthroughBinding {
    pub async fn find_by_id(db: &impl ConnectionTrait, id: Uuid) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM passthrough_bindings WHERE id=$1",
            [id.into()],
        ))
        .one(db)
        .await?)
    }
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM passthrough_bindings WHERE tenant_id=$1 ORDER BY account_id,id",
            [tenant.into()],
        ))
        .all(db)
        .await?)
    }
    /// Called only inside the serialized administration transaction. Lock
    /// tenant parents before the account, and recheck its owner afterwards.
    async fn validate_account(
        txn: &DatabaseTransaction,
        account_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Account, DbError> {
        if account_id.is_nil() || tenant_id.is_nil() {
            return Err(DbError::Other(
                "account and tenant must be valid identifiers".into(),
            ));
        }
        let before = Account::find_by_id(txn, account_id)
            .await?
            .ok_or_else(|| DbError::not_found("account", account_id))?;
        txn.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM tenants WHERE id=ANY($1::UUID[]) ORDER BY id FOR KEY SHARE",
            [vec![before.tenant_id, tenant_id].into()],
        ))
        .await?;
        let account = Account::find_by_id_for_update(txn, account_id)
            .await?
            .ok_or_else(|| DbError::not_found("account", account_id))?;
        if before.tenant_id != account.tenant_id {
            return Err(conflict(account_id));
        }
        let active=txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM tenants owner JOIN tenants target ON target.id=$2 WHERE owner.id=$1 AND owner.status='active' AND target.status='active'",
            [account.tenant_id.into(),tenant_id.into()])).await?.is_some();
        let supported = match account.provider.as_str() {
            "openai" => account
                .api_capabilities
                .iter()
                .any(|v| matches!(v.as_str(), "chat_completions" | "responses")),
            "anthropic" => account.api_capabilities.iter().any(|v| v == "messages"),
            _ => false,
        };
        if !active || !account.enabled || !supported {
            return Err(DbError::Other("account and tenant must be active and the account must declare a supported native API capability".into()));
        }
        // An authenticated system administrator is explicitly granting access;
        // legacy account visibility is not a prerequisite for the new grant.
        Ok(account)
    }

    /// Reject distinct upstream accounts with overlapping model namespaces
    /// and consumer scopes. The runtime repeats this check before health
    /// filtering to protect against unsupported out-of-band database edits.
    pub async fn ensure_no_overlap(
        db: &impl ConnectionTrait,
        account_id: Uuid,
        tenant_id: Uuid,
        is_global: bool,
        ignore_id: Option<Uuid>,
    ) -> Result<(), DbError> {
        let overlap = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
          SELECT 1 FROM passthrough_bindings other
          JOIN accounts other_account ON other_account.id=other.account_id
          JOIN accounts selected ON selected.id=$1
          WHERE other.account_id<>$1 AND ($4::UUID IS NULL OR other.id<>$4)
            AND (other.is_global OR $3 OR other.tenant_id=$2)
            AND other_account.provider=selected.provider
            AND other_account.api_capabilities && selected.api_capabilities
            AND other_account.models_supported && selected.models_supported
          LIMIT 1"#,
                [
                    account_id.into(),
                    tenant_id.into(),
                    is_global.into(),
                    ignore_id.into(),
                ],
            ))
            .await?
            .is_some();
        if overlap {
            return Err(DbError::Other("passthrough_binding_ambiguous".into()));
        }
        Ok(())
    }
    pub async fn ensure_account_models_unambiguous(
        db: &impl ConnectionTrait,
        account: Uuid,
    ) -> Result<(), DbError> {
        let rows = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM passthrough_bindings WHERE account_id=$1",
            [account.into()],
        ))
        .all(db)
        .await?;
        for row in rows {
            Self::ensure_no_overlap(db, account, row.tenant_id, row.is_global, Some(row.id))
                .await?;
        }
        Ok(())
    }
    async fn suppress_legacy_pool(db: &impl ConnectionTrait, account: Uuid) -> Result<(), DbError> {
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE accounts SET pool_enabled=FALSE,updated_at=GREATEST(updated_at,statement_timestamp())+INTERVAL '1 microsecond' WHERE id=$1 AND pool_enabled",
            [account.into()])).await?;
        Ok(())
    }
    pub async fn create(
        db: &DatabaseConnection,
        req: &CreatePassthroughBindingRequest,
    ) -> Result<Self, DbError> {
        let txn = db.begin().await?;
        lock_configuration(&txn).await?;
        Self::validate_account(&txn, req.account_id, req.tenant_id).await?;
        Self::ensure_no_overlap(&txn, req.account_id, req.tenant_id, req.is_global, None).await?;
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO passthrough_bindings(account_id,tenant_id,is_global,pool_enabled) VALUES($1,$2,$3,$4) RETURNING *",
            [req.account_id.into(),req.tenant_id.into(),req.is_global.into(),req.pool_enabled.into()])).one(&txn).await?
            .ok_or_else(||DbError::Other("passthrough binding insert returned no row".into()))?;
        Self::suppress_legacy_pool(&txn, req.account_id).await?;
        txn.commit().await?;
        Ok(row)
    }
    pub async fn update(
        &self,
        db: &DatabaseConnection,
        req: &UpdatePassthroughBindingRequest,
    ) -> Result<Self, DbError> {
        if req.expected_revision <= 0 {
            return Err(conflict(self.id));
        }
        let txn = db.begin().await?;
        lock_configuration(&txn).await?;
        let current = Self::find_by_id(&txn, self.id)
            .await?
            .ok_or_else(|| conflict(self.id))?;
        if current.revision != req.expected_revision {
            return Err(conflict(self.id));
        }
        let account = req.account_id.unwrap_or(current.account_id);
        let tenant = req.tenant_id.unwrap_or(current.tenant_id);
        let global = req.is_global.unwrap_or(current.is_global);
        let pool = req.pool_enabled.unwrap_or(current.pool_enabled);
        // Restriction-only changes remain possible during an account outage.
        let only_restricting = account == current.account_id
            && tenant == current.tenant_id
            && (!global || current.is_global)
            && (!pool || current.pool_enabled);
        if !only_restricting {
            Self::validate_account(&txn, account, tenant).await?;
        }
        // Narrowing authority is safe even if out-of-band configuration
        // created a namespace conflict. Never block revocation on repair.
        if !only_restricting {
            Self::ensure_no_overlap(&txn, account, tenant, global, Some(self.id)).await?;
        }
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE passthrough_bindings SET account_id=$1,tenant_id=$2,is_global=$3,pool_enabled=$4,revision=revision+1,updated_at=GREATEST(updated_at,statement_timestamp())+INTERVAL '1 microsecond' WHERE id=$5 AND revision=$6 RETURNING *",
            [account.into(),tenant.into(),global.into(),pool.into(),self.id.into(),req.expected_revision.into()])).one(&txn).await?.ok_or_else(||conflict(self.id))?;
        Self::suppress_legacy_pool(&txn, current.account_id).await?;
        Self::suppress_legacy_pool(&txn, account).await?;
        txn.commit().await?;
        Ok(row)
    }
    pub async fn delete_if_revision(
        &self,
        db: &DatabaseConnection,
        expected_revision: i64,
    ) -> Result<(), DbError> {
        let txn = db.begin().await?;
        lock_configuration(&txn).await?;
        let current = Self::find_by_id(&txn, self.id)
            .await?
            .ok_or_else(|| conflict(self.id))?;
        if current.revision != expected_revision {
            return Err(conflict(self.id));
        }
        Self::suppress_legacy_pool(&txn, current.account_id).await?;
        let n = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM passthrough_bindings WHERE id=$1 AND revision=$2",
                [self.id.into(), expected_revision.into()],
            ))
            .await?
            .rows_affected();
        if n != 1 {
            return Err(conflict(self.id));
        }
        txn.commit().await?;
        Ok(())
    }
    pub async fn find_all_filtered(
        db: &impl ConnectionTrait,
        tenant: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        if !(1..=PASSTHROUGH_BINDING_MAX_PAGE_SIZE).contains(&limit) || offset < 0 {
            return Err(DbError::Other(
                "invalid passthrough binding pagination".into(),
            ));
        }
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM passthrough_bindings WHERE ($1::UUID IS NULL OR tenant_id=$1) ORDER BY updated_at DESC,id LIMIT $2 OFFSET $3",
            [tenant.into(),limit.into(),offset.into()])).all(db).await?)
    }
    pub async fn count_filtered(
        db: &impl ConnectionTrait,
        tenant: Option<Uuid>,
    ) -> Result<i64, DbError> {
        Ok(PassthroughBindingCount::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS total FROM passthrough_bindings WHERE ($1::UUID IS NULL OR tenant_id=$1)",[tenant.into()])).one(db).await?.map_or(0,|v|v.total))
    }
    pub fn escaped_search(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(escape_like_pattern)
    }
}

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
fn validate_observation(
    cap: &str,
    model: &str,
    status: &str,
    reason: Option<&str>,
    checked: DateTime<Utc>,
    expires: DateTime<Utc>,
    generation: i64,
) -> Result<(), DbError> {
    if !matches!(cap, "chat_completions" | "responses" | "messages")
        || model.is_empty()
        || model.trim() != model
        || model.len() > 255
        || model.chars().any(char::is_control)
        || !matches!(status, "unknown" | "healthy" | "degraded" | "unhealthy")
        || expires <= checked
        || generation < 0
        || reason.is_some_and(|s| {
            s.is_empty()
                || s.len() > 128
                || !s.chars().all(|c| {
                    c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || matches!(c, '_' | '-' | '.' | ':')
                })
        })
    {
        return Err(DbError::Other(
            "invalid bounded model health observation".into(),
        ));
    }
    Ok(())
}
impl AccountModelHealth {
    pub async fn find(
        db: &impl ConnectionTrait,
        id: Uuid,
        cap: &str,
        model: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT * FROM account_model_health WHERE account_id=$1 AND api_capability=$2 AND model=$3",
            [id.into(),cap.into(),model.into()])).one(db).await?)
    }
    /// Enroll only a requested model, without generating any upstream request.
    /// Configuration changes invalidate old facts, not grant availability.
    pub async fn ensure_snapshot(
        db: &impl ConnectionTrait,
        id: Uuid,
        cap: &str,
        model: &str,
        version: DateTime<Utc>,
    ) -> Result<Option<Self>, DbError> {
        let now = Utc::now();
        validate_observation(
            cap,
            model,
            "unknown",
            None,
            now,
            now + chrono::Duration::seconds(1),
            0,
        )?;
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,r#"
          INSERT INTO account_model_health(account_id,api_capability,model,account_config_version)
          SELECT id,$2,$3,upstream_config_version FROM accounts WHERE id=$1 AND upstream_config_version=$4
          ON CONFLICT(account_id,api_capability,model) DO UPDATE
          SET status=CASE WHEN account_model_health.account_config_version<>EXCLUDED.account_config_version THEN 'unknown' ELSE account_model_health.status END,
              reason_code=CASE WHEN account_model_health.account_config_version<>EXCLUDED.account_config_version THEN NULL ELSE account_model_health.reason_code END,
              checked_at=CASE WHEN account_model_health.account_config_version<>EXCLUDED.account_config_version THEN NULL ELSE account_model_health.checked_at END,
              expires_at=CASE WHEN account_model_health.account_config_version<>EXCLUDED.account_config_version THEN NULL ELSE account_model_health.expires_at END,
              generation=CASE WHEN account_model_health.account_config_version<>EXCLUDED.account_config_version THEN account_model_health.generation+1 ELSE account_model_health.generation END,
              account_config_version=EXCLUDED.account_config_version
          RETURNING *"#,[id.into(),cap.into(),model.into(),version.into()])).one(db).await?)
    }
    pub async fn upsert_probe_if_current(
        db: &impl ConnectionTrait,
        probe: &AccountModelHealthProbe,
    ) -> Result<Option<Self>, DbError> {
        Self::write_probe(db, probe, None).await
    }
    pub async fn upsert_binding_probe_if_current(
        db: &impl ConnectionTrait,
        probe: &AccountModelHealthProbe,
        binding: &PassthroughBinding,
    ) -> Result<Option<Self>, DbError> {
        if probe.account_id != binding.account_id {
            return Err(DbError::Other(
                "probe account does not match binding".into(),
            ));
        }
        Self::write_probe(db, probe, Some(binding)).await
    }
    async fn write_probe(
        db: &impl ConnectionTrait,
        p: &AccountModelHealthProbe,
        b: Option<&PassthroughBinding>,
    ) -> Result<Option<Self>, DbError> {
        validate_observation(
            &p.api_capability,
            &p.model,
            &p.status,
            p.reason_code.as_deref(),
            p.checked_at,
            p.expires_at,
            p.expected_generation,
        )?;
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,r#"
          INSERT INTO account_model_health(account_id,api_capability,model,status,reason_code,checked_at,expires_at,account_config_version,generation)
          SELECT a.id,$2,$3,$4,$5,$6,$7,a.upstream_config_version,$9+1 FROM accounts a
          WHERE a.id=$1 AND a.upstream_config_version=$8 AND a.enabled
            AND $3=ANY(a.models_supported) AND $2=ANY(a.api_capabilities)
            AND EXISTS(SELECT 1 FROM tenants owner WHERE owner.id=a.tenant_id AND owner.status='active')
            AND ($10::UUID IS NULL OR EXISTS(SELECT 1 FROM passthrough_bindings pb JOIN tenants anchor ON anchor.id=pb.tenant_id
                WHERE pb.id=$10 AND pb.revision=$11 AND pb.account_id=a.id AND anchor.status='active'))
            AND ($9=0 OR EXISTS(SELECT 1 FROM account_model_health h WHERE h.account_id=a.id AND h.api_capability=$2 AND h.model=$3 AND h.generation=$9))
          ON CONFLICT(account_id,api_capability,model) DO UPDATE SET status=EXCLUDED.status,reason_code=EXCLUDED.reason_code,
             checked_at=EXCLUDED.checked_at,expires_at=EXCLUDED.expires_at,account_config_version=EXCLUDED.account_config_version,
             generation=account_model_health.generation+1,updated_at=statement_timestamp()
          WHERE account_model_health.generation=$9 RETURNING *"#,
          [p.account_id.into(),p.api_capability.as_str().into(),p.model.as_str().into(),p.status.as_str().into(),p.reason_code.clone().into(),
           p.checked_at.into(),p.expires_at.into(),p.account_config_version.into(),p.expected_generation.into(),b.map(|v|v.id).into(),b.map(|v|v.revision).into()])).one(db).await?)
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn record_runtime_if_current(
        db: &impl ConnectionTrait,
        id: Uuid,
        cap: &str,
        model: &str,
        version: DateTime<Utc>,
        generation: i64,
        status: &str,
        reason: Option<&str>,
        checked: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<Option<Self>, DbError> {
        validate_observation(cap, model, status, reason, checked, expires, generation)?;
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE account_model_health h SET status=$6,reason_code=$7,checked_at=$8,expires_at=$9,updated_at=statement_timestamp(),generation=generation+1 FROM accounts a WHERE h.account_id=$1 AND h.api_capability=$2 AND h.model=$3 AND a.id=h.account_id AND a.upstream_config_version=$4 AND h.account_config_version=$4 AND h.generation=$5 RETURNING h.*",
            [id.into(),cap.into(),model.into(),version.into(),generation.into(),status.into(),reason.into(),checked.into(),expires.into()])).one(db).await?)
    }
    pub async fn reset_for_account(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE account_model_health h SET status='unknown',reason_code=NULL,checked_at=NULL,expires_at=NULL,account_config_version=a.upstream_config_version,generation=h.generation+1,updated_at=statement_timestamp() FROM accounts a WHERE h.account_id=$1 AND a.id=h.account_id RETURNING h.*",[id.into()])).all(db).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_grant_defaults_are_private_and_exclusive() {
        let v = serde_json::json!({"account_id":Uuid::new_v4(),"tenant_id":Uuid::new_v4()});
        let req: CreatePassthroughBindingRequest = serde_json::from_value(v.clone()).unwrap();
        assert!(!req.is_global && !req.pool_enabled);
        for field in ["model", "enabled", "api_capability"] {
            let mut invalid = v.clone();
            invalid[field] = true.into();
            assert!(serde_json::from_value::<CreatePassthroughBindingRequest>(invalid).is_err());
        }
    }
}
