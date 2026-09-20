//! 用户自服务处理器
//
//! 处理用户管理自己资源的请求
//! All console roles use these endpoints for their own resources only.

use crate::handlers::pagination::{
    has_explicit_pagination, normalize_list_pagination, total_pages,
};
use crate::{
    error::{ApiError, Result},
    extractors::ConsoleAuth,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{Duration, Utc};
use keycompute_auth::{PasswordHasher, PasswordValidator, ProduceAiKeyValidator};
use keycompute_db::models::{
    api_key::{CreateProduceAiKeyRequest, ProduceAiKey},
    usage_log::UserUsageScope,
    user::User,
    user_credential::UserCredential,
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 当前用户信息响应
#[derive(Debug, Serialize)]
pub struct CurrentUserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub tenant_id: Uuid,
    pub created_at: String,
}

/// 获取当前用户信息
///
/// GET /api/v1/me
pub async fn get_current_user(
    auth: ConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<CurrentUserResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // This response is also used to refresh the client after an administrator
    // moves the account. Read the authoritative row so the client does not
    // immediately overwrite its tenant context with a lagging replica value.
    let user = User::find_by_id(pool.write_conn(), auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", auth.user_id)))?;

    Ok(Json(CurrentUserResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        role: user.role,
        tenant_id: user.tenant_id,
        created_at: user.created_at.to_rfc3339(),
    }))
}

/// 更新个人资料请求
#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// 更新个人资料
///
/// PUT /api/v1/me/profile
pub async fn update_profile(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<Json<CurrentUserResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool.write_conn(), auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", auth.user_id)))?;

    let update_req = keycompute_db::models::user::UpdateUserRequest {
        name: req.name,
        role: None,      // 不允许用户自己修改角色
        tenant_id: None, // 不允许用户自己修改租户
    };

    let updated = user
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update profile: {}", e)))?;

    Ok(Json(CurrentUserResponse {
        id: updated.id,
        email: updated.email,
        name: updated.name,
        role: updated.role,
        tenant_id: updated.tenant_id,
        created_at: updated.created_at.to_rfc3339(),
    }))
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
    auth: ConsoleAuth,
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

    // 5. 更新数据库
    let update_req = keycompute_db::models::user_credential::UpdateUserCredentialRequest {
        password_hash: Some(new_password_hash),
        ..Default::default()
    };

    credential
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update password: {}", e)))?;

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
/// - include_revoked: 是否包含已撤销的 Key（默认 false，只返回活跃的）
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
    let keys = if has_pagination {
        ProduceAiKey::find_by_user_page(
            pool,
            auth.user_id,
            params.include_revoked,
            page_size,
            offset,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?
    } else if params.include_revoked {
        ProduceAiKey::find_by_user(pool, auth.user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?
    } else {
        ProduceAiKey::find_active_by_user(pool, auth.user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?
    };

    let api_keys: Vec<ApiKeyInfo> = keys
        .into_iter()
        .map(|k| ApiKeyInfo {
            id: k.id,
            name: k.name,
            key_preview: k.produce_ai_key_preview,
            created_at: k.created_at.to_rfc3339(),
            last_used_at: k
                .last_used_at
                .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
            is_active: !k.revoked,
            expires_at: k
                .expires_at
                .map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
        })
        .collect();

    if modern_pagination {
        let total = ProduceAiKey::count_by_user(pool, auth.user_id, params.include_revoked)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count API keys: {}", e)))?;
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
        Ok(Json(serde_json::to_value(api_keys).map_err(|e| {
            ApiError::Internal(format!("Failed to serialize API keys: {}", e))
        })?))
    }
}

/// 创建 API Key 请求
#[derive(Debug, Deserialize)]
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
    State(state): State<AppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

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

    let saved_key = ProduceAiKey::create(pool, &create_req)
        .await
        .map_err(|error| match error {
            keycompute_db::DbError::UserTenantMismatch { .. } => ApiError::Conflict(
                "User tenant changed; refresh authentication and retry".to_string(),
            ),
            other => ApiError::Internal(format!("Failed to create API key: {}", other)),
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "API Key created",
        "key": new_key, // 注意：这是唯一一次返回完整 key
        "key_id": saved_key.id,
        "name": saved_key.name,
        "created_at": saved_key.created_at.to_rfc3339(),
        "expires_at": saved_key.expires_at.map(|t| t.to_rfc3339()),
        "never_expires": req.never_expires,
    })))
}

/// 删除 API Key
///
/// DELETE /api/v1/keys/{id}
pub async fn delete_api_key(
    auth: ConsoleAuth,
    Path(key_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找 API Key 并验证所有权
    let key = ProduceAiKey::find_by_id(pool, key_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find API key: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("API Key not found: {}", key_id)))?;

    // 验证所有权（只有创建者才能删除）
    if key.user_id != auth.user_id {
        return Err(ApiError::Auth(
            "You do not have permission to delete this API key".to_string(),
        ));
    }

    // 行为约定：
    // - 活跃 Key：先撤销（保留审计痕迹）
    // - 已撤销 Key：允许物理删除（便于用户清理列表）
    if key.revoked {
        key.delete(pool)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to delete API key: {}", e)))?;

        return Ok(Json(serde_json::json!({
            "success": true,
            "message": "API Key deleted",
            "key_id": key.id,
            "deleted": true,
        })));
    }

    let revoked = key
        .revoke(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to revoke API key: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "API Key revoked",
        "key_id": revoked.id,
        "revoked_at": revoked.revoked_at.map(|t| t.to_rfc3339()),
        "deleted": false,
    })))
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
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
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
    let scope = UserUsageScope::new(auth.tenant_id, auth.user_id);
    let logs = scope
        .list(
            pool,
            None,
            None,
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
            .count(pool, None, None)
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
    let scope = UserUsageScope::new(auth.tenant_id, auth.user_id);
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
    fn test_current_user_response_serialization() {
        let user = CurrentUserResponse {
            id: Uuid::new_v4(),
            email: "test@example.com".to_string(),
            name: Some("Test User".to_string()),
            role: "user".to_string(),
            tenant_id: Uuid::new_v4(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("test@example.com"));
    }
}
