//! 管理端监控追踪接口

use crate::{
    error::{ApiError, Result},
    state::AppState,
};
use axum::{Json, extract::State};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, FromQueryResult)]
pub struct MonitoringSummary {
    pub total_usage_logs: i64,
    pub total_node_tasks: i64,
    pub active_node_tasks: i64,
    pub succeeded_node_tasks: i64,
    pub failed_node_tasks: i64,
    pub online_nodes: i64,
    pub avg_node_latency_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct MonitoringTraceEntry {
    pub request_id: Uuid,
    pub task_id: Uuid,
    pub model: String,
    pub status: String,
    pub node_id: Option<Uuid>,
    pub node_name: Option<String>,
    pub lease_id: Option<Uuid>,
    pub queued_at: chrono::DateTime<chrono::Utc>,
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub deadline_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: Option<i64>,
    pub usage_status: Option<String>,
    pub total_tokens: Option<i32>,
    pub amount: Option<String>,
    pub submissions_count: i64,
    pub last_submission_action: Option<String>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct MonitoringNodeHealth {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    pub accepted_models_json: serde_json::Value,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    pub active_tasks: i64,
    pub succeeded_tasks: i64,
    pub failed_tasks: i64,
}

#[derive(Debug, Serialize)]
pub struct MonitoringOverviewResponse {
    pub summary: MonitoringSummary,
    pub traces: Vec<MonitoringTraceEntry>,
    pub nodes: Vec<MonitoringNodeHealth>,
}

pub async fn get_monitoring_overview(
    State(state): State<AppState>,
) -> Result<Json<MonitoringOverviewResponse>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            (SELECT COUNT(*) FROM usage_logs)::BIGINT AS total_usage_logs,
            (SELECT COUNT(*) FROM node_tasks)::BIGINT AS total_node_tasks,
            (SELECT COUNT(*) FROM node_tasks WHERE status IN ('queued', 'leased'))::BIGINT AS active_node_tasks,
            (SELECT COUNT(*) FROM node_tasks WHERE status = 'succeeded')::BIGINT AS succeeded_node_tasks,
            (SELECT COUNT(*) FROM node_tasks WHERE status IN ('failed', 'expired'))::BIGINT AS failed_node_tasks,
            (SELECT COUNT(*) FROM nodes
             WHERE status = 'online'
               AND (last_heartbeat_at IS NULL
                    OR last_heartbeat_at >= NOW() - INTERVAL '3 minutes')
            )::BIGINT AS online_nodes,
            (
                SELECT AVG(EXTRACT(EPOCH FROM (finished_at - queued_at)) * 1000)::BIGINT
                FROM node_tasks
                WHERE finished_at IS NOT NULL
            ) AS avg_node_latency_ms
        "#,
        [],
    );
    let summary = MonitoringSummary::find_by_statement(stmt)
        .one(pool)
        .await?
        .ok_or_else(|| {
            ApiError::Internal("Failed to load monitoring summary: no data".to_string())
        })?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            nt.request_id,
            nt.id AS task_id,
            nt.model,
            nt.status,
            nt.assigned_node_id AS node_id,
            n.display_name AS node_name,
            nt.lease_id,
            nt.queued_at,
            nt.claimed_at,
            nt.finished_at,
            nt.deadline_at,
            CASE
                WHEN nt.finished_at IS NULL THEN NULL
                ELSE (EXTRACT(EPOCH FROM (nt.finished_at - nt.queued_at)) * 1000)::BIGINT
            END AS duration_ms,
            ul.status AS usage_status,
            ul.total_tokens,
            ul.user_amount::TEXT AS amount,
            COALESCE(sub.submissions_count, 0)::BIGINT AS submissions_count,
            sub.last_submission_action
        FROM node_tasks nt
        LEFT JOIN nodes n ON n.id = nt.assigned_node_id
        LEFT JOIN usage_logs ul ON ul.request_id = nt.request_id
        LEFT JOIN LATERAL (
            SELECT
                COUNT(*)::BIGINT AS submissions_count,
                (ARRAY_AGG(action ORDER BY created_at DESC))[1] AS last_submission_action
            FROM node_task_submissions nts
            WHERE nts.task_id = nt.id
        ) sub ON TRUE
        ORDER BY nt.created_at DESC
        LIMIT 50
        "#,
        [],
    );
    let traces = MonitoringTraceEntry::find_by_statement(stmt)
        .all(pool)
        .await?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"
        SELECT
            n.id,
            n.display_name,
            CASE
                WHEN n.status = 'online'
                     AND n.last_heartbeat_at IS NOT NULL
                     AND n.last_heartbeat_at < NOW() - INTERVAL '3 minutes'
                THEN 'offline'
                ELSE n.status
            END AS status,
            COALESCE(latest_session.accepted_models_json, '[]'::jsonb) AS accepted_models_json,
            n.last_heartbeat_at,
            COUNT(nt.id) FILTER (WHERE nt.status IN ('queued', 'leased'))::BIGINT AS active_tasks,
            COUNT(nt.id) FILTER (WHERE nt.status = 'succeeded')::BIGINT AS succeeded_tasks,
            COUNT(nt.id) FILTER (WHERE nt.status IN ('failed', 'expired'))::BIGINT AS failed_tasks
        FROM nodes n
        LEFT JOIN LATERAL (
            SELECT accepted_models_json
            FROM node_sessions ns
            WHERE ns.node_id = n.id
            ORDER BY ns.last_seen_at DESC
            LIMIT 1
        ) latest_session ON TRUE
        LEFT JOIN node_tasks nt ON nt.assigned_node_id = n.id
        GROUP BY n.id, n.status, n.last_heartbeat_at, latest_session.accepted_models_json
        ORDER BY n.last_heartbeat_at DESC NULLS LAST, n.updated_at DESC
        LIMIT 20
        "#,
        [],
    );
    let nodes = MonitoringNodeHealth::find_by_statement(stmt)
        .all(pool)
        .await?;

    Ok(Json(MonitoringOverviewResponse {
        summary,
        traces,
        nodes,
    }))
}
