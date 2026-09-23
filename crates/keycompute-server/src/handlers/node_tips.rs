//! Existing platform ratio endpoints. Withdrawal routes live in tenant_tips.
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{Json, extract::State};
use keycompute_db::{
    DbRouter,
    models::system_setting::{SystemSetting, setting_keys},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize)]
pub struct UpdateTipRatioRequest {
    pub ratio: String,
}
#[derive(Debug, Serialize)]
pub struct TipRatioResponse {
    pub ratio: String,
}

/// 更新小费比例
///
/// PUT /api/v1/admin/tips/settings/ratio
pub async fn admin_update_tip_ratio(
    _auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<UpdateTipRatioRequest>,
) -> Result<Json<TipRatioResponse>> {
    let pool = get_pool(&state)?;

    // 解析为 Decimal 以避免 f64 精度问题
    let ratio: Decimal = req.ratio.parse().map_err(|_| {
        ApiError::BadRequest(
            "Invalid ratio value, expected a decimal string like '0.90'".to_string(),
        )
    })?;

    if ratio <= Decimal::ZERO || ratio > Decimal::ONE {
        return Err(ApiError::BadRequest(
            "ratio must be between 0 and 1 (exclusive of 0)".to_string(),
        ));
    }

    SystemSetting::update_value(pool, setting_keys::NODE_TIP_RATIO, &ratio.to_string())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update tip ratio: {}", e)))?;

    Ok(Json(TipRatioResponse {
        ratio: ratio.to_string(),
    }))
}

/// 获取当前小费比例
///
/// GET /api/v1/admin/tips/settings/ratio
pub async fn admin_get_tip_ratio(
    _auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<TipRatioResponse>> {
    let pool = get_pool(&state)?;

    // 使用 get_string + Decimal 解析，保持与写入端一致的精度路径
    let ratio_str = SystemSetting::get_string(pool, setting_keys::NODE_TIP_RATIO, "0.90").await;

    Ok(Json(TipRatioResponse { ratio: ratio_str }))
}

// ============================================================================
// 工具函数
// ============================================================================

fn get_pool(state: &AppState) -> Result<&DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))
}
