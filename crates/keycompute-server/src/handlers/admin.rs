//! 管理功能处理器
//
//! 处理需要 Admin 权限的管理请求
//! 注意：Admin 也是用户，通过权限系统控制访问

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
};
use bigdecimal::BigDecimal;
use keycompute_db::models::account::{
    Account, CreateAccountRequest as DbCreateAccountRequest,
    UpdateAccountRequest as DbUpdateAccountRequest,
};
use keycompute_db::models::pricing_model::{
    CreatePricingRequest, PricingModel, UpdatePricingRequest,
};
use keycompute_provider_trait::{DefaultHttpTransport, HttpTransport, UpstreamRequest};
use serde::{Deserialize, Serialize};
use std::time::Instant;
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
    pub balance: f64,
    pub is_active: bool,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

/// 列出所有用户
///
/// GET /api/v1/users
/// 支持查询参数：?tenant_id=xxx&role=xxx&search=xxx
pub async fn list_all_users(
    auth: AuthExtractor,
    State(_state): State<AppState>,
) -> Result<Json<Vec<AdminUserInfo>>> {
    // 检查权限（简化实现，实际应使用中间件）
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    // 实际实现中应从数据库查询所有用户
    let users = vec![AdminUserInfo {
        id: Uuid::new_v4(),
        email: "user1@example.com".to_string(),
        name: Some("User One".to_string()),
        role: "user".to_string(),
        tenant_id: auth.tenant_id,
        tenant_name: "Default Tenant".to_string(),
        balance: 100.0,
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
        last_login_at: Some("2024-01-15T10:30:00Z".to_string()),
    }];

    Ok(Json(users))
}

/// 获取指定用户信息
///
/// GET /api/v1/users/{id}
pub async fn get_user_by_id(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(_state): State<AppState>,
) -> Result<Json<AdminUserInfo>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    Ok(Json(AdminUserInfo {
        id: user_id,
        email: "user@example.com".to_string(),
        name: Some("Test User".to_string()),
        role: "user".to_string(),
        tenant_id: auth.tenant_id,
        tenant_name: "Default Tenant".to_string(),
        balance: 100.0,
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
        last_login_at: Some("2024-01-15T10:30:00Z".to_string()),
    }))
}

/// 更新用户请求
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<String>,
    pub is_active: Option<bool>,
}

/// 更新用户信息
///
/// PUT /api/v1/users/{id}
pub async fn update_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(_state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "User updated",
        "user_id": user_id,
        "updated_fields": {
            "name": req.name,
            "role": req.role,
            "is_active": req.is_active,
        }
    })))
}

/// 删除用户
///
/// DELETE /api/v1/users/{id}
pub async fn delete_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(_state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    // 防止删除自己
    if user_id == auth.user_id {
        return Err(ApiError::BadRequest("Cannot delete yourself".to_string()));
    }

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
    pub amount: f64, // 正数增加，负数减少
    pub reason: String,
}

/// 更新用户余额
///
/// POST /api/v1/users/{id}/balance
pub async fn update_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(_state): State<AppState>,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance updated",
        "user_id": user_id,
        "amount": req.amount,
        "reason": req.reason,
        "new_balance": 100.0 + req.amount, // 模拟计算
        "updated_by": auth.user_id,
    })))
}

/// 列出用户的所有 API Keys（Admin 视图）
///
/// GET /api/v1/users/{id}/api-keys
pub async fn list_all_api_keys(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(_state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let keys = vec![serde_json::json!({
        "id": Uuid::new_v4(),
        "user_id": user_id,
        "name": "Default Key",
        "key_preview": "sk-abc...",
        "is_active": true,
        "created_at": "2024-01-01T00:00:00Z",
    })];

    Ok(Json(keys))
}

// ==================== 账号/渠道管理 ====================

/// Provider 账号信息
#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub id: Uuid,
    pub name: String,
    pub provider: String, // openai, anthropic, etc.
    pub api_key_preview: String,
    pub base_url: Option<String>,
    pub models: Vec<String>,
    pub rpm_limit: i32,
    pub current_rpm: i32,
    pub is_active: bool,
    pub is_healthy: bool,
    pub priority: i32,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// 列出所有账号
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let db_accounts = Account::find_by_tenant(pool, auth.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query accounts: {}", e)))?;

    let accounts: Vec<AccountInfo> = db_accounts
        .into_iter()
        .map(|acc| AccountInfo {
            id: acc.id,
            name: acc.name,
            provider: acc.provider,
            api_key_preview: acc.upstream_api_key_preview,
            base_url: if acc.endpoint.is_empty() {
                None
            } else {
                Some(acc.endpoint)
            },
            models: acc.models_supported,
            rpm_limit: acc.rpm_limit,
            current_rpm: 0, // TODO: 从 account_states 获取实时 RPM
            is_active: acc.enabled,
            is_healthy: true, // TODO: 从 provider_health 获取健康状态
            priority: acc.priority,
            created_at: acc.created_at.to_rfc3339(),
            last_used_at: acc.updated_at.to_rfc3339().into(),
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
    pub base_url: Option<String>,
    pub models: Vec<String>,
    pub rpm_limit: Option<i32>,
    pub priority: Option<i32>,
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
        .as_ref()
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
        endpoint: req.base_url.clone().unwrap_or_default(),
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        models_supported: req.models.clone(),
    };

    let account = Account::create(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create account: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Account created",
        "account_id": account.id,
        "name": account.name,
        "provider": account.provider,
        "models": account.models_supported,
        "created_by": auth.user_id,
    })))
}

/// 更新账号请求
#[derive(Debug, Deserialize)]
pub struct UpdateAccountRequest {
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub models: Option<Vec<String>>,
    pub rpm_limit: Option<i32>,
    pub is_active: Option<bool>,
    pub priority: Option<i32>,
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
        .as_ref()
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
        name: req.name.clone(),
        endpoint: req.base_url.clone(),
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        enabled: req.is_active,
        models_supported: req.models.clone(),
    };

    let updated = existing
        .update(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update account: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Account updated",
        "account_id": updated.id,
        "updated_fields": {
            "name": req.name,
            "models": req.models,
            "is_active": req.is_active,
        },
        "updated_by": auth.user_id,
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
        .as_ref()
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
        .as_ref()
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
        Ok(models) => Ok(Json(serde_json::json!({
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
        }))),
        Err(e) => Ok(Json(serde_json::json!({
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
        }))),
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
        .as_ref()
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
        name: None,
        endpoint: None,
        upstream_api_key_encrypted: None,
        upstream_api_key_preview: None,
        rpm_limit: None,
        tpm_limit: None,
        priority: None,
        enabled: None,
        models_supported: Some(new_models.clone()),
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
fn decrypt_account_api_key(encrypted_key: &str) -> Result<String> {
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
fn get_default_endpoint(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "https://api.openai.com/v1".to_string(),
        "anthropic" | "claude" => "https://api.anthropic.com/v1".to_string(),
        "deepseek" => "https://api.deepseek.com/v1".to_string(),
        "gemini" | "google" => "https://generativelanguage.googleapis.com/v1".to_string(),
        "ollama" => "http://localhost:11434/v1".to_string(),
        _ => format!("https://api.{}.com/v1", provider),
    }
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

/// 列出所有租户
///
/// GET /api/v1/tenants
pub async fn list_tenants(
    auth: AuthExtractor,
    State(_state): State<AppState>,
) -> Result<Json<Vec<TenantInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let tenants = vec![TenantInfo {
        id: auth.tenant_id,
        name: "Default Tenant".to_string(),
        description: Some("System default tenant".to_string()),
        user_count: 10,
        is_active: true,
        created_at: "2024-01-01T00:00:00Z".to_string(),
    }];

    Ok(Json(tenants))
}

// ==================== 定价管理 ====================

/// 定价信息
#[derive(Debug, Serialize)]
pub struct PricingInfo {
    pub id: Uuid,
    pub model_name: String,
    pub provider: String,
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
}

/// 创建定价请求（管理员）
#[derive(Debug, Deserialize)]
pub struct CreatePricingAdminRequest {
    /// 模型名称
    pub model_name: String,
    /// Provider
    pub provider: String,
    /// 货币（默认 CNY）
    #[serde(default = "default_currency")]
    pub currency: String,
    /// 输入价格（每 1k tokens）
    pub input_price_per_1k: String,
    /// 输出价格（每 1k tokens）
    pub output_price_per_1k: String,
    /// 是否为默认定价
    #[serde(default)]
    pub is_default: bool,
    /// 生效时间（可选）
    pub effective_from: Option<String>,
    /// 失效时间（可选）
    pub effective_until: Option<String>,
}

fn default_currency() -> String {
    "CNY".to_string()
}

/// 更新定价请求（管理员）
#[derive(Debug, Deserialize)]
pub struct UpdatePricingAdminRequest {
    /// 输入价格（每 1k tokens）
    pub input_price_per_1k: Option<String>,
    /// 输出价格（每 1k tokens）
    pub output_price_per_1k: Option<String>,
    /// 失效时间
    pub effective_until: Option<String>,
}

/// 列出所有定价
///
/// GET /api/v1/pricing
pub async fn list_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<PricingInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let pricing_models = PricingModel::find_by_tenant(pool, auth.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query pricing: {}", e)))?;

    let pricing_list: Vec<PricingInfo> = pricing_models
        .into_iter()
        .map(|p| {
            let is_effective = p.is_effective();
            PricingInfo {
                id: p.id,
                model_name: p.model_name,
                provider: p.provider,
                currency: p.currency,
                input_price_per_1k: p.input_price_per_1k.to_string(),
                output_price_per_1k: p.output_price_per_1k.to_string(),
                is_default: p.is_default,
                is_effective,
                effective_from: p.effective_from.to_rfc3339(),
                effective_until: p.effective_until.map(|t| t.to_rfc3339()),
                created_at: p.created_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(pricing_list))
}

/// 创建定价
///
/// POST /api/v1/pricing
pub async fn create_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 解析价格
    let input_price: BigDecimal = req
        .input_price_per_1k
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid input_price_per_1k".to_string()))?;

    let output_price: BigDecimal = req
        .output_price_per_1k
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid output_price_per_1k".to_string()))?;

    let db_req = CreatePricingRequest {
        tenant_id: if req.is_default {
            None
        } else {
            Some(auth.tenant_id)
        },
        model_name: req.model_name.clone(),
        provider: req.provider.clone(),
        currency: Some(req.currency.clone()),
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        is_default: Some(req.is_default),
        effective_from: req.effective_from.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok()
        }),
        effective_until: req.effective_until.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .ok()
        }),
    };

    let pricing = PricingModel::create(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create pricing: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing created",
        "pricing_id": pricing.id,
        "model_name": pricing.model_name,
        "provider": pricing.provider,
        "input_price_per_1k": pricing.input_price_per_1k.to_string(),
        "output_price_per_1k": pricing.output_price_per_1k.to_string(),
        "is_default": pricing.is_default,
        "created_by": auth.user_id,
    })))
}

/// 更新定价
///
/// PUT /api/v1/pricing/{id}
pub async fn update_pricing(
    auth: AuthExtractor,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdatePricingAdminRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找现有定价
    let existing = PricingModel::find_by_id(pool, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    // 解析价格
    let input_price = req.input_price_per_1k.as_ref().and_then(|s| s.parse().ok());

    let output_price = req
        .output_price_per_1k
        .as_ref()
        .and_then(|s| s.parse().ok());

    let effective_until = req.effective_until.as_ref().and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()
    });

    let db_req = UpdatePricingRequest {
        input_price_per_1k: input_price,
        output_price_per_1k: output_price,
        effective_until,
    };

    let updated = existing
        .update(pool, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update pricing: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing updated",
        "pricing_id": updated.id,
        "updated_fields": {
            "input_price_per_1k": req.input_price_per_1k,
            "output_price_per_1k": req.output_price_per_1k,
            "effective_until": req.effective_until.clone(),
        },
        "updated_by": auth.user_id,
    })))
}

/// 删除定价
///
/// DELETE /api/v1/pricing/{id}
pub async fn delete_pricing(
    auth: AuthExtractor,
    Path(pricing_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查找并删除定价
    let existing = PricingModel::find_by_id(pool, pricing_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find pricing: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Pricing not found: {}", pricing_id)))?;

    existing
        .delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete pricing: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Pricing deleted",
        "pricing_id": pricing_id,
        "deleted_by": auth.user_id,
    })))
}

/// 批量设置默认定价
///
/// POST /api/v1/pricing/batch-defaults
///
/// 为常用模型设置默认定价
pub async fn set_default_pricing(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 默认定价数据（参考 PricingService）
    let defaults = vec![
        ("gpt-4o", "openai", "0.5", "1.5"),
        ("gpt-4o-mini", "openai", "0.15", "0.6"),
        ("gpt-4-turbo", "openai", "1.0", "3.0"),
        ("gpt-3.5-turbo", "openai", "0.05", "0.15"),
        ("claude-3-5-sonnet-20241022", "anthropic", "0.3", "1.5"),
        ("claude-3-opus-20240229", "anthropic", "1.5", "7.5"),
        ("deepseek-chat", "deepseek", "0.01", "0.03"),
        ("deepseek-reasoner", "deepseek", "0.05", "0.15"),
    ];

    let mut created = 0;
    let mut skipped = 0;

    for (model_name, provider, input_price, output_price) in defaults {
        // 检查是否已存在
        let existing = PricingModel::find_by_model(pool, auth.tenant_id, model_name, provider)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to check existing pricing: {}", e)))?;

        if existing.is_some() {
            skipped += 1;
            continue;
        }

        let db_req = CreatePricingRequest {
            tenant_id: None,
            model_name: model_name.to_string(),
            provider: provider.to_string(),
            currency: Some("CNY".to_string()),
            input_price_per_1k: input_price.parse().unwrap(),
            output_price_per_1k: output_price.parse().unwrap(),
            is_default: Some(true),
            effective_from: None,
            effective_until: None,
        };

        match PricingModel::create(pool, &db_req).await {
            Ok(_) => created += 1,
            Err(e) => {
                tracing::warn!(model = model_name, error = %e, "Failed to create default pricing");
            }
        }
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Default pricing set",
        "created": created,
        "skipped": skipped,
        "set_by": auth.user_id,
    })))
}

// ==================== 系统设置 ====================

/// 系统设置
#[derive(Debug, Serialize, Deserialize)]
pub struct SystemSettings {
    pub site_name: String,
    pub site_description: Option<String>,
    pub allow_registration: bool,
    pub default_user_quota: f64,
    pub rate_limit_rpm: i32,
    pub maintenance_mode: bool,
}

/// 获取系统设置
///
/// GET /api/v1/settings
pub async fn get_system_settings(
    auth: AuthExtractor,
    State(_state): State<AppState>,
) -> Result<Json<SystemSettings>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    Ok(Json(SystemSettings {
        site_name: "KeyCompute".to_string(),
        site_description: Some("AI Gateway Platform".to_string()),
        allow_registration: true,
        default_user_quota: 10.0,
        rate_limit_rpm: 60,
        maintenance_mode: false,
    }))
}

/// 更新系统设置
///
/// PUT /api/v1/settings
pub async fn update_system_settings(
    auth: AuthExtractor,
    State(_state): State<AppState>,
    Json(settings): Json<SystemSettings>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Settings updated",
        "updated_by": auth.user_id,
        "settings": settings,
    })))
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
            is_active: true,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("admin@example.com"));
    }
}
