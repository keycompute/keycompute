//! Distribution 分销管理处理器
//!
//! 完整的二级分销实现：
//! - 查看分销记录（从数据库）
//! - 分销统计（从数据库聚合）
//! - 分销规则管理 (Admin)
//! - 用户分销收益查询（从数据库）
//! - 推荐关系查询（从数据库）

use crate::extractors::{GlobalConsoleAuth, RequestId};
use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, ConsoleAuth},
    handlers::configured_public_base_url,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use keycompute_db::models::system_setting::setting_keys;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use sea_orm::ConnectionTrait;

type BigDecimal = bigdecimal::BigDecimal;

// ==================== 数据结构 ====================

/// 分销记录查询参数
#[derive(Debug, Deserialize)]
pub struct DistributionQuery {
    /// 分页偏移
    #[serde(default)]
    pub offset: Option<i64>,
    /// 页码（现代分页接口）
    pub page: Option<i64>,
    /// 每页数量（现代分页接口）
    pub page_size: Option<i64>,
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
    /// 推荐人（受益人）ID
    pub referrer_id: Option<String>,
    /// 被推荐用户 ID
    pub referred_id: String,
    /// 被推荐用户消费金额
    pub amount: String,
    /// 分销佣金
    pub commission: String,
    /// 状态: pending, settled, cancelled
    pub status: String,
    /// 创建时间
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct DistributionRecordPageResponse {
    pub records: Vec<DistributionRecordResponse>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

fn build_invite_link(base_url: &str, referral_code: &str, source: Option<&str>) -> Result<String> {
    let mut parsed = Url::parse(base_url)
        .map_err(|e| ApiError::Config(format!("Invalid APP_BASE_URL: {}", e)))?;
    let current_path = parsed.path().trim_end_matches('/');
    let next_path = if current_path.is_empty() || current_path == "/" {
        "/auth/register".to_string()
    } else {
        format!("{}/auth/register", current_path)
    };
    parsed.set_path(&next_path);

    {
        let mut query = parsed.query_pairs_mut();
        query.append_pair("ref", referral_code);
        if let Some(source) = source.map(str::trim).filter(|value| !value.is_empty()) {
            query.append_pair("source", source);
        }
    }

    Ok(parsed.into())
}

/// 获取我的推荐码和邀请链接
///
/// GET /api/v1/me/referral/code
pub async fn get_my_referral_code(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<ReferralCodeResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool.write_conn()).await?;

    // 获取推荐统计
    let referral_stats = keycompute_db::UserReferral::get_stats_by_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let base_url = configured_public_base_url(state.app_base_url.as_deref()).ok_or_else(|| {
        ApiError::Config("APP_BASE_URL is required to generate public invite links".to_string())
    })?;
    let referral_code = auth.user_id.to_string();
    let invite_link = build_invite_link(&base_url, &referral_code, None)?;

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
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Json(req): Json<GenerateInviteLinkRequest>,
) -> Result<Json<InviteLinkResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool.write_conn()).await?;

    let base_url = configured_public_base_url(state.app_base_url.as_deref()).ok_or_else(|| {
        ApiError::Config("APP_BASE_URL is required to generate public invite links".to_string())
    })?;
    let referral_code = auth.user_id.to_string();
    let invite_link = build_invite_link(&base_url, &referral_code, req.source.as_deref())?;

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
    /// 被推荐用户 ID
    pub id: String,
    /// 被推荐用户邮箱
    pub email: String,
    /// 被推荐用户昵称
    pub name: Option<String>,
    /// 注册时间
    pub created_at: String,
    /// 被推荐用户累计消费
    pub total_consumption: String,
    /// 当前用户从该推荐用户获得的收益
    pub earnings: String,
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
pub(crate) async fn check_distribution_enabled(pool: &impl ConnectionTrait) -> Result<()> {
    let enabled =
        keycompute_db::SystemSetting::find_by_key(pool, setting_keys::DISTRIBUTION_ENABLED)
            .await
            .map_err(|e| {
                ApiError::Internal(format!("Failed to query distribution setting: {}", e))
            })?
            .map(|setting| setting.parse_bool())
            // Missing or invalid configuration must not expose distribution
            // operations that depend on a configured public application URL.
            .unwrap_or(false);

    if enabled {
        Ok(())
    } else {
        Err(ApiError::Forbidden("Distribution is disabled".to_string()))
    }
}

fn build_distribution_record_response(
    record: keycompute_db::models::distribution_scope::DistributionRecordReport,
) -> DistributionRecordResponse {
    DistributionRecordResponse {
        id: record.id.to_string(),
        referrer_id: record.beneficiary_id.map(|id| id.to_string()),
        referred_id: record.referred_id.to_string(),
        amount: record.amount.to_string(),
        commission: record.commission.to_string(),
        status: record.status,
        created_at: record.created_at.to_rfc3339(),
    }
}

// ==================== API Handlers ====================

/// 查看分销记录
///
/// GET /api/v1/distribution/records
/// Root-only legacy view of the explicitly selected tenant.
/// This single-currency UI remains CNY; canonical report routes expose currencies.
pub async fn list_distribution_records(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(query): Query<DistributionQuery>,
) -> Result<Json<serde_json::Value>> {
    use keycompute_db::models::distribution_scope::{
        self as scoped, DistributionScope, RecordFilter,
    };
    let root = auth.require_platform(keycompute_auth::AuthorizationAction::ManagePlatform)?;
    let scope = DistributionScope::Platform(root, auth.tenant_id);
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))?;
    let modern_pagination = query.page.is_some() || query.page_size.is_some();
    let (page, page_size, offset) = normalize_list_pagination(
        query.page,
        query.page_size.map(|v| v.clamp(1, 100)),
        query.limit.map(|v| v.clamp(1, 100)),
        query.offset,
    );
    let filter = RecordFilter {
        beneficiary_id: query.beneficiary_id,
        status: query.status,
        level: query.level,
        currency: Some("CNY".into()),
        ..Default::default()
    };
    let records = scoped::records(pool.write_conn(), scope, &filter, page_size, offset)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Distribution records unavailable".into()))?;
    let responses: Vec<_> = records
        .into_iter()
        .map(build_distribution_record_response)
        .collect();
    if !modern_pagination {
        return Ok(Json(serde_json::to_value(responses).map_err(|_| {
            ApiError::Internal("Distribution serialization failed".into())
        })?));
    }
    let total = scoped::record_count(pool.write_conn(), scope, &filter)
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Distribution count unavailable".into()))?;
    Ok(Json(
        serde_json::json!({"records": responses,"total": total,"page": page,"page_size":page_size,"total_pages":total_pages(total,page_size)}),
    ))
}

/// 获取分销统计
///
/// GET /api/v1/distribution/stats
pub async fn get_distribution_stats(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<DistributionStatsResponse>> {
    auth.require_platform(keycompute_auth::AuthorizationAction::ManagePlatform)?;
    let own = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))?;
    let values = keycompute_db::models::distribution_scope::record_stats(
        pool.write_conn(),
        keycompute_db::models::distribution_scope::DistributionScope::Owned(own),
        &keycompute_db::models::distribution_scope::RecordFilter {
            currency: Some("CNY".into()),
            ..Default::default()
        },
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Distribution statistics unavailable".into()))?;
    let stats = values.into_iter().next();
    let referrals =
        keycompute_db::UserReferral::get_stats_by_referrer(pool.write_conn(), auth.user_id)
            .await
            .map_err(|_| ApiError::ServiceUnavailable("Referral statistics unavailable".into()))?;
    Ok(Json(DistributionStatsResponse {
        total_earnings: stats
            .as_ref()
            .map(|v| v.total_earnings.to_string())
            .unwrap_or_else(|| "0".into()),
        pending_amount: stats
            .as_ref()
            .map(|v| v.pending_amount.to_string())
            .unwrap_or_else(|| "0".into()),
        settled_amount: stats
            .as_ref()
            .map(|v| v.settled_amount.to_string())
            .unwrap_or_else(|| "0".into()),
        currency: "CNY".into(),
        level1_earnings: stats
            .as_ref()
            .map(|v| v.level1_earnings.to_string())
            .unwrap_or_else(|| "0".into()),
        level2_earnings: stats
            .as_ref()
            .map(|v| v.level2_earnings.to_string())
            .unwrap_or_else(|| "0".into()),
        referral_count: referrals.total_referrals,
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
    let root = auth.require_platform(keycompute_auth::AuthorizationAction::ManagePlatform)?;
    let scope = keycompute_db::models::distribution_scope::DistributionScope::Platform(
        root,
        auth.tenant_id,
    );
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    let filter = keycompute_db::models::distribution_scope::RuleFilter::default();
    let total =
        keycompute_db::models::distribution_scope::rule_count(pool.write_conn(), scope, &filter)
            .await
            .map_err(|_| ApiError::ServiceUnavailable("Distribution rules unavailable".into()))?;
    if total > 100 {
        return Err(ApiError::BadRequest(
            "Use the canonical paginated distribution rule endpoint for more than 100 rules".into(),
        ));
    }
    let rules =
        keycompute_db::models::distribution_scope::rules(pool.write_conn(), scope, &filter, 100, 0)
            .await
            .map_err(|_| ApiError::ServiceUnavailable("Distribution rules unavailable".into()))?;

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

/// Legacy platform selectors are mandatory; absent tenant never means all.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyTargetQuery {
    pub tenant_id: Uuid,
    pub expected_updated_at: Option<chrono::DateTime<chrono::Utc>>,
}
fn legacy_policy_result(
    row: keycompute_db::TenantDistributionRule,
) -> Result<Json<DistributionRuleResponse>> {
    let rate = row
        .commission_rate
        .to_string()
        .parse::<f64>()
        .map_err(|_| ApiError::Internal("Invalid stored distribution rate".into()))?;
    Ok(Json(DistributionRuleResponse {
        id: row.id.to_string(),
        name: row.name,
        commission_rate: rate,
        min_purchase_amount: None,
        max_commission_amount: None,
        is_active: row.is_active,
        created_at: row.created_at.to_rfc3339(),
    }))
}
/// Existing URL, final explicit root/target authority; no tenant-admin fallback.
pub async fn create_distribution_rule(
    auth: GlobalConsoleAuth,
    id: RequestId,
    State(state): State<AppState>,
    Query(target): Query<PolicyTargetQuery>,
    Json(req): Json<CreateDistributionRuleRequest>,
) -> Result<Json<DistributionRuleResponse>> {
    let who = super::distribution_policy::platform(&auth, target.tenant_id)?;
    if req.min_purchase_amount.is_some()
        || req.max_commission_amount.is_some()
        || !req.commission_rate.is_finite()
    {
        return Err(ApiError::BadRequest(
            "unsupported amount limits or invalid commission rate".into(),
        ));
    }
    let rate = string_to_bigdecimal(&req.commission_rate.to_string())?;
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))?;
    let row = keycompute_db::models::distribution_policy::upsert_default(
        db.write_conn(),
        who,
        &super::distribution_policy::platform_audit(&auth, id),
        &req.name,
        rate,
        "platform default policy update",
    )
    .await
    .map_err(super::distribution_policy::map)?;
    legacy_policy_result(row)
}
pub async fn update_distribution_rule(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target): Query<PolicyTargetQuery>,
    Json(req): Json<UpdateDistributionRuleRequest>,
) -> Result<Json<DistributionRuleResponse>> {
    let who = super::distribution_policy::platform(&auth, target.tenant_id)?;
    if req.min_purchase_amount.is_some()
        || req.max_commission_amount.is_some()
        || req.commission_rate.is_some_and(|v| !v.is_finite())
    {
        return Err(ApiError::BadRequest(
            "unsupported amount limits or invalid commission rate".into(),
        ));
    }
    let revision = target
        .expected_updated_at
        .ok_or_else(|| ApiError::BadRequest("expected_updated_at is required".into()))?;
    let mut patch = keycompute_db::models::distribution_policy::PolicyPatch::empty(revision);
    patch.name = req.name;
    patch.is_active = req.is_active;
    patch.commission_rate = req
        .commission_rate
        .map(|v| string_to_bigdecimal(&v.to_string()))
        .transpose()?;
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))?;
    let row = keycompute_db::models::distribution_policy::update(
        db.write_conn(),
        who,
        &super::distribution_policy::platform_audit(&auth, id),
        rule_id,
        &patch,
        "platform policy update",
    )
    .await
    .map_err(super::distribution_policy::map)?;
    legacy_policy_result(row)
}
pub async fn delete_distribution_rule(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target): Query<PolicyTargetQuery>,
) -> Result<Json<serde_json::Value>> {
    let who = super::distribution_policy::platform(&auth, target.tenant_id)?;
    let revision = target
        .expected_updated_at
        .ok_or_else(|| ApiError::BadRequest("expected_updated_at is required".into()))?;
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Distribution storage unavailable".into()))?;
    keycompute_db::models::distribution_policy::delete(
        db.write_conn(),
        who,
        &super::distribution_policy::platform_audit(&auth, id),
        rule_id,
        revision,
        "platform policy deletion",
    )
    .await
    .map_err(super::distribution_policy::map)?;
    Ok(Json(
        serde_json::json!({"success":true,"message":"Distribution rule deleted","id":rule_id}),
    ))
}

/// 获取当前用户的分销收益
///
/// GET /api/v1/me/distribution/earnings
pub async fn get_my_distribution_earnings(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<UserDistributionEarningsResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not available".to_string()))?;

    // 检查分销系统是否启用
    check_distribution_enabled(pool.write_conn()).await?;

    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?;
    let stats = keycompute_db::models::distribution_scope::record_stats(
        pool.write_conn(),
        keycompute_db::models::distribution_scope::DistributionScope::Owned(scope),
        &keycompute_db::models::distribution_scope::RecordFilter {
            currency: Some("CNY".into()),
            ..Default::default()
        },
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Distribution earnings unavailable".into()))?
    .into_iter()
    .next();

    // 获取推荐统计
    let referral_stats = keycompute_db::UserReferral::get_stats_by_referrer(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    Ok(Json(UserDistributionEarningsResponse {
        user_id: auth.user_id.to_string(),
        total_earnings: stats
            .as_ref()
            .map(|v| v.total_earnings.to_string())
            .unwrap_or_else(|| "0".into()),
        pending_amount: stats
            .as_ref()
            .map(|v| v.pending_amount.to_string())
            .unwrap_or_else(|| "0".into()),
        settled_amount: stats
            .as_ref()
            .map(|v| v.settled_amount.to_string())
            .unwrap_or_else(|| "0".into()),
        currency: "CNY".to_string(),
        level1_referrals: referral_stats.level1_count,
        level2_referrals: referral_stats.level2_count,
    }))
}

/// Optional page parameters; the legacy array response is capped at 20 rows.
#[derive(Debug, Default, Deserialize)]
pub struct ReferralQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ReferralPageResponse {
    pub referrals: Vec<ReferralInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
    pub as_of: String,
}

/// Read one bounded page. Authentication and distribution visibility checks
/// remain mandatory; the query only sees this beneficiary's relationships.
pub async fn get_my_referrals(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<ReferralQuery>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not available".into()))?;
    check_distribution_enabled(pool.write_conn()).await?;
    let modern = params.page.is_some() || params.page_size.is_some();
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, None, None);
    let result = keycompute_db::models::referral_display::find_referral_display_page(
        pool,
        auth.require_owner(
            auth.user_id,
            keycompute_auth::AuthorizationAction::ReadPersonalResource,
        )?,
        page_size,
        offset,
    )
    .await
    .map_err(|error| ApiError::Internal(format!("Failed to query referrals: {error}")))?;
    let referrals = result
        .referrals
        .into_iter()
        .map(|referral| ReferralInfo {
            id: referral.user_id.to_string(),
            email: referral.email,
            name: referral.name,
            created_at: referral.created_at.to_rfc3339(),
            total_consumption: bigdecimal_to_string(&referral.total_consumption),
            earnings: bigdecimal_to_string(&referral.earnings),
        })
        .collect::<Vec<_>>();
    let value = if modern {
        serde_json::to_value(ReferralPageResponse {
            referrals,
            total: result.total,
            page,
            page_size,
            total_pages: total_pages(result.total, page_size),
            as_of: result.as_of.to_rfc3339(),
        })
    } else {
        serde_json::to_value(referrals)
    };
    Ok(Json(value.map_err(|error| {
        ApiError::Internal(format!("Referral serialization failed: {error}"))
    })?))
}

/// One overview replaces duplicated earnings/count/link requests in the Web UI.
pub async fn get_my_distribution_overview(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    use crate::display_cache::DisplayCache;
    if !auth.has_permission(&keycompute_auth::Permission::ViewUsage) {
        return Err(ApiError::Forbidden(
            "Console display permission required".into(),
        ));
    }
    let pool = state
        .pool
        .clone()
        .ok_or_else(|| ApiError::Internal("Database not available".into()))?;
    // Check the authoritative feature switch even when a snapshot already exists.
    check_distribution_enabled(pool.write_conn()).await?;
    let base = configured_public_base_url(state.app_base_url.as_deref()).ok_or_else(|| {
        ApiError::Config("APP_BASE_URL is required to generate public invite links".into())
    })?;
    let referral_code = auth.user_id.to_string();
    let invite_link = build_invite_link(&base, &referral_code, None)?;
    let key = DisplayCache::key(&auth, "distribution-overview-tenant-v2", &invite_link);
    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?;
    let value = state
        .display_cache
        .read(
            state.cache.clone(),
            state.console_admission.origin.clone(),
            auth.tenant_id,
            key,
            async move {
                let mut value =
                    keycompute_db::models::console_display::distribution(pool.write_conn(), scope)
                        .await
                        .map_err(|error| {
                            tracing::warn!(%error,"distribution overview query failed");
                            ApiError::Internal("Distribution overview unavailable".into())
                        })?;
                value["referral"] =
                    serde_json::json!({"referral_code":referral_code,"invite_link":invite_link});
                Ok(value)
            },
        )
        .await?;
    Ok(Json(value))
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
    fn modern_distribution_query_preserves_filter_and_page_inputs() {
        let query: DistributionQuery = serde_json::from_str(
            r#"{
                "status": "pending",
                "level": "level2",
                "page": 3,
                "page_size": 10
            }"#,
        )
        .unwrap();

        assert_eq!(query.status.as_deref(), Some("pending"));
        assert_eq!(query.level.as_deref(), Some("level2"));
        assert_eq!(query.page, Some(3));
        assert_eq!(query.page_size, Some(10));
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
        )
        .unwrap();

        assert_eq!(
            link,
            "https://app.example.com/auth/register?ref=6aac8ab5-aeec-48b8-a4cc-0a446d952862"
        );
    }

    #[test]
    fn test_build_invite_link_preserves_source_param() {
        let link =
            build_invite_link("https://app.example.com", "abc123", Some("campaign")).unwrap();

        assert_eq!(
            link,
            "https://app.example.com/auth/register?ref=abc123&source=campaign"
        );
    }

    #[test]
    fn test_build_invite_link_preserves_base_path_and_encodes_source() {
        let link = build_invite_link(
            "https://app.example.com/console/",
            "abc123",
            Some("email campaign&fall"),
        )
        .unwrap();

        assert_eq!(
            link,
            "https://app.example.com/console/auth/register?ref=abc123&source=email+campaign%26fall"
        );
    }
}
