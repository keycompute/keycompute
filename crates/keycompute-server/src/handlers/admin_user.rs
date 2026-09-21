//! 用户管理处理器
//!
//! 处理需要 Admin 权限的用户管理请求

use crate::{
    error::{ApiError, Result},
    extractors::GlobalConsoleAuth,
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
use keycompute_db::models::account::Account;
use keycompute_db::models::api_key::ProduceAiKey;
use keycompute_db::models::tenant::{
    CreateTenantRequest as DbCreateTenantRequest, Tenant,
    UpdateTenantRequest as DbUpdateTenantRequest,
};
use keycompute_db::models::user::User;
use keycompute_db::models::user_credential::UserCredential;
use keycompute_types::PlatformRole;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
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
fn global_user_info(user: User, last_login_at: Option<String>) -> AdminUserInfo {
    AdminUserInfo {
        id: user.id,
        email: user.email,
        name: user.name,
        platform_role: user.platform_role,
        status: user.status,
        created_at: user.created_at.to_rfc3339(),
        updated_at: user.updated_at.to_rfc3339(),
        last_login_at,
    }
}
fn audit_actor(auth: &GlobalConsoleAuth) -> keycompute_db::AuditContext {
    keycompute_db::AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: None,
    }
}
fn platform_manage(auth: &GlobalConsoleAuth) -> Result<keycompute_types::PlatformScope> {
    auth.require_platform(AuthorizationAction::ManagePlatform)
        .map_err(ApiError::from)
}
async fn platform_audit(
    tx: &sea_orm::DatabaseTransaction,
    auth: &GlobalConsoleAuth,
    action: &str,
    resource: &str,
    id: Uuid,
    details: serde_json::Value,
) -> Result<()> {
    keycompute_db::TenantAuditEvent::append(
        tx,
        keycompute_types::AuditScopeType::Platform,
        None,
        &audit_actor(auth),
        action,
        resource,
        Some(&id.to_string()),
        keycompute_types::AuditResult::Success,
        details,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(())
}
pub async fn list_all_users(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<UserListQueryParams>,
) -> Result<Json<UserListResponse>> {
    platform_manage(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let (page, page_size, offset) =
        normalize_list_pagination(Some(query.page), Some(query.page_size), None, None);
    let users = User::find_platform_filtered(
        pool.write_conn(),
        platform_manage(&auth)?,
        query.platform_role,
        query.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let total = User::count_platform_filtered(
        pool.write_conn(),
        platform_manage(&auth)?,
        query.platform_role,
        query.search.as_deref(),
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let ids = users.iter().map(|u| u.id).collect::<Vec<_>>();
    let credentials = UserCredential::find_by_user_ids(pool.write_conn(), &ids)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let users = users
        .into_iter()
        .map(|user| {
            let last = credentials
                .get(&user.id)
                .and_then(|c| c.last_login_at)
                .map(|time| time.to_rfc3339());
            global_user_info(user, last)
        })
        .collect();
    Ok(Json(UserListResponse {
        users,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}
pub async fn get_user_by_id(
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<AdminUserInfo>> {
    platform_manage(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let user = User::find_platform(pool.write_conn(), platform_manage(&auth)?, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("user not found".into()))?;
    let credential = UserCredential::find_by_user_id(pool.write_conn(), id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(global_user_info(
        user,
        credential
            .and_then(|c| c.last_login_at)
            .map(|t| t.to_rfc3339()),
    )))
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
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<AdminUserInfo>> {
    platform_manage(&auth)?;
    let reason =
        normalize_admin_balance_reason(req.reason.as_deref().unwrap_or("platform profile update"))?;
    if (req.platform_role.is_some() || req.status.is_some()) && req.reason.is_none() {
        return Err(ApiError::BadRequest(
            "a reason is required for security changes".into(),
        ));
    }
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = User::find_by_id(&tx, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("user not found".into()))?;
    let role = req.platform_role.unwrap_or(
        user.platform_role()
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    let status = req.status.unwrap_or(
        user.user_status()
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    let user = User::set_security(&tx, id, role, status, &audit_actor(&auth))
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let user = user
        .update_in_tx(&tx, &keycompute_db::UpdateUserRequest { name: req.name })
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    platform_audit(
        &tx,
        &auth,
        "user.update",
        "user",
        id,
        serde_json::json!({"reason":reason}),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(Json(global_user_info(user, None)))
}
pub async fn delete_user(
    auth: GlobalConsoleAuth,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    platform_manage(&auth)?;
    if auth.user_id == id {
        return Err(ApiError::BadRequest("cannot delete yourself".into()));
    }
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = User::find_by_id(&tx, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("user not found".into()))?;
    let user = User::set_security(
        &tx,
        id,
        user.platform_role()
            .map_err(|e| ApiError::Internal(e.to_string()))?,
        keycompute_types::UserStatus::Suspended,
        &audit_actor(&auth),
    )
    .await
    .map_err(|e| ApiError::Conflict(e.to_string()))?;
    user.delete(&tx)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    platform_audit(&tx, &auth, "user.delete", "user", id, serde_json::json!({})).await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(Json(serde_json::json!({"success":true,"user_id":id})))
}

#[derive(Debug, Deserialize)]
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

fn decode_balance_reservation_cursor(value: &str) -> Result<BalanceReservationPageCursor> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))?;
    serde_json::from_slice(&decoded)
        .map_err(|_| ApiError::BadRequest("balance_reservations_invalid_cursor".to_string()))
}

#[derive(Debug, Deserialize)]
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
        .find_breakdown_page_by_user(query.tenant_id, user_id, cursor, limit)
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
    auth: GlobalConsoleAuth,
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
            req.tenant_id,
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
    auth: GlobalConsoleAuth,
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
    auth: GlobalConsoleAuth,
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
    auth: GlobalConsoleAuth,
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

    let keys = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 ORDER BY created_at DESC,id DESC",
        [target.tenant_id.into(),user_id.into()],
    )).all(pool.write_conn()).await
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
pub struct CreateTenantRequest {
    pub owner_user_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
}

/// 更新租户请求（Admin）。
#[derive(Debug, Deserialize)]
pub struct UpdateTenantRequest {
    pub name: Option<String>,
    pub status: Option<String>,
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
        ApiError::Internal(format!("Failed to {operation} tenant: {message}"))
    }
}

fn is_tenant_unique_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("duplicate") || message.contains("unique")
}

async fn build_tenant_info(db: &impl ConnectionTrait, tenant: Tenant) -> Result<TenantInfo> {
    let user_count = Tenant::count_users(db, tenant.id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenant users: {e}")))?;
    let account_count = Tenant::count_accounts(db, tenant.id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenant accounts: {e}")))?;
    let is_active = tenant.is_active();
    Ok(TenantInfo {
        id: tenant.id,
        name: tenant.name,
        slug: tenant.slug,
        description: tenant.description,
        user_count,
        account_count,
        status: tenant.status.clone(),
        is_active,
        created_at: tenant.created_at.to_rfc3339(),
        updated_at: tenant.updated_at.to_rfc3339(),
    })
}

/// 创建租户。
///
/// POST /api/v1/tenants
pub async fn create_tenant(
    auth: GlobalConsoleAuth,
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
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let tenant = Tenant::create_owned(
        &tx,
        &DbCreateTenantRequest {
            name: name.to_string(),
            slug,
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        },
        req.owner_user_id,
        &audit_actor(&auth),
    )
    .await
    .map_err(|e| map_tenant_db_error(e, "create"))?;
    let info = build_tenant_info(&tx, tenant).await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(Json(info))
}

/// 更新租户名称或状态。
///
/// PUT /api/v1/tenants/{id}
pub async fn update_tenant(
    auth: GlobalConsoleAuth,
    Path(tenant_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateTenantRequest>,
) -> Result<Json<TenantInfo>> {
    platform_manage(&auth)?;
    if let Some(name) = req.name.as_deref()
        && (name.trim().is_empty() || name.chars().count() > 255)
    {
        return Err(ApiError::BadRequest("Invalid tenant name".to_string()));
    }
    let status = req
        .status
        .as_deref()
        .map(normalize_tenant_status)
        .transpose()?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin tenant update: {e}")))?;
    txn.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let tenant = Tenant::find_by_id_for_update(&txn, tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find tenant: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Tenant not found: {tenant_id}")))?;
    if tenant.slug == "default" && status == Some("inactive") {
        return Err(ApiError::Forbidden(
            "The system tenant cannot be deactivated".to_string(),
        ));
    }
    let tenant = tenant
        .update(
            &txn,
            &DbUpdateTenantRequest {
                name: req.name.map(|value| value.trim().to_string()),
                description: None,
                status: status.map(|value| {
                    value
                        .parse::<keycompute_types::TenantStatus>()
                        .expect("normalized tenant status")
                }),
                default_rpm_limit: None,
                default_tpm_limit: None,
            },
        )
        .await
        .map_err(|e| map_tenant_db_error(e, "update"))?;
    platform_audit(
        &txn,
        &auth,
        "tenant.update",
        "tenant",
        tenant_id,
        serde_json::json!({"status":tenant.status}),
    )
    .await?;
    let info = build_tenant_info(&txn, tenant).await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit tenant update: {e}")))?;
    Ok(Json(info))
}

/// 删除租户。只有没有用户、渠道账号和租户级定价的租户才允许删除。
///
/// DELETE /api/v1/tenants/{id}
pub async fn delete_tenant(
    auth: GlobalConsoleAuth,
    Path(tenant_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    platform_manage(&auth)?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let txn = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin tenant deletion: {e}")))?;
    txn.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let tenant = Tenant::find_by_id_for_update(&txn, tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find tenant: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Tenant not found: {tenant_id}")))?;
    if tenant.slug == "default" {
        let _ = txn.rollback().await;
        return Err(ApiError::Forbidden(
            "The system tenant cannot be deleted".to_string(),
        ));
    }
    let user_count = Tenant::count_users(&txn, tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenant users: {e}")))?;
    let account_count = Tenant::count_accounts(&txn, tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenant accounts: {e}")))?;
    if user_count > 1 || account_count > 0 {
        let _ = txn.rollback().await;
        return Err(ApiError::Conflict(format!(
            "Tenant cannot be deleted while it has {user_count} user(s) and {account_count} channel account(s)"
        )));
    }
    tenant
        .delete_in_tx(&txn)
        .await
        .map_err(|e| map_tenant_db_error(e, "delete"))?;
    platform_audit(
        &txn,
        &auth,
        "tenant.delete",
        "tenant",
        tenant_id,
        serde_json::json!({}),
    )
    .await?;
    txn.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit tenant deletion: {e}")))?;
    Ok(Json(serde_json::json!({
        "message": "Tenant deleted successfully",
        "tenant_id": tenant_id,
    })))
}

/// 列出所有租户
///
/// GET /api/v1/tenants
pub async fn list_tenants(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<TenantListQueryParams>,
) -> Result<Json<TenantListResponse>> {
    platform_manage(&auth)?;

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    // Tenant lifecycle state and the management counters are read-after-write
    // sensitive: use the writer so a just-created/closed tenant is reflected
    // immediately instead of waiting for a replica to catch up.
    let writer = pool.write_conn();

    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let tenants = Tenant::find_all_filtered(writer, params.search.as_deref(), page_size, offset)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;
    let total = Tenant::count_all_filtered(writer, params.search.as_deref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tenants: {}", e)))?;

    // 批量统计各租户用户数量（避免 N+1 查询）
    let tenant_ids: Vec<Uuid> = tenants.iter().map(|t| t.id).collect();
    let user_counts =
        User::count_memberships_platform(writer, platform_manage(&auth)?, &tenant_ids)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;
    let account_counts = Account::count_by_tenants(writer, &tenant_ids)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count channel accounts: {}", e)))?;

    let result: Vec<TenantInfo> = tenants
        .into_iter()
        .map(|tenant| {
            let is_active = tenant.is_active();
            let description = tenant.description.clone();

            TenantInfo {
                id: tenant.id,
                name: tenant.name,
                slug: tenant.slug,
                description,
                user_count: user_counts.get(&tenant.id).copied().unwrap_or(0),
                account_count: account_counts.get(&tenant.id).copied().unwrap_or(0),
                status: tenant.status.clone(),
                is_active,
                created_at: tenant.created_at.to_rfc3339(),
                updated_at: tenant.updated_at.to_rfc3339(),
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
