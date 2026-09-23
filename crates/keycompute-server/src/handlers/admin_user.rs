//! 用户管理处理器
//!
//! 处理需要 Admin 权限的用户管理请求

use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use keycompute_auth::AuthorizationAction;
use keycompute_billing::balance::{
    BalanceReservationPageCursor, MAX_BALANCE_RESERVATION_PAGE_SIZE,
};
use keycompute_db::models::api_key::ProduceAiKey;
use keycompute_db::models::platform_identity::{
    PlatformIdentity, PlatformTenantInfo, PlatformUserInfo, PlatformUserPatch,
};
use keycompute_db::models::tenant::{
    CreateTenantRequest as DbCreateTenantRequest, UpdateTenantRequest as DbUpdateTenantRequest,
};
use keycompute_db::models::user::User;
use keycompute_types::PlatformRole;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ==================== 用户管理 ====================

/// Platform identity responses never project a user's arbitrary membership.
#[derive(Debug, Serialize)]
pub struct AdminUserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserListQueryParams {
    pub platform_role: Option<PlatformRole>,
    pub search: Option<String>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}
fn default_page() -> i64 {
    1
}
fn default_page_size() -> i64 {
    20
}
#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<AdminUserInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
fn global_user_info(user: PlatformUserInfo) -> AdminUserInfo {
    AdminUserInfo {
        id: user.id,
        email: user.email,
        name: user.name,
        platform_role: user.platform_role,
        status: user.status,
        created_at: user.created_at.to_rfc3339(),
        updated_at: user.updated_at.to_rfc3339(),
        last_login_at: user.last_login_at.map(|v| v.to_rfc3339()),
    }
}
fn platform_manage(auth: &GlobalConsoleAuth) -> Result<keycompute_types::PlatformScope> {
    auth.require_platform(AuthorizationAction::ManagePlatform)
        .map_err(ApiError::from)
}
fn identity_pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform identity storage unavailable".into()))
}
fn identity_error(error: keycompute_db::DbError) -> ApiError {
    if error.is_not_found() {
        return ApiError::NotFound("Platform identity resource not found".into());
    }
    if let keycompute_db::DbError::Other(code) = &error {
        if code == "financial_authority_invalid" {
            return ApiError::Forbidden("Current root authority required".into());
        }
        if code == "platform_identity_request_invalid" {
            return ApiError::BadRequest("Invalid platform identity request".into());
        }
        if code == "protected_default_tenant" {
            return ApiError::Forbidden("The default tenant is protected".into());
        }
        if code == "tenant_retained_members_or_accounts" {
            return ApiError::Conflict("Tenant retains members or accounts".into());
        }
    }
    let text = error.to_string();
    if [
        "active root",
        "active admin",
        "violates foreign key",
        "still referenced",
    ]
    .iter()
    .any(|v| text.contains(v))
    {
        return ApiError::Conflict(
            "Identity ownership or retained-history invariant prevents this change".into(),
        );
    }
    ApiError::ServiceUnavailable("Platform identity operation unavailable".into())
}
pub async fn list_all_users(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<UserListQueryParams>,
) -> Result<Json<UserListResponse>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    let (page, page_size, offset) =
        normalize_list_pagination(Some(query.page), Some(query.page_size), None, None);
    let rows = PlatformIdentity::users(
        identity_pool(&state)?.write_conn(),
        scope,
        query.platform_role,
        query.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(UserListResponse {
        users: rows.items.into_iter().map(global_user_info).collect(),
        total: rows.total,
        page,
        page_size,
        total_pages: total_pages(rows.total, page_size),
    }))
}
pub async fn get_user_by_id(
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<AdminUserInfo>> {
    let row = PlatformIdentity::user(
        identity_pool(&state)?.write_conn(),
        crate::financial_auth::global_scope(&auth.0)?,
        id,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(global_user_info(row)))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub platform_role: Option<PlatformRole>,
    pub status: Option<keycompute_types::UserStatus>,
    pub reason: Option<String>,
}
pub async fn update_user(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<AdminUserInfo>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    if (req.platform_role.is_some() || req.status.is_some()) && req.reason.is_none() {
        return Err(ApiError::BadRequest(
            "a reason is required for security changes".into(),
        ));
    }
    let reason =
        normalize_admin_balance_reason(req.reason.as_deref().unwrap_or("platform profile update"))?;
    let _fence = state.display_cache.mutation_guard();
    let row = PlatformIdentity::update_user(
        identity_pool(&state)?.write_conn(),
        scope,
        &crate::financial_auth::audit(&auth.0, request_id),
        id,
        &PlatformUserPatch {
            name: req.name,
            platform_role: req.platform_role,
            status: req.status,
        },
        &reason,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(global_user_info(row)))
}
pub async fn delete_user(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    if auth.user_id == id {
        return Err(ApiError::BadRequest("cannot delete yourself".into()));
    }
    let _fence = state.display_cache.mutation_guard();
    PlatformIdentity::delete_user(
        identity_pool(&state)?.write_conn(),
        scope,
        &crate::financial_auth::audit(&auth.0, request_id),
        id,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(serde_json::json!({"success":true,"user_id":id})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateBalanceRequest {
    pub tenant_id: Uuid,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminBalanceReservationsQuery {
    pub tenant_id: Uuid,
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

pub(crate) fn decode_balance_reservation_cursor(
    value: &str,
) -> Result<BalanceReservationPageCursor> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))?;
    serde_json::from_slice(&decoded)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBalanceReservationRequest {
    pub tenant_id: Uuid,
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
    auth: &GlobalConsoleAuth,
    state: &AppState,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<User> {
    platform_manage(auth)?;
    if tenant_id.is_nil() {
        return Err(ApiError::BadRequest("target tenant is required".into()));
    }
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    User::find_platform_member(
        pool.write_conn(),
        platform_manage(auth)?,
        tenant_id,
        user_id,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or_else(|| ApiError::NotFound("tenant member not found".into()))
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
    auth: &GlobalConsoleAuth,
    state: &AppState,
    user_id: Uuid,
    req: &UpdateBalanceRequest,
    require_positive: bool,
) -> Result<BalanceOpContext> {
    validate_balance_target(auth, state, req.tenant_id, user_id).await?;

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
        tenant_id: req.tenant_id,
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
    auth: GlobalConsoleAuth,
    Path(user_id): Path<Uuid>,
    Query(query): Query<AdminBalanceReservationsQuery>,
    State(state): State<AppState>,
) -> Result<Json<AdminBalanceReservationsResponse>> {
    let _ = validate_balance_target(&auth, &state, query.tenant_id, user_id).await?;

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
        .find_breakdown_page_in_scope(
            crate::financial_auth::root_scope(&auth.0, query.tenant_id)?,
            user_id,
            cursor,
            limit,
        )
        .await
        .map_err(ApiError::from)?;

    reservation_page_response(user_id, page)
}
pub(crate) fn reservation_page_response(
    user_id: Uuid,
    page: Option<keycompute_billing::balance::UserBalanceBreakdownPage>,
) -> Result<Json<AdminBalanceReservationsResponse>> {
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
    auth: GlobalConsoleAuth,
    audit_request_id: RequestId,
    Path((user_id, request_id)): Path<(Uuid, Uuid)>,
    State(state): State<AppState>,
    Json(req): Json<ReleaseBalanceReservationRequest>,
) -> Result<Json<ReleaseBalanceReservationResponse>> {
    let _ = validate_balance_target(&auth, &state, req.tenant_id, user_id).await?;
    let reason = validate_reservation_release_reason(&req.reason)?;

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;
    let Some(release) = balance_service
        .admin_release_request_reservation(
            crate::financial_auth::root_scope(&auth.0, req.tenant_id)?,
            &crate::financial_auth::audit(&auth.0, audit_request_id),
            &keycompute_billing::balance::ReleaseReservationCommand {
                user_id,
                request_id,
                expected_owner_token: req.expected_version,
                reason,
            },
        )
        .await
        .map_err(ApiError::from)?
    else {
        return Err(ApiError::Conflict(format!(
            "Balance reservation {request_id} is no longer active or its version changed; refresh the balance details before retrying"
        )));
    };
    reservation_release_response(user_id, request_id, auth.user_id, reason, release)
}
pub(crate) fn reservation_release_response(
    user_id: Uuid,
    request_id: Uuid,
    actor_user_id: Uuid,
    reason: &str,
    release: keycompute_billing::balance::AdminRequestReservationRelease,
) -> Result<Json<ReleaseBalanceReservationResponse>> {
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
        released_by: actor_user_id,
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
    auth: GlobalConsoleAuth,
    request_id: RequestId,
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
            crate::financial_auth::root_scope(&auth.0, ctx.tenant_id)?,
            &crate::financial_auth::audit(&auth.0, request_id),
            &keycompute_billing::balance::ManualBalanceCommand {
                kind,
                user_id,
                amount,
                reason: &ctx.reason,
                idempotency_key: &idempotency_key,
            },
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
    auth: GlobalConsoleAuth,
    request_id: RequestId,
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
            crate::financial_auth::root_scope(&auth.0, ctx.tenant_id)?,
            &crate::financial_auth::audit(&auth.0, request_id),
            &keycompute_billing::balance::ManualBalanceCommand {
                kind: keycompute_billing::balance::ManualBalanceOperationKind::Freeze,
                user_id,
                amount: ctx.amount,
                reason: &ctx.reason,
                idempotency_key: &idempotency_key,
            },
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
    auth: GlobalConsoleAuth,
    request_id: RequestId,
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
            crate::financial_auth::root_scope(&auth.0, ctx.tenant_id)?,
            &crate::financial_auth::audit(&auth.0, request_id),
            &keycompute_billing::balance::ManualBalanceCommand {
                kind: keycompute_billing::balance::ManualBalanceOperationKind::Unfreeze,
                user_id,
                amount: ctx.amount,
                reason: &ctx.reason,
                idempotency_key: &idempotency_key,
            },
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
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantTarget {
    pub tenant_id: Uuid,
}

pub async fn list_all_api_keys(
    auth: GlobalConsoleAuth,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    Query(target): Query<TenantTarget>,
) -> Result<Json<Vec<serde_json::Value>>> {
    platform_manage(&auth)?;

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let keys = ProduceAiKey::list_platform_member(
        pool.write_conn(),
        platform_manage(&auth)?,
        target.tenant_id,
        user_id,
    )
    .await
    .map_err(|_| {
        ApiError::ServiceUnavailable("API key storage is temporarily unavailable".into())
    })?;

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
    pub slug: String,
    pub description: Option<String>,
    pub user_count: i64,
    pub account_count: i64,
    pub status: String,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 创建租户请求（Admin）。新租户始终从 active 状态开始；Slug 可选，
/// 未提供时由服务端根据名称生成。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantRequest {
    pub owner_user_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
}

/// 更新租户请求（Admin）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenantRequest {
    pub name: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

fn normalize_tenant_status(status: &str) -> Result<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" | "enabled" => Ok("active"),
        "inactive" | "disabled" => Ok("inactive"),
        _ => Err(ApiError::BadRequest(
            "Invalid tenant status, expected active or inactive".to_string(),
        )),
    }
}

fn generated_tenant_slug(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        format!("tenant-{}", Uuid::new_v4().simple())
    } else {
        let truncated = slug.chars().take(90).collect::<String>();
        let truncated = truncated.trim_end_matches('-');
        if truncated.is_empty() {
            format!("tenant-{}", Uuid::new_v4().simple())
        } else {
            truncated.to_string()
        }
    }
}

fn map_tenant_db_error(error: keycompute_db::DbError, operation: &str) -> ApiError {
    if operation == "delete"
        && let keycompute_db::DbError::TenantHasPricingModels { count } = error
    {
        return ApiError::Conflict(format!(
            "Tenant cannot be deleted while it has {count} tenant pricing model(s); delete them first"
        ));
    }
    if operation == "delete"
        && let keycompute_db::DbError::TenantHasPendingResponsesWork {
            pending_settlements,
            pending_reservations,
            in_progress_claims,
        } = error
    {
        return ApiError::Conflict(format!(
            "Tenant cannot be deleted while it has {pending_settlements} pending Responses settlement(s), {pending_reservations} active Responses reservation(s), and {in_progress_claims} in-progress Responses request(s); wait for them to finish"
        ));
    }
    if operation == "delete"
        && let keycompute_db::DbError::TenantHasFinancialHistory {
            payment_orders,
            balance_transactions,
            balance_reservations,
        } = error
    {
        return ApiError::Conflict(format!(
            "Tenant cannot be deleted while it retains {payment_orders} payment order(s), {balance_transactions} balance transaction(s), and {balance_reservations} balance reservation(s); preserve or remove the financial history first"
        ));
    }
    let message = error.to_string();
    if is_tenant_unique_error(&message) {
        ApiError::Conflict("A tenant with the same slug already exists".to_string())
    } else {
        identity_error(error)
    }
}

fn is_tenant_unique_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("duplicate") || message.contains("unique")
}

fn build_tenant_info(tenant: PlatformTenantInfo) -> TenantInfo {
    TenantInfo {
        id: tenant.id,
        name: tenant.name,
        slug: tenant.slug,
        description: tenant.description,
        user_count: tenant.user_count,
        account_count: tenant.account_count,
        status: tenant.status,
        is_active: tenant.is_active,
        created_at: tenant.created_at.to_rfc3339(),
        updated_at: tenant.updated_at.to_rfc3339(),
    }
}

/// 创建租户。
///
/// POST /api/v1/tenants
pub async fn create_tenant(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    State(state): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<Json<TenantInfo>> {
    platform_manage(&auth)?;
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest(
            "Tenant name cannot be empty".to_string(),
        ));
    }
    if name.chars().count() > 255 {
        return Err(ApiError::BadRequest("Tenant name is too long".to_string()));
    }
    let slug = req
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| generated_tenant_slug(name));
    if slug.len() > 100
        || !slug.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        || slug.starts_with('-')
        || slug.ends_with('-')
    {
        return Err(ApiError::BadRequest(
            "Tenant slug must use lowercase letters, digits, and hyphens".to_string(),
        ));
    }
    let _fence = state.display_cache.mutation_guard();
    let tenant = PlatformIdentity::create_tenant(
        identity_pool(&state)?.write_conn(),
        crate::financial_auth::global_scope(&auth.0)?,
        &crate::financial_auth::audit(&auth.0, request_id),
        &DbCreateTenantRequest {
            name: name.to_owned(),
            slug,
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        },
        req.owner_user_id,
    )
    .await
    .map_err(|e| map_tenant_db_error(e, "create"))?;
    Ok(Json(build_tenant_info(tenant)))
}

/// 更新租户名称或状态。
///
/// PUT /api/v1/tenants/{id}
pub async fn update_tenant(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateTenantRequest>,
) -> Result<Json<TenantInfo>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    if req
        .name
        .as_ref()
        .is_some_and(|n| n.trim().is_empty() || n.len() > 255)
    {
        return Err(ApiError::BadRequest("Invalid tenant name".into()));
    }
    let status = req
        .status
        .as_deref()
        .map(normalize_tenant_status)
        .transpose()?
        .map(|s| {
            s.parse::<keycompute_types::TenantStatus>()
                .expect("normalized tenant status")
        });
    let _fence = state.display_cache.mutation_guard();
    let tenant = PlatformIdentity::update_tenant(
        identity_pool(&state)?.write_conn(),
        scope,
        &crate::financial_auth::audit(&auth.0, request_id),
        id,
        &DbUpdateTenantRequest {
            name: req.name.map(|v| v.trim().to_owned()),
            description: None,
            status,
            default_rpm_limit: None,
            default_tpm_limit: None,
        },
    )
    .await
    .map_err(|e| map_tenant_db_error(e, "update"))?;
    Ok(Json(build_tenant_info(tenant)))
}

/// 删除租户。只有没有用户、渠道账号和租户级定价的租户才允许删除。
///
/// DELETE /api/v1/tenants/{id}
pub async fn delete_tenant(
    auth: GlobalConsoleAuth,
    request_id: RequestId,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    let _fence = state.display_cache.mutation_guard();
    PlatformIdentity::delete_tenant(
        identity_pool(&state)?.write_conn(),
        scope,
        &crate::financial_auth::audit(&auth.0, request_id),
        id,
    )
    .await
    .map_err(|e| map_tenant_db_error(e, "delete"))?;
    Ok(Json(
        serde_json::json!({"message":"Tenant deleted successfully","tenant_id":id}),
    ))
}

/// 列出所有租户
///
/// GET /api/v1/tenants
pub async fn list_tenants(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<TenantListQueryParams>,
) -> Result<Json<TenantListResponse>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let rows = PlatformIdentity::tenants(
        identity_pool(&state)?.write_conn(),
        scope,
        params.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(TenantListResponse {
        tenants: rows.items.into_iter().map(build_tenant_info).collect(),
        total: rows.total,
        page,
        page_size,
        total_pages: total_pages(rows.total, page_size),
    }))
}
pub async fn get_platform_tenant(
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<TenantInfo>> {
    let row = PlatformIdentity::tenant(
        identity_pool(&state)?.write_conn(),
        crate::financial_auth::global_scope(&auth.0)?,
        id,
    )
    .await
    .map_err(identity_error)?;
    Ok(Json(build_tenant_info(row)))
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
            platform_role: "operator".to_string(),
            status: "active".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("admin@example.com"));
    }

    #[test]
    fn tenant_status_normalization_accepts_only_supported_states() {
        assert_eq!(normalize_tenant_status("active").unwrap(), "active");
        assert_eq!(normalize_tenant_status("DISABLED").unwrap(), "inactive");
        assert!(normalize_tenant_status("suspended").is_err());
    }

    #[test]
    fn tenant_pricing_delete_error_is_client_visible_as_conflict() {
        let error = keycompute_db::DbError::TenantHasPricingModels { count: 1 };
        let mapped = map_tenant_db_error(error, "delete");
        assert!(matches!(mapped, ApiError::Conflict(message) if message.contains("pricing model")));
    }

    #[test]
    fn pending_responses_delete_error_is_client_visible_as_conflict() {
        let error = keycompute_db::DbError::TenantHasPendingResponsesWork {
            pending_settlements: 2,
            pending_reservations: 3,
            in_progress_claims: 1,
        };
        let mapped = map_tenant_db_error(error, "delete");
        assert!(
            matches!(mapped, ApiError::Conflict(message) if message.contains("pending Responses") && message.contains("reservation") && message.contains("in-progress"))
        );
    }

    #[test]
    fn financial_history_delete_error_is_client_visible_as_conflict() {
        let error = keycompute_db::DbError::TenantHasFinancialHistory {
            payment_orders: 2,
            balance_transactions: 3,
            balance_reservations: 1,
        };
        let mapped = map_tenant_db_error(error, "delete");
        assert!(
            matches!(mapped, ApiError::Conflict(message) if message.contains("payment order") && message.contains("balance transaction") && message.contains("financial history"))
        );
    }

    #[test]
    fn generated_tenant_slug_is_bounded_and_has_no_trailing_separator() {
        let slug = generated_tenant_slug(&format!("{} trailing", "a ".repeat(80)));
        assert!(slug.len() <= 90);
        assert!(!slug.ends_with('-'));
        assert!(slug.chars().all(|character| character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'));
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
            "tenant_id": Uuid::new_v4(),
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

    #[test]
    fn global_identity_update_rejects_removed_role_and_tenant_fields() {
        for payload in [
            serde_json::json!({"role":"admin"}),
            serde_json::json!({"tenant_id":Uuid::new_v4()}),
            serde_json::json!({"platform_role":"system"}),
            serde_json::json!({"platform_role":"admin"}),
        ] {
            assert!(serde_json::from_value::<UpdateUserRequest>(payload).is_err());
        }
        assert!(
            serde_json::from_value::<UpdateBalanceRequest>(
                serde_json::json!({"amount":"1","reason":"test"})
            )
            .is_err()
        );
    }
    #[test]
    fn platform_gate_is_independent_of_tenant_role_and_rejects_inference_credentials() {
        use keycompute_types::{AuthorizationSubject, CredentialKind, TenantRole};
        for platform in [
            PlatformRole::Root,
            PlatformRole::Operator,
            PlatformRole::None,
        ] {
            for tenant in [None, Some(TenantRole::Admin), Some(TenantRole::Member)] {
                let id = Uuid::new_v4();
                let ctx = keycompute_auth::AuthContext::new(id, CredentialKind::Jwt)
                    .with_authorization_subject(AuthorizationSubject {
                        user_id: id,
                        platform_role: platform,
                        tenant_id: tenant.map(|_| Uuid::new_v4()),
                        tenant_role: tenant,
                    });
                let auth = GlobalConsoleAuth::try_from(ctx).unwrap();
                assert_eq!(
                    platform_manage(&auth).is_ok(),
                    platform == PlatformRole::Root
                );
            }
            let id = Uuid::new_v4();
            let ctx = keycompute_auth::AuthContext::new(id, CredentialKind::ApiKey)
                .with_authorization_subject(AuthorizationSubject::tenant(
                    id,
                    Uuid::new_v4(),
                    TenantRole::Admin,
                    platform,
                ));
            assert!(GlobalConsoleAuth::try_from(ctx).is_err());
        }
    }
}
