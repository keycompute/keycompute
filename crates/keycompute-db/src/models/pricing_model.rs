use self::pricing_model_scope::{
    PricingGroup, lock_platform_authority, lock_pricing_groups, lock_tenant_authority,
};
use super::query::escape_like_pattern;
use super::tenant_audit_event::{AuditContext, TenantAuditEvent};
use crate::DbError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType, CredentialKind};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

#[path = "pricing_model_scope.rs"]
mod pricing_model_scope;

pub use pricing_model_scope::{PlatformPricingScope, PricingTarget, TenantPricingScope};

/// Explicit pricing scope. Platform rows have no tenant identifier; tenant
/// rows always identify their owning tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingScopeType {
    Platform,
    Tenant,
}

impl PricingScopeType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Tenant => "tenant",
        }
    }
}

impl std::str::FromStr for PricingScopeType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "platform" => Ok(Self::Platform),
            "tenant" => Ok(Self::Tenant),
            other => Err(format!("unknown pricing scope: {other}")),
        }
    }
}

impl std::fmt::Display for PricingScopeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl sea_orm::TryGetable for PricingScopeType {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let value: String = res.try_get_by(idx)?;
        value
            .parse()
            .map_err(|_| sea_orm::TryGetError::Null("invalid pricing scope".into()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid billing dimension: '{0}'. Must be 'node' or 'provideraccount'")]
pub struct BillingDimensionError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingDimension {
    #[serde(rename = "node")]
    Node,
    #[serde(rename = "provideraccount")]
    ProviderAccount,
}

impl BillingDimension {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::ProviderAccount => "provideraccount",
        }
    }
}

impl std::str::FromStr for BillingDimension {
    type Err = BillingDimensionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "node" => Ok(Self::Node),
            "provideraccount" => Ok(Self::ProviderAccount),
            _ => Err(BillingDimensionError(value.to_owned())),
        }
    }
}

impl std::fmt::Display for BillingDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl sea_orm::TryGetable for BillingDimension {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let value: String = res.try_get_by(idx)?;
        value
            .parse()
            .map_err(|_| sea_orm::TryGetError::Null("invalid billing dimension".into()))
    }
}

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PricingModel {
    pub id: Uuid,
    pub scope_type: PricingScopeType,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: String,
    pub input_price_per_1k: BigDecimal,
    pub output_price_per_1k: BigDecimal,
    pub is_default: bool,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct PricingCount {
    total: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePricingRequest {
    pub scope_type: PricingScopeType,
    pub tenant_id: Option<Uuid>,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: Option<String>,
    pub input_price_per_1k: BigDecimal,
    pub output_price_per_1k: BigDecimal,
    pub is_default: Option<bool>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePricingRequest {
    pub input_price_per_1k: Option<BigDecimal>,
    pub output_price_per_1k: Option<BigDecimal>,
    pub effective_until: Option<DateTime<Utc>>,
    pub expected_version: i64,
}

fn scope_matches_target(
    scope_type: PricingScopeType,
    tenant_id: Option<Uuid>,
    target: PricingTarget,
) -> bool {
    scope_type.as_str() == target.scope_type() && tenant_id == target.tenant_id()
}

fn target_from_row(row: &PricingModel) -> Result<PricingTarget, DbError> {
    let target = match row.scope_type {
        PricingScopeType::Platform => PricingTarget::Platform,
        PricingScopeType::Tenant => PricingTarget::Tenant(
            row.tenant_id
                .ok_or_else(|| DbError::Other("tenant pricing row has no tenant".into()))?,
        ),
    };
    target.validate()?;
    Ok(target)
}

pub fn validate_pricing_model_name(value: &str) -> Result<String, DbError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err(DbError::Other(
            "pricing model_name must contain 1-100 characters".into(),
        ));
    }
    Ok(value.to_owned())
}

fn validate_currency(value: &str) -> Result<String, DbError> {
    let value = value.trim().to_ascii_uppercase();
    if value.is_empty()
        || value.len() > 10
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(DbError::Other(
            "pricing currency must contain 1-10 ASCII characters".into(),
        ));
    }
    Ok(value)
}

/// Validate bounded, exact PostgreSQL DECIMAL(20,10) input without expanding
/// an attacker-controlled exponent or overflowing scale arithmetic.
pub fn validate_pricing_amount(value: &BigDecimal) -> Result<(), DbError> {
    let (coefficient, scale) = value.as_bigint_and_scale();
    if !(-4096..=4096).contains(&scale) || coefficient.bits() > 16384 {
        return Err(DbError::Other(
            "pricing amount exponent or coefficient is too large".into(),
        ));
    }
    if value.sign() == bigdecimal::num_bigint::Sign::Minus {
        return Err(DbError::Other("pricing amount must be non-negative".into()));
    }
    let normalized = value.normalized();
    let fractional = i128::from(normalized.fractional_digit_count());
    let integer = (i128::from(normalized.digits()) - fractional).max(0);
    if fractional > 10 || integer > 10 {
        return Err(DbError::Other(
            "pricing amount must fit DECIMAL(20,10)".into(),
        ));
    }
    Ok(())
}

fn validate_price(value: &BigDecimal, _field: &str) -> Result<(), DbError> {
    validate_pricing_amount(value)
}

fn validate_window(
    effective_from: DateTime<Utc>,
    effective_until: Option<DateTime<Utc>>,
) -> Result<(), DbError> {
    if effective_until.is_some_and(|until| until <= effective_from) {
        return Err(DbError::Other(
            "effective_until must be later than effective_from".into(),
        ));
    }
    Ok(())
}

struct ValidatedPriceCreation {
    model_name: String,
    billing_dimension: String,
    currency: String,
    effective_from: DateTime<Utc>,
    effective_until: Option<DateTime<Utc>>,
}

fn validate_create_request(
    req: &CreatePricingRequest,
    target: PricingTarget,
) -> Result<ValidatedPriceCreation, DbError> {
    if !scope_matches_target(req.scope_type, req.tenant_id, target) {
        return Err(DbError::Other(
            "pricing request scope does not match the explicit target".into(),
        ));
    }
    let model_name = validate_pricing_model_name(&req.model_name)?;
    let currency = validate_currency(req.currency.as_deref().unwrap_or("CNY"))?;
    validate_price(&req.input_price_per_1k, "input_price_per_1k")?;
    validate_price(&req.output_price_per_1k, "output_price_per_1k")?;
    let effective_from = req.effective_from.unwrap_or_else(Utc::now);
    validate_window(effective_from, req.effective_until)?;
    Ok(ValidatedPriceCreation {
        model_name,
        billing_dimension: req.billing_dimension.as_str().to_owned(),
        currency,
        effective_from,
        effective_until: req.effective_until,
    })
}

fn target_sql(
    alias: &str,
    _target: PricingTarget,
    scope_placeholder: usize,
    tenant_placeholder: usize,
) -> String {
    format!(
        "{alias}.scope_type=${scope_placeholder} AND {alias}.tenant_id IS NOT DISTINCT FROM ${tenant_placeholder}"
    )
}

fn target_values(target: PricingTarget) -> Vec<sea_orm::Value> {
    vec![target.scope_type().into(), target.tenant_id().into()]
}

fn id_target_values(target: PricingTarget, id: Uuid) -> Vec<sea_orm::Value> {
    vec![
        id.into(),
        target.scope_type().into(),
        target.tenant_id().into(),
    ]
}

fn search_pattern(search: Option<&str>) -> Option<String> {
    search
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("%{}%", escape_like_pattern(&value.trim().to_lowercase())))
}

fn target_group(row: &PricingModel) -> Result<PricingGroup, DbError> {
    PricingGroup::new(
        target_from_row(row)?,
        row.model_name.clone(),
        row.billing_dimension.as_str(),
    )
}

async fn record_change(
    tx: &sea_orm::DatabaseTransaction,
    actor: &AuditContext,
    action: &str,
    before: Option<&PricingModel>,
    after: Option<&PricingModel>,
) -> Result<(), DbError> {
    let row = after
        .or(before)
        .ok_or_else(|| DbError::Other("pricing audit requires a row".into()))?;
    let target = target_from_row(row)?;
    let before_json = before
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| DbError::Other(format!("pricing audit serialization failed: {error}")))?;
    let after_json = after
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| DbError::Other(format!("pricing audit serialization failed: {error}")))?;

    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO pricing_audit_events(actor_user_id,action,pricing_id,scope_type,tenant_id,model_name,billing_dimension,before_state,after_state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        [
            actor.actor_user_id.into(),
            action.into(),
            row.id.into(),
            target.scope_type().into(),
            target.tenant_id().into(),
            row.model_name.as_str().into(),
            row.billing_dimension.as_str().into(),
            before_json.clone().into(),
            after_json.clone().into(),
        ],
    ))
    .await?;

    let scope = if target.tenant_id().is_some() {
        AuditScopeType::Tenant
    } else {
        AuditScopeType::Platform
    };
    TenantAuditEvent::append(
        tx,
        scope,
        target.tenant_id(),
        actor,
        &format!("pricing.{action}"),
        "pricing_model",
        Some(&row.id.to_string()),
        AuditResult::Success,
        serde_json::json!({
            "scope_type": target.scope_type(),
            "tenant_id": target.tenant_id(),
            "operation": action,
            "before": before_json,
            "after": after_json,
        }),
    )
    .await?;
    Ok(())
}

async fn complete_savepoint<T>(
    savepoint: DatabaseTransaction,
    result: Result<T, DbError>,
) -> Result<T, DbError> {
    match result {
        Ok(value) => {
            savepoint.commit().await?;
            Ok(value)
        }
        Err(error) => {
            let _ = savepoint.rollback().await;
            Err(error)
        }
    }
}

impl PricingModel {
    async fn list_target(
        db: &impl ConnectionTrait,
        actor_sql: &str,
        actor_values: Vec<sea_orm::Value>,
        target: PricingTarget,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        let mut values = actor_values;
        let scope_placeholder = values.len() + 1;
        let tenant_placeholder = values.len() + 2;
        let search_placeholder = values.len() + 3;
        let limit_placeholder = values.len() + 4;
        let offset_placeholder = values.len() + 5;
        values.extend(target_values(target));
        values.push(search_pattern(search).into());
        values.push(limit.clamp(1, 100).into());
        values.push(offset.max(0).into());
        let sql = format!(
            "SELECT p.* FROM pricing_models p {actor_sql} WHERE {} AND (${search_placeholder}::TEXT IS NULL OR LOWER(p.model_name) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(p.billing_dimension) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(p.id::TEXT) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(COALESCE(p.tenant_id::TEXT,'platform')) LIKE ${search_placeholder} ESCAPE '\\') ORDER BY p.model_name,p.tenant_id,p.created_at DESC,p.id LIMIT ${limit_placeholder} OFFSET ${offset_placeholder}",
            target_sql("p", target, scope_placeholder, tenant_placeholder)
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(db)
        .await?)
    }

    async fn count_target(
        db: &impl ConnectionTrait,
        actor_sql: &str,
        actor_values: Vec<sea_orm::Value>,
        target: PricingTarget,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let mut values = actor_values;
        let scope_placeholder = values.len() + 1;
        let tenant_placeholder = values.len() + 2;
        let search_placeholder = values.len() + 3;
        values.extend(target_values(target));
        values.push(search_pattern(search).into());
        let sql = format!(
            "SELECT COUNT(*)::BIGINT AS total FROM pricing_models p {actor_sql} WHERE {} AND (${search_placeholder}::TEXT IS NULL OR LOWER(p.model_name) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(p.billing_dimension) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(p.id::TEXT) LIKE ${search_placeholder} ESCAPE '\\' OR LOWER(COALESCE(p.tenant_id::TEXT,'platform')) LIKE ${search_placeholder} ESCAPE '\\')",
            target_sql("p", target, scope_placeholder, tenant_placeholder)
        );
        let row = PricingCount::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?
        .ok_or_else(|| DbError::Other("pricing count query returned no row".into()))?;
        Ok(row.total.max(0))
    }

    async fn detail_target(
        db: &impl ConnectionTrait,
        actor_sql: &str,
        actor_values: Vec<sea_orm::Value>,
        target: PricingTarget,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        let mut values = actor_values;
        let scope_placeholder = values.len() + 1;
        let tenant_placeholder = values.len() + 2;
        let id_placeholder = values.len() + 3;
        values.extend(target_values(target));
        values.push(id.into());
        let sql = format!(
            "SELECT p.* FROM pricing_models p {actor_sql} WHERE {} AND p.id=${id_placeholder}",
            target_sql("p", target, scope_placeholder, tenant_placeholder)
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?)
    }

    pub async fn find_platform_filtered(
        db: &impl ConnectionTrait,
        scope: PlatformPricingScope,
        target: PricingTarget,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        target.validate()?;
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT platform pricing scope required".into()));
        }
        Self::list_target(
            db,
            "JOIN users actor ON actor.id=$1 AND actor.status='active' AND actor.platform_role='root' AND actor.token_version=$2",
            vec![scope.actor_user_id().into(), scope.token_version().into()],
            target,
            search,
            limit,
            offset,
        )
        .await
    }

    pub async fn count_platform_filtered(
        db: &impl ConnectionTrait,
        scope: PlatformPricingScope,
        target: PricingTarget,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        target.validate()?;
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT platform pricing scope required".into()));
        }
        Self::count_target(
            db,
            "JOIN users actor ON actor.id=$1 AND actor.status='active' AND actor.platform_role='root' AND actor.token_version=$2",
            vec![scope.actor_user_id().into(), scope.token_version().into()],
            target,
            search,
        )
        .await
    }

    pub async fn find_platform_by_id(
        db: &impl ConnectionTrait,
        scope: PlatformPricingScope,
        target: PricingTarget,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        target.validate()?;
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT platform pricing scope required".into()));
        }
        Self::detail_target(
            db,
            "JOIN users actor ON actor.id=$1 AND actor.status='active' AND actor.platform_role='root' AND actor.token_version=$2",
            vec![scope.actor_user_id().into(), scope.token_version().into()],
            target,
            id,
        )
        .await
    }

    pub async fn resolve_platform_target(
        db: &impl ConnectionTrait,
        scope: PlatformPricingScope,
        id: Uuid,
    ) -> Result<Option<PricingTarget>, DbError> {
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT platform pricing scope required".into()));
        }
        #[derive(Debug, FromQueryResult)]
        struct TargetRow {
            scope_type: PricingScopeType,
            tenant_id: Option<Uuid>,
        }
        let row = TargetRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT p.scope_type,p.tenant_id FROM pricing_models p JOIN users actor ON actor.id=$1 AND actor.status='active' AND actor.platform_role='root' AND actor.token_version=$2 WHERE p.id=$3",
            [scope.actor_user_id().into(), scope.token_version().into(), id.into()],
        ))
        .one(db)
        .await?;
        row.map(|row| -> Result<PricingTarget, DbError> {
            match row.scope_type {
                PricingScopeType::Platform => Ok(PricingTarget::Platform),
                PricingScopeType::Tenant => {
                    let tenant_id = row
                        .tenant_id
                        .ok_or_else(|| DbError::Other("tenant pricing row has no tenant".into()))?;
                    let target = PricingTarget::Tenant(tenant_id);
                    target.validate()?;
                    Ok(target)
                }
            }
        })
        .transpose()
    }

    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantPricingScope,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT tenant pricing scope required".into()));
        }
        Self::list_target(
            db,
            "JOIN tenants t ON t.id=$1 AND t.status='active' AND t.authz_version=$4 JOIN users actor ON actor.id=$2 AND actor.status='active' AND actor.token_version=$3 JOIN tenant_memberships m ON m.tenant_id=t.id AND m.user_id=actor.id AND m.status='active' AND m.tenant_role='admin' AND m.authz_version=$5",
            vec![scope.tenant_id().into(), scope.actor_user_id().into(), scope.token_version().into(), scope.tenant_authz_version().into(), scope.membership_authz_version().into()],
            PricingTarget::Tenant(scope.tenant_id()),
            search,
            limit,
            offset,
        )
        .await
    }

    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantPricingScope,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT tenant pricing scope required".into()));
        }
        Self::count_target(
            db,
            "JOIN tenants t ON t.id=$1 AND t.status='active' AND t.authz_version=$4 JOIN users actor ON actor.id=$2 AND actor.status='active' AND actor.token_version=$3 JOIN tenant_memberships m ON m.tenant_id=t.id AND m.user_id=actor.id AND m.status='active' AND m.tenant_role='admin' AND m.authz_version=$5",
            vec![scope.tenant_id().into(), scope.actor_user_id().into(), scope.token_version().into(), scope.tenant_authz_version().into(), scope.membership_authz_version().into()],
            PricingTarget::Tenant(scope.tenant_id()),
            search,
        )
        .await
    }

    pub async fn find_in_tenant_by_id(
        db: &impl ConnectionTrait,
        scope: TenantPricingScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        if scope.credential_kind() != CredentialKind::Jwt {
            return Err(DbError::Other("JWT tenant pricing scope required".into()));
        }
        Self::detail_target(
            db,
            "JOIN tenants t ON t.id=$1 AND t.status='active' AND t.authz_version=$4 JOIN users actor ON actor.id=$2 AND actor.status='active' AND actor.token_version=$3 JOIN tenant_memberships m ON m.tenant_id=t.id AND m.user_id=actor.id AND m.status='active' AND m.tenant_role='admin' AND m.authz_version=$5",
            vec![scope.tenant_id().into(), scope.actor_user_id().into(), scope.token_version().into(), scope.tenant_authz_version().into(), scope.membership_authz_version().into()],
            PricingTarget::Tenant(scope.tenant_id()),
            id,
        )
        .await
    }

    /// Runtime-only lookup. It is not a console authorization check.
    pub async fn find_effective_for_runtime(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        model_name: &str,
        billing_dimension: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT p.* FROM pricing_models p WHERE p.model_name=$1 AND p.billing_dimension=$2 AND p.effective_from<=NOW() AND (p.effective_until IS NULL OR p.effective_until>NOW()) AND ((p.scope_type='tenant' AND p.tenant_id=$3) OR (p.scope_type='platform' AND p.tenant_id IS NULL AND p.is_default=TRUE)) ORDER BY CASE WHEN p.scope_type='tenant' THEN 0 ELSE 1 END,p.created_at DESC,p.id LIMIT 1",
            [model_name.into(), billing_dimension.into(), tenant_id.into()],
        ))
        .one(db)
        .await?)
    }

    /// Primary-authoritative cache identity, including future validity changes.
    /// Accepted request snapshots stay immutable; subsequent reads cannot reuse
    /// an older tenant/platform price or cross a scheduled validity boundary.
    pub async fn runtime_cache_revision(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        model: &str,
        dimension: &str,
    ) -> Result<String, DbError> {
        let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            r#"SELECT
                COALESCE((SELECT version FROM pricing_cache_revisions WHERE scope_type='platform' AND tenant_id IS NULL),0)::BIGINT AS platform_version,
                COALESCE((SELECT version FROM pricing_cache_revisions WHERE scope_type='tenant' AND tenant_id=$1),0)::BIGINT AS tenant_version,
                (SELECT extract(epoch FROM MIN(boundary))::TEXT FROM pricing_models p
                    CROSS JOIN LATERAL (VALUES(p.effective_from),(p.effective_until)) validity(boundary)
                    WHERE boundary > statement_timestamp() AND p.model_name=$2
                      AND ((p.scope_type='tenant' AND p.tenant_id=$1 AND p.billing_dimension=$3)
                        OR (p.scope_type='platform' AND p.tenant_id IS NULL AND p.is_default))) AS next_boundary"#,
            [tenant_id.into(),model.into(),dimension.into()],
        )).await?.ok_or_else(|| DbError::Other("pricing cache revision unavailable".into()))?;
        let platform: i64 = row.try_get("", "platform_version")?;
        let tenant: i64 = row.try_get("", "tenant_version")?;
        let boundary: Option<String> = row.try_get("", "next_boundary")?;
        Ok(format!(
            "p{platform}:t{tenant}:b{}",
            boundary.as_deref().unwrap_or("none")
        ))
    }

    /// Runtime-only platform defaults for cache warmup/fallback.
    pub async fn find_runtime_defaults(db: &impl ConnectionTrait) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT p.* FROM pricing_models p WHERE p.scope_type='platform' AND p.tenant_id IS NULL AND p.is_default=TRUE AND p.effective_from<=NOW() AND (p.effective_until IS NULL OR p.effective_until>NOW()) ORDER BY p.model_name,p.billing_dimension,p.id",
            [],
        ))
        .all(db)
        .await?)
    }

    pub async fn create_platform(
        tx: &sea_orm::DatabaseTransaction,
        scope: PlatformPricingScope,
        target: PricingTarget,
        req: &CreatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::create_authorized(
            &savepoint,
            target,
            AuthorizationScope::Platform(scope),
            req,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    pub async fn create_in_tenant(
        tx: &sea_orm::DatabaseTransaction,
        scope: TenantPricingScope,
        req: &CreatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::create_authorized(
            &savepoint,
            PricingTarget::Tenant(scope.tenant_id()),
            AuthorizationScope::Tenant(scope),
            req,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    async fn create_authorized(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        authorization: AuthorizationScope,
        req: &CreatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let authorized_actor = authorization.lock(tx, target, actor, true).await?;
        let ValidatedPriceCreation {
            model_name,
            billing_dimension,
            currency,
            effective_from,
            effective_until,
        } = validate_create_request(req, target)?;
        let group = PricingGroup::new(target, model_name.clone(), billing_dimension.clone())?;
        if req.is_default.unwrap_or(false) {
            lock_pricing_groups(tx, std::slice::from_ref(&group)).await?;
            let cleared = Self::clear_default_for_group(tx, &group).await?;
            for (before, after) in cleared {
                record_change(
                    tx,
                    &authorized_actor,
                    "make_default",
                    Some(&before),
                    Some(&after),
                )
                .await?;
            }
        }
        let row = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO pricing_models(scope_type,tenant_id,model_name,billing_dimension,currency,input_price_per_1k,output_price_per_1k,is_default,effective_from,effective_until) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
            [
                target.scope_type().into(),
                target.tenant_id().into(),
                model_name.into(),
                billing_dimension.into(),
                currency.into(),
                req.input_price_per_1k.clone().into(),
                req.output_price_per_1k.clone().into(),
                req.is_default.unwrap_or(false).into(),
                effective_from.into(),
                effective_until.into(),
            ],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::Other("pricing insert returned no row".into()))?;
        record_change(tx, &authorized_actor, "create", None, Some(&row)).await?;
        Ok(row)
    }

    pub async fn update_platform(
        tx: &sea_orm::DatabaseTransaction,
        scope: PlatformPricingScope,
        target: PricingTarget,
        id: Uuid,
        req: &UpdatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::update_authorized(
            &savepoint,
            target,
            AuthorizationScope::Platform(scope),
            id,
            req,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    pub async fn update_in_tenant(
        tx: &sea_orm::DatabaseTransaction,
        scope: TenantPricingScope,
        id: Uuid,
        req: &UpdatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::update_authorized(
            &savepoint,
            PricingTarget::Tenant(scope.tenant_id()),
            AuthorizationScope::Tenant(scope),
            id,
            req,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    async fn update_authorized(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        authorization: AuthorizationScope,
        id: Uuid,
        req: &UpdatePricingRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        if req.expected_version <= 0
            || (req.input_price_per_1k.is_none()
                && req.output_price_per_1k.is_none()
                && req.effective_until.is_none())
        {
            return Err(DbError::Other(
                "pricing update requires a positive version and at least one field".into(),
            ));
        }
        if let Some(value) = &req.input_price_per_1k {
            validate_price(value, "input_price_per_1k")?;
        }
        if let Some(value) = &req.output_price_per_1k {
            validate_price(value, "output_price_per_1k")?;
        }
        let authorized_actor = authorization.lock(tx, target, actor, true).await?;
        let (model_name, billing_dimension) = Self::find_group_metadata(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        let group = PricingGroup::new(target, model_name, billing_dimension)?;
        lock_pricing_groups(tx, std::slice::from_ref(&group)).await?;
        let before = Self::find_locked_in_target(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        if let Some(until) = req.effective_until {
            validate_window(before.effective_from, Some(until))?;
        }
        let values = vec![
            req.input_price_per_1k.clone().into(),
            req.output_price_per_1k.clone().into(),
            req.effective_until.into(),
            id.into(),
            req.expected_version.into(),
            target.scope_type().into(),
            target.tenant_id().into(),
        ];
        let updated = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE pricing_models SET input_price_per_1k=COALESCE($1,input_price_per_1k),output_price_per_1k=COALESCE($2,output_price_per_1k),effective_until=COALESCE($3,effective_until),version=version+1,updated_at=NOW() WHERE id=$4 AND version=$5 AND {} RETURNING *",
                target_sql("pricing_models", target, 6, 7)
            ),
            values,
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::OptimisticConflict {
            entity: "pricing model".into(),
            id: id.to_string(),
        })?;
        record_change(
            tx,
            &authorized_actor,
            "update",
            Some(&before),
            Some(&updated),
        )
        .await?;
        Ok(updated)
    }

    pub async fn delete_platform(
        tx: &sea_orm::DatabaseTransaction,
        scope: PlatformPricingScope,
        target: PricingTarget,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<(), DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::delete_authorized(
            &savepoint,
            target,
            AuthorizationScope::Platform(scope),
            id,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    pub async fn delete_in_tenant(
        tx: &sea_orm::DatabaseTransaction,
        scope: TenantPricingScope,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<(), DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::delete_authorized(
            &savepoint,
            PricingTarget::Tenant(scope.tenant_id()),
            AuthorizationScope::Tenant(scope),
            id,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    async fn delete_authorized(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        authorization: AuthorizationScope,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<(), DbError> {
        let authorized_actor = authorization.lock(tx, target, actor, false).await?;
        let (model_name, billing_dimension) = Self::find_group_metadata(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        let group = PricingGroup::new(target, model_name, billing_dimension)?;
        lock_pricing_groups(tx, std::slice::from_ref(&group)).await?;
        let before = Self::find_locked_in_target(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        if target == PricingTarget::Platform {
            return Err(DbError::Other(
                "platform pricing rows cannot be deleted".into(),
            ));
        }
        let deleted = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "DELETE FROM pricing_models WHERE id=$1 AND {}",
                    target_sql("pricing_models", target, 2, 3)
                ),
                id_target_values(target, id),
            ))
            .await?;
        if deleted.rows_affected() != 1 {
            return Err(DbError::Other("pricing delete changed no row".into()));
        }
        record_change(tx, &authorized_actor, "delete", Some(&before), None).await
    }

    pub async fn make_default_platform(
        tx: &sea_orm::DatabaseTransaction,
        scope: PlatformPricingScope,
        target: PricingTarget,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::make_default_authorized(
            &savepoint,
            target,
            AuthorizationScope::Platform(scope),
            id,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    pub async fn make_default_in_tenant(
        tx: &sea_orm::DatabaseTransaction,
        scope: TenantPricingScope,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::make_default_authorized(
            &savepoint,
            PricingTarget::Tenant(scope.tenant_id()),
            AuthorizationScope::Tenant(scope),
            id,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    async fn make_default_authorized(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        authorization: AuthorizationScope,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let authorized_actor = authorization.lock(tx, target, actor, true).await?;
        let (model_name, billing_dimension) = Self::find_group_metadata(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        let group = PricingGroup::new(target, model_name, billing_dimension)?;
        lock_pricing_groups(tx, std::slice::from_ref(&group)).await?;
        Self::make_default_locked(tx, target, id, &authorized_actor).await
    }

    async fn make_default_locked(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        id: Uuid,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        let before = Self::find_locked_in_target(tx, target, id)
            .await?
            .ok_or_else(|| DbError::not_found("pricing model", id))?;
        if before.is_default {
            return Ok(before);
        }
        let cleared = Self::clear_default_for_group(
            tx,
            &PricingGroup::new(
                target,
                before.model_name.clone(),
                before.billing_dimension.as_str(),
            )?,
        )
        .await?;
        let updated = Self::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "UPDATE pricing_models SET is_default=TRUE,version=version+1,updated_at=NOW() WHERE id=$1 AND {} RETURNING *",
                target_sql("pricing_models", target, 2, 3)
                ),
            id_target_values(target, id),
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::Other("pricing default update returned no row".into()))?;
        for (before, after) in cleared {
            record_change(tx, actor, "make_default", Some(&before), Some(&after)).await?;
        }
        record_change(tx, actor, "make_default", Some(&before), Some(&updated)).await?;
        Ok(updated)
    }

    pub async fn batch_make_defaults_platform(
        tx: &sea_orm::DatabaseTransaction,
        scope: PlatformPricingScope,
        target: PricingTarget,
        ids: &[Uuid],
        actor: &AuditContext,
    ) -> Result<Vec<Self>, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::batch_make_defaults_authorized(
            &savepoint,
            target,
            AuthorizationScope::Platform(scope),
            ids,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    pub async fn batch_make_defaults_in_tenant(
        tx: &sea_orm::DatabaseTransaction,
        scope: TenantPricingScope,
        ids: &[Uuid],
        actor: &AuditContext,
    ) -> Result<Vec<Self>, DbError> {
        let savepoint = tx.begin().await?;
        let result = Self::batch_make_defaults_authorized(
            &savepoint,
            PricingTarget::Tenant(scope.tenant_id()),
            AuthorizationScope::Tenant(scope),
            ids,
            actor,
        )
        .await;
        complete_savepoint(savepoint, result).await
    }

    async fn batch_make_defaults_authorized(
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        authorization: AuthorizationScope,
        ids: &[Uuid],
        actor: &AuditContext,
    ) -> Result<Vec<Self>, DbError> {
        if ids.is_empty() {
            return Err(DbError::Other("pricing default selection is empty".into()));
        }
        const MAX_BATCH_DEFAULTS: usize = 100;
        if ids.len() > MAX_BATCH_DEFAULTS {
            return Err(DbError::Other(format!(
                "pricing default selection exceeds {MAX_BATCH_DEFAULTS} items"
            )));
        }
        let authorized_actor = authorization.lock(tx, target, actor, true).await?;
        let mut unique_ids = ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        let rows = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT * FROM pricing_models WHERE id=ANY($1) AND {} ORDER BY id",
                target_sql("pricing_models", target, 2, 3)
            ),
            [vec![unique_ids.clone().into()], target_values(target)].concat(),
        ))
        .all(tx)
        .await?;
        if rows.len() != unique_ids.len() {
            return Err(DbError::NotFound {
                entity: "pricing model".into(),
                id: "one or more selected IDs".into(),
            });
        }
        let mut groups = BTreeMap::<String, (PricingGroup, Uuid)>::new();
        for row in &rows {
            let group = target_group(row)?;
            if groups.insert(group.lock_key(), (group, row.id)).is_some() {
                return Err(DbError::Other(
                    "batch defaults contain conflicting selections in one pricing group".into(),
                ));
            }
        }
        let all_groups = groups
            .values()
            .map(|(group, _)| group.clone())
            .collect::<Vec<_>>();
        lock_pricing_groups(tx, &all_groups).await?;
        for id in &unique_ids {
            Self::find_locked_in_target(tx, target, *id)
                .await?
                .ok_or_else(|| DbError::NotFound {
                    entity: "pricing model".into(),
                    id: id.to_string(),
                })?;
        }
        let mut result = Vec::with_capacity(groups.len());
        for (_, (_, id)) in groups {
            result.push(Self::make_default_locked(tx, target, id, &authorized_actor).await?);
        }
        Ok(result)
    }

    async fn find_group_metadata(
        db: &impl ConnectionTrait,
        target: PricingTarget,
        id: Uuid,
    ) -> Result<Option<(String, String)>, DbError> {
        #[derive(Debug, FromQueryResult)]
        struct GroupMetadata {
            model_name: String,
            billing_dimension: String,
        }
        Ok(
            GroupMetadata::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT model_name,billing_dimension FROM pricing_models WHERE id=$1 AND {}",
                    target_sql("pricing_models", target, 2, 3)
                ),
                id_target_values(target, id),
            ))
            .one(db)
            .await?
            .map(|row| (row.model_name, row.billing_dimension)),
        )
    }

    async fn find_locked_in_target(
        db: &impl ConnectionTrait,
        target: PricingTarget,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT * FROM pricing_models WHERE id=$1 AND {} FOR UPDATE",
                target_sql("pricing_models", target, 2, 3)
            ),
            id_target_values(target, id),
        ))
        .one(db)
        .await?)
    }

    async fn clear_default_for_group(
        tx: &sea_orm::DatabaseTransaction,
        group: &PricingGroup,
    ) -> Result<Vec<(Self, Self)>, DbError> {
        let target = group.target;
        let target_sql = "scope_type=$3 AND tenant_id IS NOT DISTINCT FROM $4";
        let target_values = target_values(target);
        let before_rows = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT * FROM pricing_models WHERE model_name=$1 AND billing_dimension=$2 AND is_default=TRUE AND {} FOR UPDATE",
                target_sql
            ),
            [
                vec![
                    group.model_name.as_str().into(),
                    group.billing_dimension.as_str().into(),
                ],
                target_values.clone(),
            ]
            .concat(),
        ))
        .all(tx)
        .await?;
        let after_rows = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE pricing_models SET is_default=FALSE,version=version+1,updated_at=NOW() WHERE model_name=$1 AND billing_dimension=$2 AND is_default=TRUE AND {} RETURNING *",
                target_sql
            ),
            [
                vec![
                    group.model_name.as_str().into(),
                    group.billing_dimension.as_str().into(),
                ],
                target_values,
            ]
            .concat(),
        ))
        .all(tx)
        .await?;
        let before_by_id = before_rows
            .into_iter()
            .map(|row| (row.id, row))
            .collect::<HashMap<_, _>>();
        after_rows
            .into_iter()
            .map(|after| {
                let before = before_by_id
                    .get(&after.id)
                    .cloned()
                    .ok_or_else(|| DbError::Other("default audit snapshot disappeared".into()))?;
                Ok((before, after))
            })
            .collect()
    }

    pub fn is_effective(&self) -> bool {
        let now = Utc::now();
        self.effective_from <= now
            && self
                .effective_until
                .is_none_or(|effective_until| effective_until > now)
    }

    /// Startup-only initialization. This is not a console authorization path.
    pub async fn init_default_pricing(db: &impl ConnectionTrait) -> Result<(), DbError> {
        let input_price_per_1k: BigDecimal = "0.1".parse().unwrap_or_default();
        let output_price_per_1k: BigDecimal = "0.3".parse().unwrap_or_default();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO pricing_models(scope_type,tenant_id,model_name,billing_dimension,currency,input_price_per_1k,output_price_per_1k,is_default) VALUES('platform',NULL,$1,$2,$3,$4,$5,TRUE) ON CONFLICT (tenant_id,model_name,billing_dimension) DO NOTHING",
            [
                "model-empty".into(),
                BillingDimension::ProviderAccount.as_str().into(),
                "CNY".into(),
                input_price_per_1k.into(),
                output_price_per_1k.into(),
            ],
        ))
        .await?;
        Ok(())
    }
}

enum AuthorizationScope {
    Platform(PlatformPricingScope),
    Tenant(TenantPricingScope),
}

impl AuthorizationScope {
    async fn lock(
        &self,
        tx: &sea_orm::DatabaseTransaction,
        target: PricingTarget,
        actor: &AuditContext,
        require_active_tenant: bool,
    ) -> Result<AuditContext, DbError> {
        match self {
            Self::Platform(scope) => {
                lock_platform_authority(tx, *scope, target, actor, require_active_tenant).await
            }
            Self::Tenant(scope) => {
                let _ = require_active_tenant;
                lock_tenant_authority(tx, *scope, actor).await
            }
        }
    }
}

#[cfg(test)]
mod scoped_amount_tests {
    use super::*;
    #[test]
    fn exact_prices_are_bounded_before_normalization() {
        for value in [
            "0",
            "0.0000000001",
            "9999999999.9999999999",
            "1.230000000000",
            "1e-10",
        ] {
            assert!(
                validate_pricing_amount(&value.parse().unwrap()).is_ok(),
                "{value}"
            );
        }
        for value in ["-0.1", "0.00000000001", "10000000000", "1e4097", "1e-4097"] {
            assert!(
                validate_pricing_amount(&value.parse().unwrap()).is_err(),
                "{value}"
            );
        }
        for exponent in [i64::MIN, i64::MAX] {
            let value = BigDecimal::new(bigdecimal::num_bigint::BigInt::from(10), exponent);
            assert!(validate_pricing_amount(&value).is_err());
        }
    }
}
