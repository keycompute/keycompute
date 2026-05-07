//! Node Capability Index 实现
//!
//! 基于 PostgreSQL 实现 NodeCapabilityIndex trait，用于路由决策时检查是否存在 ready 节点。

use async_trait::async_trait;
use keycompute_routing::NodeCapabilityIndex;
use sqlx::PgPool;
use std::sync::Arc;

/// 基于 PostgreSQL 的 Node 能力索引
pub struct PostgresNodeIndex {
    pool: Arc<PgPool>,
}

impl PostgresNodeIndex {
    /// 创建新的 PostgresNodeIndex 实例
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NodeCapabilityIndex for PostgresNodeIndex {
    /// 检查是否存在 ready 节点可以处理指定模型
    ///
    /// Ready predicate 条件：
    /// - nodes.status = 'online'
    /// - node_sessions.expires_at > NOW()
    /// - node_sessions.revoked_at IS NULL
    /// - node_sessions.accepted_models_json 包含目标模型
    /// - nodes.capabilities_json->>'runtime' = 'ollama'
    async fn has_ready_node(&self, model: &str) -> bool {
        let model_json = serde_json::json!([model]).to_string();

        let result = sqlx::query_as::<_, (bool,)>(
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
        )
        .bind(model_json)
        .fetch_one(self.pool.as_ref())
        .await;

        match result {
            Ok((exists,)) => exists,
            Err(_) => false,
        }
    }
}
