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
    extract::{Path, Query, State},
};
use bigdecimal::BigDecimal;
use keycompute_db::models::account::{
    Account, CreateAccountRequest as DbCreateAccountRequest,
    UpdateAccountRequest as DbUpdateAccountRequest,
};
use keycompute_db::models::api_key::ProduceAiKey;
use keycompute_db::models::pricing_model::{
    CreatePricingRequest, PricingModel, UpdatePricingRequest,
};
use keycompute_db::models::tenant::Tenant;
use keycompute_db::models::user::User;
use keycompute_db::models::user_balance::UserBalance;
use keycompute_provider_trait::{DefaultHttpTransport, HttpTransport};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
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
    pub created_at: String,
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
/// Admin 可以查询所有租户的用户
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 计算分页偏移量
    let offset = (params.page - 1) * params.page_size;

    // 查询所有用户（Admin 全局查询）
    let users = User::find_all(pool, params.page_size, offset)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query users: {}", e)))?;

    // 统计用户总数
    let total = User::count_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

    // 预加载所有租户到 HashMap（避免 N+1 查询）
    let tenants = Tenant::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;
    let tenant_map: std::collections::HashMap<Uuid, String> =
        tenants.into_iter().map(|t| (t.id, t.name)).collect();

    // 构建用户信息列表
    let mut result = Vec::new();
    for user in users {
        // 应用过滤条件
        if let Some(filter_tenant_id) = params.tenant_id {
            if user.tenant_id != filter_tenant_id {
                continue;
            }
        }
        if let Some(ref filter_role) = params.role {
            if &user.role != filter_role {
                continue;
            }
        }
        if let Some(ref search) = params.search {
            let search_lower = search.to_lowercase();
            let email_match = user.email.to_lowercase().contains(&search_lower);
            let name_match = user
                .name
                .as_ref()
                .map(|n| n.to_lowercase().contains(&search_lower))
                .unwrap_or(false);
            if !email_match && !name_match {
                continue;
            }
        }

        // 获取用户余额
        let balance = UserBalance::find_by_user(pool, user.id)
            .await
            .ok()
            .flatten();

        // 从缓存获取租户名称
        let tenant_name = tenant_map
            .get(&user.tenant_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());

        result.push(AdminUserInfo {
            id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
            role: user.role.clone(),
            tenant_id: user.tenant_id,
            tenant_name,
            balance: balance
                .as_ref()
                .map(|b| b.available_balance.to_f64().unwrap_or(0.0))
                .unwrap_or(0.0),
            created_at: user.created_at.to_rfc3339(),
            last_login_at: None,
        });
    }

    // 计算总页数
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
        .as_ref()
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
    let balance = UserBalance::find_by_user(pool, user.id)
        .await
        .ok()
        .flatten();

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
        created_at: user.created_at.to_rfc3339(),
        last_login_at: None,
    }))
}

/// 更新用户请求
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<String>,
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

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

    // 防止删除自己
    if user_id == auth.user_id {
        return Err(ApiError::BadRequest("Cannot delete yourself".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

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

/// 更新用户余额
///
/// POST /api/v1/users/{id}/balance
pub async fn update_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 解析金额
    let amount: Decimal = req
        .amount
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid amount format".to_string()))?;

    if amount == Decimal::ZERO {
        return Err(ApiError::BadRequest("Amount cannot be zero".to_string()));
    }

    // 获取或创建用户余额
    let balance = UserBalance::get_or_create(pool, auth.tenant_id, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get user balance: {}", e)))?;

    let balance_before = balance.available_balance;
    let balance_after = balance_before + amount;

    if balance_after < Decimal::ZERO {
        return Err(ApiError::BadRequest(
            "Insufficient balance for this operation".to_string(),
        ));
    }

    // 使用事务更新余额
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

    let (updated_balance, _transaction) = if amount > Decimal::ZERO {
        UserBalance::recharge(&mut tx, user_id, amount, None, Some(&req.reason)).await
    } else {
        // 负数金额视为消费
        UserBalance::consume(&mut tx, user_id, -amount, None, Some(&req.reason)).await
    }
    .map_err(|e| ApiError::Internal(format!("Failed to update balance: {}", e)))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance updated",
        "user_id": user_id,
        "amount": amount.to_string(),
        "reason": req.reason,
        "balance_before": balance_before.to_string(),
        "new_balance": updated_balance.available_balance.to_string(),
        "updated_by": auth.user_id,
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
        .as_ref()
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
        .map(|acc| {
            // 从 ProviderHealthStore 获取真实健康状态
            let is_healthy = state.provider_health.is_healthy(&acc.provider);

            // 从 AccountStateStore 获取实时 RPM
            let account_state = state.account_states.get(&acc.id);
            let current_rpm = account_state.current_rpm as i32;

            AccountInfo {
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
                current_rpm,
                is_active: acc.enabled,
                is_healthy,
                priority: acc.priority,
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
    State(state): State<AppState>,
) -> Result<Json<Vec<TenantInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let tenants = Tenant::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;

    let mut result = Vec::new();
    for tenant in tenants {
        // 统计租户用户数量
        let users = User::find_by_tenant(pool, tenant.id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

        let is_active = tenant.is_active();
        let description = tenant.description.clone();

        result.push(TenantInfo {
            id: tenant.id,
            name: tenant.name,
            description,
            user_count: users.len() as i64,
            is_active,
            created_at: tenant.created_at.to_rfc3339(),
        });
    }

    Ok(Json(result))
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

/// 系统设置（管理员视图，包含所有设置）
#[derive(Debug, Serialize, Deserialize)]
pub struct AdminSystemSettings {
    // 站点设置
    pub site_name: String,
    pub site_description: Option<String>,
    pub site_logo_url: Option<String>,
    pub site_favicon_url: Option<String>,

    // 注册设置
    pub allow_registration: bool,
    pub email_verification_required: bool,
    pub default_user_quota: f64,
    pub default_user_role: String,

    // 限流设置
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,

    // 系统状态
    pub maintenance_mode: bool,
    pub maintenance_message: Option<String>,

    // 分销设置
    pub distribution_enabled: bool,
    pub distribution_level1_default_ratio: f64,
    pub distribution_level2_default_ratio: f64,
    pub distribution_min_withdraw: f64,

    // 支付设置
    pub alipay_enabled: bool,
    pub wechatpay_enabled: bool,
    pub min_recharge_amount: f64,
    pub max_recharge_amount: f64,

    // 安全设置
    pub login_failed_limit: i32,
    pub login_lockout_minutes: i32,
    // 密码策略使用硬编码，参见 keycompute-auth/src/password/validator.rs

    // 公告设置
    pub system_notice: Option<String>,
    pub system_notice_enabled: bool,

    // 其他设置
    pub footer_content: Option<String>,
    pub about_content: Option<String>,
    pub terms_of_service_url: Option<String>,
    pub privacy_policy_url: Option<String>,
}

impl AdminSystemSettings {
    /// 从数据库设置列表构建
    pub fn from_settings(settings: &[keycompute_db::SystemSetting]) -> Self {
        use keycompute_db::models::system_setting::setting_keys;

        let get_value = |key: &str| -> Option<String> {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.value.clone())
        };

        let get_bool = |key: &str, default: bool| -> bool {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.parse_bool())
                .unwrap_or(default)
        };

        let get_int = |key: &str, default: i32| -> i32 {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.parse_int().unwrap_or(default))
                .unwrap_or(default)
        };

        let get_decimal = |key: &str, default: f64| -> f64 {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.parse_decimal().unwrap_or(default))
                .unwrap_or(default)
        };

        Self {
            site_name: get_value(setting_keys::SITE_NAME)
                .unwrap_or_else(|| "KeyCompute".to_string()),
            site_description: get_value(setting_keys::SITE_DESCRIPTION),
            site_logo_url: get_value(setting_keys::SITE_LOGO_URL),
            site_favicon_url: get_value(setting_keys::SITE_FAVICON_URL),

            allow_registration: get_bool(setting_keys::ALLOW_REGISTRATION, true),
            email_verification_required: get_bool(setting_keys::EMAIL_VERIFICATION_REQUIRED, true),
            default_user_quota: get_decimal(setting_keys::DEFAULT_USER_QUOTA, 10.0),
            default_user_role: get_value(setting_keys::DEFAULT_USER_ROLE)
                .unwrap_or_else(|| "user".to_string()),

            default_rpm_limit: get_int(setting_keys::DEFAULT_RPM_LIMIT, 60),
            default_tpm_limit: get_int(setting_keys::DEFAULT_TPM_LIMIT, 100000),

            maintenance_mode: get_bool(setting_keys::MAINTENANCE_MODE, false),
            maintenance_message: get_value(setting_keys::MAINTENANCE_MESSAGE),

            distribution_enabled: get_bool(setting_keys::DISTRIBUTION_ENABLED, false),
            distribution_level1_default_ratio: get_decimal(
                setting_keys::DISTRIBUTION_LEVEL1_DEFAULT_RATIO,
                0.03,
            ),
            distribution_level2_default_ratio: get_decimal(
                setting_keys::DISTRIBUTION_LEVEL2_DEFAULT_RATIO,
                0.01,
            ),
            distribution_min_withdraw: get_decimal(setting_keys::DISTRIBUTION_MIN_WITHDRAW, 10.0),

            alipay_enabled: get_bool(setting_keys::ALIPAY_ENABLED, false),
            wechatpay_enabled: get_bool(setting_keys::WECHATPAY_ENABLED, false),
            min_recharge_amount: get_decimal(setting_keys::MIN_RECHARGE_AMOUNT, 1.0),
            max_recharge_amount: get_decimal(setting_keys::MAX_RECHARGE_AMOUNT, 100000.0),

            login_failed_limit: get_int(setting_keys::LOGIN_FAILED_LIMIT, 5),
            login_lockout_minutes: get_int(setting_keys::LOGIN_LOCKOUT_MINUTES, 30),
            // 密码策略使用硬编码
            system_notice: get_value(setting_keys::SYSTEM_NOTICE),
            system_notice_enabled: get_bool(setting_keys::SYSTEM_NOTICE_ENABLED, false),

            footer_content: get_value(setting_keys::FOOTER_CONTENT),
            about_content: get_value(setting_keys::ABOUT_CONTENT),
            terms_of_service_url: get_value(setting_keys::TERMS_OF_SERVICE_URL),
            privacy_policy_url: get_value(setting_keys::PRIVACY_POLICY_URL),
        }
    }
}

/// 获取系统设置（管理员）
///
/// GET /api/v1/admin/settings
pub async fn get_system_settings(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<AdminSystemSettings>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let settings = keycompute_db::SystemSetting::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query settings: {}", e)))?;

    let admin_settings = AdminSystemSettings::from_settings(&settings);
    Ok(Json(admin_settings))
}

/// 更新系统设置（管理员）
///
/// PUT /api/v1/admin/settings
pub async fn update_system_settings(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 将 JSON 对象转换为 HashMap
    let settings_map: std::collections::HashMap<String, String> =
        if let serde_json::Value::Object(obj) = payload {
            obj.into_iter()
                .filter_map(|(k, v)| {
                    // 将 JSON 值转换为字符串
                    let value_str = match v {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => return None,
                        other => other.to_string(),
                    };
                    Some((k, value_str))
                })
                .collect()
        } else {
            return Err(ApiError::BadRequest("Invalid request body".to_string()));
        };

    // 批量更新设置
    let updated = keycompute_db::SystemSetting::batch_update(pool, &settings_map)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update settings: {}", e)))?;

    tracing::info!(
        user_id = %auth.user_id,
        count = updated.len(),
        "System settings updated by admin"
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("{} settings updated", updated.len()),
        "updated_by": auth.user_id,
    })))
}

/// 获取单个设置（管理员）
///
/// GET /api/v1/admin/settings/:key
pub async fn get_system_setting_by_key(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<keycompute_db::SystemSettingResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let setting = keycompute_db::SystemSetting::find_by_key(pool, &key)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query setting: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Setting not found: {}", key)))?;

    Ok(Json(setting.into()))
}

/// 更新单个设置（管理员）
///
/// PUT /api/v1/admin/settings/:key
pub async fn update_system_setting_by_key(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(payload): Json<keycompute_db::UpdateSystemSettingRequest>,
) -> Result<Json<keycompute_db::SystemSettingResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let setting = keycompute_db::SystemSetting::update_value(pool, &key, &payload.value)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update setting: {}", e)))?;

    tracing::info!(
        user_id = %auth.user_id,
        key = %key,
        "System setting updated by admin"
    );

    Ok(Json(setting.into()))
}

// ==================== 公开设置（无需认证） ====================

/// 获取公开系统设置
///
/// GET /api/v1/settings/public
///
/// 返回前端需要的非敏感系统设置，无需认证
pub async fn get_public_settings(
    State(state): State<AppState>,
) -> Result<Json<keycompute_db::PublicSettings>> {
    // 如果数据库未配置，返回默认设置
    let settings = if let Some(pool) = state.pool.as_ref() {
        keycompute_db::SystemSetting::get_public_settings(pool).await
    } else {
        keycompute_db::PublicSettings::default()
    };

    Ok(Json(settings))
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
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("admin@example.com"));
    }
}
