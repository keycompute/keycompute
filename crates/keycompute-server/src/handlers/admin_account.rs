//! 账号/渠道管理处理器
//!
//! 处理需要 Admin 权限的 Provider 账号管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    handlers::pagination::{normalize_list_pagination, total_pages},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use futures::StreamExt;
use keycompute_db::models::{
    account::{
        Account, CreateAccountRequest as DbCreateAccountRequest,
        UpdateAccountRequest as DbUpdateAccountRequest,
    },
    response_affinity::ResponseAffinity,
};
use keycompute_types::{AccountApiCapability, SensitiveString};
use llm_protocol_provider::{
    HttpTransport, NativeResponsesRequest, ProtocolType, UpstreamMessage, UpstreamRequest,
    normalize_base_url,
};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use subtle::ConstantTimeEq;
use tracing;
use uuid::Uuid;

const RESPONSES_PROBE_MAX_OUTPUT_TOKENS: u32 = 16;

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
    pub api_capabilities: Vec<String>,
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

#[derive(Debug, Deserialize)]
pub struct AccountListQueryParams {
    pub search: Option<String>,
    pub provider: Option<String>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AccountListResponse {
    pub accounts: Vec<AccountInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

fn parse_account_status(status: Option<&str>) -> Result<Option<bool>> {
    match status.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("all") => Ok(None),
        Some("active" | "enabled") => Ok(Some(true)),
        Some("inactive" | "disabled") => Ok(Some(false)),
        Some(status) => Err(ApiError::BadRequest(format!(
            "Invalid account status '{status}', expected active or inactive"
        ))),
    }
}

/// 列出所有账号（Admin 全局视图，不限租户）
///
/// GET /api/v1/accounts
pub async fn list_accounts(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<AccountListQueryParams>,
) -> Result<Json<AccountListResponse>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let enabled = parse_account_status(params.status.as_deref())?;
    let (page, page_size, offset) =
        normalize_list_pagination(params.page, params.page_size, params.limit, params.offset);
    let db_accounts = Account::find_all_filtered(
        pool,
        params.provider.as_deref(),
        enabled,
        params.search.as_deref(),
        page_size,
        offset,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to query accounts: {}", e)))?;
    let total = Account::count_all_filtered(
        pool,
        params.provider.as_deref(),
        enabled,
        params.search.as_deref(),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to count accounts: {}", e)))?;

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
                api_capabilities: acc.api_capabilities,
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

    Ok(Json(AccountListResponse {
        accounts,
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
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
    /// 账号支持的原生 API 表面；省略时使用协议的安全默认值。
    pub api_capabilities: Option<Vec<String>>,
    pub rpm_limit: Option<i32>,
    pub priority: Option<i32>,
    /// 可见性：'tenant' = 仅本租户可见（默认），'global' = 所有租户可见
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

fn default_visibility() -> String {
    "tenant".to_string()
}

fn default_api_capabilities(protocol: ProtocolType) -> Vec<String> {
    match protocol {
        // OpenAI-compatible vendors historically only guaranteed Chat Completions.
        // Responses must be opted into explicitly so old compatible endpoints are
        // never selected for a protocol they do not implement.
        ProtocolType::Openai => vec![AccountApiCapability::ChatCompletions.as_str().to_string()],
        ProtocolType::Anthropic => {
            vec![AccountApiCapability::Messages.as_str().to_string()]
        }
    }
}

fn normalize_api_capabilities(
    protocol: ProtocolType,
    capabilities: Option<&[String]>,
) -> std::result::Result<Vec<String>, String> {
    let Some(capabilities) = capabilities else {
        return Ok(default_api_capabilities(protocol));
    };
    if capabilities.is_empty() {
        return Err("At least one API capability must be specified".to_string());
    }

    let allowed: &[AccountApiCapability] = match protocol {
        ProtocolType::Openai => &[
            AccountApiCapability::ChatCompletions,
            AccountApiCapability::Responses,
        ],
        ProtocolType::Anthropic => &[AccountApiCapability::Messages],
    };
    let mut normalized = Vec::new();
    for raw in capabilities {
        let capability = AccountApiCapability::parse(raw).ok_or_else(|| {
            format!(
                "Unsupported API capability '{raw}'; expected chat_completions, responses, or messages"
            )
        })?;
        if !allowed.contains(&capability) {
            return Err(format!(
                "API capability '{}' is not valid for protocol '{}'",
                capability, protocol
            ));
        }
        let value = capability.as_str().to_string();
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
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

    // 校验协议类型：系统仅支持 openai / anthropic 两种协议，
    // 任何厂商（DeepSeek、Ollama、vLLM 等）通过协议 + base_url 接入
    let protocol = ProtocolType::parse(&req.provider).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "Unsupported protocol '{}', expected one of: openai, anthropic",
            req.provider
        ))
    })?;

    // 校验模型列表：必须至少提供一个模型（不再提供默认值）
    if req.models.is_empty() {
        return Err(ApiError::BadRequest(
            "At least one model must be specified for the channel account".to_string(),
        ));
    }

    // 规范化 Base URL：去尾部 '/'，拒绝带协议路径的输入（路径由协议层拼接）；
    // 空白输入视为未提供，使用协议默认端点（与 update 的空串重置语义一致）
    let api_base = match req.api_base.as_deref() {
        Some(url) if url.trim().is_empty() => None,
        Some(url) => Some(normalize_base_url(url).map_err(ApiError::BadRequest)?),
        None => None,
    };
    let api_capabilities = normalize_api_capabilities(protocol, req.api_capabilities.as_deref())
        .map_err(ApiError::BadRequest)?;

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
            tracing::warn!(
                "Global crypto key not set — storing upstream API key in plaintext. \
                 This is acceptable for development but should be fixed in production."
            );
            (
                req.api_key.clone(),
                format!("{}****", &req.api_key[..req.api_key.len().min(3)]),
            )
        };

    let db_req = DbCreateAccountRequest {
        tenant_id: auth.tenant_id,
        // 使用规范化后的协议名（小写），与路由/Gateway 注册键一致
        provider: protocol.as_str().to_string(),
        name: req.name.clone(),
        endpoint: api_base.unwrap_or_default(),
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        models_supported: req.models.clone(),
        api_capabilities,
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
        "api_capabilities": account.api_capabilities,
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
    pub api_capabilities: Option<Vec<String>>,
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

    let txn = pool
        .begin()
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to begin account update: {error}")))?;
    let existing = Account::find_by_id_for_update(&txn, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;
    let existing_protocol = ProtocolType::parse(&existing.provider).ok_or_else(|| {
        ApiError::Conflict(format!(
            "Account has unsupported protocol '{}'; please recreate it",
            existing.provider
        ))
    })?;

    // 规范化 Base URL（与 create 一致：只存 base，路径由协议层拼接）；
    // 显式空串表示重置为协议默认端点（存空 endpoint，
    // 与创建/路由的空 endpoint 回落语义一致），None 表示保持现状
    let api_base = match req.api_base.as_deref() {
        Some(url) if url.trim().is_empty() => Some(String::new()),
        Some(url) => Some(normalize_base_url(url).map_err(ApiError::BadRequest)?),
        None => None,
    };
    let endpoint_changed =
        requested_endpoint_changes(existing_protocol, &existing.endpoint, api_base.as_deref());
    let api_key_changed =
        requested_api_key_changes(&existing.upstream_api_key_encrypted, req.api_key.as_deref());
    let changes_upstream_connection = endpoint_changed || api_key_changed;
    if changes_upstream_connection {
        // Serialize actual connection-material updates with Responses
        // reservations and durable settlements. Merely resubmitting an
        // unchanged endpoint/key must not invalidate stored resource routes.
        let has_pending_responses_work =
            ResponseAffinity::lock_account_routes_and_has_deletion_blocker(&txn, account_id)
                .await
                .map_err(|error| {
                    ApiError::Internal(format!(
                        "Failed to inspect account Responses settlements: {error}"
                    ))
                })?;
        ensure_account_connection_update_has_no_pending_responses_work(has_pending_responses_work)?;
        // A stored upstream resource is scoped to the endpoint and credential
        // that created it. Once that connection identity changes, retaining its
        // old affinity would send the opaque resource ID to an unrelated
        // upstream account. Pending work is rejected above; settled routes can
        // be invalidated atomically with the account update.
        ResponseAffinity::delete_settled_account_routes(&txn, account_id)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to invalidate stale Responses routes: {error}"
                ))
            })?;
    }
    let api_capabilities = req
        .api_capabilities
        .as_deref()
        .map(|capabilities| normalize_api_capabilities(existing_protocol, Some(capabilities)))
        .transpose()
        .map_err(ApiError::BadRequest)?;

    // 处理 API Key 加密
    let (encrypted_key, key_preview) = if api_key_changed {
        let key = req
            .api_key
            .as_ref()
            .expect("a changed API key must have been supplied");
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
        endpoint: api_base,
        upstream_api_key_encrypted: encrypted_key,
        upstream_api_key_preview: key_preview,
        rpm_limit: req.rpm_limit,
        tpm_limit: None,
        priority: req.priority,
        enabled: req.is_active,
        models_supported: req.models.clone(),
        api_capabilities,
        visibility: req.visibility.clone(),
    };

    let updated = existing
        .update(&txn, &db_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update account: {}", e)))?;
    txn.commit()
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to commit account update: {error}")))?;

    if changes_upstream_connection {
        // Database-backed affinity lookups are authoritative, so stale Redis
        // entries cannot be used after the transaction commits. Drop this
        // replica's copies eagerly to avoid retaining dead entries until TTL.
        state
            .responses_affinity
            .write()
            .await
            .retain(|_, affinity| affinity.account_id != account_id);
    }

    // 返回更新后的账号信息
    Ok(Json(serde_json::json!({
        "id": updated.id.to_string(),
        "tenant_id": updated.tenant_id.to_string(),
        "name": updated.name,
        "provider": updated.provider,
        "api_key_preview": updated.upstream_api_key_preview,
        "api_base": if updated.endpoint.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(updated.endpoint)
        },
        "models": updated.models_supported,
        "api_capabilities": updated.api_capabilities,
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

    // Account deletion and Responses route draining share one writer
    // transaction. Locking the account first also prevents a concurrent
    // Responses create from adding a new foreign-key reference mid-delete.
    let txn = pool.begin().await.map_err(|error| {
        ApiError::Internal(format!("Failed to begin account deletion: {error}"))
    })?;
    let existing = Account::find_by_id_for_update(&txn, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))?;

    let has_pending_responses_work =
        ResponseAffinity::lock_account_routes_and_has_deletion_blocker(&txn, account_id)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to inspect account Responses settlements: {error}"
                ))
            })?;
    if let Err(error) =
        ensure_account_delete_has_no_pending_responses_work(has_pending_responses_work)
    {
        let _ = txn.rollback().await;
        return Err(error);
    }
    ResponseAffinity::delete_settled_account_routes(&txn, account_id)
        .await
        .map_err(|error| {
            ApiError::Internal(format!("Failed to drain account Responses routes: {error}"))
        })?;

    existing
        .delete(&txn)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete account: {}", e)))?;
    txn.commit().await.map_err(|error| {
        ApiError::Internal(format!("Failed to commit account deletion: {error}"))
    })?;

    // Redis entries expire with the normal Responses TTL. Remove process-local
    // entries immediately so this replica does not retain stale account routes.
    state
        .responses_affinity
        .write()
        .await
        .retain(|_, affinity| affinity.account_id != account_id);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Account deleted",
        "account_id": account_id,
        "deleted_by": auth.user_id,
    })))
}

fn ensure_account_delete_has_no_pending_responses_work(has_pending: bool) -> Result<()> {
    if has_pending {
        return Err(ApiError::Conflict(
            "Account has an active or background Responses request; retry deletion after it completes"
                .to_string(),
        ));
    }
    Ok(())
}

fn ensure_account_connection_update_has_no_pending_responses_work(has_pending: bool) -> Result<()> {
    if has_pending {
        return Err(ApiError::Conflict(
            "Account endpoint or API key cannot change while a Responses request is active or awaiting billing settlement"
                .to_string(),
        ));
    }
    Ok(())
}

fn requested_endpoint_changes(
    protocol: ProtocolType,
    existing_endpoint: &str,
    requested_endpoint: Option<&str>,
) -> bool {
    let Some(requested_endpoint) = requested_endpoint else {
        return false;
    };
    let existing_endpoint = if existing_endpoint.is_empty() {
        protocol.default_endpoint()
    } else {
        existing_endpoint
    };
    let requested_endpoint = if requested_endpoint.is_empty() {
        protocol.default_endpoint()
    } else {
        requested_endpoint
    };
    requested_endpoint != existing_endpoint
}

fn requested_api_key_changes(
    existing_encrypted_key: &str,
    requested_api_key: Option<&str>,
) -> bool {
    let Some(requested_api_key) = requested_api_key else {
        return false;
    };
    let Ok(existing_api_key) = decrypt_account_api_key(existing_encrypted_key) else {
        // A replacement key is also the recovery path for corrupt or
        // no-longer-decryptable stored material. Conservatively treat it as a
        // real rotation so all normal reservation and affinity guards apply.
        return true;
    };
    !bool::from(
        existing_api_key
            .as_bytes()
            .ct_eq(requested_api_key.as_bytes()),
    )
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

    Ok(Json(
        probe_account_for_monitoring(&state, account_id).await?,
    ))
}

pub async fn probe_account_for_monitoring(
    state: &AppState,
    account_id: Uuid,
) -> Result<serde_json::Value> {
    probe_account_for_monitoring_with_policy(state, account_id, AccountProbePolicy::Explicit)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Account not found: {}", account_id)))
}

/// Probe an account only while it is enabled on the writer.
///
/// Replica reads may still expose an account after it has been disabled or
/// deleted. Automatic jobs must use this entry point so that a stale candidate
/// never results in a real upstream inference request.
pub async fn probe_enabled_account_for_monitoring(
    state: &AppState,
    account_id: Uuid,
) -> Result<Option<serde_json::Value>> {
    probe_account_for_monitoring_with_policy(state, account_id, AccountProbePolicy::EnabledOnly)
        .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccountProbePolicy {
    Explicit,
    EnabledOnly,
}

fn account_matches_probe_policy(enabled: bool, policy: AccountProbePolicy) -> bool {
    policy == AccountProbePolicy::Explicit || enabled
}

async fn probe_account_for_monitoring_with_policy(
    state: &AppState,
    account_id: Uuid,
    policy: AccountProbePolicy,
) -> Result<Option<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;
    let writer = pool.write_conn();

    // Reload on the writer immediately before the network call. The candidate
    // list may have come from a lagging replica, but account deletion, disable,
    // credentials, endpoint, and model configuration must all be fresh here.
    let account = Account::find_by_id(writer, account_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find account: {}", e)))?
        .and_then(|account| {
            account_matches_probe_policy(account.enabled, policy).then_some(account)
        });
    let Some(account) = account else {
        return match policy {
            AccountProbePolicy::Explicit => Err(ApiError::NotFound(format!(
                "Account not found: {}",
                account_id
            ))),
            AccountProbePolicy::EnabledOnly => Ok(None),
        };
    };

    // 解密 API Key
    let api_key = decrypt_account_api_key(&account.upstream_api_key_encrypted)?;

    // 解析账号协议（非法值是数据状态问题而非服务器故障，返回 409 提示重建账号）
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::Conflict(format!(
            "Account has unsupported protocol '{}'; please recreate it with 'openai' or 'anthropic'",
            account.provider
        ))
    })?;

    // 构建 endpoint（Base URL）
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        account.endpoint.clone()
    };

    // Probe through the same provider/account proxy and timeout path as live
    // traffic, otherwise health can disagree with the production route.
    let transport = state
        .http_proxy
        .client_for_provider_and_account(protocol.as_str(), Some(account_id));

    let start = Instant::now();

    // 按协议分发调用上游模型列表接口，验证 API Key 连通性
    let test_result = probe_upstream_account(
        protocol,
        transport.as_ref(),
        &endpoint,
        &api_key,
        &account.models_supported,
        &account.api_capabilities,
    )
    .await;

    let latency_ms = start.elapsed().as_millis() as i64;
    let (probe_status, probe_error_code) = if test_result.is_ok() {
        ("succeeded", None)
    } else {
        ("failed", test_result.as_ref().err().cloned())
    };
    keycompute_observability::metrics::ACCOUNT_PROBE_TOTAL
        .with_label_values(&[probe_status])
        .inc();
    keycompute_observability::metrics::ACCOUNT_PROBE_LATENCY
        .with_label_values(&[probe_status])
        .observe(latency_ms.max(0) as f64 / 1000.0);
    match Account::record_probe_snapshot_if_config_current(
        writer,
        account_id,
        account.updated_at,
        chrono::Utc::now(),
        latency_ms,
        probe_status,
        probe_error_code.as_deref(),
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => tracing::debug!(
            %account_id,
            "discarded account probe snapshot because configuration changed during the probe"
        ),
        Err(error) => {
            tracing::warn!(%account_id, %error, "failed to persist account probe snapshot");
        }
    }

    match test_result {
        Ok(models) => Ok(Some(account_test_response(
            account_id,
            &account.provider,
            &endpoint,
            latency_ms,
            Some(models),
            None,
            &account.api_capabilities,
        ))),
        Err(_) => {
            // 上游响应可能包含供应商内部详情或凭据回显；不记录或返回其原始内容。
            tracing::warn!(
                account_id = %account_id,
                provider = %account.provider,
                "Account connection test failed"
            );
            Ok(Some(account_test_response(
                account_id,
                &account.provider,
                &endpoint,
                latency_ms,
                None,
                probe_error_code.as_deref(),
                &account.api_capabilities,
            )))
        }
    }
}

fn account_test_response(
    account_id: Uuid,
    provider: &str,
    endpoint: &str,
    latency_ms: i64,
    models: Option<Vec<String>>,
    error_code: Option<&str>,
    api_capabilities: &[String],
) -> serde_json::Value {
    // Keep the summary at the response root: AccountTestResponse is shared by
    // dashboard clients and deliberately does not need to parse test_result.
    match models {
        Some(models) => serde_json::json!({
            "success": true,
            "message": "Account connection test passed",
            "account_id": account_id,
            "latency_ms": latency_ms,
            "test_result": {
                "is_healthy": true,
                "latency_ms": latency_ms,
                "available_models": models,
                "tested_capabilities": api_capabilities,
                "provider": provider,
                "endpoint": endpoint,
            }
        }),
        None => serde_json::json!({
            "success": false,
            "message": "Account connection test failed",
            "account_id": account_id,
            "latency_ms": latency_ms,
            "test_result": {
                "is_healthy": false,
                "latency_ms": latency_ms,
                "error": "Upstream connection test failed",
                "error_code": error_code.unwrap_or("probe_failed"),
                "tested_capabilities": api_capabilities,
                "provider": provider,
                "endpoint": endpoint,
            }
        }),
    }
}

/// 按协议获取上游模型列表（兼作连通性验证）
///
/// 通过 Provider 注册表取对应协议的 adapter 调用 `list_models`，
/// 认证方式由协议实现自行处理（openai: Bearer；anthropic: x-api-key），
/// 避免在 handler 层重复协议认证逻辑
async fn fetch_upstream_models(
    protocol: ProtocolType,
    transport: &dyn HttpTransport,
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<Vec<String>, String> {
    let adapter = crate::providers::get_provider_definition(protocol.as_str())
        .map(|def| (def.create_adapter)())
        .ok_or_else(|| format!("Provider '{}' not registered", protocol))?;

    let key = SensitiveString::new(api_key);
    adapter
        .list_models(transport, endpoint, &key)
        .await
        .map_err(|e| e.to_string())
}

async fn probe_upstream_account(
    protocol: ProtocolType,
    transport: &dyn HttpTransport,
    endpoint: &str,
    api_key: &str,
    configured_models: &[String],
    api_capabilities: &[String],
) -> std::result::Result<Vec<String>, String> {
    let adapter = crate::providers::get_provider_definition(protocol.as_str())
        .map(|definition| (definition.create_adapter)())
        .ok_or_else(|| "provider_not_registered".to_string())?;
    let key = SensitiveString::new(api_key);
    // A configured model is sufficient for the real inference probe. Some
    // OpenAI-compatible services intentionally expose chat completions without
    // implementing GET /models, so requiring that endpoint would create a
    // false-negative health result. Only discover models when configuration
    // cannot provide one.
    let (model, reported_models) = if let Some(model) = configured_models.first() {
        (model.clone(), configured_models.to_vec())
    } else {
        let discovered = adapter
            .list_models(transport, endpoint, &key)
            .await
            .map_err(|error| probe_error_code(&error))?;
        let model = discovered
            .first()
            .ok_or_else(|| "probe_model_unavailable".to_string())?
            .clone();
        (model, discovered)
    };
    for capability in api_capabilities {
        let capability = AccountApiCapability::parse(capability)
            .ok_or_else(|| "account_capability_invalid".to_string())?;
        let request = UpstreamRequest {
            endpoint: endpoint.to_string(),
            upstream_api_key: key.clone(),
            model: model.clone(),
            messages: vec![UpstreamMessage {
                role: "user".to_string(),
                content: keycompute_types::MessageContent::text("ping"),
            }],
            stream: false,
            include_stream_usage: true,
            max_tokens: Some(if capability == AccountApiCapability::Responses {
                RESPONSES_PROBE_MAX_OUTPUT_TOKENS
            } else {
                1
            }),
            // Reasoning-oriented Responses models may reject temperature even
            // though text-generation models accept it. A connectivity probe
            // should use the smallest common request surface.
            temperature: if capability == AccountApiCapability::Responses {
                None
            } else {
                Some(0.0)
            },
            top_p: None,
            native_openai_chat_request: None,
            native_anthropic_request: None,
            native_anthropic_headers: BTreeMap::new(),
        };
        match capability {
            AccountApiCapability::ChatCompletions | AccountApiCapability::Messages => {
                adapter
                    .chat(transport, request)
                    .await
                    .map_err(|error| probe_error_code(&error))?;
            }
            AccountApiCapability::Responses => {
                let mut response = adapter
                    .stream_responses_with_meta(
                        transport,
                        request,
                        NativeResponsesRequest {
                            body: Arc::new(serde_json::json!({
                                "model": model.clone(),
                                "input": "ping",
                                "max_output_tokens": RESPONSES_PROBE_MAX_OUTPUT_TOKENS,
                                "store": false,
                                "stream": false,
                            })),
                            path: "/responses".to_string(),
                            headers: BTreeMap::new(),
                        },
                    )
                    .await
                    .map_err(|error| error.stable_error_code)?;
                if !(200..300).contains(&response.meta.status) {
                    return Err(format!("upstream_http_{}", response.meta.status));
                }
                while let Some(event) = response.body.next().await {
                    event.map_err(|error| probe_error_code(&error))?;
                }
            }
        }
    }
    Ok(reported_models)
}

fn probe_error_code(error: &keycompute_types::KeyComputeError) -> String {
    match error {
        keycompute_types::KeyComputeError::UpstreamFailure { stable_code, .. } => {
            stable_code.clone()
        }
        keycompute_types::KeyComputeError::ProviderTimeout(_, _)
        | keycompute_types::KeyComputeError::Timeout(_) => "upstream_timeout".to_string(),
        keycompute_types::KeyComputeError::NetworkError(_) => "upstream_transport".to_string(),
        keycompute_types::KeyComputeError::SerializationError(_) => "upstream_protocol".to_string(),
        _ => "probe_failed".to_string(),
    }
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

    // 解析账号协议（非法值返回 409，与 test_account 一致）
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::Conflict(format!(
            "Account has unsupported protocol '{}'; please recreate it with 'openai' or 'anthropic'",
            account.provider
        ))
    })?;

    // 构建 endpoint（Base URL）
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        account.endpoint.clone()
    };

    let transport = state
        .http_proxy
        .client_for_provider_and_account(protocol.as_str(), Some(account_id));

    // 按协议分发获取上游模型列表
    let fetched_models = fetch_upstream_models(protocol, transport.as_ref(), &endpoint, &api_key)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch models: {}", e)))?;

    // 更新数据库
    let db_req = refresh_models_update_request(fetched_models);

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

/// Build the targeted update used after a successful upstream model refresh.
///
/// A valid empty `/models` response is persisted as an empty list so routing
/// cannot continue selecting models that the account no longer exposes.
fn refresh_models_update_request(models_supported: Vec<String>) -> DbUpdateAccountRequest {
    DbUpdateAccountRequest {
        tenant_id: None,
        name: None,
        endpoint: None,
        upstream_api_key_encrypted: None,
        upstream_api_key_preview: None,
        rpm_limit: None,
        tpm_limit: None,
        priority: None,
        enabled: None,
        models_supported: Some(models_supported),
        api_capabilities: None,
        visibility: None,
    }
}

/// 解密账号的 API Key
pub fn decrypt_account_api_key(encrypted_key: &str) -> Result<String> {
    // 生产环境配置了全局加密器后，存储值必须是有效密文。将解密失败的密文
    // 当作明文继续使用会掩盖密钥轮换/数据损坏，并可能把无效内容发给上游。
    if keycompute_runtime::crypto::global_crypto().is_some() {
        return keycompute_runtime::crypto::decrypt_api_key(
            &keycompute_runtime::EncryptedApiKey::from(encrypted_key),
        )
        .map_err(|e| {
            tracing::warn!(error = %e, "Failed to decrypt account API key");
            ApiError::Internal("Failed to decrypt stored account API key".to_string())
        });
    }

    // 仅在未配置全局加密器的开发环境中允许旧的明文存储。
    Ok(encrypted_key.to_string())
}

/// 获取协议的默认 endpoint（Base URL）
pub fn get_default_endpoint(provider: &str) -> String {
    ProtocolType::parse(provider)
        .map(|p| p.default_endpoint().to_string())
        // 非法协议名回退 openai 默认端点（创建入口已校验，此处仅防御）；
        // 静默回落会掩盖数据问题，补充告警日志便于定位
        .unwrap_or_else(|| {
            tracing::warn!(
                provider = %provider,
                "Unknown protocol, falling back to openai default endpoint"
            );
            ProtocolType::Openai.default_endpoint().to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_status_filter_accepts_documented_values() {
        assert_eq!(parse_account_status(None).unwrap(), None);
        assert_eq!(parse_account_status(Some("active")).unwrap(), Some(true));
        assert_eq!(parse_account_status(Some("disabled")).unwrap(), Some(false));
        assert!(parse_account_status(Some("unknown")).is_err());
    }
    use llm_protocol_provider::{
        ByteStream, GetBinaryResponse, UpstreamResponse, UpstreamResponseMeta,
        test_support::RecordingGetTransport,
    };
    use serde_json::json;
    use std::{
        sync::Mutex,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    #[derive(Debug, Default)]
    struct ChatOnlyProbeTransport {
        get_calls: AtomicUsize,
        post_calls: AtomicUsize,
        post_urls: Mutex<Vec<String>>,
        post_bodies: Mutex<Vec<String>>,
        responses_status: Option<u16>,
        responses_body: Option<String>,
    }

    #[async_trait::async_trait]
    impl HttpTransport for ChatOnlyProbeTransport {
        async fn post_json_passthrough_response(
            &self,
            url: &str,
            _headers: Vec<(String, String)>,
            body: String,
        ) -> std::result::Result<
            UpstreamResponse<llm_protocol_provider::AdmittedResponseText>,
            llm_protocol_provider::UpstreamFailure,
        > {
            self.post_calls.fetch_add(1, Ordering::Relaxed);
            self.post_urls.lock().unwrap().push(url.to_string());
            self.post_bodies.lock().unwrap().push(body);
            Ok(UpstreamResponse {
                meta: UpstreamResponseMeta {
                    status: self.responses_status.unwrap_or(200),
                    headers_received_at: chrono::Utc::now(),
                    upstream_request_id: None,
                    headers: Vec::new(),
                },
                body: self
                    .responses_body
                    .clone()
                    .unwrap_or_else(|| r#"{"id":"resp_probe","object":"response","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}"#.to_string())
                    .into(),
            })
        }

        async fn post_json(
            &self,
            url: &str,
            _headers: Vec<(String, String)>,
            body: String,
        ) -> keycompute_types::Result<String> {
            self.post_calls.fetch_add(1, Ordering::Relaxed);
            self.post_urls.lock().unwrap().push(url.to_string());
            self.post_bodies.lock().unwrap().push(body);
            if url.ends_with("/responses") {
                Ok(r#"{"id":"resp_probe","object":"response","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}"#.to_string())
            } else {
                Ok(r#"{"id":"probe","object":"chat.completion","created":0,"model":"configured-model","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop","logprobs":null}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#.to_string())
            }
        }

        async fn post_stream(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> keycompute_types::Result<ByteStream> {
            unreachable!("the account probe is non-streaming")
        }

        async fn get_binary(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
        ) -> keycompute_types::Result<GetBinaryResponse> {
            self.get_calls.fetch_add(1, Ordering::Relaxed);
            Err(keycompute_types::KeyComputeError::ProviderError(
                "GET /models is intentionally unavailable".to_string(),
            ))
        }

        fn request_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn stream_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }
    }

    #[tokio::test]
    async fn configured_probe_model_does_not_require_models_endpoint() {
        let transport = ChatOnlyProbeTransport::default();
        let models = probe_upstream_account(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1",
            "test-key",
            &["configured-model".to_string()],
            &[AccountApiCapability::ChatCompletions.as_str().to_string()],
        )
        .await
        .unwrap();

        assert_eq!(models, vec!["configured-model"]);
        assert_eq!(transport.post_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.get_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn responses_only_probe_calls_the_responses_endpoint() {
        let transport = ChatOnlyProbeTransport::default();

        probe_upstream_account(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1",
            "test-key",
            &["configured-model".to_string()],
            &[AccountApiCapability::Responses.as_str().to_string()],
        )
        .await
        .unwrap();

        assert_eq!(
            *transport.post_urls.lock().unwrap(),
            ["https://provider.example/v1/responses"]
        );
        assert_eq!(transport.get_calls.load(Ordering::Relaxed), 0);
        let body: serde_json::Value =
            serde_json::from_str(&transport.post_bodies.lock().unwrap()[0]).unwrap();
        assert_eq!(body["max_output_tokens"], RESPONSES_PROBE_MAX_OUTPUT_TOKENS);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], false);
    }

    #[tokio::test]
    async fn responses_probe_rejects_non_success_http_status() {
        let transport = ChatOnlyProbeTransport {
            responses_status: Some(401),
            ..Default::default()
        };

        let error = probe_upstream_account(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1",
            "bad-key",
            &["configured-model".to_string()],
            &[AccountApiCapability::Responses.as_str().to_string()],
        )
        .await
        .unwrap_err();

        assert_eq!(error, "upstream_http_401");
    }

    #[tokio::test]
    async fn responses_probe_rejects_malformed_success_body() {
        let transport = ChatOnlyProbeTransport {
            responses_body: Some(json!({"error": {"message": "bad request"}}).to_string()),
            ..Default::default()
        };

        let error = probe_upstream_account(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1",
            "test-key",
            &["configured-model".to_string()],
            &[AccountApiCapability::Responses.as_str().to_string()],
        )
        .await
        .unwrap_err();

        assert_eq!(error, "upstream_protocol");
    }

    #[tokio::test]
    async fn dual_capability_probe_verifies_both_endpoints() {
        let transport = ChatOnlyProbeTransport::default();

        probe_upstream_account(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1",
            "test-key",
            &["configured-model".to_string()],
            &[
                AccountApiCapability::ChatCompletions.as_str().to_string(),
                AccountApiCapability::Responses.as_str().to_string(),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            *transport.post_urls.lock().unwrap(),
            [
                "https://provider.example/v1/chat/completions",
                "https://provider.example/v1/responses",
            ]
        );
    }

    #[tokio::test]
    async fn fetch_upstream_models_rejects_invalid_models_payload() {
        let transport = RecordingGetTransport::new(br#"{"data": "invalid"}"#.to_vec());

        let error = fetch_upstream_models(
            ProtocolType::Openai,
            &transport,
            "https://provider.example/v1/",
            "test-key",
        )
        .await
        .unwrap_err();

        assert!(error.contains("Invalid /models response"));
        assert_eq!(
            transport.requests(),
            vec![(
                "https://provider.example/v1/models".to_string(),
                vec![("Authorization".to_string(), "Bearer test-key".to_string())],
            )]
        );
    }

    #[tokio::test]
    async fn fetch_upstream_models_accepts_empty_models_payload() {
        let transport = RecordingGetTransport::new(br#"{"data": []}"#.to_vec());

        let models = fetch_upstream_models(
            ProtocolType::Anthropic,
            &transport,
            "https://provider.example/v1/",
            "test-key",
        )
        .await
        .unwrap();

        assert!(models.is_empty());
        assert_eq!(
            transport.requests(),
            vec![(
                "https://provider.example/v1/models".to_string(),
                vec![
                    ("x-api-key".to_string(), "test-key".to_string()),
                    (
                        "anthropic-version".to_string(),
                        llm_protocol_anthropic::ANTHROPIC_API_VERSION.to_string(),
                    ),
                ],
            )]
        );
    }

    #[test]
    fn account_test_responses_include_root_latency_for_shared_clients() {
        let account_id = Uuid::nil();
        let success = account_test_response(
            account_id,
            "openai",
            "https://provider.example/v1",
            42,
            Some(vec!["model-a".to_string()]),
            None,
            &["chat_completions".to_string()],
        );
        let failure = account_test_response(
            account_id,
            "openai",
            "https://provider.example/v1",
            42,
            None,
            Some("upstream_http_401"),
            &["chat_completions".to_string()],
        );
        let empty_models = account_test_response(
            account_id,
            "openai",
            "https://provider.example/v1",
            42,
            Some(Vec::new()),
            None,
            &["responses".to_string()],
        );

        assert_eq!(success["success"], json!(true));
        assert_eq!(failure["success"], json!(false));
        assert_eq!(success["latency_ms"], json!(42));
        assert_eq!(failure["latency_ms"], json!(42));
        assert_eq!(empty_models["success"], json!(true));
        assert_eq!(empty_models["test_result"]["available_models"], json!([]));
        assert_eq!(
            empty_models["test_result"]["tested_capabilities"],
            json!(["responses"])
        );
        assert_eq!(
            failure["test_result"]["error"],
            "Upstream connection test failed"
        );
        assert_eq!(failure["test_result"]["error_code"], "upstream_http_401");
    }

    #[test]
    fn probe_snapshot_uses_stable_upstream_error_code() {
        let error = keycompute_types::KeyComputeError::UpstreamFailure {
            status: Some(429),
            stable_code: "upstream_http_429".to_string(),
            retryable: true,
            summary: "sanitized".to_string(),
        };
        assert_eq!(probe_error_code(&error), "upstream_http_429");
    }

    #[test]
    fn automatic_probe_skips_accounts_disabled_on_writer() {
        assert!(!account_matches_probe_policy(
            false,
            AccountProbePolicy::EnabledOnly
        ));
        assert!(account_matches_probe_policy(
            true,
            AccountProbePolicy::EnabledOnly
        ));
        assert!(account_matches_probe_policy(
            false,
            AccountProbePolicy::Explicit
        ));
    }

    #[test]
    fn encrypted_mode_rejects_invalid_stored_api_keys() {
        let key = keycompute_runtime::ApiKeyCrypto::generate_key();
        keycompute_runtime::set_global_crypto(&key).unwrap();

        assert!(matches!(
            decrypt_account_api_key("not-a-valid-encrypted-api-key"),
            Err(ApiError::Internal(_))
        ));
    }

    #[test]
    fn refresh_persists_a_valid_empty_model_list() {
        let request = refresh_models_update_request(Vec::new());

        assert_eq!(request.models_supported, Some(Vec::new()));
        assert_eq!(request.api_capabilities, None);
    }

    #[test]
    fn api_capabilities_are_protocol_scoped_and_deduplicated() {
        assert_eq!(
            normalize_api_capabilities(ProtocolType::Openai, None).unwrap(),
            ["chat_completions"]
        );
        assert_eq!(
            normalize_api_capabilities(
                ProtocolType::Openai,
                Some(&[
                    "responses".to_string(),
                    "responses".to_string(),
                    "chat_completions".to_string(),
                ]),
            )
            .unwrap(),
            ["responses", "chat_completions"]
        );
        assert!(
            normalize_api_capabilities(ProtocolType::Anthropic, Some(&["responses".to_string()]),)
                .is_err()
        );
        assert!(normalize_api_capabilities(ProtocolType::Openai, Some(&[])).is_err());
    }

    #[test]
    fn active_or_background_responses_work_blocks_account_deletion() {
        assert!(ensure_account_delete_has_no_pending_responses_work(false).is_ok());
        assert!(matches!(
            ensure_account_delete_has_no_pending_responses_work(true),
            Err(ApiError::Conflict(message)) if message.contains("active or background")
        ));
    }

    #[test]
    fn active_or_background_responses_work_blocks_connection_material_updates() {
        assert!(ensure_account_connection_update_has_no_pending_responses_work(false).is_ok());
        assert!(matches!(
            ensure_account_connection_update_has_no_pending_responses_work(true),
            Err(ApiError::Conflict(message))
                if message.contains("endpoint or API key")
                    && message.contains("billing settlement")
        ));
    }

    #[test]
    fn resubmitting_the_same_normalized_endpoint_is_not_a_connection_change() {
        let normalized = normalize_base_url("https://proxy.example/v1/").unwrap();
        assert!(!requested_endpoint_changes(
            ProtocolType::Openai,
            "https://proxy.example/v1",
            None
        ));
        assert!(!requested_endpoint_changes(
            ProtocolType::Openai,
            "https://proxy.example/v1",
            Some(&normalized)
        ));
        assert!(requested_endpoint_changes(
            ProtocolType::Openai,
            "https://proxy.example/v1",
            Some("https://other.example/v1")
        ));
        assert!(requested_endpoint_changes(
            ProtocolType::Openai,
            "https://proxy.example/v1",
            Some("")
        ));
        assert!(!requested_endpoint_changes(
            ProtocolType::Openai,
            "",
            Some(ProtocolType::Openai.default_endpoint())
        ));
        assert!(!requested_endpoint_changes(
            ProtocolType::Openai,
            ProtocolType::Openai.default_endpoint(),
            Some("")
        ));
    }

    #[test]
    fn resubmitting_the_same_api_key_is_not_a_connection_change() {
        let key = keycompute_runtime::ApiKeyCrypto::generate_key();
        keycompute_runtime::set_global_crypto(&key).unwrap();
        let encrypted = keycompute_runtime::encrypt_api_key("upstream-secret")
            .expect("test API key should encrypt")
            .into_inner();

        assert!(!requested_api_key_changes(&encrypted, None));
        assert!(!requested_api_key_changes(
            &encrypted,
            Some("upstream-secret")
        ));
        assert!(requested_api_key_changes(
            &encrypted,
            Some("rotated-secret")
        ));
        assert!(requested_api_key_changes(
            "not-a-valid-encrypted-api-key",
            Some("replacement-secret")
        ));
    }
}
