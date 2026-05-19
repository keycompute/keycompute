//! 节点任务提交模型
//!
//! 节点任务提交结果表的 ORM 模型（幂等控制）

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 节点任务提交模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NodeTaskSubmission {
    pub id: Uuid,
    pub task_id: Uuid,
    pub lease_id: Uuid,
    pub node_id: Uuid,
    pub session_id: Uuid,
    pub result_kind: String,
    pub request_hash: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
}

/// 创建节点任务提交请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateNodeTaskSubmissionRequest {
    pub task_id: Uuid,
    pub lease_id: Uuid,
    pub node_id: Uuid,
    pub session_id: Uuid,
    pub result_kind: String,
    pub request_hash: String,
    pub action: String,
}

impl NodeTaskSubmission {
    /// 创建新提交记录
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreateNodeTaskSubmissionRequest,
    ) -> Result<NodeTaskSubmission, DbError> {
        let submission = sqlx::query_as::<_, NodeTaskSubmission>(
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
        )
        .bind(req.task_id)
        .bind(req.lease_id)
        .bind(req.node_id)
        .bind(req.session_id)
        .bind(&req.result_kind)
        .bind(&req.request_hash)
        .bind(&req.action)
        .fetch_one(pool)
        .await?;

        Ok(submission)
    }

    /// 根据 task_id 和 lease_id 查询提交记录
    pub async fn find_by_task_and_lease(
        pool: &sqlx::PgPool,
        task_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<NodeTaskSubmission>, DbError> {
        let submission = sqlx::query_as::<_, NodeTaskSubmission>(
            r#"
            SELECT * FROM node_task_submissions
            WHERE task_id = $1 AND lease_id = $2
            "#,
        )
        .bind(task_id)
        .bind(lease_id)
        .fetch_optional(pool)
        .await?;

        Ok(submission)
    }

    /// 检查提交是否未归档（24 小时内且任务未终态）
    /// 注意：需要联合查询 node_tasks 表
    pub async fn is_not_archived(
        pool: &sqlx::PgPool,
        task_id: Uuid,
        lease_id: Uuid,
    ) -> Result<bool, DbError> {
        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM node_task_submissions s
                JOIN node_tasks t ON s.task_id = t.id
                WHERE s.task_id = $1
                  AND s.lease_id = $2
                  AND t.status NOT IN ('succeeded', 'failed', 'expired')
                  AND s.created_at > NOW() - INTERVAL '24 hours'
            )
            "#,
        )
        .bind(task_id)
        .bind(lease_id)
        .fetch_one(pool)
        .await?;

        Ok(exists)
    }
}
