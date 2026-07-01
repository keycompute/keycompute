//! 节点任务提交模型
//!
//! 节点任务提交结果表的 ORM 模型（幂等控制）

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 节点任务提交模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
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
        db: &DatabaseConnection,
        req: &CreateNodeTaskSubmissionRequest,
    ) -> Result<NodeTaskSubmission, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_task_submissions (task_id, lease_id, node_id, session_id, result_kind, request_hash, action)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
            [
                req.task_id.into(),
                req.lease_id.into(),
                req.node_id.into(),
                req.session_id.into(),
                req.result_kind.as_str().into(),
                req.request_hash.as_str().into(),
                req.action.as_str().into(),
            ],
        );
        let submission = NodeTaskSubmission::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(submission)
    }

    /// 根据 task_id 和 lease_id 查询提交记录
    pub async fn find_by_task_and_lease(
        db: &DatabaseConnection,
        task_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<NodeTaskSubmission>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_task_submissions WHERE task_id = $1 AND lease_id = $2",
            [task_id.into(), lease_id.into()],
        );
        let submission = NodeTaskSubmission::find_by_statement(stmt).one(db).await?;

        Ok(submission)
    }

    /// 检查提交是否未归档（24 小时内且任务未终态）
    /// 注意：需要联合查询 node_tasks 表
    pub async fn is_not_archived(
        db: &impl ConnectionTrait,
        task_id: Uuid,
        lease_id: Uuid,
    ) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM node_task_submissions s
                JOIN node_tasks t ON s.task_id = t.id
                WHERE s.task_id = $1
                  AND s.lease_id = $2
                  AND t.status NOT IN ('succeeded', 'failed', 'expired')
                  AND s.created_at > NOW() - INTERVAL '24 hours'
            ) AS exist
            "#,
            [task_id.into(), lease_id.into()],
        );
        let result = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbError::Other("query failed".to_string()))?;
        let exists: bool = result.try_get_by_index(0).map_err(DbError::DatabaseError)?;
        Ok(exists)
    }
}
