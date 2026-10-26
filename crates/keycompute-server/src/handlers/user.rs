//! 用户自服务处理器
//
//! 处理用户管理自己资源的请求
//! All console roles use these endpoints for their own resources only.

use crate::handlers::pagination::{
    has_explicit_pagination, normalize_list_pagination, total_pages,
};
use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, GlobalConsoleAuth, RequestId},
    key_control_auth::{self, KeyChange},
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use keycompute_auth::{
    DEFAULT_LOCK_DURATION_MINUTES, DEFAULT_MAX_FAILED_ATTEMPTS, PasswordHasher, PasswordValidator,
    ProduceAiKeyValidator,
};
use keycompute_db::models::{
    api_key::{CreateProduceAiKeyRequest, ProduceAiKey},
    usage_log::UserUsageScope,
    user::User,
    user_credential::UserCredential,
};
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use rust_decimal::prelude::ToPrimitive;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Global profile and current membership presentation. A global session needs
/// no tenant selection to read its own identity or select a workspace.
pub async fn get_current_user(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<keycompute_auth::ConsoleSession>> {
    Ok(Json(
        state
            .auth
            .console_session(&auth.0)
            .await
            .map_err(ApiError::from)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
}

pub async fn update_profile(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<Json<keycompute_auth::ConsoleSession>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = User::find_by_id_for_no_key_update(&tx, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .filter(|user| user.status == "active" && user.token_version == auth.token_version)
        .ok_or_else(|| ApiError::Auth("identity has changed".into()))?;
    user.update_in_tx(&tx, &keycompute_db::UpdateUserRequest { name: req.name })
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    append_personal_audit(&tx, &auth, "user.profile").await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    get_current_user(auth, State(state)).await
}

async fn append_personal_audit(
    tx: &sea_orm::DatabaseTransaction,
    auth: &GlobalConsoleAuth,
    action: &str,
) -> Result<()> {
    let actor = keycompute_db::AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: None,
    };
    keycompute_db::TenantAuditEvent::append(
        tx,
        keycompute_types::AuditScopeType::User,
        auth.selected_tenant_id,
        &actor,
        action,
        "user",
        Some(&auth.user_id.to_string()),
        keycompute_types::AuditResult::Success,
        serde_json::json!({}),
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectTenantRequest {
    pub tenant_id: Option<Uuid>,
}

pub async fn select_my_tenant(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Json(req): Json<SelectTenantRequest>,
) -> Result<Json<keycompute_auth::SessionTokenResponse>> {
    Ok(Json(
        state
            .auth
            .select_tenant(&auth.0, req.tenant_id)
            .await
            .map_err(ApiError::from)?,
    ))
}

pub async fn list_my_memberships(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<Vec<keycompute_auth::session::SessionMembership>>> {
    Ok(Json(
        state
            .auth
            .console_session(&auth.0)
            .await
            .map_err(ApiError::from)?
            .memberships,
    ))
}

/// 修改密码请求
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// 修改密码
///
/// PUT /api/v1/me/password
pub async fn change_password(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 1. 验证新密码格式
    let validator = PasswordValidator::new();
    if let Err(e) = validator.validate(&req.new_password) {
        return Err(ApiError::BadRequest(format!(
            "New password does not meet requirements: {}",
            e
        )));
    }

    // 2. 获取用户凭证
    // Password verification and mutation are security-sensitive; a replica
    // that has not applied a recent password change must not be consulted.
    let credential = UserCredential::find_by_user_id(pool.write_conn(), auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query credential: {}", e)))?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "User credential not found for user: {}",
                auth.user_id
            ))
        })?;

    // 3. 验证当前密码
    let hasher = PasswordHasher::new();
    let is_valid = hasher
        .verify(&req.current_password, &credential.password_hash)
        .map_err(|e| ApiError::Auth(format!("Password verification failed: {}", e)))?;

    if !is_valid {
        return Err(ApiError::Auth("Current password is incorrect".to_string()));
    }

    // 4. 哈希新密码
    let new_password_hash = hasher
        .hash(&req.new_password)
        .map_err(|e| ApiError::Internal(format!("Failed to hash new password: {}", e)))?;

    // Retain the verified hash through the parent-first transaction so a
    // concurrent reset cannot let an old password replace a newer one.
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = User::find_by_id_for_update(&tx, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .filter(|user| user.status == "active" && user.token_version == auth.token_version)
        .ok_or_else(|| ApiError::Auth("identity has changed".into()))?;
    let current = UserCredential::find_by_user_id_for_update(&tx, user.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::Auth("credential unavailable".into()))?;
    if current.password_hash != credential.password_hash
        || current.is_locked()
        || !current.email_verified
    {
        return Err(ApiError::Auth("credential has changed".into()));
    }
    current
        .update(
            &tx,
            &keycompute_db::models::user_credential::UpdateUserCredentialRequest {
                password_hash: Some(new_password_hash),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    User::increment_token_version(&tx, user.id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    append_personal_audit(&tx, &auth, "user.password").await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    tracing::info!(
        user_id = %auth.user_id,
        "Password changed successfully"
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Password changed successfully",
        "user_id": auth.user_id,
    })))
}

/// API Key 信息
#[derive(Debug, Serialize)]
pub struct ApiKeyInfo {
    pub id: Uuid,
    pub name: String,
    pub key_preview: String, // 只显示前几位
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub is_active: bool,
    pub revoked: bool,
    pub expires_at: Option<String>,
}

/// API Key 列表查询参数
#[derive(Debug, Deserialize)]
pub struct ApiKeyQueryParams {
    /// 是否包含已撤销的 Key（默认 false）
    #[serde(default)]
    pub include_revoked: bool,
    /// 页码（传入后返回分页对象；不传则保持兼容的数组响应）
    pub page: Option<i64>,
    /// 每页数量
    pub page_size: Option<i64>,
    /// 兼容旧版 limit/offset 参数
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyPageResponse {
    pub keys: Vec<ApiKeyInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 列出我的 API Keys
///
/// GET /api/v1/keys
/// - 普通用户：只返回自己的 Keys
/// - Admin: the same ownership restriction applies.
/// - include_revoked: 是否包含内部轮换产生的已撤销 Key；默认返回所有未撤销
///   Key，并通过 `is_active` 区分仍可用和已过期状态。
pub async fn list_my_api_keys(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<ApiKeyQueryParams>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let has_pagination =
        has_explicit_pagination(params.page, params.page_size, params.limit, params.offset);
    let modern_pagination = params.page.is_some() || params.page_size.is_some();
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?;
    let keys = ProduceAiKey::list_owned(
        pool.write_conn(),
        scope,
        params.include_revoked,
        if has_pagination { page_size } else { 1000 },
        if has_pagination { offset } else { 0 },
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let api_keys: Vec<ApiKeyInfo> = keys
        .into_iter()
        .map(|k| {
            let is_active = !k.revoked && k.expires_at.is_none_or(|at| at > Utc::now());
            ApiKeyInfo {
                id: k.id,
                name: k.name,
                key_preview: k.produce_ai_key_preview,
                created_at: k.created_at.to_rfc3339(),
                last_used_at: k
                    .last_used_at
                    .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
                is_active,
                revoked: k.revoked,
                expires_at: k
                    .expires_at
                    .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
            }
        })
        .collect();

    if modern_pagination {
        let total = ProduceAiKey::count_owned(pool.write_conn(), scope, params.include_revoked)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count API keys: {}", e)))?;
        key_control_auth::finish_read(pool.write_conn(), &auth).await?;
        Ok(Json(
            serde_json::to_value(ApiKeyPageResponse {
                keys: api_keys,
                total,
                page,
                page_size,
                total_pages: total_pages(total, page_size),
            })
            .map_err(|e| ApiError::Internal(format!("Failed to serialize API keys: {}", e)))?,
        ))
    } else {
        key_control_auth::finish_read(pool.write_conn(), &auth).await?;
        Ok(Json(serde_json::to_value(api_keys).map_err(|e| {
            ApiError::Internal(format!("Failed to serialize API keys: {}", e))
        })?))
    }
}

/// 创建 API Key 请求
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateApiKeyRequest {
    /// API Key 名称
    pub name: String,
    /// 是否永不过期
    /// - None 或 Some(false): 默认 6 个月后过期
    /// - Some(true): 永不过期
    #[serde(default)]
    pub never_expires: bool,
}

/// 创建 API Key
///
/// POST /api/v1/keys
///
/// 请求体：
/// - name: API Key 名称
/// - never_expires: 是否永不过期（默认 false，即 6 个月后过期）
pub async fn create_api_key(
    auth: ConsoleAuth,
    request_id: Option<Extension<RequestId>>,
    State(state): State<AppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Response> {
    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ManagePersonalResource,
    )?;
    if req.name.trim().is_empty()
        || req.name.chars().count() > 255
        || req.name.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest("invalid key name".into()));
    }
    let tx = key_control_auth::begin(&state, &auth).await?;

    // 使用统一的 API Key 生成方法（格式：sk- + 48字符 = 51字符）
    let new_key = ProduceAiKeyValidator::generate_key();
    let key_hash = ProduceAiKeyValidator::hash_key(&new_key);
    let key_preview = format!("{}****", &new_key[..8.min(new_key.len())]);

    // 计算过期时间：默认 6 个月，never_expires=true 时永不过期
    let expires_at = if req.never_expires {
        None
    } else {
        Some(Utc::now() + Duration::days(180)) // 6 个月 ≈ 180 天
    };

    let create_req = CreateProduceAiKeyRequest {
        tenant_id: auth.tenant_id,
        user_id: auth.user_id,
        name: req.name.clone(),
        produce_ai_key_hash: key_hash,
        produce_ai_key_preview: key_preview,
        expires_at,
    };

    let saved_key = ProduceAiKey::create_owned_with_secret(
        &tx,
        scope,
        &create_req,
        &new_key,
        &key_audit(&auth, request_id.map(|Extension(id)| id.0)),
    )
    .await
    .map_err(key_mutation_error)?;
    key_control_auth::commit(&state, tx, &auth, KeyChange::Credential).await?;

    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(serde_json::json!({
            "success": true,
            "message": "API Key created",
            "key": new_key,
            "key_id": saved_key.id,
            "name": saved_key.name,
            "created_at": saved_key.created_at.to_rfc3339(),
            "expires_at": saved_key.expires_at.map(|t| t.to_rfc3339()),
            "never_expires": req.never_expires,
        })),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevealApiKeyRequest {
    pub current_password: String,
}

#[derive(Serialize)]
struct RevealApiKeyResponse<'a> {
    key_id: Uuid,
    key: &'a str,
}

const KEY_REVEAL_ATTEMPTS_PER_MINUTE: u32 = 5;
const KEY_REVEAL_RATE_NAMESPACE: Uuid = Uuid::from_u128(0x6b657963_6f6d_7075_7465_2d7265766561);

async fn admit_key_reveal(state: &AppState, user_id: Uuid) -> Result<()> {
    state
        .console_limiter
        .check_and_record_with_config(
            &RateLimitKey::new(KEY_REVEAL_RATE_NAMESPACE, user_id, Uuid::nil()),
            &RateLimitConfig::new(KEY_REVEAL_ATTEMPTS_PER_MINUTE, u32::MAX),
        )
        .await
        .map_err(|error| match error {
            keycompute_types::KeyComputeError::RateLimitExceeded(_) => {
                ApiError::RateLimit("Too many API key reveal attempts. Try again later.".into())
            }
            _ => ApiError::ServiceUnavailable(
                "API key reveal protection is temporarily unavailable".into(),
            ),
        })
}

async fn verify_key_reveal_password(password: String, password_hash: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || PasswordHasher::new().verify(&password, &password_hash))
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable("Password confirmation is temporarily unavailable".into())
        })?
        .map_err(|_| {
            ApiError::ServiceUnavailable("Password confirmation is temporarily unavailable".into())
        })
}

/// Reveal a non-revoked API key only to its current owner after password
/// confirmation. Metadata/list endpoints never include recoverable material.
pub async fn reveal_api_key(
    auth: ConsoleAuth,
    request_id: Option<Extension<RequestId>>,
    Path(key_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<RevealApiKeyRequest>,
) -> Result<Response> {
    if req.current_password.is_empty() || req.current_password.len() > 4096 {
        return Err(ApiError::BadRequest(
            "A valid current password is required".into(),
        ));
    }
    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?;
    admit_key_reveal(&state, auth.user_id).await?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Console storage unavailable".into()))?;
    // Argon2 is deliberately executed before opening a transaction. Its
    // 64-MiB memory-hard verification must not hold a database connection or
    // identity/key locks. The verified hash is compared again under lock below.
    let credential_snapshot = UserCredential::find_by_user_id(pool.write_conn(), auth.user_id)
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable("Password confirmation is temporarily unavailable".into())
        })?;
    let password_confirmed = match credential_snapshot
        .as_ref()
        .filter(|credential| credential.email_verified && !credential.is_locked())
    {
        Some(credential) => {
            verify_key_reveal_password(req.current_password, credential.password_hash.clone())
                .await?
        }
        None => false,
    };
    let tx = key_control_auth::begin(&state, &auth).await?;
    let envelope = ProduceAiKey::reveal_owned(&tx, scope, key_id)
        .await
        .map_err(key_reveal_error)?
        .ok_or_else(|| ApiError::NotFound("API Key secret is unavailable".into()))?;
    let actor = key_audit(&auth, request_id.map(|Extension(id)| id.0));
    let credential = UserCredential::find_by_user_id_for_update(&tx, auth.user_id)
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable("Password confirmation is temporarily unavailable".into())
        })?;
    let credential_unchanged = credential.as_ref().is_some_and(|current| {
        credential_snapshot.as_ref().is_some_and(|snapshot| {
            current.password_hash == snapshot.password_hash
                && current.email_verified
                && !current.is_locked()
        })
    });
    if !password_confirmed || !credential_unchanged {
        if !password_confirmed
            && credential_unchanged
            && let Some(current) = credential.as_ref()
        {
            let failed = current.increment_failed_attempts(&tx).await.map_err(|_| {
                ApiError::ServiceUnavailable(
                    "Password confirmation is temporarily unavailable".into(),
                )
            })?;
            if failed.failed_login_attempts >= DEFAULT_MAX_FAILED_ATTEMPTS {
                failed
                    .lock(&tx, DEFAULT_LOCK_DURATION_MINUTES)
                    .await
                    .map_err(|_| {
                        ApiError::ServiceUnavailable(
                            "Password confirmation is temporarily unavailable".into(),
                        )
                    })?;
            }
        }
        keycompute_db::TenantAuditEvent::append(
            &tx,
            keycompute_types::AuditScopeType::User,
            Some(auth.tenant_id),
            &actor,
            "key.reveal",
            "produce_ai_key",
            Some(&key_id.to_string()),
            keycompute_types::AuditResult::Denied,
            serde_json::json!({"reason":"password confirmation failed"}),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
        key_control_auth::finish_read(&tx, &auth).await?;
        tx.commit()
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        return Err(ApiError::Forbidden("Password confirmation failed".into()));
    }
    if let Some(credential) = credential.as_ref()
        && (credential.failed_login_attempts != 0 || credential.locked_until.is_some())
    {
        credential
            .record_successful_verification(&tx)
            .await
            .map_err(|_| {
                ApiError::ServiceUnavailable(
                    "Password confirmation is temporarily unavailable".into(),
                )
            })?;
    }
    let secret = keycompute_runtime::decrypt_produce_ai_key(
        &envelope,
        keycompute_runtime::ProduceAiKeyEnvelopeContext {
            tenant_id: auth.tenant_id,
            user_id: auth.user_id,
            key_id,
        },
    )
    .map_err(|_| {
        ApiError::ServiceUnavailable("API key decryption is temporarily unavailable".into())
    })?;
    keycompute_db::TenantAuditEvent::append(
        &tx,
        keycompute_types::AuditScopeType::User,
        Some(auth.tenant_id),
        &actor,
        "key.reveal",
        "produce_ai_key",
        Some(&key_id.to_string()),
        keycompute_types::AuditResult::Success,
        serde_json::json!({}),
    )
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    // Re-check the original console session after the secret read and audit
    // write, so a concurrent logout, tenant switch, or membership change
    // cannot receive a late reveal response.
    key_control_auth::finish_read(&tx, &auth).await?;
    tx.commit()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let response = (
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(RevealApiKeyResponse {
            key_id,
            key: secret.expose(),
        }),
    )
        .into_response();
    drop(secret);
    Ok(response)
}

/// 删除 API Key
///
/// DELETE /api/v1/keys/{id}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermanentDeleteConfirmation {
    #[serde(default)]
    pub permanent: bool,
}

pub async fn delete_api_key(
    auth: ConsoleAuth,
    request_id: Option<Extension<RequestId>>,
    Path(key_id): Path<Uuid>,
    Query(confirmation): Query<PermanentDeleteConfirmation>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    // 查找 API Key 并验证所有权
    let scope = auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ManagePersonalResource,
    )?;
    if !confirmation.permanent {
        return Err(ApiError::BadRequest(
            "Permanent API Key deletion requires explicit confirmation".into(),
        ));
    }
    let tx = key_control_auth::begin(&state, &auth).await?;
    let outcome = ProduceAiKey::remove_owned(
        &tx,
        scope,
        key_id,
        &key_audit(&auth, request_id.map(|Extension(id)| id.0)),
    )
    .await
    .map_err(key_mutation_error)?;
    key_control_auth::commit(&state, tx, &auth, KeyChange::Credential).await?;
    match outcome {
        keycompute_db::models::api_key::KeyRemoval::Deleted(id) => Ok(Json(serde_json::json!({
            "success": true, "message": "API Key permanently deleted", "key_id": id, "deleted": true,
        }))),
        keycompute_db::models::api_key::KeyRemoval::Revoked(_) => Err(ApiError::Internal(
            "Permanent API Key deletion returned an invalid outcome".into(),
        )),
    }
}

fn key_audit(auth: &ConsoleAuth, request_id: Option<Uuid>) -> keycompute_db::AuditContext {
    keycompute_db::AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id,
    }
}
fn key_mutation_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::NotFound { .. } => ApiError::NotFound("API Key not found".into()),
        keycompute_db::DbError::Other(message)
            if message == "key management authorization denied" =>
        {
            ApiError::Forbidden("Key management permission is no longer available".into())
        }
        keycompute_db::DbError::Other(message) if message.starts_with("invalid key") => {
            ApiError::BadRequest(message)
        }
        _ => ApiError::ServiceUnavailable("API key storage is temporarily unavailable".into()),
    }
}

fn key_reveal_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::Other(message)
            if message == "key management authorization denied" =>
        {
            ApiError::Forbidden("Key reveal permission is not available".into())
        }
        _ => ApiError::ServiceUnavailable("API key storage is temporarily unavailable".into()),
    }
}

/// 用量记录
#[derive(Debug, Serialize)]
pub struct UsageRecord {
    pub id: Uuid,
    pub request_id: String,
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub cost: f64,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct UsageQueryParams {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub model: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn parse_usage_date(value: Option<&str>, inclusive_end: bool) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| ApiError::BadRequest("usage date must use YYYY-MM-DD".into()))?;
    let date = if inclusive_end {
        date.succ_opt()
            .ok_or_else(|| ApiError::BadRequest("usage end date is out of range".into()))?
    } else {
        date
    };
    let timestamp = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| ApiError::BadRequest("usage date is out of range".into()))?;
    Ok(Some(DateTime::from_naive_utc_and_offset(timestamp, Utc)))
}

#[derive(Debug, Serialize)]
pub struct UsagePageResponse {
    pub records: Vec<UsageRecord>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 获取我的用量记录
///
/// GET /api/v1/usage
/// - 普通用户：只返回自己的用量
/// - Admin: the same ownership restriction applies.
pub async fn get_my_usage(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(params): Query<UsageQueryParams>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let has_pagination =
        has_explicit_pagination(params.page, params.page_size, params.limit, params.offset);
    let modern_pagination = params.page.is_some() || params.page_size.is_some();
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let from = parse_usage_date(params.start_date.as_deref(), false)?;
    let to = parse_usage_date(params.end_date.as_deref(), true)?;
    if matches!((from, to), (Some(from), Some(to)) if from >= to) {
        return Err(ApiError::BadRequest(
            "usage start date must not be after end date".into(),
        ));
    }
    let model = params
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if model.is_some_and(|value| value.len() > 128) {
        return Err(ApiError::BadRequest(
            "usage model filter is too long".into(),
        ));
    }
    let scope = UserUsageScope::new(auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?);
    let logs = scope
        .list_filtered(
            pool.write_conn(),
            from,
            to,
            model,
            if has_pagination { page_size } else { 100 },
            if has_pagination { offset } else { 0 },
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch usage logs: {}", e)))?;

    let usage: Vec<UsageRecord> = logs
        .into_iter()
        .map(|log| UsageRecord {
            id: log.id,
            request_id: log.request_id.to_string(),
            model: log.model_name,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            total_tokens: log.total_tokens,
            cost: log.user_amount.to_f64().unwrap_or(0.0),
            status: log.status,
            created_at: log.created_at.to_rfc3339(),
        })
        .collect();

    if modern_pagination {
        let total = scope
            .count_filtered(pool.write_conn(), from, to, model)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count usage logs: {}", e)))?;
        Ok(Json(
            serde_json::to_value(UsagePageResponse {
                records: usage,
                total,
                page,
                page_size,
                total_pages: total_pages(total, page_size),
            })
            .map_err(|e| ApiError::Internal(format!("Failed to serialize usage logs: {}", e)))?,
        ))
    } else {
        Ok(Json(serde_json::to_value(usage).map_err(|e| {
            ApiError::Internal(format!("Failed to serialize usage logs: {}", e))
        })?))
    }
}

/// 用量统计响应
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageStatsResponse {
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub period: String,
    pub as_of: Option<String>,
    pub cache_max_age_ms: Option<u64>,
}

/// 获取我的用量统计
///
/// GET /api/v1/usage/stats
pub async fn get_my_usage_stats(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<UsageStatsResponse>> {
    state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let db = state.pool.clone().expect("database checked above");
    let key = crate::display_cache::DisplayCache::key(&auth, "usage-stats", "all-time");
    let scope = UserUsageScope::new(auth.require_owner(
        auth.user_id,
        keycompute_auth::AuthorizationAction::ReadPersonalResource,
    )?);
    let value = state.display_cache.read(state.cache.clone(), state.console_admission.origin.clone(), auth.tenant_id, key, async move {
        let as_of = chrono::Utc::now().to_rfc3339();
        let stats = scope.all_time_stats(db.write_conn()).await
            .map_err(|e| ApiError::Internal(format!("Failed to fetch usage stats: {e}")))?;
        Ok(serde_json::json!({"total_requests":stats.total_requests,"total_tokens":stats.total_tokens,
            "total_input_tokens":stats.total_input_tokens,"total_output_tokens":stats.total_output_tokens,
            "total_cost":stats.total_cost.to_f64().unwrap_or(0.0),"period":"all_time","as_of":as_of}))
    }).await?;
    Ok(Json(serde_json::from_value(value).map_err(|e| {
        ApiError::Internal(format!("Invalid statistics snapshot: {e}"))
    })?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_requests_cannot_modify_global_or_membership_privileges() {
        for payload in [
            serde_json::json!({"name":"ok","platform_role":"root"}),
            serde_json::json!({"role":"admin"}),
            serde_json::json!({"tenant_id":Uuid::new_v4()}),
            serde_json::json!({"email":"new@fixture.invalid"}),
        ] {
            assert!(serde_json::from_value::<UpdateProfileRequest>(payload).is_err());
        }
        assert!(
            serde_json::from_value::<UpdateProfileRequest>(serde_json::json!({"name":"ok"}))
                .is_ok()
        );
    }
}
