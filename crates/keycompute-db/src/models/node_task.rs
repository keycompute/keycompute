//! 节点任务模型
//!
//! 节点任务生命周期表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 任务状态
pub const TASK_STATUS_QUEUED: &str = "queued";
pub const TASK_STATUS_LEASED: &str = "leased";
pub const TASK_STATUS_SUCCEEDED: &str = "succeeded";
pub const TASK_STATUS_FAILED: &str = "failed";
pub const TASK_STATUS_EXPIRED: &str = "expired";

/// 节点任务模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NodeTask {
    pub id: Uuid,
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub payload_json: serde_json::Value,
    pub status: String,
    pub assigned_node_id: Option<Uuid>,
    pub assigned_session_id: Option<Uuid>,
    pub lease_id: Option<Uuid>,
    pub failure_count: i32,
    pub failure_threshold: i32,
    pub result_json: Option<serde_json::Value>,
    pub error_json: Option<serde_json::Value>,
    pub queued_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    pub complete_grace_until: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建节点任务请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateNodeTaskRequest {
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub payload_json: serde_json::Value,
    pub deadline_at: DateTime<Utc>,
    pub complete_grace_until: DateTime<Utc>,
}

impl NodeTask {
    /// 创建新任务
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreateNodeTaskRequest,
    ) -> Result<NodeTask, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            INSERT INTO node_tasks (request_id, user_id, model, payload_json, status, deadline_at, complete_grace_until)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
        )
        .bind(req.request_id)
        .bind(req.user_id)
        .bind(&req.model)
        .bind(&req.payload_json)
        .bind(TASK_STATUS_QUEUED)
        .bind(req.deadline_at)
        .bind(req.complete_grace_until)
        .fetch_one(pool)
        .await?;

        Ok(task)
    }

    /// 根据 ID 查询任务
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<NodeTask>, DbError> {
        let task = sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(task)
    }

    /// 根据 request_id 查询任务
    pub async fn find_by_request_id(
        pool: &sqlx::PgPool,
        request_id: Uuid,
    ) -> Result<Option<NodeTask>, DbError> {
        let task = sqlx::query_as::<_, NodeTask>("SELECT * FROM node_tasks WHERE request_id = $1")
            .bind(request_id)
            .fetch_optional(pool)
            .await?;

        Ok(task)
    }

    /// 原子领取任务（claim）
    pub async fn claim(
        pool: &sqlx::PgPool,
        task_id: Uuid,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<NodeTask>, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                assigned_node_id = $2,
                assigned_session_id = $3,
                lease_id = $4,
                claimed_at = NOW(),
                updated_at = NOW()
            WHERE id = $5
              AND status = $6
              AND deadline_at >= NOW()
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_LEASED)
        .bind(node_id)
        .bind(session_id)
        .bind(lease_id)
        .bind(task_id)
        .bind(TASK_STATUS_QUEUED)
        .fetch_optional(pool)
        .await?;

        Ok(task)
    }

    /// 标记任务成功
    pub async fn mark_succeeded(
        pool: &sqlx::PgPool,
        task_id: Uuid,
        result_json: &serde_json::Value,
    ) -> Result<NodeTask, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                result_json = $2,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_SUCCEEDED)
        .bind(result_json)
        .bind(task_id)
        .fetch_one(pool)
        .await?;

        Ok(task)
    }

    /// 标记任务失败
    pub async fn mark_failed(
        pool: &sqlx::PgPool,
        task_id: Uuid,
        error_json: &serde_json::Value,
    ) -> Result<NodeTask, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                error_json = $2,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_FAILED)
        .bind(error_json)
        .bind(task_id)
        .fetch_one(pool)
        .await?;

        Ok(task)
    }

    /// 标记任务过期
    pub async fn mark_expired(pool: &sqlx::PgPool, task_id: Uuid) -> Result<NodeTask, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $2
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_EXPIRED)
        .bind(task_id)
        .fetch_one(pool)
        .await?;

        Ok(task)
    }

    /// 恢复任务为 queued（重新入队）
    pub async fn requeue(pool: &sqlx::PgPool, task_id: Uuid) -> Result<NodeTask, DbError> {
        let task = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                assigned_node_id = NULL,
                assigned_session_id = NULL,
                lease_id = NULL,
                claimed_at = NULL,
                failure_count = failure_count + 1,
                updated_at = NOW()
            WHERE id = $2
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_QUEUED)
        .bind(task_id)
        .fetch_one(pool)
        .await?;

        Ok(task)
    }

    /// 批量标记过期任务
    pub async fn expire_overdue_tasks(pool: &sqlx::PgPool) -> Result<Vec<NodeTask>, DbError> {
        let tasks = sqlx::query_as::<_, NodeTask>(
            r#"
            UPDATE node_tasks
            SET status = $1,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE status IN ($2, $3)
              AND deadline_at < NOW()
            RETURNING *
            "#,
        )
        .bind(TASK_STATUS_EXPIRED)
        .bind(TASK_STATUS_QUEUED)
        .bind(TASK_STATUS_LEASED)
        .fetch_all(pool)
        .await?;

        Ok(tasks)
    }

    /// 检查任务是否处于终态
    pub fn is_terminal(&self) -> bool {
        self.status == TASK_STATUS_SUCCEEDED
            || self.status == TASK_STATUS_FAILED
            || self.status == TASK_STATUS_EXPIRED
    }

    /// 检查任务是否已过期
    pub fn is_expired(&self) -> bool {
        self.deadline_at < Utc::now()
    }
}
