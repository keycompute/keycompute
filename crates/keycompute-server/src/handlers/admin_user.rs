//! 用户管理处理器
//!
//! 处理需要 Admin 权限的用户管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use keycompute_auth::Permission;
use keycompute_billing::balance::{
    BalanceReservationPageCursor, MAX_BALANCE_RESERVATION_PAGE_SIZE,
};
use keycompute_db::models::api_key::ProduceAiKey;
use keycompute_db::models::tenant::Tenant;
use keycompute_db::models::user::User;
use keycompute_db::models::user_credential::UserCredential;
use keycompute_types::{AssignableUserRole, UserRole};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ==================== 用户管理 ====================

/// 用户信息（Admin 视图）
#[derive(Debug, Serialize)]
pub struct AdminUserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    /// 可用余额
    pub balance: f64,
    /// 冻结余额
    pub frozen_balance: f64,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

/// 用户列表查询参数
#[derive(Debug, Deserialize)]
pub struct UserListQueryParams {
    /// 租户 ID 过滤（可选）
    pub tenant_id: Option<Uuid>,
    /// 角色过滤（可选）
    pub role: Option<String>,
    /// 搜索关键词（邮箱或名称）
    pub search: Option<String>,
    /// 页码（从 1 开始）
    #[serde(default = "default_page")]
    pub page: i64,
    /// 每页数量
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

/// 一阶保护：禁止缺少受保护用户管理权限的调用方修改 system 用户。
///
/// 无论是改名称、改角色还是余额操作，未授权 caller 一律拒绝。
/// 此校验与 `validate_role_change_request` 互补：
/// - 本函数覆盖"是否能触碰 system 用户"的全局边界
/// - `validate_role_change_request` 覆盖"角色变更"的精细规则
fn validate_not_admin_modifying_system(auth: &AuthExtractor, target_user: &User) -> Result<()> {
    if !auth.has_permission(&Permission::ManageProtectedUsers)
        && target_user.role == UserRole::System.as_str()
    {
        return Err(ApiError::Forbidden(
            "Protected user management permission required".to_string(),
        ));
    }
    Ok(())
}

/// 二阶保护：校验角色变更请求的合法性。
///
/// 规则（按检查顺序）：
/// 1. 未请求变更角色 → 直接通过
/// 2. 仅有受保护用户管理权限的调用方可变更他人角色
/// 3. system 角色不能变更自己的角色
/// 4. system 角色的 role 字段不可被修改（即使 caller 是 system）
///
/// 调用约定：必须和 `validate_not_admin_modifying_system` 搭配使用，
/// 后者负责拦截未授权 caller 对 system 用户的任何修改。
fn validate_role_change_request(
    auth: &AuthExtractor,
    target_user_id: Uuid,
    target_user: &User,
    requested_role: &Option<AssignableUserRole>,
) -> Result<()> {
    if requested_role.is_none() {
        return Ok(());
    }

    if !auth.has_permission(&Permission::ManageProtectedUsers) {
        return Err(ApiError::Forbidden(
            "Protected user management permission required".to_string(),
        ));
    }

    if auth.user_id == target_user_id {
        return Err(ApiError::BadRequest(
            "System cannot modify its own role".to_string(),
        ));
    }

    if target_user.role == UserRole::System.as_str() {
        return Err(ApiError::BadRequest(
            "System role cannot be modified".to_string(),
        ));
    }

    Ok(())
}

fn validate_user_delete_request(
    auth: &AuthExtractor,
    target_user_id: Uuid,
    target_user: &User,
) -> Result<()> {
    if target_user_id == auth.user_id {
        return Err(ApiError::BadRequest("Cannot delete yourself".to_string()));
    }

    if target_user.role == UserRole::System.as_str() {
        return Err(ApiError::BadRequest(
            "System user cannot be deleted".to_string(),
        ));
    }

    if target_user.role == UserRole::Admin.as_str()
        && !auth.has_permission(&Permission::ManageProtectedUsers)
    {
        return Err(ApiError::Forbidden(
            "Protected user management permission required".to_string(),
        ));
    }

    Ok(())
}

/// 用户列表响应（带分页信息）
#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<AdminUserInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 列出所有用户
///
/// GET /api/v1/users
///
/// 支持查询参数：
/// - tenant_id: 租户 ID 过滤
/// - role: 角色过滤
/// - search: 搜索关键词
/// - page: 页码（默认 1）
/// - page_size: 每页数量（默认 20）
///
/// Admin 可以查询所有租户的用户。
///
/// 过滤条件下推到 SQL 层以保证分页准确性。
pub async fn list_all_users(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<UserListQueryParams>,
) -> Result<Json<UserListResponse>> {
    // 检查权限
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 计算分页偏移量
    let offset = (params.page - 1) * params.page_size;

    // 过滤条件下推到 SQL 层，保证分页准确性
    let tenant_id_filter = params.tenant_id;
    let role_filter = params.role.as_deref();
    let search_filter = params.search.as_deref();

    let users = User::find_all_filtered(
        pool,
        tenant_id_filter,
        role_filter,
        search_filter,
        params.page_size,
        offset,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to query users: {}", e)))?;

    // 统计过滤后的用户总数（同样下推到 SQL）
    let total = User::count_all_filtered(pool, tenant_id_filter, role_filter, search_filter)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

    // 预加载所有租户到 HashMap（避免 N+1 查询）
    let tenants = Tenant::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;
    let tenant_map: std::collections::HashMap<Uuid, String> =
        tenants.into_iter().map(|t| (t.id, t.name)).collect();

    // 批量预加载余额（避免 N+1 查询）
    let user_ids: Vec<Uuid> = users.iter().map(|u| u.id).collect();
    let balance_map = if let Some(bs) = state.billing.balance_service() {
        bs.find_by_users(&user_ids).await.ok().unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };

    // 批量预加载用户的最后登录时间（避免 N+1 查询）
    let credential_map: std::collections::HashMap<Uuid, UserCredential> =
        UserCredential::find_by_user_ids(pool, &user_ids)
            .await
            .unwrap_or_default();

    // 构建用户信息列表
    let result: Vec<AdminUserInfo> = users
        .into_iter()
        .map(|user| {
            let balance = balance_map.get(&user.id);
            let tenant_name = tenant_map
                .get(&user.tenant_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            AdminUserInfo {
                id: user.id,
                email: user.email.clone(),
                name: user.name.clone(),
                role: user.role.clone(),
                tenant_id: user.tenant_id,
                tenant_name,
                balance: balance
                    .map(|b| b.available_balance.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0),
                frozen_balance: balance
                    .map(|b| b.frozen_balance.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0),
                created_at: user.created_at.to_rfc3339(),
                updated_at: user.updated_at.to_rfc3339(),
                last_login_at: credential_map
                    .get(&user.id)
                    .and_then(|c| c.last_login_at.map(|t| t.to_rfc3339())),
            }
        })
        .collect();

    // 基于过滤后的 total 计算总页数
    let total_pages = (total + params.page_size - 1) / params.page_size;

    Ok(Json(UserListResponse {
        users: result,
        total,
        page: params.page,
        page_size: params.page_size,
        total_pages,
    }))
}

/// 获取指定用户信息
///
/// GET /api/v1/users/{id}
pub async fn get_user_by_id(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<AdminUserInfo>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    // 获取租户名称
    let tenant = Tenant::find_by_id(pool, user.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenant: {}", e)))?;
    let tenant_name = tenant
        .map(|t| t.name)
        .unwrap_or_else(|| "Unknown".to_string());

    // 获取用户余额
    let balance = if let Some(bs) = state.billing.balance_service() {
        bs.find_by_user(user.id).await.ok().flatten()
    } else {
        None
    };

    // 获取用户最后登录时间
    let last_login = UserCredential::find_by_user_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query credentials: {}", e)))?
        .and_then(|c| c.last_login_at.map(|t| t.to_rfc3339()));

    Ok(Json(AdminUserInfo {
        id: user.id,
        email: user.email,
        name: user.name,
        role: user.role,
        tenant_id: user.tenant_id,
        tenant_name,
        balance: balance
            .as_ref()
            .map(|b| b.available_balance.to_f64().unwrap_or(0.0))
            .unwrap_or(0.0),
        frozen_balance: balance
            .as_ref()
            .map(|b| b.frozen_balance.to_f64().unwrap_or(0.0))
            .unwrap_or(0.0),
        created_at: user.created_at.to_rfc3339(),
        updated_at: user.updated_at.to_rfc3339(),
        last_login_at: last_login,
    }))
}

/// 更新用户请求
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<AssignableUserRole>,
}

/// 更新用户信息
///
/// PUT /api/v1/users/{id}
pub async fn update_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    // 禁止非 system 角色修改 system 用户（包括仅修改名称）
    validate_not_admin_modifying_system(&auth, &user)?;
    validate_role_change_request(&auth, user_id, &user, &req.role)?;

    let update_req = keycompute_db::models::user::UpdateUserRequest {
        name: req.name,
        role: req.role,
    };

    let updated = user
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update user: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "User updated",
        "user_id": updated.id,
        "email": updated.email,
        "name": updated.name,
        "role": updated.role,
    })))
}

/// 删除用户
///
/// DELETE /api/v1/users/{id}
pub async fn delete_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    validate_user_delete_request(&auth, user_id, &user)?;

    user.delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete user: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "User deleted",
        "user_id": user_id,
        "deleted_by": auth.user_id,
    })))
}

/// 更新用户余额请求
#[derive(Debug, Deserialize)]
pub struct UpdateBalanceRequest {
    pub amount: String, // 使用字符串避免浮点精度问题
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct AdminBalanceReservationInfo {
    pub request_id: Uuid,
    /// Opaque compare-and-set version for administrative release.
    ///
    /// This is the reservation ownership token exposed under a deliberately
    /// generic name so callers do not depend on the internal lease model.
    pub version: Uuid,
    pub amount: String,
    pub status: String,
    pub expires_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct AdminBalanceReservationsQuery {
    /// Opaque keyset cursor returned by the previous page.
    pub cursor: Option<String>,
    /// Defaults to 50 and is clamped to the data-layer maximum of 100.
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct AdminBalanceReservationsResponse {
    pub user_id: Uuid,
    pub available_balance: String,
    /// 总冻结余额，包括请求预留和管理员手工冻结。
    pub total_frozen_balance: String,
    /// 当前活跃请求持有的冻结余额，不能通过通用解冻接口释放。
    pub request_reserved_balance: String,
    /// 管理员通用解冻接口实际可释放的余额。
    pub manually_frozen_balance: String,
    pub reservations: Vec<AdminBalanceReservationInfo>,
    pub next_cursor: Option<String>,
}

fn encode_balance_reservation_cursor(cursor: &BalanceReservationPageCursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("cursor serialization"))
}

fn decode_balance_reservation_cursor(value: &str) -> Result<BalanceReservationPageCursor> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))?;
    serde_json::from_slice(&decoded)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))
}

#[derive(Debug, Deserialize)]
pub struct ReleaseBalanceReservationRequest {
    /// Version returned by the latest reservations listing. Requiring it
    /// prevents a stale UI selection from releasing a newer retry that reused
    /// the same stable request ID.
    pub expected_version: Uuid,
    pub reason: String,
}

/// 管理员按请求释放余额预留后的响应。
///
/// Keep this wire contract in sync with
/// `client-api::api::admin::ReleaseBalanceReservationResponse`.
#[derive(Debug, Serialize)]
pub struct ReleaseBalanceReservationResponse {
    pub success: bool,
    pub message: String,
    pub user_id: Uuid,
    pub request_id: Uuid,
    pub released_amount: String,
    pub reason: String,
    pub new_available_balance: String,
    pub new_total_frozen_balance: String,
    pub request_reserved_balance: String,
    pub manually_frozen_balance: String,
    pub released_by: Uuid,
    /// 迟到的用量结算仍可能从可用余额扣款，调用方应向管理员明确展示。
    pub warning: String,
}

const ADMIN_RESERVATION_RELEASE_WARNING: &str =
    "Late usage settlement may still deduct the final charge from available balance";
const ADMIN_RESERVATION_RELEASE_REASON_MAX_CHARS: usize =
    keycompute_billing::balance::MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS;

fn validate_reservation_release_reason(reason: &str) -> Result<&str> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ApiError::BadRequest("Reason is required".to_string()));
    }
    if reason.chars().count() > ADMIN_RESERVATION_RELEASE_REASON_MAX_CHARS {
        return Err(ApiError::BadRequest(format!(
            "Reason must not exceed {} characters",
            ADMIN_RESERVATION_RELEASE_REASON_MAX_CHARS
        )));
    }
    Ok(reason)
}

/// 校验管理员是否可以管理目标用户的余额。
async fn validate_balance_target(
    auth: &AuthExtractor,
    state: &AppState,
    user_id: Uuid,
) -> Result<User> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let target_user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;
    validate_not_admin_modifying_system(auth, &target_user)?;
    Ok(target_user)
}

/// 余额操作的公共上下文
struct BalanceOpContext {
    pub amount: Decimal,
    pub reason: String,
    pub tenant_id: Uuid,
}

/// 余额操作公共前置校验，返回已校验的上下文
///
/// 统一处理：权限检查、用户查询、system 保护、金额解析、原因校验
async fn validate_balance_request(
    auth: &AuthExtractor,
    state: &AppState,
    user_id: Uuid,
    req: &UpdateBalanceRequest,
    require_positive: bool,
) -> Result<BalanceOpContext> {
    let target_user = validate_balance_target(auth, state, user_id).await?;

    // 解析金额
    let amount: Decimal = req
        .amount
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid amount format".to_string()))?;

    // 校验精度：最多两位小数（防止绕过前端限制传入过高精度金额）
    if amount != amount.round_dp(2) {
        return Err(ApiError::BadRequest(
            "Amount must have at most 2 decimal places".to_string(),
        ));
    }

    // 金额校验
    if require_positive && amount <= Decimal::ZERO {
        return Err(ApiError::BadRequest("Amount must be positive".to_string()));
    }
    if !require_positive && amount == Decimal::ZERO {
        return Err(ApiError::BadRequest("Amount cannot be zero".to_string()));
    }

    let reason = normalize_admin_balance_reason(&req.reason)?;

    Ok(BalanceOpContext {
        amount,
        reason,
        tenant_id: target_user.tenant_id,
    })
}

fn normalize_admin_balance_reason(reason: &str) -> Result<String> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ApiError::BadRequest("Reason is required".to_string()));
    }
    if reason.chars().count()
        > keycompute_billing::balance::MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS
    {
        return Err(ApiError::BadRequest(format!(
            "Reason must not exceed {} characters",
            keycompute_billing::balance::MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS
        )));
    }
    Ok(reason.to_string())
}

const ADMIN_BALANCE_IDEMPOTENCY_KEY_MAX_BYTES: usize =
    keycompute_billing::balance::MAX_ADMIN_BALANCE_IDEMPOTENCY_KEY_BYTES;

fn require_admin_balance_idempotency_key(headers: &HeaderMap) -> Result<&str> {
    let value = headers
        .get("idempotency-key")
        .ok_or_else(|| ApiError::BadRequest("Idempotency-Key header is required".to_string()))?;
    let key = value.to_str().map_err(|_| {
        ApiError::BadRequest("Idempotency-Key must contain visible ASCII text".to_string())
    })?;
    if key.is_empty()
        || key.len() > ADMIN_BALANCE_IDEMPOTENCY_KEY_MAX_BYTES
        || !key.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ApiError::BadRequest(format!(
            "Idempotency-Key must contain between 1 and {ADMIN_BALANCE_IDEMPOTENCY_KEY_MAX_BYTES} visible ASCII bytes"
        )));
    }
    Ok(key)
}

/// 获取用户余额拆分及活跃请求预留。
///
/// GET /api/v1/users/{id}/balance/reservations
pub async fn list_user_balance_reservations(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    Query(query): Query<AdminBalanceReservationsQuery>,
    State(state): State<AppState>,
) -> Result<Json<AdminBalanceReservationsResponse>> {
    let _ = validate_balance_target(&auth, &state, user_id).await?;

    let cursor = query
        .cursor
        .as_deref()
        .map(decode_balance_reservation_cursor)
        .transpose()?;
    let limit = query
        .limit
        .unwrap_or(50)
        .clamp(1, MAX_BALANCE_RESERVATION_PAGE_SIZE);

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;
    let page = balance_service
        .find_breakdown_page_by_user(user_id, cursor, limit)
        .await
        .map_err(ApiError::from)?;

    let (
        available_balance,
        total_frozen_balance,
        request_reserved_balance,
        manually_frozen_balance,
        reservations,
        next_cursor,
    ) = match page {
        Some(page) => (
            page.breakdown.balance.available_balance.to_string(),
            page.breakdown.balance.frozen_balance.to_string(),
            page.breakdown.active_reserved.to_string(),
            page.breakdown.manually_frozen.to_string(),
            page.reservations,
            page.next_cursor
                .as_ref()
                .map(encode_balance_reservation_cursor),
        ),
        None => {
            let zero = Decimal::ZERO.to_string();
            (
                zero.clone(),
                zero.clone(),
                zero.clone(),
                zero,
                Vec::new(),
                None,
            )
        }
    };

    Ok(Json(AdminBalanceReservationsResponse {
        user_id,
        available_balance,
        total_frozen_balance,
        request_reserved_balance,
        manually_frozen_balance,
        reservations: reservations
            .into_iter()
            .map(|reservation| AdminBalanceReservationInfo {
                request_id: reservation.request_id,
                version: reservation.owner_token,
                amount: reservation.amount.to_string(),
                status: reservation.status,
                expires_at: reservation.expires_at.to_rfc3339(),
                created_at: reservation.created_at.to_rfc3339(),
                updated_at: reservation.updated_at.to_rfc3339(),
            })
            .collect(),
        next_cursor,
    }))
}

/// 管理员按 request_id 及列表版本释放一笔卡死的请求余额预留。
///
/// POST /api/v1/users/{id}/balance/reservations/{request_id}/release
///
/// The reservation version acts as the idempotency key. Transport retries
/// must reuse the same version and reason; an exact retry returns success,
/// while changing the version, actor, or normalized reason returns 409.
pub async fn release_user_balance_reservation(
    auth: AuthExtractor,
    Path((user_id, request_id)): Path<(Uuid, Uuid)>,
    State(state): State<AppState>,
    Json(req): Json<ReleaseBalanceReservationRequest>,
) -> Result<Json<ReleaseBalanceReservationResponse>> {
    let _ = validate_balance_target(&auth, &state, user_id).await?;
    let reason = validate_reservation_release_reason(&req.reason)?;

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;
    let Some(release) = balance_service
        .admin_release_request_reservation(
            user_id,
            request_id,
            req.expected_version,
            auth.user_id,
            reason,
        )
        .await
        .map_err(ApiError::from)?
    else {
        return Err(ApiError::Conflict(format!(
            "Balance reservation {request_id} is no longer active or its version changed; refresh the balance details before retrying"
        )));
    };
    let breakdown = release.breakdown;
    let released_reservation = release.released_reservation;

    Ok(Json(ReleaseBalanceReservationResponse {
        success: true,
        message: "Request balance reservation released".to_string(),
        user_id,
        request_id,
        released_amount: released_reservation.amount.to_string(),
        reason: reason.to_string(),
        new_available_balance: breakdown.balance.available_balance.to_string(),
        new_total_frozen_balance: breakdown.balance.frozen_balance.to_string(),
        request_reserved_balance: breakdown.active_reserved.to_string(),
        manually_frozen_balance: breakdown.manually_frozen.to_string(),
        released_by: auth.user_id,
        warning: ADMIN_RESERVATION_RELEASE_WARNING.to_string(),
    }))
}

/// 更新用户余额
///
/// POST /api/v1/users/{id}/balance
///
/// `Idempotency-Key` is required. A retry of the same logical operation must
/// reuse the same key; changing the operation, target, actor, amount, or reason
/// while reusing it returns HTTP 409.
pub async fn update_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    let ctx = validate_balance_request(&auth, &state, user_id, &req, false).await?;
    let idempotency_key = require_admin_balance_idempotency_key(&headers)?.to_string();

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;

    // 金额检查、幂等键抢占和余额变更均在数据库事务内完成，不在这里做
    // TOCTOU 式的余额预检查。
    let (kind, amount) = if ctx.amount > Decimal::ZERO {
        (
            keycompute_billing::balance::ManualBalanceOperationKind::Recharge,
            ctx.amount,
        )
    } else {
        (
            keycompute_billing::balance::ManualBalanceOperationKind::Consume,
            -ctx.amount,
        )
    };
    let outcome = balance_service
        .apply_admin_manual_operation(
            kind,
            ctx.tenant_id,
            user_id,
            auth.user_id,
            amount,
            &ctx.reason,
            &idempotency_key,
        )
        .await
        .map_err(ApiError::from)?;
    let keycompute_billing::balance::ManualBalanceOperationDecision::Completed(outcome) = outcome
    else {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different balance operation, target, actor, amount, or reason"
                .to_string(),
        ));
    };
    let response_amount =
        if kind == keycompute_billing::balance::ManualBalanceOperationKind::Consume {
            -outcome.amount
        } else {
            outcome.amount
        };

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance updated",
        "user_id": outcome.user_id,
        "amount": response_amount.to_string(),
        "reason": outcome.reason,
        "available_balance_before": outcome.balance_before.to_string(),
        "new_balance": outcome.balance_after.to_string(),
        "updated_by": outcome.actor_user_id,
    })))
}

/// 冻结用户余额
///
/// POST /api/v1/users/{id}/balance/freeze
///
/// `Idempotency-Key` is required. A retry of the same logical operation must
/// reuse the same key; changing the operation, target, actor, amount, or reason
/// while reusing it returns HTTP 409.
pub async fn freeze_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    let ctx = validate_balance_request(&auth, &state, user_id, &req, true).await?;
    let idempotency_key = require_admin_balance_idempotency_key(&headers)?.to_string();

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;

    let outcome = balance_service
        .apply_admin_manual_operation(
            keycompute_billing::balance::ManualBalanceOperationKind::Freeze,
            ctx.tenant_id,
            user_id,
            auth.user_id,
            ctx.amount,
            &ctx.reason,
            &idempotency_key,
        )
        .await
        .map_err(ApiError::from)?;
    let keycompute_billing::balance::ManualBalanceOperationDecision::Completed(outcome) = outcome
    else {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different balance operation, target, actor, amount, or reason"
                .to_string(),
        ));
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance frozen",
        "user_id": outcome.user_id,
        "amount": outcome.amount.to_string(),
        "reason": outcome.reason,
        "available_balance_before": outcome.balance_before.to_string(),
        "new_available_balance": outcome.balance_after.to_string(),
        "new_frozen_balance": outcome.frozen_balance_after.to_string(),
        "updated_by": outcome.actor_user_id,
    })))
}

/// 解冻用户余额
///
/// POST /api/v1/users/{id}/balance/unfreeze
///
/// `Idempotency-Key` is required. A retry of the same logical operation must
/// reuse the same key; changing the operation, target, actor, amount, or reason
/// while reusing it returns HTTP 409.
pub async fn unfreeze_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    let ctx = validate_balance_request(&auth, &state, user_id, &req, true).await?;
    let idempotency_key = require_admin_balance_idempotency_key(&headers)?.to_string();

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;

    let outcome = balance_service
        .apply_admin_manual_operation(
            keycompute_billing::balance::ManualBalanceOperationKind::Unfreeze,
            ctx.tenant_id,
            user_id,
            auth.user_id,
            ctx.amount,
            &ctx.reason,
            &idempotency_key,
        )
        .await
        .map_err(ApiError::from)?;
    let keycompute_billing::balance::ManualBalanceOperationDecision::Completed(outcome) = outcome
    else {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different balance operation, target, actor, amount, or reason"
                .to_string(),
        ));
    };

    // balance_before 跟踪的是操作前可用余额，不含冻结余额信息。
    // 根据持久化结果快照反推冻结前值：
    // frozen_before = updated.frozen_balance + amount（因为解冻操作: frozen -= amount）
    let frozen_before = outcome.frozen_balance_after + outcome.amount;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance unfrozen",
        "user_id": outcome.user_id,
        "amount": outcome.amount.to_string(),
        "reason": outcome.reason,
        "frozen_balance_before": frozen_before.to_string(),
        "new_available_balance": outcome.balance_after.to_string(),
        "new_frozen_balance": outcome.frozen_balance_after.to_string(),
        "updated_by": outcome.actor_user_id,
    })))
}

/// 列出用户的所有 API Keys（Admin 视图）
///
/// GET /api/v1/users/{id}/api-keys
pub async fn list_all_api_keys(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let keys = ProduceAiKey::find_by_user(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?;

    let result: Vec<serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            serde_json::json!({
                "id": k.id,
                "user_id": k.user_id,
                "name": k.name,
                "key_preview": k.produce_ai_key_preview,
                "is_active": !k.revoked,
                "revoked": k.revoked,
                "revoked_at": k.revoked_at.map(|t| t.to_rfc3339()),
                "created_at": k.created_at.to_rfc3339(),
                "last_used_at": k.last_used_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();

    Ok(Json(result))
}

// ==================== 租户管理 ====================

/// 租户信息
#[derive(Debug, Serialize)]
pub struct TenantInfo {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub user_count: i64,
    pub is_active: bool,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct TenantListQueryParams {
    pub search: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TenantListResponse {
    pub tenants: Vec<TenantInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 列出所有租户
///
/// GET /api/v1/tenants
pub async fn list_tenants(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<TenantListQueryParams>,
) -> Result<Json<TenantListResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let tenants = Tenant::find_all_filtered(pool, params.search.as_deref(), page_size, offset)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;
    let total = Tenant::count_all_filtered(pool, params.search.as_deref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenants: {}", e)))?;

    // 批量统计各租户用户数量（避免 N+1 查询）
    let tenant_ids: Vec<Uuid> = tenants.iter().map(|t| t.id).collect();
    let user_counts = User::count_by_tenants(pool, &tenant_ids)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

    let result: Vec<TenantInfo> = tenants
        .into_iter()
        .map(|tenant| {
            let is_active = tenant.is_active();
            let description = tenant.description.clone();

            TenantInfo {
                id: tenant.id,
                name: tenant.name,
                description,
                user_count: user_counts.get(&tenant.id).copied().unwrap_or(0),
                is_active,
                created_at: tenant.created_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(TenantListResponse {
        tenants: result,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_user_info_serialization() {
        let user = AdminUserInfo {
            id: Uuid::new_v4(),
            email: "admin@example.com".to_string(),
            name: Some("Admin".to_string()),
            role: "admin".to_string(),
            tenant_id: Uuid::new_v4(),
            tenant_name: "Test".to_string(),
            balance: 1000.0,
            frozen_balance: 0.0,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("admin@example.com"));
    }

    #[test]
    fn balance_reservation_response_keeps_exact_amount_strings() {
        let user_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let version = Uuid::new_v4();
        let response = AdminBalanceReservationsResponse {
            user_id,
            available_balance: "988.7800000000".to_string(),
            total_frozen_balance: "899530.0300000000".to_string(),
            request_reserved_balance: "899530.0300000000".to_string(),
            manually_frozen_balance: "0".to_string(),
            reservations: vec![AdminBalanceReservationInfo {
                request_id,
                version,
                amount: "899530.0300000000".to_string(),
                status: "active".to_string(),
                expires_at: "2026-09-09T03:10:00Z".to_string(),
                created_at: "2026-09-09T01:00:00Z".to_string(),
                updated_at: "2026-09-09T01:00:00Z".to_string(),
            }],
            next_cursor: Some("opaque-next-page".to_string()),
        };

        let json = serde_json::to_value(response).expect("response must serialize");
        assert_eq!(json["user_id"], user_id.to_string());
        assert_eq!(json["total_frozen_balance"], "899530.0300000000");
        assert_eq!(json["request_reserved_balance"], "899530.0300000000");
        assert_eq!(json["manually_frozen_balance"], "0");
        assert_eq!(
            json["reservations"][0]["request_id"],
            request_id.to_string()
        );
        assert_eq!(json["reservations"][0]["version"], version.to_string());
        assert_eq!(json["next_cursor"], "opaque-next-page");
    }

    #[test]
    fn balance_reservation_cursor_is_opaque_stable_and_round_trips() {
        let cursor = BalanceReservationPageCursor {
            created_at: chrono::DateTime::parse_from_rfc3339("2026-09-09T01:00:00.123456Z")
                .expect("cursor timestamp should parse")
                .with_timezone(&chrono::Utc),
            id: Uuid::parse_str("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                .expect("cursor id should parse"),
        };

        let encoded = encode_balance_reservation_cursor(&cursor);
        assert!(!encoded.contains(' '));
        assert_eq!(encoded, encode_balance_reservation_cursor(&cursor));
        assert_eq!(
            decode_balance_reservation_cursor(&encoded).expect("cursor should decode"),
            cursor
        );
        assert!(decode_balance_reservation_cursor("not a cursor").is_err());
    }

    #[test]
    fn release_balance_reservation_response_has_exact_client_wire_contract() {
        let user_id =
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("valid user id");
        let request_id =
            Uuid::parse_str("22222222-2222-4222-8222-222222222222").expect("valid request id");
        let released_by =
            Uuid::parse_str("33333333-3333-4333-8333-333333333333").expect("valid actor id");
        let response = ReleaseBalanceReservationResponse {
            success: true,
            message: "Request balance reservation released".to_string(),
            user_id,
            request_id,
            released_amount: "899530.0300000000".to_string(),
            reason: "confirmed upstream failure".to_string(),
            new_available_balance: "900518.8100000000".to_string(),
            new_total_frozen_balance: "0".to_string(),
            request_reserved_balance: "0".to_string(),
            manually_frozen_balance: "0".to_string(),
            released_by,
            warning: ADMIN_RESERVATION_RELEASE_WARNING.to_string(),
        };

        let json = serde_json::to_value(response).expect("response must serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "success": true,
                "message": "Request balance reservation released",
                "user_id": "11111111-1111-4111-8111-111111111111",
                "request_id": "22222222-2222-4222-8222-222222222222",
                "released_amount": "899530.0300000000",
                "reason": "confirmed upstream failure",
                "new_available_balance": "900518.8100000000",
                "new_total_frozen_balance": "0",
                "request_reserved_balance": "0",
                "manually_frozen_balance": "0",
                "released_by": "33333333-3333-4333-8333-333333333333",
                "warning": ADMIN_RESERVATION_RELEASE_WARNING,
            })
        );
    }

    #[test]
    fn release_balance_reservation_request_requires_a_reason_field() {
        let version = Uuid::new_v4();
        let request: ReleaseBalanceReservationRequest = serde_json::from_value(serde_json::json!({
            "expected_version": version,
            "reason": "confirmed upstream failure"
        }))
        .expect("release request must deserialize");
        assert_eq!(request.expected_version, version);
        assert_eq!(request.reason, "confirmed upstream failure");
        assert!(
            serde_json::from_value::<ReleaseBalanceReservationRequest>(serde_json::json!({}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ReleaseBalanceReservationRequest>(serde_json::json!({
                "reason": "confirmed upstream failure"
            }))
            .is_err()
        );
        assert!(matches!(
            validate_reservation_release_reason(" \n\t"),
            Err(ApiError::BadRequest(_))
        ));
        let too_long = "x".repeat(ADMIN_RESERVATION_RELEASE_REASON_MAX_CHARS + 1);
        assert!(matches!(
            validate_reservation_release_reason(&too_long),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn administrator_balance_idempotency_key_is_required_and_strict_ascii() {
        let headers = HeaderMap::new();
        assert!(matches!(
            require_admin_balance_idempotency_key(&headers),
            Err(ApiError::BadRequest(message)) if message.contains("required")
        ));

        for invalid in ["", " ", " key", "key ", "key\tvalue", "é"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "idempotency-key",
                axum::http::HeaderValue::from_bytes(invalid.as_bytes())
                    .expect("test header bytes should be representable"),
            );
            assert!(
                matches!(
                    require_admin_balance_idempotency_key(&headers),
                    Err(ApiError::BadRequest(_))
                ),
                "{invalid:?} must be rejected"
            );
        }

        let maximum = "k".repeat(ADMIN_BALANCE_IDEMPOTENCY_KEY_MAX_BYTES);
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            axum::http::HeaderValue::from_str(&maximum).expect("maximum key should be valid"),
        );
        assert_eq!(
            require_admin_balance_idempotency_key(&headers).expect("maximum key should pass"),
            maximum
        );

        let too_long = "k".repeat(ADMIN_BALANCE_IDEMPOTENCY_KEY_MAX_BYTES + 1);
        headers.insert(
            "idempotency-key",
            axum::http::HeaderValue::from_str(&too_long).expect("long key is still a valid header"),
        );
        assert!(matches!(
            require_admin_balance_idempotency_key(&headers),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn administrator_balance_reason_is_trimmed_and_bounded() {
        assert_eq!(
            normalize_admin_balance_reason(" \n audit reason \t").expect("reason should normalize"),
            "audit reason"
        );
        assert!(matches!(
            normalize_admin_balance_reason(" \n\t "),
            Err(ApiError::BadRequest(_))
        ));
        assert!(
            normalize_admin_balance_reason(
                &"界".repeat(keycompute_billing::balance::MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS)
            )
            .is_ok()
        );
        assert!(matches!(
            normalize_admin_balance_reason(
                &"界".repeat(
                    keycompute_billing::balance::MAX_ADMIN_BALANCE_OPERATION_REASON_CHARS + 1
                )
            ),
            Err(ApiError::BadRequest(_))
        ));
    }

    fn make_test_user(id: Uuid, role: &str) -> User {
        use chrono::Utc;

        User {
            id,
            tenant_id: Uuid::new_v4(),
            email: "target@example.com".to_string(),
            name: Some("Target".to_string()),
            role: role.to_string(),
            token_version: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_validate_role_change_request_requires_protected_user_permission() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "user");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(msg) if msg.contains("permission required")));
    }

    #[test]
    fn test_validate_role_change_request_rejects_self_role_change() {
        let user_id = Uuid::new_v4();
        let auth = AuthExtractor::new(user_id, Uuid::new_v4(), Uuid::new_v4(), "system")
            .with_permissions(vec![Permission::ManageProtectedUsers]);
        let target = make_test_user(user_id, "system");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("own role")));
    }

    #[test]
    fn test_validate_role_change_request_rejects_modifying_system_role() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "system")
            .with_permissions(vec![Permission::ManageProtectedUsers]);
        let target = make_test_user(Uuid::new_v4(), "system");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("cannot be modified")));
    }

    #[test]
    fn test_validate_user_delete_request_rejects_self_delete() {
        let user_id = Uuid::new_v4();
        let auth = AuthExtractor::new(user_id, Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(user_id, "admin");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("yourself")));
    }

    #[test]
    fn test_validate_user_delete_request_rejects_system_user() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "system");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("cannot be deleted")));
    }

    #[test]
    fn test_validate_user_delete_request_requires_protected_user_permission_for_admin_target() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "admin");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(msg) if msg.contains("permission required")));
    }

    #[test]
    fn test_validate_user_delete_request_allows_system_to_delete_admin_target() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "user")
            .with_permissions(vec![Permission::ManageProtectedUsers]);
        let target = make_test_user(Uuid::new_v4(), "admin");
        assert!(validate_user_delete_request(&auth, target.id, &target).is_ok());
    }
}
