//! 用户自服务处理器
//
//! 处理用户管理自己资源的请求
//! Admin 也可以访问这些端点，但会根据权限返回不同范围的数据

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
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
    usage_log::UsageLog,
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
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<CurrentUserResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, auth.user_id)
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
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", auth.user_id)))?;

    let update_req = keycompute_db::models::user::UpdateUserRequest {
        name: req.name,
        role: None, // 不允许用户自己修改角色
    };

    let updated = user
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update profile: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Profile updated",
        "user_id": updated.id,
        "name": updated.name,
    })))
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
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
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
    let credential = UserCredential::find_by_user_id(pool, auth.user_id)
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
}

/// 列出我的 API Keys
///
/// GET /api/v1/keys
/// - 普通用户：只返回自己的 Keys
/// - Admin：可以返回所有 Keys（通过查询参数控制）
/// - include_revoked: 是否包含已撤销的 Key（默认 false，只返回活跃的）
pub async fn list_my_api_keys(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<ApiKeyQueryParams>,
) -> Result<Json<Vec<ApiKeyInfo>>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let keys = if params.include_revoked {
        // 包含已撤销的 Key
        ProduceAiKey::find_by_user(pool, auth.user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?
    } else {
        // 默认只返回活跃的 Key
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

    Ok(Json(api_keys))
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
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
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
        .map_err(|e| ApiError::Internal(format!("Failed to create API key: {}", e)))?;

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
    auth: AuthExtractor,
    Path(key_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_ref()
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

/// 获取我的用量记录
///
/// GET /api/v1/usage
/// - 普通用户：只返回自己的用量
/// - Admin：可以返回所有用量（通过查询参数控制）
pub async fn get_my_usage(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<UsageRecord>>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let logs = UsageLog::find_by_user(pool, auth.user_id, 100, 0)
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

    Ok(Json(usage))
}

/// 用量统计响应
#[derive(Debug, Serialize)]
pub struct UsageStatsResponse {
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub period: String,
}

/// 获取我的用量统计
///
/// GET /api/v1/usage/stats
pub async fn get_my_usage_stats(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<UsageStatsResponse>> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let stats = UsageLog::get_user_stats(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch usage stats: {}", e)))?;

    Ok(Json(UsageStatsResponse {
        total_requests: stats.total_requests,
        total_tokens: stats.total_tokens,
        total_input_tokens: stats.total_input_tokens,
        total_output_tokens: stats.total_output_tokens,
        total_cost: stats.total_cost.to_f64().unwrap_or(0.0),
        period: "all_time".to_string(),
    }))
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
