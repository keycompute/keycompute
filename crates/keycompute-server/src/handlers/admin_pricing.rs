//! 定价管理处理器
//!
//! 处理需要 Admin 权限的定价管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
};
use bigdecimal::BigDecimal;
use keycompute_db::models::pricing_model::{
    CreatePricingRequest, GLOBAL_DEFAULT_TENANT_ID, PricingModel, UpdatePricingRequest,
};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};

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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找目标定价
    let target = PricingModel::find_by_id(pool, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;
    tracing::info!(
        pricing_id = %pricing_id,
        model_name = %target.model_name,
        billing_dimension = %target.billing_dimension,
        is_default = target.is_default,
        "Attempting to set pricing as default"
    );

    // 查询同一模型+计费维度+租户的所有定价（实现作用域内互斥）
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT * FROM pricing_models
        WHERE model_name = $1
          AND billing_dimension = $2
          AND (
              (tenant_id = $3 AND $3 IS NOT NULL)  -- 同一租户
              OR (tenant_id IS NULL AND $3 IS NULL)  -- 全局默认
          )
        ORDER BY model_name, tenant_id NULLS LAST
        "#,
        [
            target.model_name.as_str().into(),
            target.billing_dimension.as_str().into(),
            target
                .tenant_id
                .map(|id| id.into())
                .unwrap_or(sea_orm::Value::Uuid(None)),
        ],
    );
    let all_pricing: Vec<PricingModel> = PricingModel::find_by_statement(stmt)
        .all(pool.as_ref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query pricing: {}", e)))?;

    let mut updated_count = 0;
    // 将同一模型+计费维度的所有记录（无论租户还是全局）的 is_default 互斥更新
    for pricing in all_pricing {
        // 匹配同一分组：模型名一致 + 计费维度一致（不限制租户ID，实现全局互斥）
        let same_model_dimension = pricing.model_name == target.model_name
            && pricing.billing_dimension == target.billing_dimension;

        if same_model_dimension {
            let new_is_default = pricing.id == pricing_id;
            if pricing.is_default != new_is_default {
                tracing::debug!(
                    pricing_id = %pricing.id,
                    old_is_default = pricing.is_default,
                    new_is_default = new_is_default,
                    "Updating is_default flag"
                );
                let stmt = Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE pricing_models SET is_default = $1, updated_at = NOW() WHERE id = $2",
                    [new_is_default.into(), pricing.id.into()],
                );
                pool.as_ref()
                    .execute(stmt)
                    .await
                    .map_err(|e| ApiError::Internal(format!("Failed to update pricing: {}", e)))?;
                updated_count += 1;
            }
        }
    }
    tracing::info!(
        pricing_id = %pricing_id,
        updated_count = updated_count,
        "Successfully set pricing as default"
    );

    // 清除缓存
    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing set as default",
        "pricing_id": pricing_id,
    })))
}
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 定价信息
#[derive(Debug, Serialize)]
pub struct PricingInfo {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
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
}

/// 创建定价请求（管理员）
#[derive(Debug, Deserialize)]
pub struct CreatePricingAdminRequest {
    /// 模型名称
    pub model_name: String,
    /// 计费维度: node 或 provideraccount
    #[serde(rename = "billing_dimension")]
    pub billing_dimension: String,
    /// 租户ID（可选，不指定则为全局默认定价）
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
}

/// 列出所有定价
///
/// GET /api/v1/pricing
///
/// Admin 可以看到所有租户的定价，普通用户只能看到自己的
pub async fn list_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<PricingInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // Admin 查看所有定价（包括所有租户和全局默认）
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT * FROM pricing_models
        ORDER BY model_name, tenant_id NULLS LAST, created_at DESC
    "#,
        [],
    );
    let pricing_models = PricingModel::find_by_statement(stmt)
        .all(pool.as_ref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query pricing: {}", e)))?;

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
            }
        })
        .collect();

    Ok(Json(pricing_list))
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
        .as_ref()
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

    // 解析价格
    let input_price: BigDecimal = req
        .input_price_per_1k
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid input_price_per_1k".to_string()))?;

    let output_price: BigDecimal = req
        .output_price_per_1k
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid output_price_per_1k".to_string()))?;

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

    let db_req = CreatePricingRequest {
        tenant_id: req.tenant_id, // 使用请求中的 tenant_id，None 表示全局默认
        model_name: req.model_name.clone(),
        billing_dimension,
        currency: Some(req.currency.clone()),
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        is_default: Some(req.is_default),
        effective_from: req.effective_from.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok()
        }),
        effective_until: req.effective_until.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok()
        }),
    };

    let pricing = PricingModel::create(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create pricing: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing created",
        "pricing_id": pricing.id,
        "model_name": pricing.model_name,
        "billing_dimension": pricing.billing_dimension.as_str(),
        "input_price_per_1k": pricing.input_price_per_1k.to_string(),
        "output_price_per_1k": pricing.output_price_per_1k.to_string(),
        "is_default": pricing.is_default,
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找现有定价
    let existing = PricingModel::find_by_id(pool, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    // 解析价格
    let input_price = req.input_price_per_1k.as_ref().and_then(|s| s.parse().ok());

    let output_price = req
        .output_price_per_1k
        .as_ref()
        .and_then(|s| s.parse().ok());

    let effective_until = req.effective_until.as_ref().and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()
    });

    let db_req = UpdatePricingRequest {
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        effective_until,
    };

    let updated = existing
        .update(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update pricing: {}", e)))?;

    // 清除缓存
    state.pricing.clear_cache().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing updated",
        "pricing_id": updated.id,
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找并删除定价
    let existing = PricingModel::find_by_id(pool, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    // 禁止删除全局默认定价（tenant_id 为 None 或 GLOBAL_DEFAULT_TENANT_ID）
    if existing.tenant_id.is_none() || existing.tenant_id == Some(GLOBAL_DEFAULT_TENANT_ID) {
        tracing::warn!(
            pricing_id = %pricing_id,
            model_name = %existing.model_name,
            tenant_id = ?existing.tenant_id,
            "Attempted to delete global default pricing, which is not allowed"
        );
        return Err(ApiError::BadRequest(
            "Cannot delete global default pricing. Only update is allowed.".to_string(),
        ));
    }

    existing
        .delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete pricing: {}", e)))?;

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
/// 为常用模型设置默认定价（使用计费维度 provideraccount）
pub async fn set_default_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    tracing::info!(tenant_id = %auth.tenant_id, "Setting up default pricing");
    // 默认定价数据（仅保留一个示例模型）
    let defaults = vec![("model-empty", "0.1", "0.3")];

    let mut created = 0;
    let mut skipped = 0;

    for (model_name, input_price, output_price) in defaults {
        // 使用 provideraccount 计费维度
        let billing_dimension = keycompute_pricing::DEFAULT_PRICING_PROVIDER;

        // 检查是否已存在
        let existing =
            PricingModel::find_by_model(pool, auth.tenant_id, model_name, billing_dimension)
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("Failed to check existing pricing: {}", e))
                })?;

        if existing.is_some() {
            skipped += 1;
            continue;
        }

        let db_req = CreatePricingRequest {
            tenant_id: None,
            model_name: model_name.to_string(),
            billing_dimension:
                keycompute_db::models::pricing_model::BillingDimension::ProviderAccount,
            currency: Some("CNY".to_string()),
            input_price_per_1k: input_price.parse().unwrap(),
            output_price_per_1k: output_price.parse().unwrap(),
            is_default: Some(true),
            effective_from: None,
            effective_until: None,
        };

        match PricingModel::create(pool, &db_req).await {
            Ok(_) => created += 1,
            Err(e) => {
                tracing::warn!(model = model_name, error = %e, "Failed to create default pricing");
            }
        }
    }
    tracing::info!(
        created = created,
        skipped = skipped,
        "Default pricing setup completed"
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Default pricing set",
        "created": created,
        "skipped": skipped,
        "set_by": auth.user_id,
    })))
}
