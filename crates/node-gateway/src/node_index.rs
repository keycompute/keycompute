//! Node Capability Index 实现
//!
//! 基于 PostgreSQL 实现 NodeCapabilityIndex trait，用于路由决策时检查是否存在 ready 节点。

use async_trait::async_trait;
use keycompute_routing::NodeCapabilityIndex;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use std::sync::Arc;

/// 基于 PostgreSQL 的 Node 能力索引
pub struct PostgresNodeIndex {
    pool: Arc<DatabaseConnection>,
}

impl PostgresNodeIndex {
    /// 创建新的 PostgresNodeIndex 实例
    pub fn new(pool: Arc<DatabaseConnection>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NodeCapabilityIndex for PostgresNodeIndex {
    /// 检查是否存在 ready 节点可以处理指定模型
    async fn has_ready_node(&self, model: &str) -> bool {
        let model_json: serde_json::Value = serde_json::json!([model]);

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT EXISTS (
                SELECT 1 FROM nodes n
                INNER JOIN node_sessions ns ON n.id = ns.node_id
                WHERE n.status = 'online'
                  AND ns.expires_at > NOW()
                  AND ns.revoked_at IS NULL
                  AND ns.accepted_models_json @> $1::jsonb
                  AND n.capabilities_json->>'runtime' = 'ollama'
                LIMIT 1
            )
            "#,
            [model_json.into()],
        );

        let result = self.pool.query_one(stmt).await;

        match result {
            Ok(Some(row)) => row.try_get_by_index::<bool>(0).unwrap_or(false),
            _ => false,
        }
    }
}
