//! 用户节点网关注册令牌管理
//!
//! 审批制 + HMAC 签名：
//!   1. POST   → 用户申请 token → status='pending'（返回预览 + 状态）
//!   2. Admin审批 → status='approved'
//!   3. GET    → 用户查看 token 明文（始终可重建，is_revealed=true 时附加提醒）
//!   4. 节点注册 → status='consumed'（一次性使用）

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::http::StatusCode;
use axum::{
    Json,
    extract::{Path, State},
};
use keycompute_db::models::user::User;
use keycompute_db::models::user_node_gateway_token::{
    PendingTokenWithUser, UserNodeGatewayToken, UserNodeGatewayTokenResponse,
};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 已注册节点信息（用户端查看令牌时展示）
#[derive(Debug, Serialize, FromQueryResult)]
pub struct RegisteredNodeInfo {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 获取当前用户的 token 响应
#[derive(Debug, Serialize)]
pub struct NodeGatewayTokenDetailResponse {
    pub token: UserNodeGatewayTokenResponse,
    /// token 明文（status='approved' 时始终返回；is_revealed=true 时仍返回但附带安全提醒）
    pub registration_token: Option<String>,
    /// 提示信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 已注册的节点信息（token 已消费或被吊销时返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_node: Option<RegisteredNodeInfo>,
}

/// 构建单个 token 的响应（从 UserNodeGatewayToken 到 NodeGatewayTokenDetailResponse）
async fn build_token_response(
    pool: &impl ConnectionTrait,
    state: &AppState,
    t: UserNodeGatewayToken,
) -> Result<NodeGatewayTokenDetailResponse> {
    let token_response = UserNodeGatewayTokenResponse::from(t.clone());

    let (plaintext, message) = if t.status == "approved" {
        let secret = state.node_gateway_secret().ok_or_else(|| {
            ApiError::Internal("Registration token secret not configured".to_string())
        })?;

        let plain = UserNodeGatewayToken::reconstruct_token(secret.as_bytes(), t.id);

        let msg = if t.is_revealed {
            Some("Token has already been viewed. Please save it securely this time.".to_string())
        } else {
            if let Err(e) = t.mark_revealed(pool).await {
                tracing::warn!(
                    token_id = %t.id,
                    error = %e,
                    "Failed to mark token as revealed; token remains unread-aware"
                );
            }
            None
        };

        (Some(plain), msg)
    } else if t.status == "pending" {
        (None, Some("Token is pending admin approval.".to_string()))
    } else if t.status == "rejected" {
        let msg = if t.consumed_node_id.is_some() {
            Some(
                "Registration token has been revoked. Your existing node may still be operational."
                    .to_string(),
            )
        } else {
            Some("Token was rejected by admin. You may re-apply.".to_string())
        };
        (None, msg)
    } else if t.status == "consumed" {
        (
            None,
            Some("Token has been used for node registration.".to_string()),
        )
    } else {
        (None, None)
    };

    let registered_node: Option<RegisteredNodeInfo> = if t.status == "consumed"
        || (t.status == "rejected" && t.consumed_node_id.is_some())
        || (t.status == "approved" && t.consumed_node_id.is_some())
    {
        if let Some(node_id) = t.consumed_node_id {
            let stmt = Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"
                    SELECT
                        id,
                        display_name,
                        CASE
                            WHEN status = 'online'
                                 AND last_heartbeat_at IS NOT NULL
                                 AND last_heartbeat_at < NOW() - INTERVAL '3 minutes'
                            THEN 'offline'
                            ELSE status
                        END AS status,
                        last_heartbeat_at
                    FROM nodes
                    WHERE id = $1
                    "#,
                [node_id.into()],
            );
            RegisteredNodeInfo::find_by_statement(stmt)
                .one(pool)
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    } else {
        None
    };

    Ok(NodeGatewayTokenDetailResponse {
        token: token_response,
        registration_token: plaintext,
        message,
        registered_node,
    })
}

/// 获取当前用户的节点网关注册令牌（最近一条）
///
/// GET /api/v1/me/node-gateway/token
///
/// - 如果用户没有申请过 token → 404
/// - 如果 token 状态为 'approved' → 始终返回 token 明文（is_revealed=true 时附加安全提醒）
/// - 否则只返回 token 信息（不含明文）
pub async fn get_my_node_gateway_token(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<NodeGatewayTokenDetailResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let token = UserNodeGatewayToken::find_latest_by_user(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query token: {}", e)))?;

    match token {
        Some(t) => Ok(Json(build_token_response(pool, &state, t).await?)),
        None => Err(ApiError::NotFound(
            "No node gateway token found. Create one via POST.".to_string(),
        )),
    }
}

/// 获取当前用户的所有节点网关注册令牌（历史列表）
///
/// GET /api/v1/me/node-gateway/tokens
///
/// 返回用户历史上所有申请的令牌，按 issued_at 倒序排列。
pub async fn list_my_node_gateway_tokens(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<NodeGatewayTokenDetailResponse>>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let tokens = UserNodeGatewayToken::find_all_by_user(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tokens: {}", e)))?;

    let mut responses = Vec::with_capacity(tokens.len());
    for t in tokens {
        responses.push(build_token_response(pool, &state, t).await?);
    }

    Ok(Json(responses))
}

/// 创建/刷新当前用户的节点网关注册令牌（申请）
///
/// POST /api/v1/me/node-gateway/token
///
/// - 如果用户已有 pending token → 返回已有 token 信息
/// - 如果用户已有 approved 但未消费的 token → 返回已有 token 信息
/// - 否则创建新的 pending token
/// - token 明文在审批通过后 GET 时可随时查看（is_revealed 标记仅用于提醒）
pub async fn create_my_node_gateway_token(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<NodeGatewayTokenDetailResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let secret = state.node_gateway_secret().ok_or_else(|| {
        ApiError::Internal("Registration token secret not configured".to_string())
    })?;

    // 1. 检查是否已有阻止新申请的令牌
    //    阻止状态: pending(待审批) / approved(已通过) / consumed(已使用) / rejected+revoke_reason(已吊销)
    if let Some(existing) = UserNodeGatewayToken::find_blocking_token(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query existing token: {}", e)))?
    {
        let message = match existing.status.as_str() {
            "pending" => "You already have a pending token. Please wait for admin approval.",
            "approved" => {
                "You already have an approved token. Revoke it first before applying for a new one."
            }
            "consumed" => {
                "You already have a token in use by a registered node. Revoke the node first before applying for a new one."
            }
            "rejected" => {
                "Your token was revoked by admin but the associated node still exists. Contact admin to recover or delete the node first."
            }
            _ => {
                "A registration token already exists for your account. Please check your existing token."
            }
        };
        let token_response = UserNodeGatewayTokenResponse::from(existing);
        return Ok(Json(NodeGatewayTokenDetailResponse {
            token: token_response,
            registration_token: None,
            message: Some(message.to_string()),
            registered_node: None,
        }));
    }

    // 2. 生成新的 HMAC 签名 token
    let (token_id, _token_plaintext, token_hash, token_preview) =
        UserNodeGatewayToken::generate_hmac_token(secret.as_bytes());

    // 3. 创建数据库记录（状态为 pending）
    //    处理并发 TOCTOU：依赖 DB UNIQUE 约束兜底，识别冲突返回友好错误
    let token = match UserNodeGatewayToken::create_with_id(
        pool,
        token_id,
        auth.user_id,
        &token_hash,
        &token_preview,
    )
    .await
    {
        Ok(t) => t,
        Err(e) if e.is_duplicate() => {
            return Err(ApiError::BadRequest(
                "A registration token already exists for your account. Please check your existing token."
                    .to_string(),
            ));
        }
        Err(e) => return Err(ApiError::Internal(format!("Failed to create token: {}", e))),
    };

    let token_response = UserNodeGatewayTokenResponse::from(token);

    Ok(Json(NodeGatewayTokenDetailResponse {
        token: token_response,
        registration_token: None,
        message: Some("Token created successfully. Please wait for admin approval.".to_string()),
        registered_node: None,
    }))
}

/// DELETE /api/v1/me/node-gateway/token/:id
/// 仅允许删除 status='rejected' 且 revoke_reason IS NULL 的令牌
/// （即管理员拒绝申请的记录，用户可自行清理历史记录）
pub async fn delete_my_node_gateway_token(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(token_id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let deleted = UserNodeGatewayToken::delete_if_rejected_no_reason(pool, token_id, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete token: {}", e)))?;

    if deleted {
        Ok((
            StatusCode::OK,
            Json(serde_json::json!({"message": "Token deleted"})),
        ))
    } else {
        Err(ApiError::NotFound(
            "Token not found or cannot be deleted. Only rejected tokens (without revoke reason) can be deleted."
                .to_string(),
        ))
    }
}

// =============================================================================
// Admin 端点用的请求/响应
// =============================================================================

/// Admin: 获取所有待审批 token 列表
///
/// 认证由 `admin_auth_middleware` 中间件层完成，此 handler 无需 `AuthExtractor`。
pub async fn admin_list_pending_tokens(
    State(state): State<AppState>,
) -> Result<Json<Vec<PendingTokenWithUser>>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let tokens = UserNodeGatewayToken::list_pending_with_users(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to list pending tokens: {}", e)))?;

    Ok(Json(tokens))
}

/// Admin 审批请求
#[derive(Debug, Deserialize)]
pub struct ApproveTokenRequest {
    pub action: String, // "approve" or "reject"
}

/// Admin: 审批/拒绝 token 申请
pub async fn admin_approve_token(
    auth: AuthExtractor,
    State(state): State<AppState>,
    axum::extract::Path(token_id): axum::extract::Path<uuid::Uuid>,
    Json(body): Json<ApproveTokenRequest>,
) -> Result<Json<serde_json::Value>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let token = UserNodeGatewayToken::find_by_id(pool, token_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find token: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("Token {} not found", token_id)))?;

    match body.action.as_str() {
        "approve" => {
            if token.status != "pending" {
                let msg = match token.status.as_str() {
                    "consumed" => {
                        "Token has already been consumed and cannot be approved again".to_string()
                    }
                    _ => format!("Token is not in pending status (current: {})", token.status),
                };
                return Err(ApiError::BadRequest(msg));
            }
            let approved = token
                .approve(pool, auth.user_id)
                .await
                .map_err(|e| ApiError::Internal(format!("Failed to approve token: {}", e)))?;

            if !approved {
                return Err(ApiError::BadRequest(
                    "Token status has changed or token no longer exists (concurrent modification)"
                        .to_string(),
                ));
            }

            // 审批成功后，发送 token 明文邮件给用户
            // 邮件发送失败不阻塞审批流程
            self::send_token_approved_email(
                &state,
                pool,
                token_id,
                token.user_id,
                &token.token_preview,
            )
            .await;

            Ok(Json(serde_json::json!({
                "success": true,
                "message": "Token approved successfully"
            })))
        }
        "reject" => {
            if token.status != "pending" {
                let msg = match token.status.as_str() {
                    "consumed" => {
                        "Token has already been consumed and cannot be rejected".to_string()
                    }
                    _ => format!("Token is not in pending status (current: {})", token.status),
                };
                return Err(ApiError::BadRequest(msg));
            }
            let rejected = token
                .reject(pool, auth.user_id)
                .await
                .map_err(|e| ApiError::Internal(format!("Failed to reject token: {}", e)))?;

            if !rejected {
                return Err(ApiError::BadRequest(
                    "Token status has changed or token no longer exists (concurrent modification)"
                        .to_string(),
                ));
            }

            Ok(Json(serde_json::json!({
                "success": true,
                "message": "Token rejected"
            })))
        }
        _ => Err(ApiError::BadRequest(
            "action must be 'approve' or 'reject'".to_string(),
        )),
    }
}

/// 发送 token 审批通过邮件给用户
///
/// 邮件发送失败不阻塞审批流程，仅记录警告日志。
async fn send_token_approved_email(
    state: &AppState,
    pool: &impl ConnectionTrait,
    token_id: uuid::Uuid,
    user_id: uuid::Uuid,
    token_preview: &str,
) {
    // 1. 查找用户邮箱
    let user = match User::find_by_id(pool, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            tracing::warn!(
                token_id = %token_id,
                user_id = %user_id,
                "Cannot send token email: user not found"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                token_id = %token_id,
                user_id = %user_id,
                error = %e,
                "Cannot send token email: failed to lookup user"
            );
            return;
        }
    };

    // 2. 获取密钥并重建 token 明文
    let secret = match state.node_gateway_secret() {
        Some(s) => s,
        None => {
            tracing::warn!(
                token_id = %token_id,
                user_id = %user_id,
                "Cannot send token email: node gateway secret not configured"
            );
            return;
        }
    };

    let token_plaintext = UserNodeGatewayToken::reconstruct_token(secret.as_bytes(), token_id);

    // 3. 发送邮件
    match state
        .email_service
        .send_node_gateway_token_email(&user.email, &token_plaintext, token_preview)
        .await
    {
        Ok(()) => {
            tracing::info!(
                token_id = %token_id,
                email = %user.email,
                "Node gateway token email sent successfully"
            );
        }
        Err(e) => {
            tracing::warn!(
                token_id = %token_id,
                email = %user.email,
                error = %e,
                "Failed to send node gateway token email"
            );
        }
    }
}
