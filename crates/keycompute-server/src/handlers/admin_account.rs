//! 账号/渠道管理处理器
//!
//! 处理需要 Admin 权限的 Provider 账号管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
};
use keycompute_db::models::account::{
    Account, CreateAccountRequest as DbCreateAccountRequest,
    UpdateAccountRequest as DbUpdateAccountRequest,
};
use keycompute_provider_trait::{DefaultHttpTransport, HttpTransport};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

/// Provider 账号信息
#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub id: Uuid,
    /// 所属租户 ID
    pub tenant_id: Uuid,
    pub name: String,
    pub provider: String, // openai, anthropic, etc.
    pub api_key_preview: String,
    /// 自定义 Base URL（Provider 端点地址）
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub rpm_limit: i32,
    pub current_rpm: i32,
    pub is_active: bool,
    pub is_healthy: bool,
    pub priority: i32,
    /// 可见性：'tenant' = 仅本租户可见，'global' = 所有租户可见
    pub visibility: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// 列出所有账号（Admin 全局视图，不限租户）
///
/// GET /api/v1/accounts
pub async fn list_accounts(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<AccountInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // Admin 管理面加载所有租户的账号
    let db_accounts = Account::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query accounts: {}", e)))?;

    let accounts: Vec<AccountInfo> = db_accounts
        .into_iter()
        .map(|acc| {
            // 从 ProviderHealthStore 获取真实健康状态
            let is_healthy = state.provider_health.is_healthy(&acc.provider);

            // 检查账号是否在冷却中
            let is_cooling = state.account_states.is_cooling_down(&acc.id);

            AccountInfo {
                id: acc.id,
                tenant_id: acc.tenant_id,
                name: acc.name,
                provider: acc.provider,
                api_key_preview: acc.upstream_api_key_preview,
                api_base: if acc.endpoint.is_empty() {
                    None
                } else {
                    Some(acc.endpoint)
                },
                models: acc.models_supported,
                rpm_limit: acc.rpm_limit,
                current_rpm: if is_cooling { -1 } else { 0 }, // -1 表示冷却中
                is_active: acc.enabled,
                is_healthy,
                priority: acc.priority,
                visibility: acc.visibility,
                created_at: acc.created_at.to_rfc3339(),
                last_used_at: acc.updated_at.to_rfc3339().into(),
            }
        })
        .collect();

    Ok(Json(accounts))
}

/// 创建账号请求
#[derive(Debug, Deserialize)]
pub struct CreateAccountRequest {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    /// 自定义 Base URL（Provider 端点地址）
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub rpm_limit: Option<i32>,
    pub priority: Option<i32>,
    /// 可见性：'tenant' = 仅本租户可见（默认），'global' = 所有租户可见
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

fn default_visibility() -> String {
    "tenant".to_string()
}

/// 创建账号
///
/// POST /api/v1/accounts
pub async fn create_account(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreateAccountRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 加密 API Key（如果配置了加密密钥）
    let (encrypted_key, key_preview) =
        if let Some(_crypto) = keycompute_runtime::crypto::global_crypto() {
            let encrypted = keycompute_runtime::crypto::encrypt_api_key(&req.api_key)
                .map_err(|e| ApiError::Internal(format!("Failed to encrypt API key: {}", e)))?;
            (
                encrypted.into_inner(),
                keycompute_runtime::crypto::ApiKeyCrypto::create_preview(&req.api_key),
            )
        } else {
            // 未配置加密，直接存储明文
            (
                req.api_key.clone(),
                format!("{}****", &req.api_key[..req.api_key.len().min(3)]),
            )
        };

    let db_req = DbCreateAccountRequest {
        tenant_id: auth.tenant_id,
        provider: req.provider.clone(),
        name: req.name.clone(),
        endpoint: req.api_base.clone().unwrap_or_default(),
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        models_supported: req.models.clone(),
        visibility: Some(req.visibility.clone()),
    };

    let account = Account::create(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create account: {}", e)))?;

    // 返回完整的账号信息，与前端 AccountInfo 类型匹配
    Ok(Json(serde_json::json!({
        "id": account.id.to_string(),
        "tenant_id": account.tenant_id.to_string(),
        "name": account.name,
        "provider": account.provider,
        "api_key_preview": account.upstream_api_key_preview,
        "api_base": if account.endpoint.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(account.endpoint)
        },
        "models": account.models_supported,
        "rpm_limit": account.rpm_limit,
        "current_rpm": 0,
        "is_active": account.enabled,
        "is_healthy": true,
        "priority": account.priority,
        "visibility": account.visibility,
        "created_at": account.created_at.to_rfc3339(),
        "last_used_at": serde_json::Value::Null,
    })))
}

/// 更新账号请求
#[derive(Debug, Deserialize)]
pub struct UpdateAccountRequest {
    pub tenant_id: Option<Uuid>,
    pub name: Option<String>,
    pub api_key: Option<String>,
    /// 自定义 Base URL（Provider 端点地址）
    pub api_base: Option<String>,
    pub models: Option<Vec<String>>,
    pub rpm_limit: Option<i32>,
    pub is_active: Option<bool>,
    pub priority: Option<i32>,
    /// 可见性：'tenant' = 仅本租户可见，'global' = 所有租户可见
    pub visibility: Option<String>,
}

/// 更新账号
///
/// PUT /api/v1/accounts/{id}
pub async fn update_account(
    auth: AuthExtractor,
    Path(account_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateAccountRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找现有账号
    let existing = Account::find_by_id(pool, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;

    // 处理 API Key 加密
    let (encrypted_key, key_preview) = if let Some(ref key) = req.api_key {
        if let Some(_crypto) = keycompute_runtime::crypto::global_crypto() {
            let encrypted = keycompute_runtime::crypto::encrypt_api_key(key)
                .map_err(|e| ApiError::Internal(format!("Failed to encrypt API key: {}", e)))?;
            (
                Some(encrypted.into_inner()),
                Some(keycompute_runtime::crypto::ApiKeyCrypto::create_preview(
                    key,
                )),
            )
        } else {
            (
                Some(key.clone()),
                Some(format!("{}****", &key[..key.len().min(3)])),
            )
        }
    } else {
        (None, None)
    };

    let db_req = DbUpdateAccountRequest {
        tenant_id: req.tenant_id,
        name: req.name.clone(),
        endpoint: req.api_base.clone(),
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        enabled: req.is_active,
        models_supported: req.models.clone(),
        visibility: req.visibility.clone(),
    };

    let updated = existing
        .update(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update account: {}", e)))?;

    // 返回更新后的账号信息
    Ok(Json(serde_json::json!({
        "id": updated.id.to_string(),
        "tenant_id": updated.tenant_id.to_string(),
        "name": updated.name,
        "provider": updated.provider,
        "api_key_preview": updated.upstream_api_key_preview,
        "api_base": updated.endpoint,
        "models": updated.models_supported,
        "rpm_limit": updated.rpm_limit,
        "current_rpm": 0,
        "is_active": updated.enabled,
        "is_healthy": true,
        "priority": updated.priority,
        "visibility": updated.visibility,
        "created_at": updated.created_at.to_rfc3339(),
        "last_used_at": serde_json::Value::Null,
    })))
}

/// 删除账号
///
/// DELETE /api/v1/accounts/{id}
pub async fn delete_account(
    auth: AuthExtractor,
    Path(account_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找并删除账号
    let existing = Account::find_by_id(pool, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;

    existing
        .delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete account: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Account deleted",
        "account_id": account_id,
        "deleted_by": auth.user_id,
    })))
}

/// 测试账号连接
///
/// POST /api/v1/accounts/{id}/test
///
/// 实际调用上游 API 进行连接测试，验证 API Key 是否有效
pub async fn test_account(
    auth: AuthExtractor,
    Path(account_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找账号
    let account = Account::find_by_id(pool, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;

    // 解密 API Key
    let api_key = decrypt_account_api_key(&account.upstream_api_key_encrypted)?;

    // 构建 endpoint
    let endpoint = if account.endpoint.is_empty() {
        get_default_endpoint(&account.provider)
    } else {
        account.endpoint.clone()
    };

    // 创建 HTTP 传输层
    let transport = DefaultHttpTransport::new();

    // 构建测试请求 - 使用简单的模型列表请求
    let test_endpoint = format!(
        "{}/models",
        endpoint
            .trim_end_matches('/')
            .trim_end_matches("/chat/completions")
    );

    let start = Instant::now();

    // 尝试调用上游 API
    let test_result = test_upstream_connection(&transport, &test_endpoint, &api_key).await;

    let latency_ms = start.elapsed().as_millis() as i64;

    match test_result {
        Ok(models) => {
            // 测试成功：清除错误计数
            state.account_states.clear_cooldown(account_id);

            Ok(Json(serde_json::json!({
                "success": true,
                "message": "Account connection test passed",
                "account_id": account_id,
                "test_result": {
                    "is_healthy": true,
                    "latency_ms": latency_ms,
                    "available_models": models,
                    "provider": account.provider,
                    "endpoint": endpoint,
                }
            })))
        }
        Err(e) => {
            // 测试失败：标记错误（仅管理员测试时触发）
            state.account_states.mark_error(account_id);

            Ok(Json(serde_json::json!({
                "success": false,
                "message": "Account connection test failed",
                "account_id": account_id,
                "test_result": {
                    "is_healthy": false,
                    "latency_ms": latency_ms,
                    "error": e,
                    "provider": account.provider,
                    "endpoint": endpoint,
                }
            })))
        }
    }
}

/// 测试上游连接
async fn test_upstream_connection(
    transport: &DefaultHttpTransport,
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<Vec<String>, String> {
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {}", api_key)),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];

    let response = transport
        .post_json(endpoint, headers, "{}".to_string())
        .await
        .map_err(|e| e.to_string())?;

    // 尝试解析模型列表
    let parsed: serde_json::Value =
        serde_json::from_str(&response).unwrap_or(serde_json::json!({}));

    // 提取模型 ID 列表
    let models = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

/// 刷新账号信息（重新获取模型列表等）
///
/// POST /api/v1/accounts/{id}/refresh
///
/// 从上游 API 获取模型列表并更新数据库中的 models_supported 字段
pub async fn refresh_account(
    auth: AuthExtractor,
    Path(account_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找账号
    let account = Account::find_by_id(pool, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;

    // 解密 API Key
    let api_key = decrypt_account_api_key(&account.upstream_api_key_encrypted)?;

    // 构建 endpoint
    let endpoint = if account.endpoint.is_empty() {
        get_default_endpoint(&account.provider)
    } else {
        account.endpoint.clone()
    };

    // 创建 HTTP 传输层
    let transport = DefaultHttpTransport::new();

    // 构建模型列表请求 endpoint
    let models_endpoint = format!(
        "{}/models",
        endpoint
            .trim_end_matches('/')
            .trim_end_matches("/chat/completions")
    );

    // 调用上游 API 获取模型列表
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {}", api_key)),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];

    let response = transport
        .post_json(&models_endpoint, headers, "{}".to_string())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch models: {}", e)))?;

    // 解析模型列表
    let parsed: serde_json::Value = serde_json::from_str(&response)
        .map_err(|e| ApiError::Internal(format!("Failed to parse response: {}", e)))?;

    let new_models: Vec<String> = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or(account.models_supported.clone());

    // 更新数据库
    let db_req = DbUpdateAccountRequest {
        tenant_id: None,
        name: None,
        endpoint: None,
        upstream_api_key_encrypted: None,
        upstream_api_key_preview: None,
        rpm_limit: None,
        tpm_limit: None,
        priority: None,
        enabled: None,
        models_supported: Some(new_models.clone()),
        visibility: None,
    };

    let updated = account
        .update(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update account: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Account refreshed",
        "account_id": updated.id,
        "refreshed_by": auth.user_id,
        "previous_models": account.models_supported,
        "updated_models": updated.models_supported,
    })))
}

/// 解密账号的 API Key
pub fn decrypt_account_api_key(encrypted_key: &str) -> Result<String> {
    // 尝试使用全局密钥解密
    if let Some(_crypto) = keycompute_runtime::crypto::global_crypto() {
        match keycompute_runtime::crypto::decrypt_api_key(
            &keycompute_runtime::EncryptedApiKey::from(encrypted_key),
        ) {
            Ok(decrypted) => return Ok(decrypted),
            Err(e) => {
                // 解密失败，可能是明文存储，尝试直接使用
                tracing::warn!(
                    error = %e,
                    "Failed to decrypt API key, trying as plaintext"
                );
            }
        }
    }
    // 无加密或解密失败，直接返回原值
    Ok(encrypted_key.to_string())
}

/// 获取 Provider 的默认 endpoint
pub fn get_default_endpoint(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "https://api.openai.com/v1".to_string(),
        "anthropic" | "claude" => "https://api.anthropic.com/v1".to_string(),
        "deepseek" => "https://api.deepseek.com/v1".to_string(),
        "gemini" | "google" => "https://generativelanguage.googleapis.com/v1".to_string(),
        "ollama" => "http://localhost:11434/v1".to_string(),
        _ => format!("https://api.{}.com/v1", provider),
    }
}
