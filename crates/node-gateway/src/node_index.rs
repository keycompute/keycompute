//! Node Capability Index 实现
//!
//! 基于 PostgreSQL 实现 NodeCapabilityIndex trait，用于路由决策时检查是否存在 ready 节点。

use async_trait::async_trait;
use keycompute_db::DbRouter;
use keycompute_routing::NodeCapabilityIndex;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::sync::Arc;

const READY_NODE_QUERY: &str = r#"
            SELECT EXISTS (
                SELECT 1 FROM nodes n
                INNER JOIN node_sessions ns ON n.id = ns.node_id
                INNER JOIN users u ON u.id = n.owner_user_id
                INNER JOIN tenants t ON t.id = u.tenant_id
                WHERE n.status = 'online'
                  AND t.status = 'active'
                  AND ns.expires_at > NOW()
                  AND ns.revoked_at IS NULL
                  AND ns.accepted_models_json @> $1::jsonb
                  AND n.capabilities_json->>'runtime' = 'ollama'
                LIMIT 1
            )
            "#;

/// 基于 PostgreSQL 的 Node 能力索引
pub struct PostgresNodeIndex {
    pool: Arc<DbRouter>,
}

impl PostgresNodeIndex {
    /// 创建新的 PostgresNodeIndex 实例
    pub fn new(pool: Arc<DbRouter>) -> Self {
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
            READY_NODE_QUERY,
            [model_json.into()],
        );

        // Routing is authorization-sensitive: a replica may still advertise a
        // node after its tenant was closed. Read the readiness predicate from
        // the writer so closure takes effect immediately.
        let result = self.pool.write_conn().query_one(stmt).await;

        match result {
            Ok(Some(row)) => row.try_get_by_index::<bool>(0).unwrap_or(false),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::READY_NODE_QUERY;

    #[test]
    fn ready_node_query_requires_an_active_owner_tenant() {
        assert!(READY_NODE_QUERY.contains("INNER JOIN tenants t"));
        assert!(READY_NODE_QUERY.contains("t.status = 'active'"));
        assert!(READY_NODE_QUERY.contains("ns.expires_at > NOW()"));
        assert!(READY_NODE_QUERY.contains("ns.revoked_at IS NULL"));
    }
}
