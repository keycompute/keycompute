//! 节点模型
//!
//! 节点注册信息表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 节点状态
pub const NODE_STATUS_ONLINE: &str = "online";
pub const NODE_STATUS_OFFLINE: &str = "offline";
pub const NODE_STATUS_EXCLUDED: &str = "excluded";

/// 节点模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct Node {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub client_instance_id: String,
    pub display_name: String,
    pub status: String,
    pub capabilities_json: serde_json::Value,
    pub consecutive_failure_count: i32,
    pub failure_threshold: i32,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建节点请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateNodeRequest {
    pub owner_user_id: Uuid,
    pub client_instance_id: String,
    pub display_name: String,
    pub capabilities_json: serde_json::Value,
}

impl Node {
    /// 创建新节点
    pub async fn create(db: &DatabaseConnection, req: &CreateNodeRequest) -> Result<Node, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO nodes (owner_user_id, client_instance_id, display_name, status, capabilities_json)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
            [
                req.owner_user_id.into(),
                req.client_instance_id.as_str().into(),
                req.display_name.as_str().into(),
                NODE_STATUS_OFFLINE.into(),
                req.capabilities_json.clone().into(),
            ],
        );
        let node = Node::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(node)
    }

    /// 根据 owner_user_id 和 client_instance_id 查询节点
    pub async fn find_by_owner_and_client(
        db: &DatabaseConnection,
        owner_user_id: Uuid,
        client_instance_id: &str,
    ) -> Result<Option<Node>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE owner_user_id = $1 AND client_instance_id = $2",
            [owner_user_id.into(), client_instance_id.into()],
        );
        let node = Node::find_by_statement(stmt).one(db).await?;

        Ok(node)
    }

    /// 根据 ID 查询节点
    pub async fn find_by_id(db: &DatabaseConnection, id: Uuid) -> Result<Option<Node>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM nodes WHERE id = $1",
            [id.into()],
        );
        let node = Node::find_by_statement(stmt).one(db).await?;

        Ok(node)
    }

    /// 更新节点状态
    pub async fn update_status(
        db: &DatabaseConnection,
        id: Uuid,
        status: &str,
    ) -> Result<Node, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING *",
            [status.into(), id.into()],
        );
        let node = Node::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("Node", id.to_string()))?;

        Ok(node)
    }

    /// 更新最后心跳时间
    pub async fn update_heartbeat(db: &DatabaseConnection, id: Uuid) -> Result<Node, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET last_heartbeat_at = NOW(), updated_at = NOW() WHERE id = $1 RETURNING *",
            [id.into()],
        );
        let node = Node::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("Node", id.to_string()))?;

        Ok(node)
    }

    /// 增加连续失败计数
    pub async fn increment_failure_count(
        db: &DatabaseConnection,
        id: Uuid,
    ) -> Result<Node, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET consecutive_failure_count = consecutive_failure_count + 1, updated_at = NOW() WHERE id = $1 RETURNING *",
            [id.into()],
        );
        let node = Node::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("Node", id.to_string()))?;

        Ok(node)
    }

    /// 重置失败计数
    pub async fn reset_failure_count(db: &DatabaseConnection, id: Uuid) -> Result<Node, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET consecutive_failure_count = 0, updated_at = NOW() WHERE id = $1 RETURNING *",
            [id.into()],
        );
        let node = Node::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("Node", id.to_string()))?;

        Ok(node)
    }

    /// 检查节点是否被排除
    pub fn is_excluded(&self) -> bool {
        self.status == NODE_STATUS_EXCLUDED
    }

    /// 检查节点是否在线
    pub fn is_online(&self) -> bool {
        self.status == NODE_STATUS_ONLINE
    }

    /// 删除节点（CASCADE 自动清理 sessions/submissions，tasks 的 assigned_node_id 设为 NULL）
    pub async fn delete(db: &DatabaseConnection, node_id: Uuid) -> Result<bool, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM nodes WHERE id = $1",
            [node_id.into()],
        );
        let result = db.execute(stmt).await?;

        Ok(result.rows_affected() > 0)
    }
}
