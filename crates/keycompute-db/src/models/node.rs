//! 节点模型
//!
//! 节点注册信息表的 ORM 模型

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 节点状态
pub const NODE_STATUS_ONLINE: &str = "online";
pub const NODE_STATUS_OFFLINE: &str = "offline";
pub const NODE_STATUS_EXCLUDED: &str = "excluded";

/// 节点模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    pub async fn create(pool: &sqlx::PgPool, req: &CreateNodeRequest) -> Result<Node, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            INSERT INTO nodes (owner_user_id, client_instance_id, display_name, status, capabilities_json)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(req.owner_user_id)
        .bind(&req.client_instance_id)
        .bind(&req.display_name)
        .bind(NODE_STATUS_OFFLINE)
        .bind(&req.capabilities_json)
        .fetch_one(pool)
        .await?;

        Ok(node)
    }

    /// 根据 owner_user_id 和 client_instance_id 查询节点
    pub async fn find_by_owner_and_client(
        pool: &sqlx::PgPool,
        owner_user_id: Uuid,
        client_instance_id: &str,
    ) -> Result<Option<Node>, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            SELECT * FROM nodes
            WHERE owner_user_id = $1 AND client_instance_id = $2
            "#,
        )
        .bind(owner_user_id)
        .bind(client_instance_id)
        .fetch_optional(pool)
        .await?;

        Ok(node)
    }

    /// 根据 ID 查询节点
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<Node>, DbError> {
        let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(node)
    }

    /// 更新节点状态
    pub async fn update_status(
        pool: &sqlx::PgPool,
        id: Uuid,
        status: &str,
    ) -> Result<Node, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            UPDATE nodes
            SET status = $1, updated_at = NOW()
            WHERE id = $2
            RETURNING *
            "#,
        )
        .bind(status)
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(node)
    }

    /// 更新最后心跳时间
    pub async fn update_heartbeat(pool: &sqlx::PgPool, id: Uuid) -> Result<Node, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            UPDATE nodes
            SET last_heartbeat_at = NOW(), updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(node)
    }

    /// 增加连续失败计数
    pub async fn increment_failure_count(pool: &sqlx::PgPool, id: Uuid) -> Result<Node, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            UPDATE nodes
            SET consecutive_failure_count = consecutive_failure_count + 1,
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_one(pool)
        .await?;

        Ok(node)
    }

    /// 重置失败计数
    pub async fn reset_failure_count(pool: &sqlx::PgPool, id: Uuid) -> Result<Node, DbError> {
        let node = sqlx::query_as::<_, Node>(
            r#"
            UPDATE nodes
            SET consecutive_failure_count = 0, updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_one(pool)
        .await?;

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
}
