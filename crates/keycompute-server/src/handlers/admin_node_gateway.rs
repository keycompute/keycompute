//! Node Gateway 管理接口

use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
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
}

#[derive(Debug, Serialize, FromRow)]
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

#[derive(Debug, Serialize, FromRow)]
pub struct NodeGatewayNodeStats {
    pub total: i64,
    pub online: i64,
    pub offline: i64,
    pub excluded: i64,
}

#[derive(Debug, Serialize, FromRow)]
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
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let node_stats = sqlx::query_as::<_, NodeGatewayNodeStats>(
        r#"
        SELECT
            COUNT(*)::BIGINT AS total,
            COUNT(*) FILTER (WHERE status = 'online')::BIGINT AS online,
            COUNT(*) FILTER (WHERE status = 'offline')::BIGINT AS offline,
            COUNT(*) FILTER (WHERE status = 'excluded')::BIGINT AS excluded
        FROM nodes
        "#,
    )
    .fetch_one(pool.as_ref())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to load node stats: {}", e)))?;

    let task_stats = sqlx::query_as::<_, NodeGatewayTaskStats>(
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
    )
    .fetch_one(pool.as_ref())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to load node task stats: {}", e)))?;

    let nodes = sqlx::query_as::<_, NodeGatewayNodeInfo>(
        r#"
        SELECT
            n.id,
            n.display_name,
            n.client_instance_id,
            n.status,
            COALESCE(latest_session.accepted_models_json, '[]'::jsonb) AS accepted_models_json,
            n.consecutive_failure_count,
            n.failure_threshold,
            n.last_heartbeat_at,
            n.updated_at
        FROM nodes n
        LEFT JOIN LATERAL (
            SELECT accepted_models_json
            FROM node_sessions ns
            WHERE ns.node_id = n.id
            ORDER BY ns.last_seen_at DESC
            LIMIT 1
        ) latest_session ON TRUE
        ORDER BY n.updated_at DESC
        LIMIT 20
        "#,
    )
    .fetch_all(pool.as_ref())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to load nodes: {}", e)))?;

    let recent_tasks = sqlx::query_as::<_, NodeGatewayTaskInfo>(
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
    )
    .fetch_all(pool.as_ref())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to load node tasks: {}", e)))?;

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

/// POST /api/v1/admin/nodes/{id}/recover (B6 修复)
/// 把 excluded 节点重置为 online, 清零 consecutive_failure_count
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

    Ok(Json(RecoverNodeResponse {
        id: node.id,
        status: node.status,
        consecutive_failure_count: node.consecutive_failure_count,
    }))
}
