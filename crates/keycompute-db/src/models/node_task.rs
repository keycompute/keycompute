//! 节点任务模型
//!
//! 节点任务生命周期表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 任务状态
pub const TASK_STATUS_QUEUED: &str = "queued";
pub const TASK_STATUS_LEASED: &str = "leased";
pub const TASK_STATUS_SUCCEEDED: &str = "succeeded";
pub const TASK_STATUS_FAILED: &str = "failed";
pub const TASK_STATUS_EXPIRED: &str = "expired";

/// 节点任务模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct NodeTask {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub payload_json: serde_json::Value,
    pub native_requirements_json: Option<serde_json::Value>,
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
    pub tenant_id: Uuid,
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
        db: &impl ConnectionTrait,
        req: &CreateNodeTaskRequest,
    ) -> Result<NodeTask, DbError> {
        let native = match req.payload_json.get("native").filter(|v| !v.is_null()) {
            Some(value) => {
                let request: keycompute_types::node_native::NodeNativeRequest =
                    serde_json::from_value(value.clone())
                        .map_err(|_| DbError::Other("invalid native payload".into()))?;
                if request
                    .body
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    != Some(req.model.as_str())
                {
                    return Err(DbError::Other("native model mismatch".into()));
                }
                let required =
                    keycompute_types::node_capability::NativeRequirements::from_request(&request)
                        .map_err(|error| DbError::Other(error.into()))?;
                let value = serde_json::to_value(required)
                    .map_err(|_| DbError::Other("invalid native requirements".into()))?;
                Some(value)
            }
            None => None,
        };
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO node_tasks (request_id, tenant_id, user_id, model, payload_json, status, deadline_at, complete_grace_until,native_requirements_json)
            SELECT $1, m.tenant_id, $2, $3, $4, $5, $6, $7, $8
            FROM tenant_memberships m
            JOIN tenants t ON t.id=m.tenant_id AND t.status='active'
            JOIN users u ON u.id=m.user_id AND u.status='active'
            WHERE m.tenant_id=$9 AND m.user_id = $2 AND m.status = 'active'
            RETURNING *
            "#,
            [
                req.request_id.into(),
                req.user_id.into(),
                req.model.as_str().into(),
                req.payload_json.clone().into(),
                TASK_STATUS_QUEUED.into(),
                req.deadline_at.into(),
                req.complete_grace_until.into(),
                native.into(),
                req.tenant_id.into(),
            ],
        );
        let task = NodeTask::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(task)
    }

    /// 根据 ID 查询任务
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<NodeTask>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tasks WHERE id = $1",
            [id.into()],
        );
        let task = NodeTask::find_by_statement(stmt).one(db).await?;

        Ok(task)
    }

    /// 根据 request_id 查询任务
    pub async fn find_by_request_id(
        db: &impl ConnectionTrait,
        request_id: Uuid,
    ) -> Result<Option<NodeTask>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM node_tasks WHERE request_id = $1",
            [request_id.into()],
        );
        let task = NodeTask::find_by_statement(stmt).one(db).await?;

        Ok(task)
    }

    /// 原子领取任务（claim）
    pub async fn claim(
        db: &impl ConnectionTrait,
        task_id: Uuid,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<NodeTask>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
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
              AND EXISTS (
                SELECT 1 FROM nodes n JOIN node_sessions ns ON ns.node_id=n.id
                JOIN tenant_memberships owner_m ON owner_m.tenant_id=n.tenant_id AND owner_m.user_id=n.owner_user_id AND owner_m.status='active'
                JOIN users owner_u ON owner_u.id=n.owner_user_id AND owner_u.status='active'
                JOIN tenant_memberships caller_m ON caller_m.tenant_id=node_tasks.tenant_id AND caller_m.user_id=node_tasks.user_id AND caller_m.status='active'
                JOIN users caller_u ON caller_u.id=node_tasks.user_id AND caller_u.status='active'
                JOIN tenants current_t ON current_t.id=n.tenant_id AND current_t.status='active'
                WHERE n.id=$2 AND ns.id=$3 AND n.tenant_id=node_tasks.tenant_id AND n.status='online'
                  AND ns.accepted_models_json @> jsonb_build_array(node_tasks.model)
                  AND ns.revoked_at IS NULL AND ns.expires_at>NOW() AND ns.accepting_tasks=TRUE
              )
              AND (payload_json->'native' IS NULL OR payload_json->'native'='null'::jsonb
                OR (payload_json->'native'->'body'->>'model'=node_tasks.model
                    AND EXISTS (
                      SELECT 1 FROM node_sessions ns JOIN nodes n ON n.id=ns.node_id
                      JOIN tenants t ON t.id=n.tenant_id
                      WHERE ns.id=$3 AND ns.node_id=$2 AND n.status='online' AND t.status='active'
                        AND ns.expires_at>NOW() AND ns.revoked_at IS NULL AND ns.accepting_tasks=TRUE
                        AND n.capabilities_json->>'runtime'='ollama'
                        AND ns.accepted_models_json @> jsonb_build_array(node_tasks.model)
                        AND ns.native_operations_json @> jsonb_build_array(node_tasks.native_requirements_json->>'operation')
                        AND EXISTS(SELECT 1 FROM jsonb_array_elements(ns.native_profiles_json) p WHERE {profile_match}))
                    AND EXISTS (
                      SELECT 1 FROM users caller
                      JOIN tenant_memberships cm ON cm.tenant_id=node_tasks.tenant_id
                        AND cm.user_id=caller.id AND cm.status='active'
                      JOIN tenants ct ON ct.id=node_tasks.tenant_id
                      WHERE caller.id=node_tasks.user_id AND ct.status='active')))
            RETURNING *
            "#,
                profile_match = super::native_capability::profile_matches(
                    "p",
                    "node_tasks.native_requirements_json"
                )
            ),
            [
                TASK_STATUS_LEASED.into(),
                node_id.into(),
                session_id.into(),
                lease_id.into(),
                task_id.into(),
                TASK_STATUS_QUEUED.into(),
            ],
        );
        let task = NodeTask::find_by_statement(stmt).one(db).await?;

        Ok(task)
    }

    /// A native worker only removes work it can execute. Heterogeneous model
    /// features/limits cannot cause incompatible workers to steal queue hints.
    pub async fn claim_next_native(
        db: &impl ConnectionTrait,
        node_id: Uuid,
        session_id: Uuid,
        lease_id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        let sql = format!(
            r#"
            UPDATE node_tasks SET status='leased',assigned_node_id=$1,assigned_session_id=$2,
                lease_id=$3,claimed_at=NOW(),updated_at=NOW()
            WHERE id=(
                SELECT nt.id FROM node_tasks nt
                JOIN tenant_memberships cm ON cm.tenant_id=nt.tenant_id
                  AND cm.user_id=nt.user_id AND cm.status='active'
                JOIN tenants ct ON ct.id=nt.tenant_id
                WHERE nt.status='queued' AND nt.deadline_at>NOW() AND ct.status='active'
                  AND nt.native_requirements_json IS NOT NULL
                  AND EXISTS(SELECT 1 FROM node_sessions ns JOIN nodes n ON n.id=ns.node_id
                    JOIN tenants ot ON ot.id=n.tenant_id
                    WHERE ns.id=$2 AND n.id=$1 AND n.tenant_id=nt.tenant_id AND n.status='online' AND ot.status='active'
                      AND EXISTS(SELECT 1 FROM users u WHERE u.id=nt.user_id AND u.status='active')
                      AND EXISTS(SELECT 1 FROM tenant_memberships om JOIN users ou ON ou.id=om.user_id AND ou.status='active' WHERE om.tenant_id=n.tenant_id AND om.user_id=n.owner_user_id AND om.status='active')
                      AND ns.expires_at>NOW() AND ns.revoked_at IS NULL AND ns.accepting_tasks=TRUE
                      AND n.capabilities_json->>'runtime'='ollama'
                      AND ns.accepted_models_json @> jsonb_build_array(nt.model)
                      AND ns.native_operations_json @> jsonb_build_array(nt.native_requirements_json->>'operation')
                      AND EXISTS(SELECT 1 FROM jsonb_array_elements(ns.native_profiles_json) p WHERE {profile_match}))
                ORDER BY nt.queued_at,nt.id FOR UPDATE OF nt SKIP LOCKED LIMIT 1
            ) RETURNING *
        "#,
            profile_match =
                super::native_capability::profile_matches("p", "nt.native_requirements_json")
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [node_id.into(), session_id.into(), lease_id.into()],
        ))
        .one(db)
        .await?)
    }

    /// 标记任务成功
    pub async fn mark_succeeded(
        db: &impl ConnectionTrait,
        task_id: Uuid,
        result_json: &serde_json::Value,
    ) -> Result<NodeTask, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE node_tasks
            SET status = $1,
                result_json = $2,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
            [
                TASK_STATUS_SUCCEEDED.into(),
                result_json.clone().into(),
                task_id.into(),
            ],
        );
        let task = NodeTask::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;

        Ok(task)
    }

    /// 标记任务失败
    pub async fn mark_failed(
        db: &impl ConnectionTrait,
        task_id: Uuid,
        error_json: &serde_json::Value,
    ) -> Result<NodeTask, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE node_tasks
            SET status = $1,
                error_json = $2,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
            [
                TASK_STATUS_FAILED.into(),
                error_json.clone().into(),
                task_id.into(),
            ],
        );
        let task = NodeTask::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;

        Ok(task)
    }

    /// 标记任务过期
    pub async fn mark_expired(
        db: &impl ConnectionTrait,
        task_id: Uuid,
    ) -> Result<NodeTask, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE node_tasks
            SET status = $1,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE id = $2
            RETURNING *
            "#,
            [TASK_STATUS_EXPIRED.into(), task_id.into()],
        );
        let task = NodeTask::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;

        Ok(task)
    }

    /// 恢复任务为 queued（重新入队）
    pub async fn requeue(db: &impl ConnectionTrait, task_id: Uuid) -> Result<NodeTask, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
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
              AND NOT (claimed_at IS NOT NULL AND payload_json->'native' IS NOT NULL AND payload_json->'native'<>'null'::jsonb)
            RETURNING *
            "#,
            [TASK_STATUS_QUEUED.into(), task_id.into()],
        );
        let task = NodeTask::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("NodeTask", task_id.to_string()))?;

        Ok(task)
    }

    /// 批量标记过期任务
    pub async fn expire_overdue_tasks(db: &impl ConnectionTrait) -> Result<Vec<NodeTask>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE node_tasks
            SET status = $1,
                finished_at = NOW(),
                updated_at = NOW()
            WHERE status IN ($2, $3)
              AND deadline_at < NOW()
            RETURNING *
            "#,
            [
                TASK_STATUS_EXPIRED.into(),
                TASK_STATUS_QUEUED.into(),
                TASK_STATUS_LEASED.into(),
            ],
        );
        let tasks = NodeTask::find_by_statement(stmt).all(db).await?;

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
