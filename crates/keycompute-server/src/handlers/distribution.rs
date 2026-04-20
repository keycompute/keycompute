//! Distribution 分销管理处理器
//!
//! 完整的二级分销实现：
//! - 查看分销记录（从数据库）
//! - 分销统计（从数据库聚合）
//! - 分销规则管理 (Admin)
//! - 用户分销收益查询（从数据库）
//! - 推荐关系查询（从数据库）

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::resolve_public_base_url,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// 使用 sqlx::types::BigDecimal 替代 bigdecimal crate
type BigDecimal = sqlx::types::BigDecimal;

// ==================== 数据结构 ====================

/// 分销记录查询参数
#[derive(Debug, Deserialize)]
pub struct DistributionQuery {
    /// 分页偏移
    #[serde(default)]
    pub offset: Option<i64>,
    /// 分页限制
    #[serde(default = "default_limit")]
    pub limit: Option<i64>,
    /// 按状态筛选
    pub status: Option<String>,
    /// 按层级筛选
    pub level: Option<String>,
    /// 按受益人筛选 (Admin 使用)
    pub beneficiary_id: Option<Uuid>,
}

fn default_limit() -> Option<i64> {
    Some(20)
}

/// 分销记录响应
#[derive(Debug, Serialize)]
pub struct DistributionRecordResponse {
    /// 记录 ID
    pub id: String,
    /// 关联的 usage_log ID
    pub usage_log_id: String,
    /// 租户 ID
    pub tenant_id: String,
    /// 受益人 ID
    pub beneficiary_id: String,
    /// 受益人名称
    pub beneficiary_name: String,
    /// 分成金额
    pub share_amount: String,
    /// 分成比例
    pub share_ratio: String,
    /// 分销层级: level1, level2
    pub level: String,
    /// 状态: pending, settled, cancelled
    pub status: String,
    /// 创建时间
    pub created_at: String,
}

/// 分销统计响应
#[derive(Debug, Serialize)]
pub struct DistributionStatsResponse {
    /// 总收益
    pub total_earnings: String,
    /// 待结算金额
    pub pending_amount: String,
    /// 已结算金额
    pub settled_amount: String,
    /// 货币
    pub currency: String,
    /// 一级分销收益
    pub level1_earnings: String,
    /// 二级分销收益
    pub level2_earnings: String,
    /// 推荐人数
    pub referral_count: i64,
}

/// 分销规则响应
#[derive(Debug, Serialize)]
pub struct DistributionRuleResponse {
    /// 规则 ID
    pub id: String,
    /// 规则名称
    pub name: String,
    /// 佣金比例 (0.0 - 1.0)
    pub commission_rate: f64,
    /// 最小购买金额（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_purchase_amount: Option<f64>,
    /// 最大佣金金额（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_commission_amount: Option<f64>,
    /// 是否启用
    pub is_active: bool,
    /// 创建时间
    pub created_at: String,
}

/// 创建分销规则请求
#[derive(Debug, Deserialize)]
pub struct CreateDistributionRuleRequest {
    /// 规则名称
    pub name: String,
    /// 佣金比例 (0.0 - 1.0, 例如 0.03 表示 3%)
    pub commission_rate: f64,
    /// 最小购买金额（可选）
    pub min_purchase_amount: Option<f64>,
    /// 最大佣金金额（可选）
    pub max_commission_amount: Option<f64>,
}

/// 更新分销规则请求
#[derive(Debug, Deserialize)]
pub struct UpdateDistributionRuleRequest {
    /// 规则名称
    pub name: Option<String>,
    /// 佣金比例
    pub commission_rate: Option<f64>,
    /// 最小购买金额（可选）
    pub min_purchase_amount: Option<f64>,
    /// 最大佣金金额（可选）
    pub max_commission_amount: Option<f64>,
    /// 是否启用
    pub is_active: Option<bool>,
}

/// 用户分销收益查询响应
#[derive(Debug, Serialize)]
pub struct UserDistributionEarningsResponse {
    /// 用户 ID
    pub user_id: String,
    /// 总收益
    pub total_earnings: String,
    /// 待结算
    pub pending_amount: String,
    /// 已结算
    pub settled_amount: String,
    /// 货币
    pub currency: String,
    /// 一级推荐人数
    pub level1_referrals: i64,
    /// 二级推荐人数
    pub level2_referrals: i64,
}

/// 推荐码响应
#[derive(Debug, Serialize)]
pub struct ReferralCodeResponse {
    /// 用户 ID（作为推荐码）
    pub referral_code: String,
    /// 推荐链接
    pub invite_link: String,
    /// 一级推荐人数
    pub level1_count: i64,
    /// 二级推荐人数
    pub level2_count: i64,
}

/// 生成邀请链接请求
#[derive(Debug, Deserialize)]
pub struct GenerateInviteLinkRequest {
    /// 自定义来源标识（可选，用于追踪不同渠道）
    pub source: Option<String>,
}

/// 邀请链接响应
#[derive(Debug, Serialize)]
pub struct InviteLinkResponse {
    /// 完整邀请链接
    pub invite_link: String,
    /// 推荐码
    pub referral_code: String,
    /// 短链接（可选）
    pub short_link: Option<String>,
    /// 过期时间（可选）
    pub expires_at: Option<String>,
}

fn build_invite_link(base_url: &str, referral_code: &str, source: Option<&str>) -> String {
    if let Some(source) = source {
        format!(
            "{}/auth/register?ref={}&source={}",
            base_url, referral_code, source
        )
    } else {
        format!("{}/auth/register?ref={}", base_url, referral_code)
    }
}

/// 获取我的推荐码和邀请链接
///
/// GET /api/v1/me/referral/code
pub async fn get_my_referral_code(
    auth: AuthExtractor,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ReferralCodeResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool).await?;

    // 获取推荐统计
    let referral_stats = keycompute_db::UserReferral::get_stats_by_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 构建邀请链接
    // 优先使用全局 APP_BASE_URL，缺失时回退到可信请求头推导
    let base_url = resolve_public_base_url(&headers, state.app_base_url.as_deref());
    let referral_code = auth.user_id.to_string();
    let invite_link = build_invite_link(&base_url, &referral_code, None);

    Ok(Json(ReferralCodeResponse {
        referral_code,
        invite_link,
        level1_count: referral_stats.level1_count,
        level2_count: referral_stats.level2_count,
    }))
}

/// 生成邀请链接（支持自定义来源）
///
/// POST /api/v1/me/referral/invite-link
pub async fn generate_invite_link(
    auth: AuthExtractor,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<GenerateInviteLinkRequest>,
) -> Result<Json<InviteLinkResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool).await?;

    // 构建基础邀请链接
    let base_url = resolve_public_base_url(&headers, state.app_base_url.as_deref());
    let referral_code = auth.user_id.to_string();
    let invite_link = build_invite_link(&base_url, &referral_code, req.source.as_deref());

    Ok(Json(InviteLinkResponse {
        invite_link,
        referral_code,
        short_link: None, // 可以集成短链接服务
        expires_at: None, // 可以添加过期时间
    }))
}

/// 推荐人信息
#[derive(Debug, Serialize)]
pub struct ReferralInfo {
    /// 用户 ID
    pub user_id: String,
    /// 用户名/邮箱
    pub user_name: String,
    /// 层级
    pub level: String,
    /// 注册时间
    pub registered_at: String,
    /// 产生的收益
    pub total_earnings: String,
}

// ==================== 辅助函数 ====================

/// 将 BigDecimal 转换为字符串
fn bigdecimal_to_string(value: &BigDecimal) -> String {
    value.to_string()
}

/// 将字符串解析为 BigDecimal
fn string_to_bigdecimal(value: &str) -> Result<BigDecimal> {
    value
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("Invalid decimal: {}", e)))
}

/// 检查分销系统是否启用
async fn check_distribution_enabled(_pool: &sqlx::PgPool) -> Result<()> {
    // 分销系统默认启用，不再检查系统设置
    // 如需禁用，可通过租户级别的 distribution_enabled 控制
    Ok(())
}

// ==================== API Handlers ====================

/// 查看分销记录
///
/// GET /api/v1/distribution/records
/// - Admin: 查看所有记录
/// - 普通用户: 查看自己的记录
pub async fn list_distribution_records(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(query): Query<DistributionQuery>,
) -> Result<Json<Vec<DistributionRecordResponse>>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);

    let records = if auth.is_admin() {
        // Admin 可以查看所有记录，或按受益人筛选
        if let Some(beneficiary_id) = query.beneficiary_id {
            keycompute_db::DistributionRecord::find_by_beneficiary(
                pool,
                beneficiary_id,
                limit,
                offset,
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
        } else {
            keycompute_db::DistributionRecord::find_by_tenant(pool, auth.tenant_id, limit, offset)
                .await
                .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
        }
    } else {
        // 普通用户只能查看自己的记录
        keycompute_db::DistributionRecord::find_by_beneficiary(pool, auth.user_id, limit, offset)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
    };

    // 转换为响应格式
    let responses: Vec<DistributionRecordResponse> = records
        .into_iter()
        .filter(|r| {
            // 应用状态筛选
            if let Some(ref status) = query.status {
                r.status == *status
            } else {
                true
            }
        })
        .filter(|r| {
            // 应用层级筛选
            if let Some(ref level) = query.level {
                r.level == *level
            } else {
                true
            }
        })
        .map(|r| DistributionRecordResponse {
            id: r.id.to_string(),
            usage_log_id: r.usage_log_id.to_string(),
            tenant_id: r.tenant_id.to_string(),
            beneficiary_id: r.beneficiary_id.to_string(),
            beneficiary_name: r.beneficiary_id.to_string(), // 使用 ID 作为名称，前端可进一步查询
            share_amount: bigdecimal_to_string(&r.share_amount),
            share_ratio: bigdecimal_to_string(&r.share_ratio),
            level: r.level.clone(),
            status: r.status,
            created_at: r.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(responses))
}

/// 获取分销统计
///
/// GET /api/v1/distribution/stats
pub async fn get_distribution_stats(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<DistributionStatsResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用（普通用户）
    if !auth.is_admin() {
        check_distribution_enabled(pool).await?;
    }

    // 获取当前用户的分销统计
    let stats = keycompute_db::DistributionRecord::get_stats_by_beneficiary(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 获取按层级的统计
    let level_stats =
        keycompute_db::DistributionRecord::get_level_stats_by_beneficiary(pool, auth.user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 获取推荐统计
    let referral_stats = keycompute_db::UserReferral::get_stats_by_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    Ok(Json(DistributionStatsResponse {
        total_earnings: bigdecimal_to_string(&stats.total_amount),
        pending_amount: bigdecimal_to_string(&stats.pending_amount),
        settled_amount: bigdecimal_to_string(&stats.settled_amount),
        currency: "CNY".to_string(),
        level1_earnings: bigdecimal_to_string(&level_stats.level1_amount),
        level2_earnings: bigdecimal_to_string(&level_stats.level2_amount),
        referral_count: referral_stats.total_referrals,
    }))
}

/// 查看分销规则列表
///
/// GET /api/v1/distribution/rules
/// 仅 Admin 可访问
pub async fn list_distribution_rules(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<DistributionRuleResponse>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 查询租户的所有规则
    let rules = keycompute_db::TenantDistributionRule::find_all_by_tenant(pool, auth.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let responses: Vec<DistributionRuleResponse> = rules
        .into_iter()
        .map(|r| DistributionRuleResponse {
            id: r.id.to_string(),
            name: r.name,
            commission_rate: r.commission_rate.to_string().parse().unwrap_or(0.0),
            min_purchase_amount: None,   // 数据库模型中暂无此字段
            max_commission_amount: None, // 数据库模型中暂无此字段
            is_active: r.is_active,
            created_at: r.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(responses))
}

/// 创建分销规则
///
/// POST /api/v1/distribution/rules
/// 仅 Admin 可访问
pub async fn create_distribution_rule(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreateDistributionRuleRequest>,
) -> Result<Json<DistributionRuleResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    // 验证参数
    if req.commission_rate < 0.0 || req.commission_rate > 1.0 {
        return Err(ApiError::BadRequest(
            "commission_rate must be between 0.0 and 1.0".to_string(),
        ));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 创建规则 - 使用当前用户作为受益人（简化处理）
    let create_req = keycompute_db::CreateDistributionRuleRequest {
        tenant_id: auth.tenant_id,
        beneficiary_id: auth.user_id, // 使用当前用户作为受益人
        name: req.name.clone(),
        description: None,
        commission_rate: string_to_bigdecimal(&req.commission_rate.to_string())?,
        priority: Some(0),
        effective_from: Some(chrono::Utc::now()),
        effective_until: None,
    };

    let rule = keycompute_db::TenantDistributionRule::create(pool, &create_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create rule: {}", e)))?;

    Ok(Json(DistributionRuleResponse {
        id: rule.id.to_string(),
        name: rule.name,
        commission_rate: req.commission_rate,
        min_purchase_amount: req.min_purchase_amount,
        max_commission_amount: req.max_commission_amount,
        is_active: rule.is_active,
        created_at: rule.created_at.to_rfc3339(),
    }))
}

/// 更新分销规则
///
/// PUT /api/v1/distribution/rules/{id}
/// 仅 Admin 可访问
pub async fn update_distribution_rule(
    auth: AuthExtractor,
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateDistributionRuleRequest>,
) -> Result<Json<DistributionRuleResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    // 验证参数
    if let Some(rate) = req.commission_rate
        && !(0.0..=1.0).contains(&rate)
    {
        return Err(ApiError::BadRequest(
            "commission_rate must be between 0.0 and 1.0".to_string(),
        ));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 查找规则
    let rule = keycompute_db::TenantDistributionRule::find_by_id(pool, rule_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
        .ok_or_else(|| ApiError::NotFound("Distribution rule not found".to_string()))?;

    // 更新规则
    let update_req = keycompute_db::UpdateDistributionRuleRequest {
        name: req.name,
        description: None,
        commission_rate: req.commission_rate.and_then(|r| r.to_string().parse().ok()),
        priority: None,
        is_active: req.is_active,
        effective_until: None,
    };

    let updated_rule = rule
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update rule: {}", e)))?;

    Ok(Json(DistributionRuleResponse {
        id: updated_rule.id.to_string(),
        name: updated_rule.name,
        commission_rate: req.commission_rate.unwrap_or_else(|| {
            updated_rule
                .commission_rate
                .to_string()
                .parse()
                .unwrap_or(0.0)
        }),
        min_purchase_amount: req.min_purchase_amount,
        max_commission_amount: req.max_commission_amount,
        is_active: updated_rule.is_active,
        created_at: updated_rule.created_at.to_rfc3339(),
    }))
}

/// 删除分销规则
///
/// DELETE /api/v1/distribution/rules/{id}
/// 仅 Admin 可访问
pub async fn delete_distribution_rule(
    auth: AuthExtractor,
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 查找并删除规则
    let rule = keycompute_db::TenantDistributionRule::find_by_id(pool, rule_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
        .ok_or_else(|| ApiError::NotFound("Distribution rule not found".to_string()))?;

    rule.delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete rule: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Distribution rule deleted",
        "rule_id": rule_id.to_string(),
        "deleted_by": auth.user_id.to_string(),
    })))
}

/// 获取当前用户的分销收益
///
/// GET /api/v1/me/distribution/earnings
pub async fn get_my_distribution_earnings(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<UserDistributionEarningsResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool).await?;

    // 获取分销统计
    let stats = keycompute_db::DistributionRecord::get_stats_by_beneficiary(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 获取推荐统计
    let referral_stats = keycompute_db::UserReferral::get_stats_by_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    Ok(Json(UserDistributionEarningsResponse {
        user_id: auth.user_id.to_string(),
        total_earnings: bigdecimal_to_string(&stats.total_amount),
        pending_amount: bigdecimal_to_string(&stats.pending_amount),
        settled_amount: bigdecimal_to_string(&stats.settled_amount),
        currency: "CNY".to_string(),
        level1_referrals: referral_stats.level1_count,
        level2_referrals: referral_stats.level2_count,
    }))
}

/// 获取当前用户的推荐列表
///
/// GET /api/v1/me/distribution/referrals
pub async fn get_my_referrals(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<ReferralInfo>>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool).await?;

    // 获取一级推荐
    let level1_referrals = keycompute_db::UserReferral::find_by_level1_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 获取二级推荐
    let level2_referrals = keycompute_db::UserReferral::find_by_level2_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // 合并并转换为响应格式，查询真实收益
    let mut referrals: Vec<ReferralInfo> = Vec::new();

    for r in level1_referrals {
        // 查询该推荐用户产生的分销收益
        let earnings = keycompute_db::DistributionRecord::get_earnings_for_referral(
            pool,
            auth.user_id,
            r.user_id,
        )
        .await
        .unwrap_or(BigDecimal::from(0));

        referrals.push(ReferralInfo {
            user_id: r.user_id.to_string(),
            user_name: r.user_id.to_string(), // 使用 ID 作为名称
            level: "level1".to_string(),
            registered_at: r.created_at.to_rfc3339(),
            total_earnings: bigdecimal_to_string(&earnings),
        });
    }

    for r in level2_referrals {
        // 查询该推荐用户产生的分销收益
        let earnings = keycompute_db::DistributionRecord::get_earnings_for_referral(
            pool,
            auth.user_id,
            r.user_id,
        )
        .await
        .unwrap_or(BigDecimal::from(0));

        referrals.push(ReferralInfo {
            user_id: r.user_id.to_string(),
            user_name: r.user_id.to_string(),
            level: "level2".to_string(),
            registered_at: r.created_at.to_rfc3339(),
            total_earnings: bigdecimal_to_string(&earnings),
        });
    }

    Ok(Json(referrals))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distribution_query_default_limit() {
        let query: DistributionQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(query.limit, Some(20));
    }

    #[test]
    fn test_create_distribution_rule_request_deserialize() {
        let json = r#"{
            "name": "默认分销规则",
            "commission_rate": 0.03
        }"#;
        let req: CreateDistributionRuleRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.commission_rate, 0.03);
        assert_eq!(req.name, "默认分销规则");
    }

    #[test]
    fn test_distribution_stats_response_serialize() {
        let stats = DistributionStatsResponse {
            total_earnings: "100.00".to_string(),
            pending_amount: "30.00".to_string(),
            settled_amount: "70.00".to_string(),
            currency: "CNY".to_string(),
            level1_earnings: "60.00".to_string(),
            level2_earnings: "40.00".to_string(),
            referral_count: 5,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("100.00"));
        assert!(json.contains("CNY"));
    }

    #[test]
    fn test_build_invite_link_uses_short_ref_param() {
        let link = build_invite_link(
            "https://app.example.com",
            "6aac8ab5-aeec-48b8-a4cc-0a446d952862",
            None,
        );

        assert_eq!(
            link,
            "https://app.example.com/auth/register?ref=6aac8ab5-aeec-48b8-a4cc-0a446d952862"
        );
    }

    #[test]
    fn test_build_invite_link_preserves_source_param() {
        let link = build_invite_link("https://app.example.com", "abc123", Some("campaign"));

        assert_eq!(
            link,
            "https://app.example.com/auth/register?ref=abc123&source=campaign"
        );
    }
}
