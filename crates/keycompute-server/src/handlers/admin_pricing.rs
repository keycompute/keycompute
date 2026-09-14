//! 定价管理处理器
//!
//! 处理需要 Admin 权限的定价管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use keycompute_db::models::pricing_model::{
    CreatePricingRequest, GLOBAL_DEFAULT_TENANT_ID, PricingModel, UpdatePricingRequest,
};
use keycompute_db::models::tenant::Tenant;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn validate_active_pricing_tenant_status(status: Option<&str>, tenant_id: Uuid) -> Result<()> {
    match status {
        Some("active") => Ok(()),
        Some(_) => Err(ApiError::Conflict(
            "The target tenant is inactive and cannot receive pricing models".to_string(),
        )),
        None => Err(ApiError::NotFound(format!("Tenant not found: {tenant_id}"))),
    }
}

async fn ensure_active_pricing_tenant(db: &impl ConnectionTrait, tenant_id: Uuid) -> Result<()> {
    let tenant = Tenant::find_by_id_for_update(db, tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing tenant: {e}")))?
        .map(|tenant| tenant.status);
    validate_active_pricing_tenant_status(tenant.as_deref(), tenant_id)
}

async fn set_pricing_default_in_transaction(
    db: &impl ConnectionTrait,
    actor_user_id: Uuid,
    pricing_id: Uuid,
) -> Result<PricingModel> {
    let target = PricingModel::find_by_id_for_update(db, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {pricing_id}")))?;
    // Retrying an already successful command must not bump the version or add
    // a duplicate audit event. This also keeps the endpoint safe for clients
    // that retry after a lost response.
    if target.is_default {
        return Ok(target);
    }
    let before = target.clone();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE pricing_models SET is_default = FALSE, version = version + 1, updated_at = NOW() WHERE model_name = $1 AND billing_dimension = $2 AND tenant_id = $3 AND is_default = TRUE AND id <> $4",
        [
            target.model_name.as_str().into(),
            target.billing_dimension.as_str().into(),
            target.tenant_id.into(),
            pricing_id.into(),
        ],
    ))
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to clear previous default: {e}")))?;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE pricing_models SET is_default = TRUE, version = version + 1, updated_at = NOW() WHERE id = $1",
        [pricing_id.into()],
    ))
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to set pricing default: {e}")))?;
    let updated = PricingModel::find_by_id(db, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to reload pricing: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {pricing_id}")))?;
    record_pricing_audit(
        db,
        actor_user_id,
        "make_default",
        Some(&before),
        Some(&updated),
    )
    .await?;
    Ok(updated)
}

/// 将某个定价设为默认
///
/// POST /api/v1/pricing/{id}/make-default
pub async fn make_pricing_default(
    auth: AuthExtractor,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin default pricing update: {e}")))?;
    let updated = set_pricing_default_in_transaction(&txn, auth.user_id, pricing_id).await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit default pricing update: {e}")))?;

    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing set as default",
        "pricing_id": pricing_id,
        "version": updated.version,
    })))
}

/// 定价信息
#[derive(Debug, Serialize)]
pub struct PricingInfo {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub model_name: String,
    pub billing_dimension: String,
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
    pub version: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct PricingListQueryParams {
    pub search: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PricingListResponse {
    pub pricing: Vec<PricingInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 创建定价请求（管理员）
#[derive(Debug, Deserialize)]
pub struct CreatePricingAdminRequest {
    /// 模型名称
    pub model_name: String,
    /// 计费维度: node 或 provideraccount
    #[serde(rename = "billing_dimension")]
    pub billing_dimension: String,
    /// 租户ID（管理接口只允许显式的非 nil 租户；全局默认由系统初始化）
    pub tenant_id: Option<Uuid>,
    /// 货币（默认 CNY）
    #[serde(default = "default_currency")]
    pub currency: String,
    /// 输入价格（每 1k tokens）
    pub input_price_per_1k: String,
    /// 输出价格（每 1k tokens）
    pub output_price_per_1k: String,
    /// 是否为默认定价
    #[serde(default)]
    pub is_default: bool,
    /// 生效时间（可选）
    pub effective_from: Option<String>,
    /// 失效时间（可选）
    pub effective_until: Option<String>,
}

fn default_currency() -> String {
    "CNY".to_string()
}

/// 更新定价请求（管理员）
#[derive(Debug, Deserialize)]
pub struct UpdatePricingAdminRequest {
    /// 输入价格（每 1k tokens）
    pub input_price_per_1k: Option<String>,
    /// 输出价格（每 1k tokens）
    pub output_price_per_1k: Option<String>,
    /// 失效时间
    pub effective_until: Option<String>,
    /// 客户端读取到的版本号，用于防止覆盖并发修改。
    pub expected_version: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SetDefaultPricingAdminRequest {
    #[serde(default, alias = "pricing_ids")]
    pub model_ids: Vec<Uuid>,
}

fn parse_price(raw: &str, field: &str) -> Result<BigDecimal> {
    let value = raw
        .trim()
        .parse::<BigDecimal>()
        .map_err(|_| ApiError::BadRequest(format!("Invalid {field}: expected a decimal number")))?;
    if value < 0 {
        return Err(ApiError::BadRequest(format!(
            "{field} must be non-negative"
        )));
    }
    // pricing_models stores prices as DECIMAL(20,10). PostgreSQL rounds
    // excess fractional digits on assignment, so reject them here instead of
    // silently charging a value different from the one the administrator sent.
    let normalized = value.normalized();
    let fractional_digits = normalized.fractional_digit_count();
    let integer_digits = (normalized.digits() as i64 - fractional_digits).max(0);
    if fractional_digits > 10 || integer_digits > 10 {
        return Err(ApiError::BadRequest(format!(
            "{field} must fit DECIMAL(20,10) (at most 10 integer and 10 fractional digits)"
        )));
    }
    Ok(value)
}

fn parse_timestamp(raw: Option<&String>, field: &str) -> Result<Option<DateTime<Utc>>> {
    raw.map(|value| {
        DateTime::parse_from_rfc3339(value.trim())
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .map_err(|_| ApiError::BadRequest(format!("Invalid {field}: expected RFC3339")))
    })
    .transpose()
}

fn validate_model_name(model_name: &str) -> Result<String> {
    let model_name = model_name.trim();
    if model_name.is_empty()
        || model_name.chars().count() > 100
        || model_name.chars().any(|character| character.is_control())
    {
        return Err(ApiError::BadRequest(
            "model_name must contain 1-100 characters".to_string(),
        ));
    }
    Ok(model_name.to_string())
}

fn validate_currency(currency: &str) -> Result<String> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.is_empty()
        || currency.len() > 10
        || !currency
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(ApiError::BadRequest(
            "currency must contain 1-10 characters".to_string(),
        ));
    }
    Ok(currency)
}

async fn record_pricing_audit(
    db: &impl ConnectionTrait,
    actor_user_id: Uuid,
    action: &str,
    before: Option<&PricingModel>,
    after: Option<&PricingModel>,
) -> Result<()> {
    let row = after.or(before).ok_or_else(|| {
        ApiError::Internal("Pricing audit requires a before or after state".to_string())
    })?;
    let before_json = before
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("Failed to serialize pricing audit: {e}")))?;
    let after_json = after
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("Failed to serialize pricing audit: {e}")))?;
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        INSERT INTO pricing_audit_events
            (actor_user_id, action, pricing_id, tenant_id, model_name,
             billing_dimension, before_state, after_state)
        VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)
        "#,
        [
            actor_user_id.into(),
            action.into(),
            row.id.into(),
            row.tenant_id.into(),
            row.model_name.as_str().into(),
            row.billing_dimension.as_str().into(),
            before_json.into(),
            after_json.into(),
        ],
    );
    db.execute(stmt)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to record pricing audit: {e}")))?;
    Ok(())
}

fn map_pricing_db_error(error: keycompute_db::DbError, operation: &str) -> ApiError {
    if error.is_duplicate() {
        ApiError::Conflict("Pricing already exists for this tenant/model/dimension".to_string())
    } else if error.is_optimistic_conflict() {
        ApiError::Conflict("Pricing was modified by another request; reload and retry".to_string())
    } else if error.to_string().contains("numeric field overflow")
        || error.to_string().contains("violates check constraint")
    {
        ApiError::BadRequest("Pricing values exceed the supported database range".to_string())
    } else {
        ApiError::Internal(format!("Failed to {operation}: {error}"))
    }
}

/// 列出所有定价
///
/// GET /api/v1/pricing
///
/// Admin 可以看到所有租户的定价，普通用户只能看到自己的
pub async fn list_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<PricingListQueryParams>,
) -> Result<Json<PricingListResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let writer = pool.write_conn();
    let pricing_models =
        PricingModel::find_all_filtered(writer, params.search.as_deref(), page_size, offset)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to query pricing: {}", e)))?;
    let total = PricingModel::count_all_filtered(writer, params.search.as_deref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count pricing: {}", e)))?;

    let pricing_list: Vec<PricingInfo> = pricing_models
        .into_iter()
        .map(|p| {
            let is_effective = p.is_effective();
            PricingInfo {
                id: p.id,
                tenant_id: p.tenant_id,
                model_name: p.model_name,
                billing_dimension: p.billing_dimension.as_str().to_string(),
                currency: p.currency,
                input_price_per_1k: p.input_price_per_1k.to_string(),
                output_price_per_1k: p.output_price_per_1k.to_string(),
                is_default: p.is_default,
                is_effective,
                effective_from: p.effective_from.to_rfc3339(),
                effective_until: p.effective_until.map(|t| t.to_rfc3339()),
                created_at: p.created_at.to_rfc3339(),
                version: p.version,
            }
        })
        .collect();

    Ok(Json(PricingListResponse {
        pricing: pricing_list,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

/// 创建定价
///
/// POST /api/v1/pricing
pub async fn create_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 校验计费维度必须是合法值
    let billing_dimension: keycompute_db::models::pricing_model::BillingDimension =
        req.billing_dimension.parse().map_err(
            |e: keycompute_db::models::pricing_model::BillingDimensionError| {
                ApiError::BadRequest(format!(
                    "Invalid billing dimension: '{}'. Must be 'node' or 'provideraccount'",
                    e.0
                ))
            },
        )?;

    let input_price = parse_price(&req.input_price_per_1k, "input_price_per_1k")?;
    let output_price = parse_price(&req.output_price_per_1k, "output_price_per_1k")?;
    let model_name = validate_model_name(&req.model_name)?;
    let currency = validate_currency(&req.currency)?;
    let effective_from =
        parse_timestamp(req.effective_from.as_ref(), "effective_from")?.unwrap_or_else(Utc::now);
    let effective_until = parse_timestamp(req.effective_until.as_ref(), "effective_until")?;
    if let Some(until) = effective_until
        && until <= effective_from
    {
        return Err(ApiError::BadRequest(
            "effective_until must be later than effective_from".to_string(),
        ));
    }

    // 禁止创建新的全局默认定价（tenant_id 不能为 None 或 GLOBAL_DEFAULT_TENANT_ID）
    if req.tenant_id.is_none() || req.tenant_id == Some(GLOBAL_DEFAULT_TENANT_ID) {
        tracing::warn!(
            tenant_id = ?req.tenant_id,
            model_name = %req.model_name,
            "Attempted to create new global default pricing, which is not allowed"
        );
        return Err(ApiError::BadRequest(
            "Cannot create new global default pricing. Global defaults are managed by system initialization only.".to_string(),
        ));
    }

    // Pricing is a tenant-owned configuration resource. Do not allow an
    // administrator to attach a new pricing model to a closed or missing
    // tenant, even when the caller itself is the active system tenant.
    let tenant_id = req
        .tenant_id
        .expect("global pricing IDs are rejected immediately above");
    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin pricing creation: {e}")))?;
    ensure_active_pricing_tenant(&txn, tenant_id).await?;

    // A create request may make the new row the default. Clear only the
    // matching tenant/model/dimension scope in this same transaction so the
    // partial unique index is never used as a substitute for business logic.
    if req.is_default {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE pricing_models SET is_default = FALSE, version = version + 1, updated_at = NOW() WHERE model_name = $1 AND billing_dimension = $2 AND tenant_id = $3 AND is_default = TRUE",
            [
                req.model_name.trim().into(),
                billing_dimension.as_str().into(),
                tenant_id.into(),
            ],
        ))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to clear previous default: {e}")))?;
    }

    let db_req = CreatePricingRequest {
        tenant_id: req.tenant_id,
        model_name,
        billing_dimension,
        currency: Some(currency),
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        is_default: Some(req.is_default),
        effective_from: Some(effective_from),
        effective_until,
    };

    let pricing = PricingModel::create(&txn, &db_req)
        .await
        .map_err(|e| map_pricing_db_error(e, "create pricing"))?;
    record_pricing_audit(&txn, auth.user_id, "create", None, Some(&pricing)).await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit pricing creation: {e}")))?;
    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing created",
        "pricing_id": pricing.id,
        "model_name": pricing.model_name,
        "billing_dimension": pricing.billing_dimension.as_str(),
        "input_price_per_1k": pricing.input_price_per_1k.to_string(),
        "output_price_per_1k": pricing.output_price_per_1k.to_string(),
        "is_default": pricing.is_default,
        "version": pricing.version,
        "created_by": auth.user_id,
    })))
}

/// 更新定价
///
/// PUT /api/v1/pricing/{id}
pub async fn update_pricing(
    auth: AuthExtractor,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let expected_version = req.expected_version.ok_or_else(|| {
        ApiError::BadRequest("expected_version is required for pricing updates".to_string())
    })?;
    if expected_version <= 0 {
        return Err(ApiError::BadRequest(
            "expected_version must be positive".to_string(),
        ));
    }
    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin pricing update: {e}")))?;
    // 查找并锁定现有定价，确保审计和更新处于同一事务。
    let existing = PricingModel::find_by_id_for_update(&txn, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    let input_price = req
        .input_price_per_1k
        .as_deref()
        .map(|value| parse_price(value, "input_price_per_1k"))
        .transpose()?;
    let output_price = req
        .output_price_per_1k
        .as_deref()
        .map(|value| parse_price(value, "output_price_per_1k"))
        .transpose()?;
    let effective_until = parse_timestamp(req.effective_until.as_ref(), "effective_until")?;
    if input_price.is_none() && output_price.is_none() && req.effective_until.is_none() {
        return Err(ApiError::BadRequest(
            "At least one pricing field must be provided".to_string(),
        ));
    }
    if let Some(until) = effective_until
        && until <= existing.effective_from
    {
        return Err(ApiError::BadRequest(
            "effective_until must be later than effective_from".to_string(),
        ));
    }

    let db_req = UpdatePricingRequest {
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        effective_until,
        expected_version,
    };

    let updated = existing
        .update(&txn, &db_req)
        .await
        .map_err(|e| map_pricing_db_error(e, "update pricing"))?;
    record_pricing_audit(
        &txn,
        auth.user_id,
        "update",
        Some(&existing),
        Some(&updated),
    )
    .await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit pricing update: {e}")))?;

    // 清除缓存
    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing updated",
        "pricing_id": updated.id,
        "version": updated.version,
        "updated_fields": {
            "input_price_per_1k": req.input_price_per_1k,
            "output_price_per_1k": req.output_price_per_1k,
            "effective_until": req.effective_until.clone(),
        },
        "updated_by": auth.user_id,
    })))
}

/// 删除定价
///
/// DELETE /api/v1/pricing/{id}
pub async fn delete_pricing(
    auth: AuthExtractor,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin pricing deletion: {e}")))?;
    // 查找并锁定定价，审计记录必须和删除原子提交。
    let existing = PricingModel::find_by_id_for_update(&txn, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    // 禁止删除全局默认定价（tenant_id 为 GLOBAL_DEFAULT_TENANT_ID）
    if existing.tenant_id == GLOBAL_DEFAULT_TENANT_ID {
        tracing::warn!(
            pricing_id = %pricing_id,
            model_name = %existing.model_name,
            tenant_id = %existing.tenant_id,
            "Attempted to delete global default pricing, which is not allowed"
        );
        return Err(ApiError::BadRequest(
            "Cannot delete global default pricing. Only update is allowed.".to_string(),
        ));
    }

    existing
        .delete(&txn)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete pricing: {}", e)))?;
    record_pricing_audit(&txn, auth.user_id, "delete", Some(&existing), None).await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit pricing deletion: {e}")))?;

    // 清除缓存
    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing deleted",
        "pricing_id": pricing_id,
        "deleted_by": auth.user_id,
    })))
}

/// 批量设置默认定价
///
/// POST /api/v1/pricing/batch-defaults
///
/// 按定价 ID 批量设置各自作用域内的默认定价。
pub async fn set_default_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<SetDefaultPricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    if req.model_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "model_ids must contain at least one pricing id".to_string(),
        ));
    }
    let mut pricing_ids = req.model_ids;
    pricing_ids.sort_unstable();
    pricing_ids.dedup();
    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin batch default update: {e}")))?;
    let mut updated = Vec::with_capacity(pricing_ids.len());
    for pricing_id in pricing_ids {
        set_pricing_default_in_transaction(&txn, auth.user_id, pricing_id).await?;
        updated.push(pricing_id);
    }
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit batch default update: {e}")))?;
    state.pricing.clear_cache().await;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing defaults set",
        "pricing_ids": updated,
        "set_by": auth.user_id,
    })))
}

#[cfg(test)]
mod tests {
    use super::{
        SetDefaultPricingAdminRequest, parse_price, parse_timestamp,
        validate_active_pricing_tenant_status, validate_model_name,
    };
    use crate::error::ApiError;
    use uuid::Uuid;

    #[test]
    fn pricing_creation_requires_an_existing_active_tenant() {
        let tenant_id = Uuid::new_v4();
        assert!(validate_active_pricing_tenant_status(Some("active"), tenant_id).is_ok());
        assert!(matches!(
            validate_active_pricing_tenant_status(Some("inactive"), tenant_id),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            validate_active_pricing_tenant_status(None, tenant_id),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn pricing_inputs_reject_negative_or_malformed_values() {
        assert!(parse_price("0.125", "input_price").is_ok());
        assert!(parse_price("-0.1", "input_price").is_err());
        assert!(parse_price("not-a-number", "input_price").is_err());
        assert!(parse_price("0.12345678901", "input_price").is_err());
        assert!(parse_price("10000000000", "input_price").is_err());
        assert!(parse_price("9999999999.9999999999", "input_price").is_ok());
        assert!(parse_price("1.23000000000", "input_price").is_ok());
        assert!(parse_timestamp(Some(&"not-a-date".to_string()), "effective_until").is_err());
        assert!(validate_model_name(&"模".repeat(100)).is_ok());
    }

    #[test]
    fn batch_default_payload_accepts_legacy_and_explicit_id_field_names() {
        let id = Uuid::new_v4();
        let legacy: SetDefaultPricingAdminRequest =
            serde_json::from_value(serde_json::json!({"model_ids": [id]})).unwrap();
        let explicit: SetDefaultPricingAdminRequest =
            serde_json::from_value(serde_json::json!({"pricing_ids": [id]})).unwrap();
        assert_eq!(legacy.model_ids, vec![id]);
        assert_eq!(explicit.model_ids, vec![id]);
    }
}
