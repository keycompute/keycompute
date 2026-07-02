//! Node Gateway 管理接口

use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, FromQueryResult)]
pub struct NodeGatewayNodeInfo {
    pub id: Uuid,
    pub display_name: String,
    pub client_instance_id: String,
    pub status: String,
    pub accepted_models_json: serde_json::Value,
    pub consecutive_failure_count: i32,
    pub failure_threshold: i32,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 注册该节点时使用的 token 预览（ LEFT JOIN user_node_gateway_tokens ）
    pub token_preview: Option<String>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct NodeGatewayTaskInfo {
    pub id: Uuid,
    pub model: String,
    pub status: String,
    pub assigned_node_id: Option<Uuid>,
    pub failure_count: i32,
    pub failure_threshold: i32,
    pub queued_at: chrono::DateTime<chrono::Utc>,
    pub deadline_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct NodeGatewayNodeStats {
    pub total: i64,
    pub online: i64,
    pub offline: i64,
    pub excluded: i64,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct NodeGatewayTaskStats {
    pub total: i64,
    pub queued: i64,
    pub leased: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub expired: i64,
}

#[derive(Debug, Serialize)]
pub struct NodeGatewayOverviewResponse {
    pub enabled: bool,
    pub node_stats: NodeGatewayNodeStats,
    pub task_stats: NodeGatewayTaskStats,
    pub nodes: Vec<NodeGatewayNodeInfo>,
    pub recent_tasks: Vec<NodeGatewayTaskInfo>,
}

pub async fn get_node_gateway_overview(
    State(state): State<AppState>,
) -> Result<Json<NodeGatewayOverviewResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            COUNT(*)::BIGINT AS total,
            COUNT(*) FILTER (WHERE status = 'online')::BIGINT AS online,
            COUNT(*) FILTER (WHERE status = 'offline')::BIGINT AS offline,
            COUNT(*) FILTER (WHERE status = 'excluded')::BIGINT AS excluded
        FROM nodes
        "#,
        [],
    );
    let node_stats = NodeGatewayNodeStats::find_by_statement(stmt)
        .one(pool)
        .await?
        .ok_or_else(|| ApiError::Internal("Failed to load node stats: no data".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            COUNT(*)::BIGINT AS total,
            COUNT(*) FILTER (WHERE status = 'queued')::BIGINT AS queued,
            COUNT(*) FILTER (WHERE status = 'leased')::BIGINT AS leased,
            COUNT(*) FILTER (WHERE status = 'succeeded')::BIGINT AS succeeded,
            COUNT(*) FILTER (WHERE status = 'failed')::BIGINT AS failed,
            COUNT(*) FILTER (WHERE status = 'expired')::BIGINT AS expired
        FROM node_tasks
        "#,
        [],
    );
    let task_stats = NodeGatewayTaskStats::find_by_statement(stmt)
        .one(pool)
        .await?
        .ok_or_else(|| ApiError::Internal("Failed to load task stats: no data".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            n.id,
            n.display_name,
            n.client_instance_id,
            CASE
                WHEN n.status = 'online'
                     AND n.last_heartbeat_at IS NOT NULL
                     AND n.last_heartbeat_at < NOW() - INTERVAL '3 minutes'
                THEN 'offline'
                ELSE n.status
            END AS status,
            COALESCE(latest_session.accepted_models_json, '[]'::jsonb) AS accepted_models_json,
            n.consecutive_failure_count,
            n.failure_threshold,
            n.last_heartbeat_at,
            n.updated_at,
            t.token_preview
        FROM nodes n
        LEFT JOIN LATERAL (
            SELECT accepted_models_json
            FROM node_sessions ns
            WHERE ns.node_id = n.id
            ORDER BY ns.last_seen_at DESC
            LIMIT 1
        ) latest_session ON TRUE
        LEFT JOIN user_node_gateway_tokens t ON t.consumed_node_id = n.id
        ORDER BY n.updated_at DESC
        LIMIT 20
        "#,
        [],
    );
    let nodes = NodeGatewayNodeInfo::find_by_statement(stmt)
        .all(pool)
        .await?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            id,
            model,
            status,
            assigned_node_id,
            failure_count,
            failure_threshold,
            queued_at,
            deadline_at,
            updated_at
        FROM node_tasks
        ORDER BY created_at DESC
        LIMIT 20
        "#,
        [],
    );
    let recent_tasks = NodeGatewayTaskInfo::find_by_statement(stmt)
        .all(pool)
        .await?;

    Ok(Json(NodeGatewayOverviewResponse {
        enabled: state.node_gateway.is_some(),
        node_stats,
        task_stats,
        nodes,
        recent_tasks,
    }))
}

#[derive(Debug, Serialize)]
pub struct RecoverNodeResponse {
    pub id: Uuid,
    pub status: String,
    pub consecutive_failure_count: i32,
}

/// POST /api/v1/admin/nodes/{id}/recover
/// 把 excluded 节点重置为 online, 清零 consecutive_failure_count,
/// 同时恢复关联的被吊销 token 状态为 approved
pub async fn recover_node(
    State(state): State<AppState>,
    Path(node_id): Path<Uuid>,
) -> Result<Json<RecoverNodeResponse>> {
    let service = state
        .node_gateway
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Node gateway not configured".to_string()))?;

    let node = service
        .store
        .recover_node(node_id)
        .await
        .map_err(ApiError::from)?;

    // 同步恢复关联的被吊销 token
    // 使用 clone 获取独立的 Arc<DatabaseConnection>，避免与 service 同时借用 state 的不同字段
    let pool = state
        .pool
        .clone()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    match keycompute_db::models::user_node_gateway_token::UserNodeGatewayToken::find_by_consumed_node_id(pool.as_ref(), node_id).await
    {
        Ok(Some(token)) => {
            let token_id = token.id;
            let token_status = token.status.clone();
            tracing::info!(
                node_id = %node_id,
                token_id = %token_id,
                token_status = %token_status,
                "Recovering node: found associated token, attempting restore"
            );
            match keycompute_db::models::user_node_gateway_token::UserNodeGatewayToken::restore_from_revoked(pool.as_ref(), token_id).await
            {
                Ok(true) => {
                    tracing::info!(
                        node_id = %node_id,
                        token_id = %token_id,
                        "Successfully restored token from revoked to approved"
                    );
                }
                Ok(false) => {
                    // false 可能表示两种场景：
                    // 1. 用户已申请新令牌（pending/approved），旧令牌不恢复以避免唯一约束冲突
                    // 2. 令牌状态不再是 rejected（如已被其他操作修改）
                    tracing::warn!(
                        node_id = %node_id,
                        token_id = %token_id,
                        token_status = %token_status,
                        "Token restore skipped: user may have a newer active token, or token is no longer in revoked state"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        node_id = %node_id,
                        token_id = %token_id,
                        error = %e,
                        "Failed to restore token during node recovery"
                    );
                }
            }
        }
        Ok(None) => {
            tracing::info!(
                node_id = %node_id,
                "Recovering node: no associated token found"
            );
        }
        Err(e) => {
            tracing::error!(
                node_id = %node_id,
                error = %e,
                "Failed to lookup token during node recovery"
            );
        }
    }

    Ok(Json(RecoverNodeResponse {
        id: node.id,
        status: node.status,
        consecutive_failure_count: node.consecutive_failure_count,
    }))
}

#[derive(Debug, Serialize)]
pub struct ExcludeNodeResponse {
    pub id: Uuid,
    pub status: String,
}

/// POST /api/v1/admin/nodes/{id}/exclude
/// 将节点标记为 excluded（从节点池中移除，不再分配任务）
///
/// 设计说明：本 handler 直接通过 state.pool 调用 Node::update_status()，
/// 不经过 node-gateway service 层。因为 exclude 是单次 UPDATE 操作，
/// 不需事务编排，直接访问 pool 更简洁。同级 recover_node 因需同步恢复
/// 关联 token 状态，走 service 层（store.recover_node + token restore）。
pub async fn exclude_node(
    State(state): State<AppState>,
    Path(node_id): Path<Uuid>,
) -> Result<Json<ExcludeNodeResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let node = keycompute_db::models::node::Node::update_status(pool, node_id, "excluded")
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to exclude node: {}", e)))?;

    Ok(Json(ExcludeNodeResponse {
        id: node.id,
        status: node.status,
    }))
}

/// 吊销节点请求
#[derive(Debug, Deserialize)]
pub struct RevokeNodeRequest {
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct RevokeNodeTokenResponse {
    pub id: Uuid,
    pub node_status: String,
    pub token_status: String,
    pub revoke_reason: String,
}

/// POST /api/v1/admin/nodes/{id}/revoke-token
/// 吊销节点注册令牌：将节点标记为 excluded，并将对应的 user_node_gateway_tokens 状态改为 rejected 并记录原因
pub async fn revoke_node_token(
    State(state): State<AppState>,
    Path(node_id): Path<Uuid>,
    Json(req): Json<RevokeNodeRequest>,
) -> Result<Json<RevokeNodeTokenResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 校验原因不能为空
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::BadRequest(
            "Revoke reason cannot be empty".to_string(),
        ));
    }

    // 1. 将节点标记为 excluded
    let node = keycompute_db::models::node::Node::update_status(pool, node_id, "excluded")
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to exclude node: {}", e)))?;

    // 2. 查找关联 token 并吊销
    let token = keycompute_db::models::user_node_gateway_token::UserNodeGatewayToken::find_by_consumed_node_id(
        pool, node_id,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to find token: {}", e)))?;

    let token_status = if let Some(t) = token {
        keycompute_db::models::user_node_gateway_token::UserNodeGatewayToken::revoke_with_reason(
            pool, t.id, &reason,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to revoke token: {}", e)))?;
        "rejected".to_string()
    } else {
        "no_token".to_string()
    };

    Ok(Json(RevokeNodeTokenResponse {
        id: node.id,
        node_status: node.status,
        token_status,
        revoke_reason: reason,
    }))
}

/// 删除节点响应
#[derive(Debug, Serialize)]
pub struct DeleteNodeResponse {
    pub id: Uuid,
    pub deleted: bool,
}

/// DELETE /api/v1/admin/nodes/{id}
/// 彻底删除节点：清理关联数据 → 删除 token → 删除节点（CASCADE 清理 sessions）
///
/// 删除前显式清理：
/// 1. node_tasks.assigned_node_id / assigned_session_id 设为 NULL（安全网）
/// 2. node_task_submissions 硬删除（安全网，FK 约束也会 CASCADE）
/// 3. 关联的 user_node_gateway_token 硬删除
pub async fn delete_node(
    State(state): State<AppState>,
    Path(node_id): Path<Uuid>,
) -> Result<(StatusCode, Json<DeleteNodeResponse>)> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 使用事务确保原子性
    let tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

    // 0. 查找并删除关联 token（在事务内）
    let token = keycompute_db::models::user_node_gateway_token::UserNodeGatewayToken::find_by_consumed_node_id(
        &tx, node_id,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to find token: {}", e)))?;

    if let Some(t) = token {
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM user_node_gateway_tokens WHERE id = $1",
            [t.id.into()],
        ))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete token: {}", e)))?;
    }

    // 1. 显式清理 node_tasks 引用
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_tasks SET assigned_node_id = NULL WHERE assigned_node_id = $1",
        [node_id.into()],
    ))
    .await
    .map_err(|e| {
        ApiError::Internal(format!(
            "Failed to update node_tasks.assigned_node_id: {}",
            e
        ))
    })?;

    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        UPDATE node_tasks
        SET assigned_session_id = NULL
        WHERE assigned_session_id IN (
            SELECT id FROM node_sessions WHERE node_id = $1
        )
        "#,
        [node_id.into()],
    ))
    .await
    .map_err(|e| {
        ApiError::Internal(format!(
            "Failed to update node_tasks.assigned_session_id: {}",
            e
        ))
    })?;

    // 2. 显式清理 node_task_submissions
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM node_task_submissions WHERE node_id = $1",
        [node_id.into()],
    ))
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to delete node_task_submissions: {}", e)))?;

    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        DELETE FROM node_task_submissions
        WHERE session_id IN (
            SELECT id FROM node_sessions WHERE node_id = $1
        )
        "#,
        [node_id.into()],
    ))
    .await
    .map_err(|e| {
        ApiError::Internal(format!(
            "Failed to delete node_task_submissions by session: {}",
            e
        ))
    })?;

    // 3. 删除节点（CASCADE 自动清理 node_sessions）
    let result = tx
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM nodes WHERE id = $1",
            [node_id.into()],
        ))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete node: {}", e)))?;

    if result.rows_affected() == 0 {
        // 节点不存在，回滚事务
        tx.rollback()
            .await
            .map_err(|e| ApiError::Internal(format!("Rollback failed: {}", e)))?;
        return Err(ApiError::NotFound("Node not found".to_string()));
    }

    // 提交事务
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Commit failed: {}", e)))?;

    Ok((
        StatusCode::OK,
        Json(DeleteNodeResponse {
            id: node_id,
            deleted: true,
        }),
    ))
}
